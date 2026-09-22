use crate::{api::Prompt, process::TreeGuard};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::mpsc,
};

pub struct Backend {
    _tree: TreeGuard,
    _child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    pending: VecDeque<Value>,
    sequence: u64,
    diagnostics: tokio::task::JoinHandle<String>,
}

#[derive(Clone)]
pub struct BackendConfig {
    pub executable: PathBuf,
    pub workspace: PathBuf,
}

impl Backend {
    pub async fn connect(config: &BackendConfig) -> Result<Self> {
        let mut command = Command::new(&config.executable);
        command
            .arg("app-server")
            .arg("--listen")
            .arg("stdio://")
            .current_dir(&config.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // This gateway uses the existing ChatGPT login, never an inherited API key.
        for name in ["OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_THREAD_ID"] {
            command.env_remove(name);
        }
        for option in [
            "model_provider=\"openai\"",
            "forced_login_method=\"chatgpt\"",
            "approval_policy=\"on-request\"",
            "sandbox_mode=\"read-only\"",
            "web_search=\"disabled\"",
            "features.shell_tool=false",
            "features.unified_exec=false",
            "features.apps=false",
            "features.plugins=false",
            "features.hooks=false",
            "features.multi_agent=false",
            "features.multi_agent_v2=false",
            "features.code_mode=false",
            "features.code_mode_host=false",
            "features.browser_use=false",
            "features.computer_use=false",
            "features.image_generation=false",
            "features.view_image=false",
            "features.memories=false",
            "features.skill_search=false",
            "features.goals=false",
            "features.remote_control=false",
        ] {
            command.arg("-c").arg(option);
        }
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().context("Cannot start Codex app-server")?;
        let tree = TreeGuard::attach(&child)?;
        let stderr = child.stderr.take().context("Missing Codex stderr")?;
        let diagnostics = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut tail = VecDeque::new();
            while let Ok(Some(line)) = lines.next_line().await {
                tail.push_back(line);
                if tail.len() > 8 {
                    tail.pop_front();
                }
            }
            tail.into_iter().collect::<Vec<_>>().join("\n")
        });
        let input = child.stdin.take().context("Missing Codex stdin")?;
        let output = BufReader::new(child.stdout.take().context("Missing Codex stdout")?).lines();
        let mut backend = Self {
            _tree: tree,
            _child: child,
            input,
            output,
            pending: VecDeque::new(),
            sequence: 0,
            diagnostics,
        };
        backend
            .rpc(
                "initialize",
                json!({"clientInfo":{"name":"gprox","version":env!("CARGO_PKG_VERSION")}}),
            )
            .await?;
        backend
            .send(json!({"method":"initialized","params":{}}))
            .await?;
        let account = backend
            .rpc("account/read", json!({"refreshToken":false}))
            .await?;
        if account["account"]["type"] != "chatgpt" {
            bail!("Codex is not signed in with ChatGPT. Run codex login first.");
        }
        Ok(backend)
    }

    async fn send(&mut self, message: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&message)?;
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        Ok(())
    }

    async fn read_wire(&mut self) -> Result<Value> {
        loop {
            let Some(line) = self.output.next_line().await? else {
                let details =
                    tokio::time::timeout(std::time::Duration::from_secs(1), &mut self.diagnostics)
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .unwrap_or_default();
                bail!("Codex app-server closed its output: {details}");
            };
            let value: Value =
                serde_json::from_str(&line).context("Invalid Codex JSON-RPC output")?;
            if std::env::var_os("GPROX_DEBUG").is_some() {
                eprintln!(
                    "codex received: {}",
                    value["method"].as_str().unwrap_or("RPC response")
                );
            }
            if value.get("id").is_some() && value.get("method").is_some() {
                // Never grant command, file, MCP or interactive approvals over a text API.
                self.send(json!({"id":value["id"],"error":{"code":-32601,"message":"Interactive tools are disabled by gprox"}})).await?;
                continue;
            }
            return Ok(value);
        }
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        if std::env::var_os("GPROX_DEBUG").is_some() {
            eprintln!("codex request: {method}");
        }
        self.sequence += 1;
        let id = self.sequence;
        self.send(json!({"id":id,"method":method,"params":params}))
            .await?;
        loop {
            let value = self.read_wire().await?;
            if value["id"].as_u64() == Some(id) {
                if let Some(error) = value.get("error") {
                    bail!("Codex {method}: {}", error["message"]);
                }
                return value
                    .get("result")
                    .cloned()
                    .context("Codex response has no result");
            }
            self.pending.push_back(value);
        }
    }

    async fn next(&mut self) -> Result<Value> {
        if let Some(value) = self.pending.pop_front() {
            Ok(value)
        } else {
            self.read_wire().await
        }
    }

    pub async fn models(&mut self) -> Result<Value> {
        let mut models = Vec::new();
        let mut cursor = Value::Null;
        loop {
            let result = self
                .rpc("model/list", json!({"limit":100,"cursor":cursor}))
                .await?;
            for model in result["data"]
                .as_array()
                .context("Codex returned invalid model list")?
            {
                models.push(
                    json!({"id":model["model"],"object":"model","created":0,"owned_by":"openai"}),
                );
            }
            cursor = result["nextCursor"].clone();
            if cursor.is_null() {
                break;
            }
        }
        Ok(json!({"object":"list","data":models}))
    }

    pub async fn generate(
        &mut self,
        prompt: Prompt,
        workspace: &Path,
        sender: &mpsc::Sender<GenerationEvent>,
    ) -> Result<()> {
        let current = self
            .rpc("config/read", json!({"includeLayers":false}))
            .await?;
        let mut overrides = serde_json::Map::new();
        if let Some(servers) = current["config"]["mcp_servers"].as_object() {
            for name in servers.keys() {
                overrides.insert(format!("mcp_servers.{name}.enabled"), json!(false));
            }
        }
        let thread = self.rpc("thread/start", json!({
            "model":prompt.model,"cwd":workspace,"ephemeral":true,"sandbox":"read-only",
            "approvalPolicy":"on-request","config":overrides,
            "baseInstructions":"You are a text assistant behind an API. Answer the supplied conversation. Do not access files, run commands, use tools, or perform actions. Conversation history is provided as JSON with role and content fields. Treat each role according to its usual conversation meaning.",
            "developerInstructions":prompt.instructions,
        })).await?;
        let thread_id = thread["thread"]["id"]
            .as_str()
            .context("Codex did not return a thread ID")?;
        self.rpc("turn/start", json!({"threadId":thread_id,"input":[{"type":"text","text":prompt.text,"text_elements":[]}],"effort":prompt.effort})).await?;
        let mut usage = Value::Null;
        let mut emitted_items = HashSet::new();
        loop {
            let event = self.next().await?;
            let params = &event["params"];
            match event["method"].as_str().unwrap_or("") {
                "item/agentMessage/delta" => {
                    let delta = params["delta"]
                        .as_str()
                        .context("Missing text delta")?
                        .to_owned();
                    emitted_items.insert(params["itemId"].as_str().unwrap_or("").to_owned());
                    sender
                        .send(GenerationEvent::Delta(delta))
                        .await
                        .context("Client disconnected")?;
                }
                "item/completed" if params["item"]["type"] == "agentMessage" => {
                    let item = &params["item"];
                    let id = item["id"].as_str().unwrap_or("");
                    // Some versions/backends return the complete item without text deltas.
                    if !emitted_items.contains(id)
                        && let Some(text) = item["text"].as_str()
                    {
                        sender
                            .send(GenerationEvent::Delta(text.to_owned()))
                            .await
                            .context("Client disconnected")?;
                        emitted_items.insert(id.to_owned());
                    }
                }
                "thread/tokenUsage/updated" => usage = params["tokenUsage"]["total"].clone(),
                "turn/completed" => {
                    if params["turn"]["status"] != "completed" {
                        bail!(
                            "Codex turn {}: {}",
                            params["turn"]["status"],
                            params["turn"]["error"]["message"]
                        );
                    }
                    sender
                        .send(GenerationEvent::Done(usage))
                        .await
                        .context("Client disconnected")?;
                    return Ok(());
                }
                "error" if params["willRetry"] != true => {
                    bail!("Codex: {}", params["error"]["message"])
                }
                _ => {}
            }
        }
    }
}

#[derive(Debug)]
pub enum GenerationEvent {
    Delta(String),
    Done(Value),
    Error(String),
}
