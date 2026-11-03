//! Live check of the performance sampler on the real machine. Opt in with `MAW_LIVE_PERF=1`
//! and run with `--ignored --nocapture`; it only reads CPU/RAM/GPU figures.

use maw_core::OrchestratorConfig;
use maw_orchestrator::{Engine, Supervisor};
use maw_storage::Store;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: reads the real machine; set MAW_LIVE_PERF=1"]
async fn live_sampler_reports_cpu_ram_gpu() {
    if std::env::var("MAW_LIVE_PERF").is_err() {
        return;
    }
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = Engine::open(store, "p", "/tmp/p", OrchestratorConfig::default()).unwrap();
    let sup = Supervisor::new(engine);
    sup.start_perf();
    sup.start_perf(); // idempotent
    tokio::time::sleep(Duration::from_secs(9)).await;
    let h = sup.perf();
    eprintln!("sources: {:?}", h.sources);
    eprintln!("samples: {}", h.len());
    for s in h.all().iter().rev().take(2) {
        eprintln!("{s:?}");
    }
    assert!(
        h.len() >= 3 && h.len() <= 8,
        "one sample per ~2 s, started once: {}",
        h.len()
    );
    let last = h.latest().unwrap();
    assert!(last.ram_total_mb.is_some(), "RAM must be readable");
}
