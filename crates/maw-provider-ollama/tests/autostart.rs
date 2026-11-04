//! Starting the server by itself. A shell script named `ollama` stands in for the real binary:
//! `serve` launches a tiny HTTP server on the address in `OLLAMA_HOST`, like the real one.

#![cfg(unix)]

use maw_core::provider::*;
use maw_provider_ollama::{OllamaConfig, OllamaProvider};
use std::os::unix::fs::PermissionsExt;

fn fake_binary(dir: &std::path::Path) -> String {
    let py = r#"
import os, http.server, json
host, _, port = os.environ["OLLAMA_HOST"].rpartition(":")
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"version": "9.9.fake"}).encode()
        self.send_response(200); self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self, *a): pass
http.server.HTTPServer((host, int(port)), H).serve_forever()
"#;
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = serve ]; then echo started >> '{}/started'; echo \"$OLLAMA_MODELS\" >> '{}/models'; exec python3 -c '{}'; fi\n",
        dir.display(),
        dir.display(),
        py.replace('\'', "'\\''")
    );
    let p = dir.join("ollama");
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p.to_string_lossy().into_owned()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_session_starts_the_server_when_nothing_answers() {
    let dir = tempfile::tempdir().unwrap();
    let bin = fake_binary(dir.path());
    let url = format!("127.0.0.1:{}", free_port());
    let p = OllamaProvider::new(OllamaConfig {
        url: Some(url.clone()),
        binary: Some(bin),
        ..Default::default()
    });
    // looking (doctor, model lists) never starts anything
    assert!(p.base_url().await.is_none());
    assert!(!dir.path().join("started").exists());
    // an agent that needs the server starts it, once, even when two start together
    let (a, b) = tokio::join!(p.ensure_server(), p.ensure_server());
    assert_eq!(a.unwrap(), format!("http://{url}"));
    assert!(b.is_ok());
    let started = std::fs::read_to_string(dir.path().join("started")).unwrap();
    assert_eq!(
        started.lines().count(),
        1,
        "started exactly once: {started:?}"
    );
    assert!(p.detect().await.unwrap().version.as_deref() == Some("9.9.fake"));
}

/// After a reboot the server is down. The supervisor asks `auth_status` before it starts a
/// session, and treats "not authenticated" as a stop sign, so a stopped server must NOT look like
/// that while autostart is on - otherwise the session start (which launches the server) never runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_server_is_not_an_auth_failure_while_autostart_is_on() {
    let on = OllamaProvider::new(OllamaConfig {
        url: Some(format!("127.0.0.1:{}", free_port())),
        ..Default::default()
    });
    let s = on.auth_status().await.unwrap();
    assert!(
        matches!(s, AuthStatus::Unknown { .. }) && !s.is_authenticated(),
        "{s:?}"
    );
    let off = OllamaProvider::new(OllamaConfig {
        url: Some(format!("127.0.0.1:{}", free_port())),
        autostart: false,
        ..Default::default()
    });
    assert!(matches!(
        off.auth_status().await.unwrap(),
        AuthStatus::NotAuthenticated { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn autostart_can_be_switched_off_and_a_missing_install_is_explained() {
    let p = OllamaProvider::new(OllamaConfig {
        url: Some(format!("127.0.0.1:{}", free_port())),
        autostart: false,
        ..Default::default()
    });
    let e = p.ensure_server().await.unwrap_err();
    assert!(
        matches!(e, ProviderError::NotInstalled(_)) && e.to_string().contains("autostart is off"),
        "{e}"
    );

    let p = OllamaProvider::new(OllamaConfig {
        url: Some(format!("127.0.0.1:{}", free_port())),
        binary: Some("/no/such/ollama-binary".into()),
        ..Default::default()
    });
    let e = p.ensure_server().await.unwrap_err();
    assert!(e.to_string().contains("not installed"), "{e}");
}

/// Where downloaded models are kept: a server MAW starts is told, and the folder exists before the
/// first download.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_mawstarts_is_told_where_to_keep_downloaded_models() {
    let dir = tempfile::tempdir().unwrap();
    let bin = fake_binary(dir.path());
    let models = dir.path().join("my-models").join("ollama");
    let p = OllamaProvider::new(OllamaConfig {
        url: Some(format!("127.0.0.1:{}", free_port())),
        binary: Some(bin),
        models_dir: Some(models.clone()),
        ..Default::default()
    });
    p.ensure_server().await.unwrap();
    let said = std::fs::read_to_string(dir.path().join("models")).unwrap();
    assert_eq!(said.trim(), models.to_string_lossy());
    assert!(models.is_dir(), "created before the first download");
}
