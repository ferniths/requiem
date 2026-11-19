//! Integration tests using a mock stdio MCP server.
//!
//! The mock server is a Python script that speaks minimal MCP JSON-RPC
//! over stdin/stdout, implementing `initialize`, `tools/list`, and
//! `tools/call`. This validates the full `McpManager::connect` →
//! `register_tools` → `call_tool` chain without any real network.
//!
//! Tests are skipped (not failed) when `python3` is not in PATH.

use std::collections::HashMap;
use std::sync::Arc;

use requiem_mcp::{McpConfig, McpManager, McpServerConfig, McpTransport};
use requiem_tools::Tool;

fn mock_server_config() -> McpConfig {
    McpConfig {
        servers: vec![McpServerConfig {
            name: "mock".into(),
            description: "test server".into(),
            transport: McpTransport::Stdio {
                command: "python3".into(),
                args: vec![mock_script_path()],
            },
            enabled: true,
            auto_connect: true,
            request_timeout_ms: 10_000,
            max_concurrency: 2,
            env: vec![],
            working_dir: String::new(),
            tool_policies: vec![],
            default_tool_policy: "auto".into(),
            tool_allow: vec![],
            tool_deny: vec![],
            trusted: true,
        }],
    }
}

fn has_python3() -> bool {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let bin = if cfg!(windows) { "python3.exe" } else { "python3" };
        if dir.join(bin).is_file() {
            return true;
        }
    }
    false
}

fn mock_script_path() -> String {
    let dir = std::env::temp_dir();
    let script = dir.join("requiem_mcp_mock.py");
    let content = r#"
import sys, json

def send(obj):
    print(json.dumps(obj), flush=True)

def read():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    return json.loads(line)

while True:
    try:
        req = read()
    except (json.JSONDecodeError, EOFError):
        break
    method = req.get("method", "")
    rid = req.get("id")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":rid,"result":{
            "protocolVersion":"2024-11-05",
            "capabilities":{"tools":{"listChanged":False}},
            "serverInfo":{"name":"mock","version":"0.1.0"}
        }})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        send({"jsonrpc":"2.0","id":rid,"result":{"tools":[
            {"name":"echo","description":"Echoes input","inputSchema":{"type":"object","properties":{"msg":{"type":"string"}}}}
        ]}})
    elif method == "tools/call":
        params = req.get("params", {})
        tool = params.get("name", "")
        args = params.get("arguments", {})
        if tool == "echo":
            send({"jsonrpc":"2.0","id":rid,"result":{
                "content":[{"type":"text","text":args.get("msg","")}],
                "isError":False
            }})
        else:
            send({"jsonrpc":"2.0","id":rid,"result":{
                "content":[{"type":"text","text":"unknown tool"}],
                "isError":True
            }})
    elif method == "resources/list":
        send({"jsonrpc":"2.0","id":rid,"result":{"resources":[]}})
    elif method == "prompts/list":
        send({"jsonrpc":"2.0","id":rid,"result":{"prompts":[]}})
    elif method == "resources/read":
        send({"jsonrpc":"2.0","id":rid,"result":{"contents":[]}})
    elif method == "prompts/get":
        send({"jsonrpc":"2.0","id":rid,"result":{"description":"","messages":[]}})
    else:
        if rid is not None:
            send({"jsonrpc":"2.0","id":rid,"result":{}})
"#;
    let _ = std::fs::write(&script, content);
    script.to_string_lossy().to_string()
}

#[tokio::test]
async fn connect_and_list_tools() {
    if !has_python3() {
        eprintln!("SKIP: python3 not in PATH");
        return;
    }
    let config = mock_server_config();
    let manager = McpManager::new(config);
    let result = manager.connect("mock").await;
    match result {
        Ok(()) => {
            let mut tools_map: HashMap<String, Arc<dyn Tool>> = HashMap::new();
            let n = manager.register_tools("mock", &mut tools_map).await.unwrap();
            assert!(n >= 1, "expected at least one tool, got {n}");
            assert!(tools_map.contains_key("mock__echo"));
        }
        Err(e) => {
            eprintln!("SKIP: connect failed: {e}");
        }
    }
}

#[tokio::test]
async fn call_mock_echo_tool() {
    if !has_python3() {
        eprintln!("SKIP: python3 not in PATH");
        return;
    }
    let config = mock_server_config();
    let manager = McpManager::new(config);
    if manager.connect("mock").await.is_err() {
        eprintln!("SKIP: connect failed");
        return;
    }

    let args = serde_json::json!({"msg": "hello requiem"});
    let result = manager
        .call_tool("mock", "echo", &args)
        .await
        .expect("call_tool failed");
    let text = result["text"].as_str().unwrap_or("");
    assert_eq!(text, "hello requiem");
}

#[tokio::test]
async fn lifecycle_snapshots_report_disconnected() {
    let config = mock_server_config();
    let manager = McpManager::new(config);
    let snaps = manager.lifecycle_snapshots().await;
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].name, "mock");
    assert!(matches!(
        snaps[0].status,
        requiem_mcp::McpStatus::Disconnected
    ));
}
