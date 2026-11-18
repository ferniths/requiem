//! Cross-crate end-to-end integration tests.
//!
//! These exercise the seams between REQUIEM's crates — the agent loop
//! (`requiem-orchestration`) driving a real tool executor (`requiem-tools`)
//! against a mock model server, subagent spawning under the depth limit,
//! permission-gate propagation, and task-graph checkpoint round-trips —
//! without touching the user's home directory or the network.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use reqwest::header::HeaderValue;
use reqwest::StatusCode;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

use requiem_core::agent::Agent;
use requiem_core::error::RequiemError;
use requiem_core::mode::Mode;
use requiem_core::task::Task;
use requiem_model::client::ModelClient;
use requiem_model::config::ModelConfig;
use requiem_model::request::ToolDef;
use requiem_orchestration::agent_runtime::{
    run_agent, run_agent_halted, AgentRun, CancelFlag, ToolExecutor,
};
use requiem_permissions::PermissionGate;
use requiem_tools::subagent::{make_subagent_tool, SpawnToolMap};
use requiem_tools::Tool;

/// A minimal blocking HTTP mock of the OpenAI `/v1/chat/completions`
/// endpoint. Each pre-programmed (status, body) pair is served in order;
/// the last pair repeats for subsequent requests. Request headers are
/// recorded so tests can assert the client's wire behaviour.
struct MockServer {
    addr: String,
    auth: Arc<Mutex<Vec<Option<HeaderValue>>>>,
}

impl MockServer {
    async fn start(status: Vec<StatusCode>, bodies: Vec<String>) -> Self {
        let status = Arc::new(status);
        let body = Arc::new(bodies);
        let auth: Arc<Mutex<Vec<Option<HeaderValue>>>> = Arc::new(Mutex::new(Vec::new()));
        let counter: Arc<std::sync::atomic::AtomicUsize> = Arc::new(0.into());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());

        let s_status = status.clone();
        let s_body = body.clone();
        let s_auth = auth.clone();
        let s_counter = counter.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let st = s_status.clone();
                let bd = s_body.clone();
                let au = s_auth.clone();
                let c = s_counter.clone();
                tokio::spawn(async move {
                    let req = read_request(stream).await;
                    if let Some(auth) = &req.auth {
                        au.lock().unwrap().push(Some(auth.clone()));
                    }
                    let idx = c.fetch_add(1, Ordering::SeqCst);
                    let (status, payload) = match st.len() {
                        0 => (StatusCode::OK, String::new()),
                        1 => (st[0], bd[0].clone()),
                        _ => {
                            let i = idx.min(st.len() - 1);
                            (st[i], bd[i].clone())
                        }
                    };
                    let status_code = status.to_string();
                    let response = format!(
                        "HTTP/1.1 {status_code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let mut writer = tokio::io::BufWriter::new(req.stream);
                    let _ = writer.write_all(response.as_bytes()).await;
                    let _ = writer.flush().await;
                });
            }
        });

        Self {
            addr,
            auth,
        }
    }

    /// The base URL for a `ModelConfig` pointing at this server.
    fn endpoint(&self) -> String {
        self.addr.clone()
    }

    /// The model client that talks to this server.
    fn client(&self) -> ModelClient {
        ModelClient::new(self.config()).unwrap()
    }

    fn config(&self) -> ModelConfig {
        ModelConfig {
            endpoint: self.endpoint(),
            api_key: "test-key".into(),
            model: "test-model".into(),
            timeout_ms: 5_000,
            max_concurrency: 4,
            max_retries: 0,
            max_context_tokens: None,
            max_output_tokens: None,
        }
    }

    /// The `Authorization` header value seen on each request so far, in
    /// arrival order. `None` for requests that carried no auth header.
    fn seen_auth(&self) -> Vec<Option<HeaderValue>> {
        self.auth.lock().unwrap().clone()
    }
}

/// Read one HTTP request (headers + body) and report the recorded
/// `Authorization` header. The stream is returned so the caller can write
/// the response back on the same connection.
struct ParsedRequest {
    auth: Option<HeaderValue>,
    stream: tokio::net::TcpStream,
}

async fn read_request(mut stream: tokio::net::TcpStream) -> ParsedRequest {
    use tokio::io::AsyncReadExt;

    let mut raw: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    let mut header_end = 0usize;
    loop {
        let n = stream.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(pos) = find_subsequence(&raw, b"\r\n\r\n") {
            header_end = pos + 4;
            break;
        }
    }
    let head = String::from_utf8_lossy(&raw[..header_end.min(raw.len())]);
    let lines: Vec<&str> = head.lines().collect();
    let mut auth: Option<HeaderValue> = None;
    let mut content_length = 0usize;
    for line in lines.iter().skip(1) {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("authorization:") {
            if let Ok(v) = rest.trim().parse::<HeaderValue>() {
                auth = Some(v);
            }
        }
        if let Some(rest) = lower.strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }
    // Drain the declared request body so the connection is in a clean state.
    while raw.len() < header_end + content_length {
        let n = stream.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
    }
    ParsedRequest {
        auth,
        stream,
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn chat_body(content: &str) -> String {
    serde_json::json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 12, "completion_tokens": 3, "total_tokens": 15 }
    })
    .to_string()
}

/// A model tool definition the mock responses reference.
fn calc_tool_def() -> ToolDef {
    ToolDef::new(
        "calc",
        "Add two integers and return their sum.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "a": { "type": "integer" },
                "b": { "type": "integer" }
            },
            "required": ["a", "b"]
        }),
    )
}

fn calc_tool_call_body(id: &str, a: i64, b: i64) -> String {
    serde_json::json!({
        "id": "chatcmpl-1",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": { "name": "calc", "arguments": format!("{{\"a\":{a},\"b\":{b}}}") }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 20, "completion_tokens": 8, "total_tokens": 28 }
    })
    .to_string()
}

/// A `ToolExecutor` that routes tool calls to a registry of concrete
/// `requiem-tools` implementations, exactly the way `ToolBridge` does in
/// the CLI. This is the real seam under test: the model's tool call must
/// land in a concrete tool and the result must flow back into the agent
/// loop.
struct RouteExecutor {
    tools: HashMap<String, Box<dyn ToolRef>>,
    calls: Arc<Mutex<Vec<String>>>,
}
trait ToolRef: Send + Sync {
    fn run(&self, args: &serde_json::Value) -> Result<serde_json::Value, String>;
}

impl RouteExecutor {
    fn with_calc_and_echo() -> Self {
        let mut tools = HashMap::new();
        tools.insert(
            "calc".to_string(),
            Box::new(CalcTool {}) as Box<dyn ToolRef>,
        );
        tools.insert(
            "echo".to_string(),
            Box::new(EchoTool {}) as Box<dyn ToolRef>,
        );
        Self {
            tools,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

struct CalcTool;
impl ToolRef for CalcTool {
    fn run(&self, args: &serde_json::Value) -> Result<serde_json::Value, String> {
        let a = args.get("a").and_then(|v| v.as_i64()).ok_or("a missing")?;
        let b = args.get("b").and_then(|v| v.as_i64()).ok_or("b missing")?;
        Ok(serde_json::json!({ "sum": a + b }))
    }
}

struct EchoTool;
impl ToolRef for EchoTool {
    fn run(&self, args: &serde_json::Value) -> Result<serde_json::Value, String> {
        Ok(args.clone())
    }
}

#[async_trait]
impl ToolExecutor for RouteExecutor {
    async fn execute(
        &self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, RequiemError> {
        self.calls.lock().unwrap().push(name.to_string());
        let tool = self.tools.get(name).ok_or_else(|| {
            RequiemError::InvalidState(format!("no tool registered for '{name}'"))
        })?;
        tool
            .run(arguments)
            .map_err(|e| RequiemError::Tool {
                tool: name.to_string(),
                message: e,
            })
    }
}

/// Shared handle to a `RouteExecutor`'s call log and tool registry.
#[derive(Clone)]
struct ExecutorProbe {
    calls: Arc<Mutex<Vec<String>>>,
    tools: Vec<String>,
}

impl RouteExecutor {
    fn probe(&self) -> ExecutorProbe {
        let mut v: Vec<String> = self.tools.keys().cloned().collect();
        v.sort();
        ExecutorProbe {
            calls: self.calls.clone(),
            tools: v,
        }
    }
}

// ───────────────────────────────────────────────────────────────────
// 1. Agent loop drives a real tool executor against the mock server.
//    The model requests a `calc` tool call; the executor routes it to a
//    concrete tool; the result flows back and the model's final answer
//    (which cites the tool output) is recorded.
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn agent_loop_executes_model_tool_calls_and_records_usage() {
    let server = MockServer::start(
        vec![StatusCode::OK, StatusCode::OK],
        vec![
            calc_tool_call_body("call_1", 40, 2),
            chat_body("The sum is 42."),
        ],
    )
    .await;
    let client = server.client();

    let concrete = RouteExecutor::with_calc_and_echo();
    let probe = concrete.probe();
    let executor: Arc<dyn ToolExecutor> = Arc::new(concrete);
    let agent = Agent::master("master", requiem_core::Id::new());
    let task = Task::new("compute 40 + 2");
    let mut run = AgentRun::new(agent, task);

    run_agent(&client, &executor, &mut run, &[calc_tool_def()]).await.unwrap();

    // The model stopped with a final text answer.
    let text = run
        .final_response
        .as_ref()
        .and_then(|r| r.text())
        .expect("final response has text")
        .to_string();
    assert!(text.contains("42"), "final answer must cite the tool result, got: {text}");

    // The executor really ran the concrete tool, not a stub.
    assert_eq!(probe.calls.lock().unwrap().as_ref(), vec!["calc"]);

    // Two model calls, each with provider-reported usage (12+3 and 20+8).
    assert!(!run.token_totals.has_estimates);
    assert_eq!(run.token_totals.input, 32);
    assert_eq!(run.token_totals.output, 11);

    // History grew: system, user, assistant(tool call), tool result, final.
    assert!(run.history.len() >= 5);
}

// ───────────────────────────────────────────────────────────────────
// 2. Unknown tool call degrades to a "tool error" result, not a crash.
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn unknown_tool_call_yields_error_result_not_crash() {
    let server = MockServer::start(
        vec![StatusCode::OK, StatusCode::OK],
        vec![
            calc_tool_call_body("call_9", 1, 2),
            chat_body("tool failed as expected"),
        ],
    )
    .await;
    // The model calls `calc`, but this executor has no such tool registered,
    // so the call must degrade to a "tool error" result, not abort the run.
    struct FailingExecutor;
    #[async_trait]
    impl ToolExecutor for FailingExecutor {
        async fn execute(
            &self,
            name: &str,
            _args: &serde_json::Value,
        ) -> Result<serde_json::Value, RequiemError> {
            Err(RequiemError::Tool {
                tool: name.to_string(),
                message: "not implemented".into(),
            })
        }
    }

    let executor: Arc<dyn ToolExecutor> = Arc::new(FailingExecutor);
    let agent = Agent::master("master", requiem_core::Id::new());
    let mut run = AgentRun::new(agent, Task::new("try an unknown tool"));
    run_agent(&client_for(&server), &executor, &mut run, &[calc_tool_def()])
        .await
        .unwrap();

    // The loop survives the tool error and reaches the model's final answer.
    assert!(run.final_response.is_some());
    // The tool result that was fed back must be the error text.
    let tool_msgs: Vec<&str> = run
        .history
        .iter()
        .filter(|m| m.role == requiem_model::request::Role::Tool)
        .filter_map(|m| m.content.as_ref().and_then(|c| c.as_str()))
        .collect();
    assert!(
        tool_msgs
            .iter()
            .any(|s| s.contains("tool error") && s.contains("calc")),
        "tool error must be fed back, got: {tool_msgs:?}"
    );
}

fn client_for(server: &MockServer) -> ModelClient {
    server.client()
}

// ───────────────────────────────────────────────────────────────────
// 2b. The model client sends the configured API key on the wire.
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn model_client_sends_configured_api_key() {
    let server = MockServer::start(
        vec![StatusCode::OK],
        vec![chat_body("hi")],
    )
    .await;
    let client = server.client();
    let agent = Agent::master("master", requiem_core::Id::new());
    let mut run = AgentRun::new(agent, Task::new("ping"));
    run_agent(&client, &no_op_executor(), &mut run, &[]).await.unwrap();

    let auth = server.seen_auth();
    assert_eq!(auth.len(), 1, "exactly one request seen");
    let header = auth[0].as_ref().expect("request carried an Authorization header");
    let h = header.to_str().unwrap().to_ascii_lowercase();
    assert!(h.starts_with("bearer "), "header must be a Bearer token, got: {h}");
}

fn no_op_executor() -> Arc<dyn ToolExecutor> {
    struct Noop;
    #[async_trait]
    impl ToolExecutor for Noop {
        async fn execute(
            &self,
            _name: &str,
            _args: &serde_json::Value,
        ) -> Result<serde_json::Value, RequiemError> {
            Ok(serde_json::json!({}))
        }
    }
    Arc::new(Noop)
}

// ───────────────────────────────────────────────────────────────────
// 3. Subagent spawn: a child runs to completion against the mock server
//    and reports its result back to the parent.
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn subagent_spawn_runs_child_and_reports_result() {
    let server = MockServer::start(
        vec![StatusCode::OK],
        vec![chat_body("child finished its task")],
    )
    .await;
    let client = Arc::new(server.client());

    let executor: Arc<dyn ToolExecutor> = Arc::new(RouteExecutor::with_calc_and_echo());
    let parent = Agent::master("master", requiem_core::Id::new());
    let tool = make_subagent_tool(
        client,
        executor,
        parent.clone(),
        requiem_core::Id::new(),
        vec![],
    );

    let res = tool
        .execute(&serde_json::json!({
            "name": "research-1",
            "role": "researcher",
            "task": "investigate the issue"
        }))
        .await
        .unwrap();

    assert_eq!(res["status"], "completed");
    assert_eq!(res["agent_name"], "research-1");
    assert_eq!(res["response"], "child finished its task");
    assert!(res["turns"].as_u64().unwrap() >= 1);
}

// ───────────────────────────────────────────────────────────────────
// 4. Subagent spawn is refused when the parent is already at max depth.
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn subagent_spawn_refused_at_max_depth() {
    let server = MockServer::start(
        vec![StatusCode::OK],
        vec![chat_body("x")],
    )
    .await;
    let client = Arc::new(server.client());

    let mut deep = Agent::master("deep", requiem_core::Id::new());
    deep.depth = requiem_orchestration::subagent::MAX_DEPTH;

    let tool = make_subagent_tool(
        client,
        Arc::new(RouteExecutor::with_calc_and_echo()),
        deep,
        requiem_core::Id::new(),
        vec![],
    )
    .with_max_depth(requiem_orchestration::subagent::MAX_DEPTH);

    let err = tool
        .execute(&serde_json::json!({
            "name": "too-deep",
            "role": "tester",
            "task": "t"
        }))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("depth"),
        "spawn at max depth must be refused, got: {err}"
    );
}

// ───────────────────────────────────────────────────────────────────
// 5. SpawnToolMap routes the child's tool calls to a shared executor.
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn spawn_tool_map_routes_child_calls_to_executor() {
    let server = MockServer::start(
        vec![StatusCode::OK],
        vec![chat_body("done")],
    )
    .await;
    let client = Arc::new(server.client());

    let concrete = RouteExecutor::with_calc_and_echo();
    let probe = concrete.probe();
    let executor: Arc<dyn ToolExecutor> = Arc::new(concrete);
    let map = SpawnToolMap::new(vec![calc_tool_def()], executor.clone());

    let parent = Agent::master("master", requiem_core::Id::new());
    let tool = make_subagent_tool(
        client,
        executor.clone(),
        parent,
        requiem_core::Id::new(),
        vec![],
    )
    .with_spawn_tool_map(map);

    let res = tool
        .execute(&serde_json::json!({
            "name": "calc-worker",
            "role": "implementer",
            "task": "use the shared executor"
        }))
        .await
        .unwrap();
    assert_eq!(res["status"], "completed");
    // The child resolved its executor from the map; that shared executor
    // advertised the `calc` tool definition and never handled a call
    // (the child's model response requested no tools).
    assert!(probe.calls.lock().unwrap().is_empty());
    assert!(probe.tools.contains(&"calc".to_string()));
}

// ───────────────────────────────────────────────────────────────────
// 6. Cancellation is observed between turns and returned as Cancelled.
// ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn cancelled_run_returns_cancelled_error() {
    let server = MockServer::start(
        vec![StatusCode::OK],
        vec![chat_body("never reached")],
    )
    .await;
    let client = server.client();

    let executor: Arc<dyn ToolExecutor> = Arc::new(RouteExecutor::with_calc_and_echo());
    let agent = Agent::master("master", requiem_core::Id::new());
    let mut run = AgentRun::new(agent, Task::new("work that gets cancelled"));

    let flag = CancelFlag::new();
    flag.cancel();
    let events = tokio::sync::broadcast::channel::<requiem_core::events::Event>(64).0;

    let err =
        run_agent_halted(&client, &executor, &mut run, &[], Some(&events), Some(&flag))
            .await
            .unwrap_err();
    assert!(matches!(err, RequiemError::Cancelled));
}

// ───────────────────────────────────────────────────────────────────
// 7. Permission gate: mode profiles propagate to concrete decisions.
//    This is the same gate the CLI's ToolBridge consults before executing
//    a tool, so it pins down the cross-crate permission boundary.
// ───────────────────────────────────────────────────────────────────

#[test]
fn permission_gate_mode_profiles() {
    let ask = PermissionGate::for_mode(Mode::Ask);
    assert!(!ask.can_write_file().is_allowed());
    assert!(!ask.can_mutate_git().is_allowed());
    assert!(!ask.can_spawn_agent().is_allowed());
    assert!(ask.can_run_terminal(false).is_allowed());
    assert!(!ask.can_run_terminal(true).is_allowed());

    let code = PermissionGate::for_mode(Mode::Code);
    assert!(code.can_write_file().is_allowed());
    assert!(code.can_mutate_git().is_allowed());
    assert!(code.can_spawn_agent().is_allowed());

    let auto = PermissionGate::for_mode(Mode::Auto);
    assert!(auto.can_write_file().is_allowed());
    assert!(auto.can_run_terminal(true).is_allowed());
    assert!(auto.can_mutate_git().is_allowed());
    assert!(auto.can_write_vault().is_allowed());
    assert!(auto.can_spawn_agent().is_allowed());

    let research = PermissionGate::for_mode(Mode::Research);
    assert!(!research.can_write_file().is_allowed());
    assert!(research.can_write_vault().is_allowed());
    assert!(research.can_spawn_agent().is_allowed());
}
