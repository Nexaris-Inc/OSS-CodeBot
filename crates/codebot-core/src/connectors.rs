//! Connectors: local MCP (Model Context Protocol) servers started over stdio.
//!
//! A connector is a program the user registers. CodeBot starts it only for an
//! explicit operation (connection test or a confirmed tool call), speaks
//! newline-delimited JSON-RPC 2.0 on its stdin/stdout, and stops the whole
//! process group afterwards. The environment is cleared except for a small base
//! set and the variable names the user listed for that connector.
use process_wrap::std::{ChildWrapper, CommandWrap};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_LINE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TOOLS: usize = 128;
/// Maximum characters of a tool result passed back to the model.
pub const MAX_CONNECTOR_RESULT_CHARS: usize = 12_000;
/// Maximum connectors attached to one chat.
pub const MAX_ATTACHED_CONNECTORS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConnectorConfig {
    /// Identifier used in tool names: 1–24 lowercase letters, digits, `-`, `_`.
    pub name: String,
    /// Executable started for each operation (searched in PATH when relative).
    pub command: String,
    /// Literal arguments. No shell is involved.
    pub args: Vec<String>,
    /// Extra environment variable names copied from CodeBot's environment.
    pub env_names: Vec<String>,
    /// Limit for one connection test or tool call, 1–300 seconds.
    pub timeout_seconds: u64,
}
impl Default for ConnectorConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            args: Vec::new(),
            env_names: Vec::new(),
            timeout_seconds: 30,
        }
    }
}
impl ConnectorConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty()
            || self.name.len() > 24
            || !self
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        {
            return Err("コネクター名は24文字以内の英小文字・数字・-・_にしてください".into());
        }
        if self.command.trim().is_empty()
            || self.command.len() > 4096
            || self.command.contains('\0')
        {
            return Err("コネクターの実行ファイルを指定してください".into());
        }
        if self.args.len() > 64 || self.args.iter().any(|a| a.len() > 4096 || a.contains('\0')) {
            return Err("コネクターの引数は64個・各4096バイト以内にしてください".into());
        }
        if self.env_names.len() > 32
            || self
                .env_names
                .iter()
                .any(|n| !crate::commands::valid_name(n))
        {
            return Err("環境変数名は英字・数字・_で、32個以内にしてください".into());
        }
        if !(1..=300).contains(&self.timeout_seconds) {
            return Err("コネクターのタイムアウトは1〜300秒にしてください".into());
        }
        Ok(())
    }
}

/// A tool advertised by a connector.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConnectorTool {
    pub connector: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}
impl ConnectorTool {
    /// Name presented to the model: `mcp__<connector>__<tool>` restricted to
    /// `[A-Za-z0-9_-]{1,64}`, the strictest common function-name rule.
    pub fn exposed_name(&self) -> Option<String> {
        let tool: String = self
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let name = format!("mcp__{}__{}", self.connector, tool);
        (name.len() <= 64).then_some(name)
    }
}

/// Result of a confirmed tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorOutput {
    pub is_error: bool,
    pub text: String,
}

struct OwnedChild(Box<dyn ChildWrapper>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
        let _ = self.0.wait();
    }
}

struct Session {
    stdin: Box<dyn Write + Send>,
    rx: mpsc::Receiver<Result<Value, String>>,
    next_id: u64,
    deadline: Instant,
    // Dropped last: closing stdin first lets well-behaved servers exit.
    _child: OwnedChild,
}
impl Session {
    fn start(config: &ConnectorConfig, cwd: Option<&Path>) -> Result<Self, String> {
        config.validate()?;
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for name in crate::commands::BASE_ENV
            .iter()
            .copied()
            .chain(config.env_names.iter().map(String::as_str))
        {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        if let Some(cwd) = cwd.filter(|p| p.is_dir()) {
            command.current_dir(cwd);
        }
        let mut command = CommandWrap::from(command);
        #[cfg(unix)]
        {
            command.wrap(process_wrap::std::ProcessGroup::leader());
        }
        #[cfg(windows)]
        {
            command.wrap(process_wrap::std::CreationFlags(
                windows::Win32::System::Threading::CREATE_NO_WINDOW,
            ));
            command.wrap(process_wrap::std::JobObject);
        }
        let mut child = OwnedChild(command.spawn().map_err(|_| {
            "コネクターを起動できません。実行ファイル・引数・PATHを確認してください".to_owned()
        })?);
        let stdin = child
            .0
            .stdin()
            .take()
            .ok_or("コネクターの入力を開けません")?;
        let stdout = child
            .0
            .stdout()
            .take()
            .ok_or("コネクターの出力を開けません")?;
        let (tx, rx) = mpsc::sync_channel(16);
        std::thread::spawn(move || read_messages(stdout, tx));
        Ok(Self {
            stdin: Box::new(stdin),
            rx,
            next_id: 1,
            deadline: Instant::now() + Duration::from_secs(config.timeout_seconds),
            _child: child,
        })
    }
    fn send(&mut self, message: &Value) -> Result<(), String> {
        let mut line = serde_json::to_vec(message).map_err(|_| "要求を変換できません")?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .and_then(|_| self.stdin.flush())
            .map_err(|_| "コネクターへ送信できません。コネクターが終了した可能性があります".into())
    }
    fn notify(&mut self, method: &str) -> Result<(), String> {
        self.send(&json!({"jsonrpc":"2.0","method":method}))
    }
    fn request(
        &mut self,
        method: &str,
        params: Value,
        cancel: &AtomicBool,
    ) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("コネクターの処理を中止しました".into());
            }
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("コネクターが時間内に応答しませんでした".into());
            }
            let message = match self.rx.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(message) => message?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("コネクターが応答せずに終了しました".into())
                }
            };
            // Requests from the server: answer ping, decline everything else
            // (sampling, roots, elicitation are not offered to connectors).
            if let (Some(method), Some(request_id)) =
                (message["method"].as_str(), message.get("id"))
            {
                let reply = if method == "ping" {
                    json!({"jsonrpc":"2.0","id":request_id,"result":{}})
                } else {
                    json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"Method not supported by OSS-CodeBot"}})
                };
                self.send(&reply)?;
                continue;
            }
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(format!(
                    "コネクターがエラーを返しました：{}",
                    clean(error["message"].as_str().unwrap_or("詳細なし"), 300)
                ));
            }
            return message
                .get("result")
                .cloned()
                .ok_or("コネクターの応答にresultがありません".into());
        }
    }
    fn initialize(
        config: &ConnectorConfig,
        cwd: Option<&Path>,
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        let mut session = Self::start(config, cwd)?;
        let result = session.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "OSS-CodeBot", "version": env!("CARGO_PKG_VERSION")}
            }),
            cancel,
        )?;
        if !result["protocolVersion"].is_string() {
            return Err("MCPの初期化応答が不正です".into());
        }
        if result["capabilities"].get("tools").is_none() {
            return Err("このコネクターはツールを提供していません".into());
        }
        session.notify("notifications/initialized")?;
        Ok(session)
    }
}

fn read_messages(stdout: impl Read, tx: mpsc::SyncSender<Result<Value, String>>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = Vec::new();
        match (&mut reader)
            .take(MAX_LINE_BYTES + 1)
            .read_until(b'\n', &mut line)
        {
            Ok(0) | Err(_) => return,
            Ok(_) if line.len() as u64 > MAX_LINE_BYTES => {
                let _ = tx.send(Err("コネクターの応答が4 MiBを超えています".into()));
                return;
            }
            Ok(_) => {}
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let message = serde_json::from_slice::<Value>(&line)
            .map_err(|_| "コネクターがJSON-RPC以外を出力しました".to_owned());
        let failed = message.is_err();
        if tx.send(message).is_err() || failed {
            return;
        }
    }
}

fn clean(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(max)
        .collect()
}

/// Start the connector, list its tools, and stop it.
pub fn list_connector_tools(
    config: &ConnectorConfig,
    cwd: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<Vec<ConnectorTool>, String> {
    let mut session = Session::initialize(config, cwd, cancel)?;
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let params = cursor
            .as_ref()
            .map(|c| json!({"cursor": c}))
            .unwrap_or_else(|| json!({}));
        let result = session.request("tools/list", params, cancel)?;
        for tool in result["tools"]
            .as_array()
            .ok_or("tools/listの応答が不正です")?
        {
            let name = tool["name"].as_str().unwrap_or_default();
            if name.is_empty()
                || name.len() > 64
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
            {
                continue;
            }
            let schema = if tool["inputSchema"].is_object() {
                tool["inputSchema"].clone()
            } else {
                json!({"type":"object"})
            };
            tools.push(ConnectorTool {
                connector: config.name.clone(),
                name: name.to_owned(),
                description: clean(tool["description"].as_str().unwrap_or(""), 1024),
                input_schema: schema,
            });
            if tools.len() >= MAX_TOOLS {
                return Ok(tools);
            }
        }
        cursor = result["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    Ok(tools)
}

/// Start the connector and call one tool. Call only after the user confirmed it.
pub fn call_connector_tool(
    config: &ConnectorConfig,
    cwd: Option<&Path>,
    tool: &str,
    arguments: &Value,
    cancel: &AtomicBool,
) -> Result<ConnectorOutput, String> {
    if !arguments.is_object() {
        return Err("ツールの引数はJSONオブジェクトにしてください".into());
    }
    let mut session = Session::initialize(config, cwd, cancel)?;
    let result = session.request(
        "tools/call",
        json!({"name": tool, "arguments": arguments}),
        cancel,
    )?;
    let mut text = String::new();
    for item in result["content"].as_array().into_iter().flatten() {
        if !text.is_empty() {
            text.push('\n');
        }
        match item["type"].as_str() {
            Some("text") => text.push_str(item["text"].as_str().unwrap_or("")),
            Some(kind) => text.push_str(&format!("[{}の内容は省略しました]", clean(kind, 32))),
            None => {}
        }
    }
    if text.trim().is_empty() {
        if let Some(structured) = result.get("structuredContent") {
            text = structured.to_string();
        }
    }
    let mut text = clean(&text, MAX_CONNECTOR_RESULT_CHARS + 1);
    if text.chars().count() > MAX_CONNECTOR_RESULT_CHARS {
        text = text.chars().take(MAX_CONNECTOR_RESULT_CHARS - 40).collect();
        text.push_str("\n（結果が長いため以降を省略しました）");
    }
    Ok(ConnectorOutput {
        is_error: result["isError"] == true,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Python interpreter, so the fake servers run on every CI platform.
    fn python() -> Option<String> {
        ["python3", "python"]
            .into_iter()
            .map(str::to_owned)
            .find(|name| {
                Command::new(name)
                    .arg("-c")
                    .arg("import sys; sys.exit(0 if sys.version_info >= (3, 8) else 1)")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            })
    }
    /// A small MCP server script, enough to exercise the protocol.
    fn server(dir: &Path, body: &str) -> Option<ConnectorConfig> {
        let Some(python) = python() else {
            eprintln!("skipping connector test: Python 3 is not installed");
            return None;
        };
        let path = dir.join("server.py");
        std::fs::write(&path, body).unwrap();
        Some(ConnectorConfig {
            name: "fake".into(),
            command: python,
            args: vec!["-u".into(), path.to_string_lossy().into_owned()],
            env_names: vec!["CODEBOT_TEST_VALUE".into()],
            timeout_seconds: 10,
        })
    }
    const SERVER: &str = r#"
import json, os, sys
def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()
for line in sys.stdin:
    request = json.loads(line)
    method, rid = request.get("method"), request.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "method": "notifications/message", "params": {}})
        send({"jsonrpc": "2.0", "id": 99, "method": "ping"})
        send({"jsonrpc": "2.0", "id": rid, "result": {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "fake", "version": "1"}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": rid, "result": {"tools": [
            {"name": "echo.env", "description": "Echo", "inputSchema": {"type": "object", "properties": {"x": {"type": "string"}}}},
            {"name": "bad name"}]}})
    elif method == "tools/call":
        text = "value=%s home=%s" % (os.environ.get("CODEBOT_TEST_VALUE", "unset"), os.environ.get("CODEBOT_SECRET", "unset"))
        send({"jsonrpc": "2.0", "id": rid, "result": {"content": [{"type": "text", "text": text}, {"type": "image", "data": "AA=="}], "isError": False}})
"#;

    #[test]
    fn lists_and_calls_tools_with_only_allowed_environment() {
        let dir = tempfile::tempdir().unwrap();
        let Some(config) = server(dir.path(), SERVER) else {
            return;
        };
        std::env::set_var("CODEBOT_TEST_VALUE", "visible");
        std::env::set_var("CODEBOT_SECRET", "hidden");
        let cancel = AtomicBool::new(false);
        let tools = list_connector_tools(&config, Some(dir.path()), &cancel).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].exposed_name().unwrap(), "mcp__fake__echo_env");
        let output =
            call_connector_tool(&config, None, "echo.env", &json!({"x":"1"}), &cancel).unwrap();
        assert!(!output.is_error);
        assert!(output.text.contains("value=visible"), "{}", output.text);
        assert!(output.text.contains("home=unset"), "{}", output.text);
        assert!(output.text.contains("[imageの内容は省略しました]"));
        assert!(call_connector_tool(&config, None, "echo.env", &json!([1]), &cancel).is_err());
    }

    #[test]
    fn timeouts_errors_and_invalid_configs_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let Some(mut silent) = server(dir.path(), "import time\ntime.sleep(30)\n") else {
            return;
        };
        silent.timeout_seconds = 1;
        let started = Instant::now();
        let error = list_connector_tools(&silent, None, &AtomicBool::new(false)).unwrap_err();
        assert!(error.contains("時間内"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(8));

        let cancelled = list_connector_tools(&silent, None, &AtomicBool::new(true)).unwrap_err();
        assert!(cancelled.contains("中止"));

        let failing = server(
            dir.path(),
            "import sys\nsys.stdin.readline()\nprint('{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-1,\"message\":\"boom\"}}', flush=True)\nsys.stdin.read()\n",
        )
        .unwrap();
        assert!(
            list_connector_tools(&failing, None, &AtomicBool::new(false))
                .unwrap_err()
                .contains("boom")
        );

        for config in [
            ConnectorConfig {
                name: "Bad".into(),
                command: "x".into(),
                ..Default::default()
            },
            ConnectorConfig {
                name: "ok".into(),
                command: " ".into(),
                ..Default::default()
            },
            ConnectorConfig {
                name: "ok".into(),
                command: "x".into(),
                env_names: vec!["1A".into()],
                ..Default::default()
            },
            ConnectorConfig {
                name: "ok".into(),
                command: "x".into(),
                timeout_seconds: 0,
                ..Default::default()
            },
        ] {
            assert!(config.validate().is_err());
        }
        let missing = ConnectorConfig {
            name: "missing".into(),
            command: "codebot-nonexistent-mcp-server".into(),
            ..Default::default()
        };
        assert!(list_connector_tools(&missing, None, &AtomicBool::new(false)).is_err());
    }
}
