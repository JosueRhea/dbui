//! Updating dbui from its own GitHub releases.
//!
//! Three steps, deliberately separate so the UI can put a decision between
//! them: [`check`] asks what the latest release is, [`download`] fetches and
//! verifies it into a staging directory, and [`install`] swaps the bundle and
//! relaunches. Nothing happens without the user saying so except the check.
//!
//! The security boundary is [`install`]: it refuses any bundle that is not
//! signed and notarized by the same team as the copy that is running. A
//! download over TLS from a URL that redirects wherever GitHub likes is not on
//! its own a reason to execute what comes back.
//!
//! macOS only for now. Every other target compiles to "no updates available",
//! which is the truthful answer until there is something to update *to*.

use std::path::{Path, PathBuf};

use crate::runtime::{DbRuntime, Task};

/// Where releases come from. A constant rather than configuration: an updater
/// that can be pointed somewhere else is an updater that can be pointed
/// somewhere hostile.
pub const REPO: &str = "JosueRhea/dbui";

const API: &str = "https://api.github.com";
const USER_AGENT: &str = concat!("dbui/", env!("CARGO_PKG_VERSION"));
/// The universal .zip, not the .dmg: a zip expands in place, where a disk image
/// would have to be mounted first.
const ASSET_SUFFIX: &str = "-universal.zip";
const CHECKSUMS: &str = "SHA256SUMS";

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("Could not reach GitHub: {0}")]
    Network(String),
    #[error("GitHub returned something unexpected: {0}")]
    Protocol(String),
    #[error("Release {tag} has no {suffix} asset")]
    NoAsset { tag: String, suffix: String },
    #[error("The download did not match its published checksum")]
    Checksum,
    #[error("The downloaded app is not signed by the team that built this one")]
    Signature(String),
    #[error("dbui is not running from an installed .app bundle")]
    NotBundled,
    #[error("Cannot write to {0} -- move dbui to /Applications and try again")]
    NotWritable(PathBuf),
    #[error("{0}")]
    Io(String),
}

/// A release newer than the one running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub version: Version,
    pub tag: String,
    pub notes: String,
    pub url: String,
    pub size: u64,
    /// From the release's `SHA256SUMS`. `None` if the release predates it, in
    /// which case the signature check in [`install`] is the only gate.
    pub sha256: Option<String>,
}

/// A downloaded, checksum-verified bundle waiting to be installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub version: Version,
    /// The expanded `dbui.app` inside the staging directory.
    app: PathBuf,
    /// Removed wholesale when the staged update is dropped or installed.
    dir: PathBuf,
}

// -- versions -------------------------------------------------------------

/// `MAJOR.MINOR.PATCH` with an optional pre-release tag.
///
/// Not a full semver implementation -- no build metadata, and pre-release
/// identifiers compare as one string rather than dot-separated fields. That is
/// enough for tags this project actually cuts, and it is 40 lines instead of a
/// dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub pre: Option<String>,
}

impl Version {
    /// Parses `1.2.3`, `v1.2.3` and `1.2.3-alpha`. Returns `None` for anything
    /// else, which callers treat as "no update" rather than guessing.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().strip_prefix('v').unwrap_or(text.trim());
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) if !pre.is_empty() => (core, Some(pre.to_string())),
            Some(_) => return None,
            None => (text, None),
        };
        let mut parts = core.split('.');
        let mut next = || parts.next()?.parse::<u32>().ok();
        let (major, minor, patch) = (next()?, next()?, next()?);
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                // 1.0.0 is newer than 1.0.0-alpha: a pre-release sorts *before*
                // the release it leads to.
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        Ok(())
    }
}

/// The version compiled into this binary.
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("our own version parses")
}

// -- check ----------------------------------------------------------------

/// Ask GitHub for the latest release; `None` when it is not newer than ours.
pub fn check(runtime: &DbRuntime) -> Task<Result<Option<Update>, UpdateError>> {
    runtime.spawn(async move { fetch_latest(current_version()).await })
}

async fn fetch_latest(current: Version) -> Result<Option<Update>, UpdateError> {
    let client = client()?;
    let url = format!("{API}/repos/{REPO}/releases/latest");
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| UpdateError::Network(e.to_string()))?;

    // A repository with no releases yet answers 404, and so does one that has
    // been renamed. Neither is an error worth showing: there is simply nothing
    // to update to. Reporting it would put "update failed" in front of every
    // user until the first release was cut.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    let body: serde_json::Value = response
        .error_for_status()
        .map_err(|e| UpdateError::Protocol(e.to_string()))?
        .json()
        .await
        .map_err(|e| UpdateError::Protocol(e.to_string()))?;

    let Some(update) = parse_release(&body, &current)? else {
        return Ok(None);
    };

    // The checksums file is best-effort: a release without one still installs,
    // because `install` verifies the signature either way.
    let sha256 = match checksum_for(&client, &body, &update.url).await {
        Ok(sum) => sum,
        Err(_) => None,
    };
    Ok(Some(Update { sha256, ..update }))
}

/// Pull an [`Update`] out of GitHub's release JSON, or `None` if it is not
/// newer than `current`. Split out from the request so it can be tested.
fn parse_release(
    body: &serde_json::Value,
    current: &Version,
) -> Result<Option<Update>, UpdateError> {
    let tag = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| UpdateError::Protocol("release has no tag_name".into()))?;

    let Some(version) = Version::parse(tag) else {
        return Ok(None);
    };
    if version <= *current {
        return Ok(None);
    }

    let assets = body.get("assets").and_then(|v| v.as_array());
    let asset = assets
        .into_iter()
        .flatten()
        .find(|a| {
            a.get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|n| n.ends_with(ASSET_SUFFIX))
        })
        .ok_or_else(|| UpdateError::NoAsset {
            tag: tag.to_string(),
            suffix: ASSET_SUFFIX.to_string(),
        })?;

    Ok(Some(Update {
        version,
        tag: tag.to_string(),
        notes: body
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        url: asset
            .get("browser_download_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| UpdateError::Protocol("asset has no download URL".into()))?
            .to_string(),
        size: asset.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
        sha256: None,
    }))
}

async fn checksum_for(
    client: &reqwest::Client,
    body: &serde_json::Value,
    asset_url: &str,
) -> Result<Option<String>, UpdateError> {
    let Some(sums_url) = body
        .get("assets")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(CHECKSUMS))
        .and_then(|a| a.get("browser_download_url"))
        .and_then(|v| v.as_str())
    else {
        return Ok(None);
    };

    let text = client
        .get(sums_url)
        .send()
        .await
        .map_err(|e| UpdateError::Network(e.to_string()))?
        .text()
        .await
        .map_err(|e| UpdateError::Protocol(e.to_string()))?;

    let wanted = asset_url.rsplit('/').next().unwrap_or_default();
    Ok(find_checksum(&text, wanted))
}

/// Look a filename up in `shasum -a 256` output.
fn find_checksum(sums: &str, filename: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (sum, name) = line.split_once(char::is_whitespace)?;
        // `shasum` writes "<sum>  <name>"; the name may carry a `*` for binary.
        let name = name.trim().trim_start_matches('*');
        (name == filename && sum.len() == 64).then(|| sum.to_ascii_lowercase())
    })
}

// -- download -------------------------------------------------------------

/// Fetch the release zip, check it against its published SHA-256, and expand it
/// into a staging directory beside the installed app.
pub fn download(runtime: &DbRuntime, update: Update) -> Task<Result<Staged, UpdateError>> {
    runtime.spawn(async move { fetch_and_stage(update).await })
}

async fn fetch_and_stage(update: Update) -> Result<Staged, UpdateError> {
    let client = client()?;
    let bytes = client
        .get(&update.url)
        .send()
        .await
        .map_err(|e| UpdateError::Network(e.to_string()))?
        .error_for_status()
        .map_err(|e| UpdateError::Protocol(e.to_string()))?
        .bytes()
        .await
        .map_err(|e| UpdateError::Network(e.to_string()))?;

    if let Some(expected) = &update.sha256 {
        if sha256_hex(&bytes) != *expected {
            return Err(UpdateError::Checksum);
        }
    }

    // Stage next to the installed bundle so the final swap is a rename within
    // one filesystem, which is atomic. A staging area in /tmp could be on a
    // different volume and turn the swap into a slow, interruptible copy.
    let installed = installed_app()?;
    let parent = installed
        .parent()
        .ok_or_else(|| UpdateError::Io("installed app has no parent directory".into()))?;
    ensure_writable(parent)?;

    let dir = parent.join(format!(".dbui-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| UpdateError::Io(e.to_string()))?;

    let zip = dir.join("dbui.zip");
    std::fs::write(&zip, &bytes).map_err(|e| UpdateError::Io(e.to_string()))?;

    // `ditto -x -k` rather than `unzip`: it is what created the archive, and it
    // is the only extractor that restores the bundle's extended attributes --
    // including the stapled notarization ticket.
    run("/usr/bin/ditto", &["-x", "-k", path(&zip)?, path(&dir)?])?;
    let _ = std::fs::remove_file(&zip);

    let app = dir.join("dbui.app");
    if !app.is_dir() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(UpdateError::Io("the archive held no dbui.app".into()));
    }

    Ok(Staged {
        version: update.version,
        app,
        dir,
    })
}

// -- install --------------------------------------------------------------

/// Verify the staged bundle, swap it over the installed one, and relaunch.
///
/// Returns only on failure: on success the process has already been replaced by
/// a fresh one and this call ends with `exit(0)`.
pub fn install(staged: &Staged) -> Result<std::convert::Infallible, UpdateError> {
    let installed = installed_app()?;
    ensure_writable(
        installed
            .parent()
            .ok_or_else(|| UpdateError::Io("installed app has no parent".into()))?,
    )?;

    verify_bundle(&staged.app)?;

    // Move the old one aside rather than deleting it, so a failed swap can put
    // things back. macOS lets a running bundle be renamed -- this process keeps
    // the file handles it already opened.
    let backup = installed.with_extension("app.old");
    let _ = std::fs::remove_dir_all(&backup);
    std::fs::rename(&installed, &backup).map_err(|e| UpdateError::Io(e.to_string()))?;

    if let Err(error) = std::fs::rename(&staged.app, &installed) {
        // Put the working copy back before reporting: an update that fails must
        // not leave the machine with no dbui at all.
        let _ = std::fs::rename(&backup, &installed);
        return Err(UpdateError::Io(error.to_string()));
    }

    let _ = std::fs::remove_dir_all(&backup);
    let _ = std::fs::remove_dir_all(&staged.dir);

    // `-n` forces a new instance: without it `open` would just focus the copy
    // that is still running and about to exit.
    run("/usr/bin/open", &["-n", path(&installed)?])?;
    std::process::exit(0);
}

/// Refuse anything not signed and notarized by the team that signed us.
///
/// `spctl` is the same assessment Gatekeeper runs, so this is the check a user
/// would get on first open -- done before the swap instead of after.
fn verify_bundle(app: &Path) -> Result<(), UpdateError> {
    run("/usr/bin/codesign", &["--verify", "--deep", "--strict", path(app)?])
        .map_err(|e| UpdateError::Signature(format!("codesign rejected it: {e}")))?;

    run(
        "/usr/sbin/spctl",
        &["--assess", "--type", "execute", path(app)?],
    )
    .map_err(|e| UpdateError::Signature(format!("Gatekeeper rejected it: {e}")))?;

    // A valid Developer ID signature is not enough on its own -- it only says
    // *somebody* signed it. Require the same team as the running copy.
    let ours = team_identifier(&installed_app()?)?;
    let theirs = team_identifier(app)?;
    if ours != theirs {
        return Err(UpdateError::Signature(format!(
            "signed by team {theirs}, expected {ours}"
        )));
    }
    Ok(())
}

fn team_identifier(app: &Path) -> Result<String, UpdateError> {
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["-dv", "--verbose=4"])
        .arg(app)
        .output()
        .map_err(|e| UpdateError::Signature(e.to_string()))?;
    // codesign writes its report to stderr.
    let text = String::from_utf8_lossy(&out.stderr);
    find_team_identifier(&text)
        .ok_or_else(|| UpdateError::Signature("no TeamIdentifier in the signature".into()))
}

fn find_team_identifier(report: &str) -> Option<String> {
    report.lines().find_map(|line| {
        line.trim()
            .strip_prefix("TeamIdentifier=")
            .filter(|id| !id.is_empty() && *id != "not set")
            .map(str::to_string)
    })
}

// -- placement ------------------------------------------------------------

/// The `dbui.app` this process is running out of.
///
/// `Err(NotBundled)` for a `cargo run` build, which is the signal to leave the
/// updater switched off rather than try to replace a target/ directory.
pub fn installed_app() -> Result<PathBuf, UpdateError> {
    let exe = std::env::current_exe().map_err(|e| UpdateError::Io(e.to_string()))?;
    // .../dbui.app/Contents/MacOS/dbui -> .../dbui.app
    let app = exe
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or(UpdateError::NotBundled)?;
    if app.extension().is_some_and(|e| e == "app") && app.join("Contents/Info.plist").is_file() {
        Ok(app.to_path_buf())
    } else {
        Err(UpdateError::NotBundled)
    }
}

/// Whether this build can update itself at all. False for `cargo run`.
pub fn is_updatable() -> bool {
    cfg!(target_os = "macos") && installed_app().is_ok()
}

fn ensure_writable(dir: &Path) -> Result<(), UpdateError> {
    let probe = dir.join(format!(".dbui-write-test-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(_) => Err(UpdateError::NotWritable(dir.to_path_buf())),
    }
}

// -- plumbing -------------------------------------------------------------

fn client() -> Result<reqwest::Client, UpdateError> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| UpdateError::Network(e.to_string()))
}

fn path(p: &Path) -> Result<&str, UpdateError> {
    p.to_str()
        .ok_or_else(|| UpdateError::Io(format!("{} is not valid UTF-8", p.display())))
}

fn run(program: &str, args: &[&str]) -> Result<(), UpdateError> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| UpdateError::Io(format!("{program}: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(UpdateError::Io(format!(
        "{program} failed: {}",
        stderr.trim()
    )))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("parses")
    }

    #[test]
    fn versions_parse_with_and_without_the_v() {
        assert_eq!(v("1.2.3"), v("v1.2.3"));
        assert_eq!(v("0.1.0").patch, 0);
        assert_eq!(v("1.2.3-alpha").pre.as_deref(), Some("alpha"));
    }

    #[test]
    fn rubbish_versions_are_refused_rather_than_guessed() {
        for text in ["", "1.2", "1.2.3.4", "one.two.three", "v", "1.2.3-"] {
            assert!(Version::parse(text).is_none(), "{text} should not parse");
        }
    }

    #[test]
    fn versions_order_by_field_then_prerelease() {
        assert!(v("1.0.0") > v("0.9.9"));
        assert!(v("0.2.0") > v("0.1.9"));
        assert!(v("0.1.10") > v("0.1.9"));
        // A pre-release leads to its version, so it sorts before it.
        assert!(v("1.0.0") > v("1.0.0-alpha"));
        assert!(v("1.0.0-beta") > v("1.0.0-alpha"));
        assert_eq!(v("1.2.3"), v("1.2.3"));
    }

    fn release(tag: &str, asset: &str) -> serde_json::Value {
        serde_json::json!({
            "tag_name": tag,
            "body": "notes",
            "assets": [
                { "name": "SHA256SUMS", "browser_download_url": "https://x/SHA256SUMS", "size": 90 },
                { "name": asset, "browser_download_url": format!("https://x/{asset}"), "size": 1234 },
            ]
        })
    }

    #[test]
    fn a_newer_release_becomes_an_update() {
        let body = release("v9.9.9", "dbui-9.9.9-universal.zip");
        let update = parse_release(&body, &v("0.1.0")).unwrap().expect("newer");
        assert_eq!(update.version, v("9.9.9"));
        assert_eq!(update.url, "https://x/dbui-9.9.9-universal.zip");
        assert_eq!(update.size, 1234);
    }

    #[test]
    fn the_same_or_an_older_release_is_not_an_update() {
        let body = release("v0.1.0", "dbui-0.1.0-universal.zip");
        assert!(parse_release(&body, &v("0.1.0")).unwrap().is_none());
        assert!(parse_release(&body, &v("0.2.0")).unwrap().is_none());
    }

    #[test]
    fn a_release_with_no_universal_zip_is_an_error_not_a_silent_skip() {
        // Silently reporting "up to date" for a broken release would hide a
        // packaging mistake until someone noticed nobody had updated in weeks.
        let body = release("v9.9.9", "dbui-9.9.9.dmg");
        assert!(matches!(
            parse_release(&body, &v("0.1.0")),
            Err(UpdateError::NoAsset { .. })
        ));
    }

    #[test]
    fn an_unparseable_tag_is_treated_as_no_update() {
        let body = release("nightly", "dbui-x-universal.zip");
        assert!(parse_release(&body, &v("0.1.0")).unwrap().is_none());
    }

    #[test]
    fn checksums_are_looked_up_by_filename() {
        let sums = "\
1111111111111111111111111111111111111111111111111111111111111111  dbui-1.0.0-universal.dmg
2222222222222222222222222222222222222222222222222222222222222222  dbui-1.0.0-universal.zip
";
        assert_eq!(
            find_checksum(sums, "dbui-1.0.0-universal.zip").as_deref(),
            Some("2222222222222222222222222222222222222222222222222222222222222222")
        );
        assert!(find_checksum(sums, "dbui-9.9.9-universal.zip").is_none());
    }

    #[test]
    fn the_team_identifier_is_read_out_of_codesigns_report() {
        let report = "\
Executable=/Applications/dbui.app/Contents/MacOS/dbui
Identifier=com.gzenit.dbui
TeamIdentifier=D7HN42D467
Sealed Resources version=2
";
        assert_eq!(find_team_identifier(report).as_deref(), Some("D7HN42D467"));
        // An ad-hoc signature reports this literally; it must not be accepted
        // as a team, or any locally-signed bundle would pass the check.
        assert!(find_team_identifier("TeamIdentifier=not set").is_none());
        assert!(find_team_identifier("no signature here").is_none());
    }

    #[test]
    fn sha256_matches_the_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn our_own_version_parses() {
        assert_eq!(current_version().to_string(), env!("CARGO_PKG_VERSION"));
    }
}

/// Live checks against the real GitHub API. Ignored by default so the suite
/// stays offline; run with `cargo test -p dbui-app -- --ignored --nocapture`.
#[cfg(test)]
mod live {
    use super::*;

    /// The whole check, against whatever is actually published right now:
    /// the tag parses, the universal .zip is there, and its checksum is found
    /// in SHA256SUMS. A release that fails this is one the updater could not
    /// install, and this is the only test that would notice.
    #[tokio::test]
    #[ignore = "hits the network"]
    async fn the_published_release_is_installable_by_an_older_build() {
        let update = fetch_latest(Version::parse("0.0.1").unwrap())
            .await
            .expect("the check succeeds")
            .expect("something newer than 0.0.1 is published");

        assert!(
            update.url.ends_with(ASSET_SUFFIX),
            "asset {} is not the universal zip",
            update.url
        );
        assert!(update.size > 0, "asset has no size");
        let sum = update
            .sha256
            .as_deref()
            .expect("the release publishes SHA256SUMS covering its zip");
        assert_eq!(sum.len(), 64, "not a sha256: {sum}");
    }

    /// ...and the running build is never offered itself.
    #[tokio::test]
    #[ignore = "hits the network"]
    async fn the_current_version_is_not_offered_an_update() {
        let got = fetch_latest(current_version()).await;
        assert!(matches!(got, Ok(None)), "expected no update, got {got:?}");
    }

    #[tokio::test]
    #[ignore = "hits the network"]
    async fn a_real_release_parses_end_to_end() {
        // A repo that definitely has releases with assets and a checksums file,
        // used only to prove the parsing path against real API output.
        let client = client().unwrap();
        let body: serde_json::Value = client
            .get(format!("{API}/repos/BurntSushi/ripgrep/releases/latest"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let tag = body["tag_name"].as_str().unwrap();
        assert!(Version::parse(tag).is_some(), "tag {tag} should parse");
        // ripgrep publishes no `-universal.zip`, so this is the NoAsset path --
        // which is exactly the error a mis-packaged dbui release would give.
        assert!(matches!(
            parse_release(&body, &Version::parse("0.0.1").unwrap()),
            Err(UpdateError::NoAsset { .. })
        ));
    }
}
