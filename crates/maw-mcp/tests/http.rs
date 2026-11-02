use maw_core::{EventKind, OrchestratorConfig, PermissionProfile, ProviderKind, Role};
use maw_orchestrator::Engine;
use maw_storage::Store;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::ServiceExt;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

struct Fx {
    engine: Arc<Engine>,
    server: maw_mcp::McpHttpServer,
    leader_token: String,
    worker_token: String,
}

async fn fixture() -> Fx {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Engine::open(store, "p", "/tmp/p", OrchestratorConfig::default()).unwrap();
    let leader = engine
        .add_agent(
            "lead",
            ProviderKind::Mock,
            Role::Leader,
            None,
            PermissionProfile::Safe,
            None,
        )
        .unwrap();
    let worker = engine
        .add_agent(
            "w1",
            ProviderKind::Mock,
            Role::Worker,
            None,
            PermissionProfile::Safe,
            None,
        )
        .unwrap();
    engine.create_goal("ship it", vec![]).unwrap();
    let leader_token = engine.mint_token(&leader.id);
    let worker_token = engine.mint_token(&worker.id);
    let server = maw_mcp::serve_http(engine.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    Fx {
        engine,
        server,
        leader_token,
        worker_token,
    }
}

type Client = rmcp::service::RunningService<rmcp::RoleClient, ()>;

async fn connect(url: &str, token: &str) -> Client {
    let cfg = StreamableHttpClientTransportConfig::with_uri(url.to_string())
        .auth_header(token.to_string());
    ().serve(StreamableHttpClientTransport::from_config(cfg))
        .await
        .expect("MCP handshake")
}

async fn call(c: &Client, tool: &str, args: Value) -> (bool, Value) {
    let params = CallToolRequestParams::new(tool.to_string())
        .with_arguments(args.as_object().cloned().unwrap_or_default());
    let res = c.call_tool(params).await.expect("call_tool transport");
    let is_err = res.is_error.unwrap_or(false);
    let body = res.structured_content.unwrap_or(Value::Null);
    (is_err, body)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_missing_and_forged_tokens() {
    let f = fixture().await;
    let http = reqwest::Client::new();
    let r = http
        .post(&f.server.url)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = http
        .post(&f.server.url)
        .bearer_auth("maw_tok_forged")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    f.server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_origins_and_foreign_hosts_are_refused() {
    let f = fixture().await;
    let http = reqwest::Client::new();
    let r = http
        .post(&f.server.url)
        .bearer_auth(&f.leader_token)
        .header("origin", "https://evil.example")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
        .send()
        .await
        .unwrap();
    assert!(
        r.status().is_client_error(),
        "origin must be rejected, got {}",
        r.status()
    );
    let r = http
        .post(&f.server.url)
        .bearer_auth(&f.leader_token)
        .header("host", "attacker.example")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
        .send()
        .await
        .unwrap();
    assert!(
        r.status().is_client_error(),
        "foreign Host must be rejected (DNS rebinding), got {}",
        r.status()
    );
    f.server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refuses_to_bind_non_loopback() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Engine::open(store, "p", "/tmp/p", OrchestratorConfig::default()).unwrap();
    let addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
    assert!(maw_mcp::serve_http(engine, addr).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_lists_are_filtered_by_the_role_the_token_maps_to() {
    let f = fixture().await;
    let lead = connect(&f.server.url, &f.leader_token).await;
    let work = connect(&f.server.url, &f.worker_token).await;
    let lt: Vec<String> = lead
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    let wt: Vec<String> = work
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    assert!(
        lt.contains(&"maw_task_create".to_string()) && lt.contains(&"maw_task_verify".to_string())
    );
    assert!(
        !wt.contains(&"maw_task_create".to_string()),
        "workers must not even see leader tools"
    );
    assert!(
        wt.contains(&"maw_task_submit_result".to_string())
            && wt.contains(&"maw_task_claim".to_string())
    );
    assert!(lt.len() > wt.len());
    // schemas are real JSON Schemas
    let tools = lead.list_all_tools().await.unwrap();
    let create = tools.iter().find(|t| t.name == "maw_task_create").unwrap();
    assert!(create
        .input_schema
        .get("properties")
        .and_then(|p| p.get("title"))
        .is_some());
    lead.cancel().await.unwrap();
    work.cancel().await.unwrap();
    f.server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_client_round_trip_with_structured_errors() {
    let f = fixture().await;
    let lead = connect(&f.server.url, &f.leader_token).await;
    let work = connect(&f.server.url, &f.worker_token).await;

    // leader creates a task via MCP; the engine state changes for real
    let (err, body) = call(
        &lead,
        "maw_task_create",
        json!({ "title": "Implement login", "assignee": "w1", "idempotency_key": "x1" }),
    )
    .await;
    assert!(!err, "{body}");
    assert_eq!(body["task"], "T-1");
    let (_, again) = call(
        &lead,
        "maw_task_create",
        json!({ "title": "Implement login", "assignee": "w1", "idempotency_key": "x1" }),
    )
    .await;
    assert_eq!(again["id"], body["id"], "retries do not duplicate");
    assert_eq!(f.engine.list_tasks().unwrap().len(), 1);

    // the worker calling a leader-only tool gets a structured error, not a crash
    let (err, body) = call(&work, "maw_task_create", json!({ "title": "sneaky" })).await;
    assert!(err);
    assert_eq!(body["error"]["code"], "forbidden");
    // unknown tool and bad arguments are structured too
    let (err, body) = call(&work, "maw_nope", json!({})).await;
    assert!(err && body["error"]["code"] == "not_found");
    let (err, body) = call(
        &lead,
        "maw_task_create",
        json!({ "description": "no title" }),
    )
    .await;
    assert!(err && body["error"]["code"] == "invalid_input", "{body}");

    // worker drives its task: progress -> blocker visible to leader's inbox
    f.engine.start_task("T-1").unwrap();
    let (err, _) = call(
        &work,
        "maw_progress_report",
        json!({ "task": "T-1", "percent": 40, "note": "halfway" }),
    )
    .await;
    assert!(!err);
    let (err, _) = call(
        &work,
        "maw_task_block",
        json!({ "task": "T-1", "reason": "need schema", "needs": "db schema" }),
    )
    .await;
    assert!(!err);
    let (_, inbox) = call(&lead, "maw_message_inbox", json!({})).await;
    let msgs = inbox["messages"].as_array().unwrap();
    assert!(
        msgs.iter()
            .any(|m| m["type"] == "BLOCKER" && m["from"] == "w1"),
        "{inbox}"
    );

    // identity comes from the token: a worker passing someone else's name changes nothing
    let (err, body) = call(
        &work,
        "maw_message_ack",
        json!({ "message_id": msgs[0]["message_id"] }),
    )
    .await;
    assert!(
        err && body["error"]["code"] == "forbidden",
        "worker may not ack the leader's mail: {body}"
    );

    // tool calls are recorded as events
    let events = f
        .engine
        .read(|db| db.events_since(f.engine.project_id(), 0, 10_000))
        .unwrap();
    assert!(events
        .iter()
        .any(|e| e.event_type == EventKind::McpToolCompleted
            && e.payload["tool"] == "maw_task_block"));

    lead.cancel().await.unwrap();
    work.cancel().await.unwrap();
    f.server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoked_tokens_stop_working() {
    let f = fixture().await;
    let http = reqwest::Client::new();
    let ok = http
        .post(&f.server.url)
        .bearer_auth(&f.worker_token)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
        .send()
        .await
        .unwrap();
    assert_ne!(ok.status(), 401);
    let worker = f.engine.resolve_agent("w1").unwrap();
    f.engine.revoke_tokens(&worker.id);
    let r = http
        .post(&f.server.url)
        .bearer_auth(&f.worker_token)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    f.server.stop().await;
}
