//! Downloading models through MAW, against a scripted fake server.

use maw_core::provider::*;
use maw_provider_ollama::{OllamaConfig, OllamaProvider};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

async fn fake(pull_body: String, caps: Vec<&'static str>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let (body, caps) = (pull_body.clone(), caps.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let out = if head.contains("/api/version") {
                    json!({"version":"t"}).to_string()
                } else if head.contains("/api/show") {
                    json!({"capabilities": caps}).to_string()
                } else if head.contains("/api/pull") {
                    body
                } else {
                    "{}".to_string()
                };
                let r = format!("HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n{out}");
                let _ = s.write_all(r.as_bytes()).await;
                let _ = s.shutdown().await;
            });
        }
    });
    url
}

fn lines(v: Vec<serde_json::Value>) -> String {
    v.into_iter().map(|x| format!("{x}\n")).collect()
}

fn provider(url: &str, max_bytes: u64) -> OllamaProvider {
    OllamaProvider::new(OllamaConfig {
        url: Some(url.to_string()),
        max_pull_bytes: max_bytes,
        autostart: false,
        ..Default::default()
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pull_reports_progress_and_checks_tool_support_afterwards() {
    let body = lines(vec![
        json!({"status":"pulling manifest"}),
        json!({"status":"pulling aaa","digest":"sha256:aaa","total":4_000_000_000u64,"completed":1_000_000_000u64}),
        json!({"status":"pulling aaa","digest":"sha256:aaa","total":4_000_000_000u64,"completed":4_000_000_000u64}),
        json!({"status":"pulling bbb","digest":"sha256:bbb","total":1000,"completed":1000}),
        json!({"status":"success"}),
    ]);
    let url = fake(body, vec!["completion", "tools", "thinking"]).await;
    let (tx, mut rx) = mpsc::channel(64);
    let msg = provider(&url, 6_500_000_000)
        .pull_model("qwen3:8b", tx)
        .await
        .unwrap();
    assert!(
        msg.contains("qwen3:8b downloaded") && msg.contains("supports tools and thinking"),
        "{msg}"
    );
    let mut seen = vec![];
    while let Ok(p) = rx.try_recv() {
        seen.push(p);
    }
    assert!(
        seen.iter()
            .any(|p| p.total == 4_000_000_000 && p.completed == 1_000_000_000),
        "{seen:?}"
    );
    assert!(seen.last().unwrap().status == "success");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_without_tool_support_is_flagged_after_the_download() {
    let url = fake(lines(vec![json!({"status":"success"})]), vec!["completion"]).await;
    let (tx, _rx) = mpsc::channel(8);
    let msg = provider(&url, 6_500_000_000)
        .pull_model("gemma3:4b", tx)
        .await
        .unwrap();
    assert!(
        msg.contains("WARNING") && msg.contains("cannot run as an agent"),
        "{msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_downloads_are_cancelled_and_big_names_are_refused_up_front() {
    let body = lines(vec![
        json!({"status":"pulling big","digest":"sha256:big","total":20_000_000_000u64,"completed":0}),
    ]);
    let url = fake(body, vec!["tools"]).await;
    let (tx, _rx) = mpsc::channel(8);
    let e = provider(&url, 6_500_000_000)
        .pull_model("sneaky:latest", tx)
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("20.0 GB") && e.to_string().contains("limit"),
        "{e}"
    );
    let (tx, _rx) = mpsc::channel(8);
    let e = provider(&url, 6_500_000_000)
        .pull_model("llama3.1:70b", tx)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("limited to 8B"), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_errors_are_shown_to_the_user() {
    let url = fake(
        lines(vec![
            json!({"error":"pull model manifest: file does not exist"}),
        ]),
        vec![],
    )
    .await;
    let (tx, _rx) = mpsc::channel(8);
    let e = provider(&url, 6_500_000_000)
        .pull_model("nope:7b", tx)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("file does not exist"), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_too_old_for_the_model_is_told_apart_from_a_broken_download() {
    let url = fake(
        lines(vec![
            json!({"error":"pull model manifest: 412: \nThe model you are attempting to pull requires a newer version of Ollama.\nPlease download the latest version at:\n"}),
        ]),
        vec![],
    )
    .await;
    let (tx, _rx) = mpsc::channel(8);
    let e = provider(&url, 6_500_000_000)
        .pull_model("nemotron-3.5-bolt:e4b", tx)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("requires a newer version of Ollama"), "{e}");
    assert!(e.contains("version t"), "names the running version: {e}");
    assert!(e.contains("https://ollama.com/download"), "{e}");
    assert!(!e.contains('\n'), "one line: {e:?}");
    assert!(
        !e.contains("pre-release build"),
        "no pre-release advice for a plain 412: {e}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pre_release_model_says_so() {
    let url = fake(
        lines(vec![
            json!({"error":"pull model manifest: 412: The model you are attempting to pull requires a newer version of Ollama that may be in pre-release. Please see https://github.com/ollama/ollama/releases"}),
        ]),
        vec![],
    )
    .await;
    let (tx, _rx) = mpsc::channel(8);
    let e = provider(&url, 6_500_000_000)
        .pull_model("some-preview:7b", tx)
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("pre-release build"), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_means_no_limit_on_the_download_size() {
    let body = lines(vec![
        json!({"status":"pulling manifest"}),
        json!({"status":"pulling aaa","digest":"sha256:aaa","total":40_000_000_000u64,"completed":1_000u64}),
        json!({"status":"pulling aaa","digest":"sha256:aaa","total":40_000_000_000u64,"completed":40_000_000_000u64}),
        json!({"status":"success"}),
    ]);
    // 40 GB: refused under a 6.5 GB limit ...
    let url = fake(body.clone(), vec!["tools"]).await;
    let (tx, _rx) = mpsc::channel(64);
    let e = provider(&url, 6_500_000_000)
        .pull_model("big:7b", tx)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("limit"), "{e}");
    // ... and allowed with 0
    let url = fake(body, vec!["tools"]).await;
    let (tx, _rx) = mpsc::channel(64);
    let ok = provider(&url, 0).pull_model("big:7b", tx).await.unwrap();
    assert!(ok.contains("40.0 GB"), "{ok}");
}
