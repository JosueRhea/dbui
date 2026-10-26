//! The assistant: a chat with the user's own Claude Code CLI, handed a few
//! dbui tools against one connection.
//!
//! dbui ships no model and no binary. It finds the `claude` the user
//! installed, drives it over the CLI's stream-json control protocol -- the one
//! the Agent SDK speaks -- and hosts the tools in-process as an `sdk` MCP
//! server. Claude's own tools (shell, files, web) are switched off, so the
//! only thing the agent can touch is the database, and every statement it
//! wants to run passes [`decide`] before it gets there.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value as Json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, OnceCell};

use crate::domain::{self, ConnectionConfig, QueryOutcome, QueryResult, TableRef, Value};
use crate::{DatabaseDriver, DbRuntime};

const SERVER: &str = "dbui";
const TOOL_PREFIX: &str = "mcp__dbui__";
/// Rows of a result handed back to the model. More is noise it pays for.
const ROW_LIMIT: usize = 200;
const CELL_LIMIT: usize = 300;

/// What the agent may do to the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccessMode {
    /// Reads only, on a connection the server itself holds read only.
    #[default]
    ReadOnly,
    /// Reads run; every statement that may write waits for the user.
    Ask,
    /// Everything runs -- except on a production connection, which still asks.
    FullAccess,
}

impl AccessMode {
    pub const ALL: [AccessMode; 3] = [
        AccessMode::ReadOnly,
        AccessMode::Ask,
        AccessMode::FullAccess,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AccessMode::ReadOnly => "Read only",
            AccessMode::Ask => "Ask",
            AccessMode::FullAccess => "Full access",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            AccessMode::ReadOnly => "Writes are refused",
            AccessMode::Ask => "Writes wait for your approval",
            AccessMode::FullAccess => "Writes run without asking",
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            AccessMode::ReadOnly => 0,
            AccessMode::Ask => 1,
            AccessMode::FullAccess => 2,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => AccessMode::Ask,
            2 => AccessMode::FullAccess,
            _ => AccessMode::ReadOnly,
        }
    }
}

/// What happens to one statement the agent asked to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Run,
    Ask,
    Refuse(String),
}

/// The gate every agent statement passes.
///
/// [`domain::writes`] is the same conservative test a read-only connection
/// uses: anything it is not sure reads -- including statements that would
/// lift read only -- counts as a write. The connection's own settings outrank
/// the mode: a read-only connection never writes, and a production one always
/// asks.
pub fn decide(mode: AccessMode, config: &ConnectionConfig, sql: &str) -> Decision {
    if !domain::writes(sql) {
        return Decision::Run;
    }
    if config.read_only {
        return Decision::Refuse(
            "This connection is marked read only in dbui, so statements that may write are \
             refused. Nothing was run."
                .into(),
        );
    }
    match mode {
        AccessMode::ReadOnly => Decision::Refuse(
            "The assistant is in Read only mode, so statements that may write are refused. \
             Nothing was run. If the change is needed, ask the user to switch the assistant to \
             Ask or Full access."
                .into(),
        ),
        AccessMode::Ask => Decision::Ask,
        AccessMode::FullAccess if config.environment.confirms_writes() => Decision::Ask,
        AccessMode::FullAccess => Decision::Run,
    }
}

/// Something the session reports to the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Ready {
        model: String,
    },
    Text(String),
    ToolStarted {
        call: u64,
        tool: String,
        detail: Option<String>,
    },
    ApprovalNeeded {
        call: u64,
    },
    ToolFinished {
        call: u64,
        outcome: Result<String, String>,
    },
    TurnFinished {
        error: Option<String>,
    },
    /// The turn ended because the user stopped it.
    Stopped,
    Exited {
        error: Option<String>,
    },
}

/// One line of the conversation, as the panel draws it.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    User(String),
    Assistant(String),
    Tool {
        call: u64,
        tool: String,
        detail: Option<String>,
        state: ToolState,
    },
    Notice {
        text: String,
        error: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolState {
    Running,
    AwaitingApproval,
    Done(String),
    Failed(String),
}

/// A conversation: what was said, and whether a turn is under way.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Thread {
    pub entries: Vec<Entry>,
    pub busy: bool,
    pub model: Option<String>,
    /// The process is gone; the next message starts a new one.
    pub ended: bool,
}

impl Thread {
    pub fn push_user(&mut self, text: impl Into<String>) {
        self.entries.push(Entry::User(text.into()));
        self.busy = true;
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Ready { model } => self.model = Some(model),
            Event::Text(text) => {
                if !text.trim().is_empty() {
                    self.entries.push(Entry::Assistant(text));
                }
            }
            Event::ToolStarted { call, tool, detail } => self.entries.push(Entry::Tool {
                call,
                tool,
                detail,
                state: ToolState::Running,
            }),
            Event::ApprovalNeeded { call } => self.set_tool(call, ToolState::AwaitingApproval),
            Event::ToolFinished { call, outcome } => self.set_tool(
                call,
                match outcome {
                    Ok(summary) => ToolState::Done(summary),
                    Err(error) => ToolState::Failed(error),
                },
            ),
            Event::TurnFinished { error } => {
                self.busy = false;
                self.settle_tools();
                if let Some(text) = error {
                    self.entries.push(Entry::Notice { text, error: true });
                }
            }
            Event::Stopped => {
                self.busy = false;
                self.settle_tools();
                self.entries.push(Entry::Notice {
                    text: "Stopped.".into(),
                    error: false,
                });
            }
            Event::Exited { error } => {
                self.busy = false;
                self.ended = true;
                self.settle_tools();
                if let Some(text) = error {
                    self.entries.push(Entry::Notice { text, error: true });
                }
            }
        }
    }

    /// The tool call waiting for the user, if one is.
    pub fn pending_approval(&self) -> Option<u64> {
        self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Tool {
                call,
                state: ToolState::AwaitingApproval,
                ..
            } => Some(*call),
            _ => None,
        })
    }

    fn set_tool(&mut self, target: u64, next: ToolState) {
        for entry in self.entries.iter_mut().rev() {
            if let Entry::Tool { call, state, .. } = entry {
                if *call == target {
                    *state = next;
                    return;
                }
            }
        }
    }

    /// A turn that ended with a call still spinning never finished it.
    fn settle_tools(&mut self) {
        for entry in &mut self.entries {
            if let Entry::Tool { state, .. } = entry {
                if matches!(state, ToolState::Running | ToolState::AwaitingApproval) {
                    *state = ToolState::Failed("Stopped".into());
                }
            }
        }
    }
}

/// What a session is started against.
pub struct Launch {
    /// The connection's open driver. Approved writes go through it.
    pub driver: Arc<dyn DatabaseDriver>,
    /// The connection's settings, password included: reads open a second,
    /// read-only connection from them.
    pub config: ConnectionConfig,
    pub mode: AccessMode,
}

/// A running `claude` process. Dropping it ends the process.
pub struct Session {
    shared: Arc<Shared>,
    _shutdown: oneshot::Sender<()>,
}

struct Shared {
    mode: AtomicU8,
    driver: Arc<dyn DatabaseDriver>,
    config: ConnectionConfig,
    reader: OnceCell<Arc<dyn DatabaseDriver>>,
    approvals: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    /// Set by [`Session::interrupt`] so the turn's result reads as stopped,
    /// not failed.
    stopping: AtomicBool,
    next_call: AtomicU64,
    next_request: AtomicU64,
    events: mpsc::UnboundedSender<Event>,
    outbox: mpsc::UnboundedSender<String>,
}

impl Session {
    /// Find the user's `claude` and start it. Everything it says arrives on
    /// the returned receiver; a missing binary arrives as [`Event::Exited`].
    pub fn start(runtime: &DbRuntime, launch: Launch) -> (Session, mpsc::UnboundedReceiver<Event>) {
        let (events, received) = mpsc::unbounded_channel();
        let (outbox, outgoing) = mpsc::unbounded_channel();
        let (shutdown, stop) = oneshot::channel();
        let shared = Arc::new(Shared {
            mode: AtomicU8::new(launch.mode.to_u8()),
            driver: launch.driver,
            config: launch.config,
            reader: OnceCell::new(),
            approvals: Mutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
            next_call: AtomicU64::new(1),
            next_request: AtomicU64::new(1),
            events,
            outbox,
        });
        // The task's result goes nowhere: everything it has to say is an event.
        drop(runtime.spawn(run(shared.clone(), outgoing, stop)));
        (
            Session {
                shared,
                _shutdown: shutdown,
            },
            received,
        )
    }

    pub fn send(&self, text: &str) {
        self.shared.write(json!({
            "type": "user",
            "session_id": "",
            "message": {"role": "user", "content": [{"type": "text", "text": text}]},
            "parent_tool_use_id": null,
        }));
    }

    pub fn mode(&self) -> AccessMode {
        AccessMode::from_u8(self.shared.mode.load(Ordering::SeqCst))
    }

    /// Takes effect from the next statement, including one already waiting.
    pub fn set_mode(&self, mode: AccessMode) {
        self.shared.mode.store(mode.to_u8(), Ordering::SeqCst);
    }

    pub fn answer(&self, call: u64, approve: bool) {
        let waiting = self.shared.approvals.lock().unwrap().remove(&call);
        if let Some(tx) = waiting {
            let _ = tx.send(approve);
        }
    }

    /// Stop the turn. Anything waiting for approval is declined.
    pub fn interrupt(&self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        let waiting: Vec<_> = self.shared.approvals.lock().unwrap().drain().collect();
        for (_, tx) in waiting {
            let _ = tx.send(false);
        }
        let id = self.shared.request_id();
        self.shared.write(json!({
            "type": "control_request",
            "request_id": id,
            "request": {"subtype": "interrupt"},
        }));
    }
}

impl Shared {
    fn write(&self, line: Json) {
        let _ = self.outbox.send(line.to_string());
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn request_id(&self) -> String {
        format!("dbui-{}", self.next_request.fetch_add(1, Ordering::SeqCst))
    }

    fn mode(&self) -> AccessMode {
        AccessMode::from_u8(self.mode.load(Ordering::SeqCst))
    }

    /// A connection of its own, opened read only, for everything the agent
    /// runs that [`domain::writes`] calls a read. A `SELECT` that calls a
    /// function which writes is still a write, and only the server can tell.
    async fn reader(&self) -> Result<Arc<dyn DatabaseDriver>, String> {
        self.reader
            .get_or_try_init(|| async {
                let mut config = self.config.clone();
                config.read_only = true;
                crate::connect_driver(&config)
                    .await
                    .map_err(|error| format!("Could not open a read-only connection: {error}"))
            })
            .await
            .cloned()
    }

    fn reply(&self, request_id: &Json, response: Json) {
        self.write(json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": request_id, "response": response},
        }));
    }
}

async fn run(
    shared: Arc<Shared>,
    mut outgoing: mpsc::UnboundedReceiver<String>,
    mut stop: oneshot::Receiver<()>,
) {
    let error = drive(&shared, &mut outgoing, &mut stop).await.err();
    if let Some(reader) = shared.reader.get() {
        reader.close().await;
    }
    shared.emit(Event::Exited { error });
}

async fn drive(
    shared: &Arc<Shared>,
    outgoing: &mut mpsc::UnboundedReceiver<String>,
    stop: &mut oneshot::Receiver<()>,
) -> Result<(), String> {
    let claude = match locate_claude() {
        Some(path) => path,
        None => locate_with_login_shell()
            .await
            .ok_or_else(|| NOT_FOUND.to_string())?,
    };
    let scratch = std::env::temp_dir().join("dbui-assistant");
    let _ = std::fs::create_dir_all(&scratch);

    let mut child = command(&claude, &system_prompt(shared))
        .current_dir(&scratch)
        .spawn()
        .map_err(|error| format!("Could not start {}: {error}", claude.display()))?;
    let mut stdin = child.stdin.take().ok_or("claude has no stdin")?;
    let stdout = child.stdout.take().ok_or("claude has no stdout")?;
    let stderr = child.stderr.take().ok_or("claude has no stderr")?;

    let tail = Arc::new(Mutex::new(VecDeque::new()));
    tokio::spawn(keep_tail(stderr, tail.clone()));

    // The SDK handshake: names the in-process MCP server the config points at.
    let id = shared.request_id();
    shared.write(json!({
        "type": "control_request",
        "request_id": id,
        "request": {"subtype": "initialize", "sdkMcpServers": [SERVER]},
    }));

    let mut lines = BufReader::new(stdout).lines();
    loop {
        tokio::select! {
            _ = &mut *stop => {
                let _ = child.kill().await;
                return Ok(());
            }
            Some(line) = outgoing.recv() => {
                let line = line + "\n";
                if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
            line = lines.next_line() => match line {
                Ok(Some(line)) => handle_line(shared, &line),
                _ => break,
            }
        }
    }

    let status = child.wait().await.map_err(|error| error.to_string())?;
    if status.success() {
        return Ok(());
    }
    let tail: Vec<String> = tail.lock().unwrap().iter().cloned().collect();
    Err(if tail.is_empty() {
        format!("claude exited ({status})")
    } else {
        tail.join("\n")
    })
}

const NOT_FOUND: &str = "Claude Code isn't installed, or dbui can't find it. Install it \
     (npm install -g @anthropic-ai/claude-code), run `claude` once to sign in, then try again. \
     Set DBUI_CLAUDE_PATH if it lives somewhere unusual.";

fn command(claude: &Path, prompt: &str) -> Command {
    let mcp = json!({"mcpServers": {SERVER: {"type": "sdk", "name": SERVER}}});
    let mut command = Command::new(claude);
    command
        .args(["-p", "--verbose"])
        .args(["--input-format", "stream-json"])
        .args(["--output-format", "stream-json"])
        // Every tool use comes back to dbui as a `can_use_tool` request.
        .args(["--permission-prompt-tool", "stdio"])
        // No shell, no files, no web: the database tools are all there is.
        .args(["--tools", ""])
        .arg("--strict-mcp-config")
        .args(["--mcp-config", &mcp.to_string()])
        .args(["--system-prompt", prompt])
        // Not the user's project settings and hooks; the scratch dir has none,
        // but a CLAUDE.md or settings file put there should not count either.
        .args(["--setting-sources", ""])
        .arg("--no-session-persistence")
        .env("PATH", child_path(claude))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

fn system_prompt(shared: &Shared) -> String {
    let config = &shared.config;
    let driver = shared.driver.as_ref();
    format!(
        "You are the assistant built into dbui, a desktop database client. You are connected to \
         the connection \"{name}\": a {engine} database ({version}), database \"{database}\".\n\
         \n\
         Use the dbui tools to answer: list_tables and describe_table for structure, run_query \
         for SQL in the {engine} dialect. Send one statement per run_query call. Put a LIMIT on \
         exploratory queries. Results are cut at {ROW_LIMIT} rows.\n\
         \n\
         The user chooses what you may change. A statement can be refused (read only) or \
         declined by the user; when that happens, say so plainly and do not try to get around \
         it with a different statement. Never claim a change was made unless run_query reported \
         it.\n\
         \n\
         Be brief. Show the SQL you ran in ```sql blocks when it helps, and summarise results \
         rather than repeating every row.",
        name = config.name,
        engine = config.driver.label(),
        version = driver.server_version(),
        database = config.database,
    )
}

async fn keep_tail(stderr: tokio::process::ChildStderr, tail: Arc<Mutex<VecDeque<String>>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut tail = tail.lock().unwrap();
        if tail.len() == 20 {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

fn handle_line(shared: &Arc<Shared>, line: &str) {
    let Ok(message) = serde_json::from_str::<Json>(line) else {
        return;
    };
    match message["type"].as_str() {
        Some("control_request") => handle_control(shared, &message),
        Some("system") if message["subtype"] == "init" => shared.emit(Event::Ready {
            model: message["model"].as_str().unwrap_or_default().to_string(),
        }),
        Some("assistant") => {
            for block in message["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if block["type"] == "text" {
                    if let Some(text) = block["text"].as_str() {
                        shared.emit(Event::Text(text.to_string()));
                    }
                }
            }
        }
        Some("result") if shared.stopping.swap(false, Ordering::SeqCst) => {
            shared.emit(Event::Stopped);
        }
        Some("result") => {
            let error = (message["is_error"] == true).then(|| {
                message["result"]
                    .as_str()
                    .or_else(|| message["subtype"].as_str())
                    .unwrap_or("The turn failed")
                    .to_string()
            });
            shared.emit(Event::TurnFinished { error });
        }
        _ => {}
    }
}

fn handle_control(shared: &Arc<Shared>, message: &Json) {
    let request_id = message["request_id"].clone();
    let request = &message["request"];
    match request["subtype"].as_str() {
        Some("can_use_tool") => {
            let tool = request["tool_name"].as_str().unwrap_or_default();
            let response = if tool.starts_with(TOOL_PREFIX) {
                json!({"behavior": "allow", "updatedInput": request["input"]})
            } else {
                json!({"behavior": "deny", "message": "Only dbui's database tools are available."})
            };
            shared.reply(&request_id, response);
        }
        Some("mcp_message") => {
            let shared = shared.clone();
            let message = request["message"].clone();
            tokio::spawn(async move {
                let response = handle_mcp(&shared, &message).await;
                shared.reply(&request_id, json!({"mcp_response": response}));
            });
        }
        _ => shared.write(json!({
            "type": "control_response",
            "response": {"subtype": "error", "request_id": request_id, "error": "Not supported by dbui"},
        })),
    }
}

async fn handle_mcp(shared: &Shared, message: &Json) -> Json {
    let id = message.get("id").cloned().unwrap_or(json!(0));
    let result = match message["method"].as_str().unwrap_or_default() {
        "initialize" => json!({
            "protocolVersion": message["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER, "version": env!("CARGO_PKG_VERSION")},
        }),
        "tools/list" => json!({"tools": tool_definitions()}),
        "tools/call" => {
            let params = &message["params"];
            let name = params["name"].as_str().unwrap_or_default();
            match call_tool(shared, name, &params["arguments"]).await {
                Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
                Err(text) => json!({"content": [{"type": "text", "text": text}], "isError": true}),
            }
        }
        method if method.starts_with("notifications/") => json!({}),
        _ => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "Method not found"},
            })
        }
    };
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn tool_definitions() -> Json {
    json!([
        {
            "name": "list_tables",
            "description": "List the schemas, tables and views in the connected database.",
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "describe_table",
            "description": "Columns of one table or view: type, nullability, default, primary key and foreign keys.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "table": {"type": "string", "description": "Table or view name"},
                    "schema": {"type": "string", "description": "Schema; optional when the name is unique"},
                },
                "required": ["table"],
            },
        },
        {
            "name": "run_query",
            "description": "Run one SQL statement on the connected database and return its rows or affected-row count. Statements that may write are subject to the user's access mode.",
            "inputSchema": {
                "type": "object",
                "properties": {"sql": {"type": "string", "description": "A single SQL statement"}},
                "required": ["sql"],
            },
        },
    ])
}

async fn call_tool(shared: &Shared, name: &str, arguments: &Json) -> Result<String, String> {
    let call = shared.next_call.fetch_add(1, Ordering::SeqCst);
    let detail = match name {
        "run_query" => arguments["sql"].as_str().map(|sql| sql.trim().to_string()),
        "describe_table" => arguments["table"].as_str().map(|table| {
            match arguments["schema"].as_str().filter(|s| !s.is_empty()) {
                Some(schema) => format!("{schema}.{table}"),
                None => table.to_string(),
            }
        }),
        _ => None,
    };
    shared.emit(Event::ToolStarted {
        call,
        tool: name.to_string(),
        detail,
    });
    let outcome = match name {
        "list_tables" => list_tables(shared).await,
        "describe_table" => describe_table(shared, arguments).await,
        "run_query" => run_query(shared, call, arguments["sql"].as_str().unwrap_or_default()).await,
        other => Err(format!("Unknown tool {other}")),
    };
    shared.emit(Event::ToolFinished {
        call,
        outcome: outcome
            .as_ref()
            .map(|(_, summary)| summary.clone())
            .map_err(Clone::clone),
    });
    outcome.map(|(text, _)| text)
}

/// The text for the model, and the short summary for the panel.
type Reply = (String, String);

async fn list_tables(shared: &Shared) -> Result<Reply, String> {
    let catalog = shared.driver.catalog().await.map_err(|e| e.to_string())?;
    let mut text = String::new();
    let mut count = 0;
    for schema in &catalog.schemas {
        for table in &schema.tables {
            count += 1;
            text.push_str(&format!(
                "{}.{} ({})\n",
                table.schema,
                table.name,
                table.kind.label()
            ));
        }
    }
    if count == 0 {
        text.push_str("No tables.");
    }
    Ok((text, format!("{count} tables")))
}

async fn describe_table(shared: &Shared, arguments: &Json) -> Result<Reply, String> {
    let name = arguments["table"].as_str().unwrap_or_default();
    let schema = arguments["schema"].as_str().filter(|s| !s.is_empty());
    let catalog = shared.driver.catalog().await.map_err(|e| e.to_string())?;
    let found: Vec<TableRef> = catalog
        .schemas
        .iter()
        .flat_map(|s| &s.tables)
        .filter(|t| t.name == name && schema.is_none_or(|s| t.schema == s))
        .map(|t| t.reference())
        .collect();
    let table = match found.as_slice() {
        [one] => one.clone(),
        [] => {
            return Err(format!(
                "No table named {name}. Call list_tables to see what exists."
            ))
        }
        _ => {
            return Err(format!(
                "{name} exists in more than one schema; pass `schema`."
            ))
        }
    };
    let columns = shared
        .driver
        .columns(&table)
        .await
        .map_err(|e| e.to_string())?;
    let mut text = format!("{}.{}\n", table.schema, table.name);
    for column in &columns {
        text.push_str(&format!("- {} {}", column.name, column.data_type));
        if !column.nullable {
            text.push_str(" NOT NULL");
        }
        if column.is_primary_key {
            text.push_str(" PRIMARY KEY");
        }
        if let Some(default) = &column.default {
            text.push_str(&format!(" DEFAULT {default}"));
        }
        if let Some(key) = &column.references {
            text.push_str(&format!(
                " REFERENCES {}.{}({})",
                key.references.schema, key.references.name, key.references_column
            ));
        }
        text.push('\n');
    }
    Ok((text, format!("{} columns", columns.len())))
}

async fn run_query(shared: &Shared, call: u64, sql: &str) -> Result<Reply, String> {
    let sql = sql.trim();
    if sql.is_empty() {
        return Err("No SQL given.".into());
    }
    if domain::split_statements_for(shared.config.driver, sql).len() > 1 {
        return Err("Send one statement per run_query call.".into());
    }
    let mut approved = false;
    loop {
        match decide(shared.mode(), &shared.config, sql) {
            Decision::Run => break,
            Decision::Refuse(reason) => return Err(reason),
            Decision::Ask if approved => break,
            Decision::Ask => {
                let (tx, rx) = oneshot::channel();
                shared.approvals.lock().unwrap().insert(call, tx);
                shared.emit(Event::ApprovalNeeded { call });
                if rx.await != Ok(true) {
                    return Err("The user declined to run this statement. Nothing was run.".into());
                }
                // The mode may have changed while the user decided; ask again.
                approved = true;
            }
        }
    }
    let driver = if domain::writes(sql) {
        shared.driver.clone()
    } else {
        shared.reader().await?
    };
    let result = driver.execute(sql).await.map_err(|e| e.to_string())?;
    Ok(format_result(&result))
}

fn format_result(result: &QueryResult) -> Reply {
    match &result.outcome {
        QueryOutcome::Affected(count) => {
            let text = format!("{count} row(s) affected.");
            (text.clone(), text)
        }
        QueryOutcome::Rows(set) => {
            let mut text = set
                .columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join("\t");
            text.push('\n');
            for row in set.rows.iter().take(ROW_LIMIT) {
                let cells: Vec<String> = row.0.iter().map(cell).collect();
                text.push_str(&cells.join("\t"));
                text.push('\n');
            }
            let shown = set.rows.len().min(ROW_LIMIT);
            let more = set.rows.len() > ROW_LIMIT || set.truncated;
            let summary = format!(
                "{}{} row{}",
                shown,
                if more { "+" } else { "" },
                if shown == 1 && !more { "" } else { "s" }
            );
            if more {
                text.push_str(&format!(
                    "(showing the first {shown} rows; there are more)\n"
                ));
            } else {
                text.push_str(&format!("({summary})\n"));
            }
            (text, summary)
        }
    }
}

fn cell(value: &Value) -> String {
    if matches!(value, Value::Null) {
        return "NULL".into();
    }
    let text = value.to_text().replace(['\t', '\n', '\r'], " ");
    match text.char_indices().nth(CELL_LIMIT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
    }
}

#[cfg(windows)]
const EXECUTABLES: &[&str] = &["claude.exe", "claude.cmd"];
#[cfg(not(windows))]
const EXECUTABLES: &[&str] = &["claude"];

/// The user's own `claude`. A GUI app launched from the Dock does not get the
/// shell's `PATH`, so the usual install locations are searched as well.
pub fn locate_claude() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("DBUI_CLAUDE_PATH") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        for dir in [
            ".claude/local",
            ".local/bin",
            ".npm-global/bin",
            ".bun/bin",
            ".volta/bin",
            ".asdf/shims",
            "Library/pnpm",
            ".local/share/pnpm",
        ] {
            dirs.push(home.join(dir));
        }
        if let Ok(versions) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            dirs.extend(versions.flatten().map(|v| v.path().join("bin")));
        }
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    dirs.iter()
        .flat_map(|dir| EXECUTABLES.iter().map(move |name| dir.join(name)))
        .find(|path| path.is_file())
}

/// Last resort: ask the user's login shell, which has their real `PATH`.
async fn locate_with_login_shell() -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(shell)
            .args(["-lc", "command -v claude"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    path.is_file().then_some(path)
}

/// npm installs `claude` as a node script, which needs `node` on `PATH` --
/// and a GUI app's `PATH` rarely has it. Its own directory usually does.
fn child_path(claude: &Path) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = claude.parent() {
        dirs.push(dir.to_path_buf());
    }
    if let Ok(real) = std::fs::canonicalize(claude) {
        if let Some(dir) = real.parent() {
            dirs.push(dir.to_path_buf());
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
    std::env::join_paths(dirs).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ColumnInfo, ConnectionConfig, Driver, Environment, QueryStats, ResultSet, Row,
    };

    fn config() -> ConnectionConfig {
        ConnectionConfig::new(Driver::Postgres)
    }

    #[test]
    fn reads_run_in_every_mode() {
        for mode in AccessMode::ALL {
            assert_eq!(
                decide(mode, &config(), "select * from users"),
                Decision::Run
            );
        }
    }

    #[test]
    fn writes_follow_the_mode() {
        let sql = "delete from users where id = 2";
        assert!(matches!(
            decide(AccessMode::ReadOnly, &config(), sql),
            Decision::Refuse(_)
        ));
        assert_eq!(decide(AccessMode::Ask, &config(), sql), Decision::Ask);
        assert_eq!(
            decide(AccessMode::FullAccess, &config(), sql),
            Decision::Run
        );
    }

    #[test]
    fn lifting_read_only_is_a_write() {
        let sql = "set default_transaction_read_only = off";
        assert!(matches!(
            decide(AccessMode::ReadOnly, &config(), sql),
            Decision::Refuse(_)
        ));
    }

    #[test]
    fn a_read_only_connection_outranks_full_access() {
        let mut config = config();
        config.read_only = true;
        assert!(matches!(
            decide(AccessMode::FullAccess, &config, "drop table users"),
            Decision::Refuse(_)
        ));
    }

    #[test]
    fn production_still_asks_in_full_access() {
        let mut config = config();
        config.environment = Environment::Production;
        assert_eq!(
            decide(AccessMode::FullAccess, &config, "update users set a = 1"),
            Decision::Ask
        );
    }

    #[test]
    fn the_thread_follows_a_tool_call_through_approval() {
        let mut thread = Thread::default();
        thread.push_user("delete user 2");
        thread.apply(Event::ToolStarted {
            call: 1,
            tool: "run_query".into(),
            detail: Some("delete from users where id = 2".into()),
        });
        thread.apply(Event::ApprovalNeeded { call: 1 });
        assert_eq!(thread.pending_approval(), Some(1));
        thread.apply(Event::ToolFinished {
            call: 1,
            outcome: Ok("1 row(s) affected.".into()),
        });
        assert_eq!(thread.pending_approval(), None);
        thread.apply(Event::Text("Deleted.".into()));
        thread.apply(Event::TurnFinished { error: None });
        assert!(!thread.busy);
        assert_eq!(thread.entries.len(), 3);
    }

    #[test]
    fn a_stopped_turn_reads_as_stopped_not_failed() {
        let mut thread = Thread::default();
        thread.push_user("delete everything".to_string());
        thread.apply(Event::ToolStarted {
            call: 1,
            tool: "run_query".into(),
            detail: Some("DELETE FROM t".into()),
        });
        thread.apply(Event::Stopped);
        assert!(!thread.busy);
        assert!(matches!(
            thread.entries.last(),
            Some(Entry::Notice { error: false, .. })
        ));
        assert!(!matches!(
            thread.entries[1],
            Entry::Tool {
                state: ToolState::Running,
                ..
            }
        ));
    }

    #[test]
    fn an_ended_turn_settles_calls_left_running() {
        let mut thread = Thread::default();
        thread.apply(Event::ToolStarted {
            call: 4,
            tool: "run_query".into(),
            detail: None,
        });
        thread.apply(Event::Exited {
            error: Some("gone".into()),
        });
        assert!(thread.ended);
        assert!(matches!(
            &thread.entries[0],
            Entry::Tool {
                state: ToolState::Failed(_),
                ..
            }
        ));
    }

    #[test]
    fn results_are_tabbed_and_capped() {
        let result = QueryResult {
            statement: "select".into(),
            outcome: QueryOutcome::Rows(ResultSet {
                columns: vec![
                    ColumnInfo {
                        name: "id".into(),
                        type_name: "int".into(),
                    },
                    ColumnInfo {
                        name: "note".into(),
                        type_name: "text".into(),
                    },
                ],
                rows: (0..250)
                    .map(|i| Row(vec![Value::Int(i), Value::Null]))
                    .collect(),
                truncated: false,
            }),
            stats: QueryStats {
                elapsed: Duration::ZERO,
            },
        };
        let (text, summary) = format_result(&result);
        assert!(text.starts_with("id\tnote\n0\tNULL\n"));
        assert_eq!(text.lines().count(), 1 + ROW_LIMIT + 1);
        assert_eq!(summary, "200+ rows");
    }
}
