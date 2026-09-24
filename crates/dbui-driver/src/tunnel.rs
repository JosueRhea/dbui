//! SSH tunnels, by way of the system's own `ssh`.
//!
//! The obvious alternative is an SSH library linked in. The system client wins
//! on what people with a bastion host already have set up: `~/.ssh/config`
//! (`ProxyJump`, `IdentityFile`, per-host users), the agent, hardware keys,
//! and a `known_hosts` file they trust. A library would honour none of that
//! without reimplementing it, and a tunnel that ignores the user's SSH config
//! is one that fails exactly where their terminal succeeds.
//!
//! The tunnel is `ssh -N -L 127.0.0.1:<local>:<db host>:<db port> bastion`,
//! and it lives exactly as long as the [`SshTunnel`] value: the adapter that
//! connected through it holds it, and dropping the adapter kills `ssh`.
//!
//! A password or key passphrase reaches `ssh` through `SSH_ASKPASS`: a
//! two-line helper script, private to this user, that prints the secret `ssh`
//! was handed in its environment. See [`Askpass`].

use crate::error::{DriverError, Result};
use dbui_domain::{ConnectionConfig, SshConfig};
use std::io::{BufRead as _, BufReader};
use std::net::{Ipv4Addr, TcpListener};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Set on the `ssh` child, and so on the askpass helper it runs: the secret to
/// answer with. The environment rather than the command line, because every
/// user on the machine can read another process's arguments with `ps`.
const ASKPASS_SECRET_VAR: &str = "DBUI_SSH_ASKPASS_SECRET";

/// How long `ssh` gets to authenticate and open the forward.
const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// A running `ssh -L`, killed on drop.
pub struct SshTunnel {
    /// The [`leashed`] shell that runs ssh, not ssh itself.
    child: Child,
    /// The leash: closing it -- here on drop, or by the OS when this process
    /// dies -- is what stops ssh.
    leash: Option<ChildStdin>,
    local_port: u16,
    /// Kept until the tunnel closes: ssh asks again when it re-keys.
    _askpass: Option<Askpass>,
}

impl SshTunnel {
    /// Start the tunnel for `config` and wait until its local end accepts.
    pub async fn open(config: &ConnectionConfig) -> Result<Self> {
        let ssh = &config.ssh;
        let bastion = ssh.summary();
        let local_port = free_port().map_err(|error| tunnel_error(&bastion, error.to_string()))?;

        let mut command = leashed("ssh");
        command
            .args(ssh_args(ssh, local_port, &config.host, config.port))
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let askpass = if ssh.password.is_empty() {
            None
        } else {
            Some(Askpass::write().map_err(|error| {
                tunnel_error(
                    &bastion,
                    format!("could not prepare the password helper: {error}"),
                )
            })?)
        };
        if let Some(askpass) = &askpass {
            // `SSH_ASKPASS_REQUIRE=force` makes ssh use the helper even with
            // a terminal attached, which is what a `cargo run` has.
            command
                .env("SSH_ASKPASS", &askpass.script)
                .env("SSH_ASKPASS_REQUIRE", "force")
                // Older clients only consult askpass when DISPLAY is set.
                .env(
                    "DISPLAY",
                    std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into()),
                )
                .env(ASKPASS_SECRET_VAR, &ssh.password);
        }

        let mut child = command.spawn().map_err(|error| {
            let message = if error.kind() == std::io::ErrorKind::NotFound {
                "the `ssh` command is not installed".to_string()
            } else {
                error.to_string()
            };
            tunnel_error(&bastion, message)
        })?;

        // Drained on a thread for the life of the process: a pipe nobody
        // reads fills, and ssh blocks writing a warning into it.
        let log = Arc::new(Mutex::new(SshLog::default()));
        if let Some(pipe) = child.stderr.take() {
            let sink = Arc::clone(&log);
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines() {
                    let Ok(line) = line else { break };
                    sink.lock().unwrap_or_else(|p| p.into_inner()).push(&line);
                }
            });
        }

        let leash = child.stdin.take();
        let mut tunnel = SshTunnel {
            child,
            leash,
            local_port,
            _askpass: askpass,
        };
        let started = Instant::now();
        loop {
            if let Ok(Some(status)) = tunnel.child.try_wait() {
                // Give the reader a moment to take the last of the pipe.
                std::thread::sleep(Duration::from_millis(50));
                let log = log.lock().unwrap_or_else(|p| p.into_inner());
                return Err(tunnel_error(&bastion, log.explain_exit(status.code())));
            }
            if log.lock().unwrap_or_else(|p| p.into_inner()).ready {
                return Ok(tunnel);
            }
            if started.elapsed() > READY_TIMEOUT {
                // Dropping `tunnel` on the way out stops ssh.
                return Err(tunnel_error(
                    &bastion,
                    format!(
                        "no answer after {} s (is the host reachable?)",
                        READY_TIMEOUT.as_secs()
                    ),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// `config`, re-aimed at the tunnel's local end.
    pub fn rewrite(&self, config: &ConnectionConfig) -> ConnectionConfig {
        let mut through = config.clone();
        through.host = Ipv4Addr::LOCALHOST.to_string();
        through.port = self.local_port;
        through
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        // Let go of the leash; the shell stops ssh and exits after it.
        drop(self.leash.take());
        let _ = self.child.wait();
    }
}

/// `program`, run so that it cannot outlive this process.
///
/// `Drop` does not run when the app is force-quit or crashes, and an ssh left
/// behind keeps a forward into production open on a loopback port for as
/// long as the machine stays up. So ssh runs under a small shell that also
/// reads a pipe from us: the pipe reaches end-of-file when we close it *or*
/// when the OS tears this process down for any reason, and on end-of-file
/// the shell kills ssh. It then exits with ssh's own status, so an ssh that
/// gives up on its own still reads as an exit here. Linux's `PDEATHSIG`
/// would do the same job on one platform; this does it on both.
fn leashed(program: &str) -> Command {
    // The reader gets the pipe as fd 3, explicitly: a background job in a
    // non-interactive shell otherwise has its stdin replaced by /dev/null.
    const LEASH: &str = r#"exec 3<&0
"$@" </dev/null 3<&- &
child=$!
(read _ignored <&3; kill "$child" 2>/dev/null) &
reader=$!
exec 3<&-
wait "$child"
status=$?
kill "$reader" 2>/dev/null
exit "$status"
"#;
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(LEASH)
        .arg("dbui-ssh")
        .arg(program)
        .stdin(Stdio::piped());
    command
}

/// The arguments for `ssh`, separated out so a test can read them.
pub(crate) fn ssh_args(
    ssh: &SshConfig,
    local_port: u16,
    db_host: &str,
    db_port: u16,
) -> Vec<String> {
    let mut args = vec![
        // No remote command, no terminal: just the forward.
        "-N".to_string(),
        // Verbose, for one line: "Local forwarding listening on ...", which
        // is the only reliable word that the forward is up. Connecting to the
        // port to find out proves nothing -- anything in between that accepts
        // local connections (a proxy, a firewall agent) answers first.
        "-v".into(),
        "-T".into(),
        "-L".into(),
        format!(
            "127.0.0.1:{local_port}:{}:{db_port}",
            bracket_ipv6(db_host.trim())
        ),
        "-p".into(),
        ssh.port.to_string(),
        // A forward that cannot bind is a failure, not a warning -- otherwise
        // ssh stays up with nothing listening and the database dial hangs.
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-o".into(),
        "ConnectTimeout=15".into(),
        // Notice a dead link instead of holding a socket open forever.
        "-o".into(),
        "ServerAliveInterval=30".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        // There is no terminal to answer "are you sure you want to continue
        // connecting?". Trust a host on first use, and still refuse one whose
        // key has *changed* -- the case that check exists for.
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
    ];
    if ssh.password.is_empty() {
        // Nothing to answer a prompt with, so fail rather than wait for one.
        args.push("-o".into());
        args.push("BatchMode=yes".into());
    } else {
        // Answer at most once: a wrong password tried three times is three
        // failed logins in the bastion's audit log, and maybe a lockout.
        args.push("-o".into());
        args.push("NumberOfPasswordPrompts=1".into());
    }
    let key = ssh.key_path.trim();
    if !key.is_empty() {
        args.push("-i".into());
        args.push(expand_home(key));
        // Only that key: an agent offering five others first can trip the
        // server's MaxAuthTries before the one that works is tried.
        args.push("-o".into());
        args.push("IdentitiesOnly=yes".into());
    }
    let username = ssh.username.trim();
    if !username.is_empty() {
        args.push("-l".into());
        args.push(username.into());
    }
    // `--` so a host that starts with a dash is a host, not an option.
    args.push("--".into());
    args.push(ssh.host.trim().into());
    args
}

/// The `SSH_ASKPASS` helper: a script in a directory only this user can
/// enter, removed with the tunnel.
///
/// It prints the secret for a password or passphrase prompt and nothing for
/// anything else -- a yes/no about a host key gets no answer, which ssh reads
/// as no. The secret itself is never in the file; it comes from the
/// environment ssh passes down.
struct Askpass {
    dir: std::path::PathBuf,
    script: std::path::PathBuf,
}

impl Askpass {
    const SCRIPT: &'static str = "#!/bin/sh\n\
        case \"$1\" in\n\
        *[Pp]assword*|*[Pp]assphrase*|*PIN*) printf '%s\\n' \"$DBUI_SSH_ASKPASS_SECRET\" ;;\n\
        esac\n";

    fn write() -> std::io::Result<Self> {
        use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "dbui-askpass-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let script = dir.join("askpass");
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&script)
            .and_then(|mut file| std::io::Write::write_all(&mut file, Self::SCRIPT.as_bytes()));
        if let Err(error) = written {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(error);
        }
        Ok(Self { dir, script })
    }
}

impl Drop for Askpass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn tunnel_error(bastion: &str, message: String) -> DriverError {
    DriverError::Connect {
        address: format!("SSH {bastion}"),
        message,
    }
}

/// What ssh has said on stderr: whether the forward is up, and the last few
/// lines worth showing if it never comes up.
#[derive(Default)]
struct SshLog {
    ready: bool,
    said: std::collections::VecDeque<String>,
}

impl SshLog {
    fn push(&mut self, line: &str) {
        let line = line.trim();
        if line.contains("forwarding listening on") {
            self.ready = true;
        }
        // `-v` is only there for the line above; its chatter is not an
        // explanation, and neither is ssh noting a first-seen host key.
        if line.is_empty()
            || line.starts_with("debug")
            || line.starts_with("OpenSSH_")
            || line.starts_with("Warning: Permanently added")
            || line.starts_with("Authenticated to")
            || line.starts_with("Transferred:")
        {
            return;
        }
        if self.said.len() == 8 {
            self.said.pop_front();
        }
        self.said.push_back(line.to_string());
    }

    /// A sentence for the status bar: the last thing ssh said.
    fn explain_exit(&self, code: Option<i32>) -> String {
        match (self.said.back(), code) {
            (Some(line), _) => line.trim_end_matches('.').to_string(),
            (None, Some(code)) => format!("ssh exited with status {code}"),
            (None, None) => "ssh was stopped".into(),
        }
    }
}

/// A port nothing is listening on, found by asking the OS for one.
///
/// Released before ssh binds it, so another process could take it in between;
/// `ExitOnForwardFailure` turns that rare race into an error, not a hang.
fn free_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    Ok(listener.local_addr()?.port())
}

fn bracket_ipv6(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn expand_home(path: &str) -> String {
    match (path.strip_prefix("~/"), dirs_home()) {
        (Some(rest), Some(home)) => format!("{home}/{rest}"),
        _ => path.to_string(),
    }
}

fn dirs_home() -> Option<String> {
    std::env::var("HOME").ok().filter(|home| !home.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bastion() -> SshConfig {
        SshConfig {
            enabled: true,
            host: "bastion.example.com".into(),
            port: 2222,
            username: "deploy".into(),
            key_path: String::new(),
            password: String::new(),
        }
    }

    #[test]
    fn the_forward_points_at_the_database_as_the_bastion_sees_it() {
        let args = ssh_args(&bastion(), 40000, "db.internal", 5432);
        let forward = args.iter().position(|a| a == "-L").unwrap();
        assert_eq!(args[forward + 1], "127.0.0.1:40000:db.internal:5432");
        assert!(args.windows(2).any(|w| w == ["-p", "2222"]));
        assert!(args.windows(2).any(|w| w == ["-l", "deploy"]));
        assert_eq!(args.last().unwrap(), "bastion.example.com");
        assert_eq!(args[args.len() - 2], "--");
    }

    #[test]
    fn without_a_password_ssh_never_waits_for_one() {
        let args = ssh_args(&bastion(), 1, "db", 1);
        assert!(args.iter().any(|a| a == "BatchMode=yes"));

        let mut with = bastion();
        with.password = "secret".into();
        let args = ssh_args(&with, 1, "db", 1);
        assert!(!args.iter().any(|a| a == "BatchMode=yes"));
        assert!(args.iter().any(|a| a == "NumberOfPasswordPrompts=1"));
        // The secret travels in the environment, never on the command line
        // where `ps` shows it to every user on the machine.
        assert!(!args.iter().any(|a| a.contains("secret")));
    }

    #[test]
    fn a_key_file_is_the_only_identity_offered() {
        let mut ssh = bastion();
        ssh.key_path = "/keys/id_ed25519".into();
        let args = ssh_args(&ssh, 1, "db", 1);
        assert!(args.windows(2).any(|w| w == ["-i", "/keys/id_ed25519"]));
        assert!(args.iter().any(|a| a == "IdentitiesOnly=yes"));
    }

    #[test]
    fn an_ipv6_database_host_is_bracketed() {
        let args = ssh_args(&bastion(), 1, "fd00::5", 5432);
        assert!(args.iter().any(|a| a == "127.0.0.1:1:[fd00::5]:5432"));
    }

    #[test]
    fn a_blank_user_is_left_to_the_ssh_config() {
        let mut ssh = bastion();
        ssh.username = "  ".into();
        let args = ssh_args(&ssh, 1, "db", 1);
        assert!(!args.iter().any(|a| a == "-l"));
    }

    fn log(lines: &[&str]) -> SshLog {
        let mut log = SshLog::default();
        for line in lines {
            log.push(line);
        }
        log
    }

    #[test]
    fn the_exit_message_is_the_last_thing_ssh_said() {
        let log = log(&[
            "OpenSSH_9.6p1, LibreSSL 3.3.6",
            "debug1: Reading configuration data /etc/ssh/ssh_config",
            "Warning: Permanently added 'x' (ED25519) to the list of known hosts.",
            "deploy@bastion: Permission denied (publickey).",
            "debug1: No more authentication methods to try.",
        ]);
        assert!(!log.ready);
        assert_eq!(
            log.explain_exit(Some(255)),
            "deploy@bastion: Permission denied (publickey)"
        );
        assert_eq!(
            SshLog::default().explain_exit(Some(255)),
            "ssh exited with status 255"
        );
    }

    #[test]
    fn the_forward_is_up_when_ssh_says_it_is_listening() {
        let log = log(&[
            "debug1: Authentication succeeded (publickey).",
            "Authenticated to bastion ([10.0.0.1]:22) using \"publickey\".",
            "debug1: Local connections to 127.0.0.1:40000 forwarded to remote address db:5432",
            "debug1: Local forwarding listening on 127.0.0.1 port 40000.",
        ]);
        assert!(log.ready);
    }

    #[test]
    fn a_free_port_can_be_bound() {
        let port = free_port().unwrap();
        assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok());
    }

    #[test]
    fn the_tunnel_takes_over_host_and_port_and_nothing_else() {
        let mut config = ConnectionConfig::new(dbui_domain::Driver::Postgres);
        config.host = "db.internal".into();
        config.database = "app".into();
        let child = Command::new("true").spawn().unwrap();
        let tunnel = SshTunnel {
            child,
            leash: None,
            local_port: 41234,
            _askpass: None,
        };
        let through = tunnel.rewrite(&config);
        assert_eq!(through.host, "127.0.0.1");
        assert_eq!(through.port, 41234);
        assert_eq!(through.database, "app");
    }

    #[test]
    fn the_helper_answers_a_password_prompt_and_nothing_else() {
        let askpass = Askpass::write().unwrap();
        let ask = |prompt: &str| {
            let out = Command::new(&askpass.script)
                .arg(prompt)
                .env(ASKPASS_SECRET_VAR, "s3cret $x")
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        assert_eq!(ask("deploy@bastion's password: "), "s3cret $x\n");
        assert_eq!(ask("Enter passphrase for key '/k': "), "s3cret $x\n");
        assert_eq!(
            ask("Are you sure you want to continue connecting (yes/no)? "),
            ""
        );

        let dir = askpass.dir.clone();
        drop(askpass);
        assert!(!dir.exists(), "the helper goes with the tunnel");
    }

    fn alive(pid: &str) -> bool {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Whatever ends the tunnel's owner -- a drop, a crash, a force quit --
    /// closes the leash, and the leash takes the program with it.
    #[test]
    fn the_program_dies_with_the_leash() {
        let mut child = leashed("sh")
            .args(["-c", "echo $$; exec sleep 30"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut pid = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut pid)
            .unwrap();
        let pid = pid.trim().to_string();
        assert!(alive(&pid), "running while held");

        drop(child.stdin.take());
        let status = child.wait().unwrap();
        assert!(!status.success(), "killed, not finished: {status:?}");
        assert!(!alive(&pid), "and gone with the leash");
    }

    /// A program that exits by itself is seen to, with its own status.
    #[test]
    fn the_programs_own_exit_comes_through() {
        let mut child = leashed("sh").args(["-c", "exit 7"]).spawn().unwrap();
        // Held, as `SshTunnel` holds it: `Child::wait` closes stdin before it
        // waits, which would pull the leash and race the exit being tested.
        let _leash = child.stdin.take();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(status.code(), Some(7));
    }
}
