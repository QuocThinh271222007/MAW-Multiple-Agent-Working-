//! Machine performance sampling while local models run.
//!
//! Sources, each optional and labelled in the view:
//! * CPU and RAM of the machine that actually runs the model. Under WSL that is the **Windows
//!   host**, read through one long-lived `powershell.exe`; plain Linux reads `/proc`.
//! * GPU load, VRAM, temperature and power from `nvidia-smi` (whole GPU, other programs included).
//! * Models held in memory and the latest generation speed, from providers that run local models.
//!
//! A source that cannot be read leaves its fields empty; nothing is estimated.

use crate::supervisor::Supervisor;
use chrono::Utc;
use maw_core::perf::{PerfHistory, PerfSample};
use std::process::Stdio;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub const SAMPLE_EVERY: Duration = Duration::from_secs(2);
/// 30 minutes at one sample per 2 s.
pub const HISTORY_SAMPLES: usize = 900;

/// Shared, bounded history filled by the sampler.
pub struct PerfMonitor {
    history: RwLock<PerfHistory>,
    started: Mutex<bool>,
}

impl PerfMonitor {
    pub fn new() -> Arc<Self> {
        Arc::new(PerfMonitor {
            history: RwLock::new(PerfHistory::new(HISTORY_SAMPLES)),
            started: Mutex::new(false),
        })
    }

    pub fn snapshot(&self) -> PerfHistory {
        self.history
            .read()
            .map(|h| h.clone())
            .unwrap_or_else(|_| PerfHistory::new(1))
    }

    fn push(&self, s: PerfSample) {
        if let Ok(mut h) = self.history.write() {
            h.push(s);
        }
    }

    fn set_sources(&self, system: String, gpu: String) {
        if let Ok(mut h) = self.history.write() {
            h.sources.system = system;
            h.sources.gpu = gpu;
        }
    }

    /// Start sampling once; later calls do nothing. The task ends when the supervisor is dropped.
    pub fn start(self: &Arc<Self>, sup: Weak<Supervisor>) {
        {
            let Ok(mut s) = self.started.lock() else {
                return;
            };
            if *s {
                return;
            }
            *s = true;
        }
        let me = self.clone();
        tokio::spawn(async move { me.run(sup).await });
    }

    async fn run(self: Arc<Self>, sup: Weak<Supervisor>) {
        let mut host = HostProbe::start();
        let gpu_cmd = detect_gpu_tool().await;
        self.set_sources(
            host.describe(),
            gpu_cmd
                .map(|c| format!("{c} (whole GPU, including other programs)"))
                .unwrap_or_default(),
        );
        let mut cpu_prev: Option<(u64, u64)> = None;
        let mut runtime = maw_core::perf::RuntimeStatus::default();
        let mut tick = 0u64;
        loop {
            let Some(sup) = sup.upgrade() else { return };
            if let HostProbe::Windows { child, .. } = &mut host {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    host = HostProbe::start();
                }
            }
            let mut s = PerfSample {
                at: Utc::now(),
                ..Default::default()
            };
            match &host {
                HostProbe::Windows { latest, .. } => {
                    if let Some(r) = latest.lock().ok().and_then(|g| *g) {
                        if r.at.elapsed() < Duration::from_secs(10) {
                            s.cpu_pct = Some(r.cpu_pct);
                            s.ram_used_mb = Some(r.used_mb);
                            s.ram_total_mb = Some(r.total_mb);
                        }
                    }
                }
                HostProbe::Proc { .. } | HostProbe::None => {
                    if let Ok(t) = std::fs::read_to_string("/proc/stat") {
                        if let Some(cur) = parse_proc_stat_cpu(&t) {
                            s.cpu_pct = cpu_percent(cpu_prev, cur);
                            cpu_prev = Some(cur);
                        }
                    }
                    if let Ok(t) = std::fs::read_to_string("/proc/meminfo") {
                        if let Some((u, tot)) = parse_meminfo(&t) {
                            s.ram_used_mb = Some(u);
                            s.ram_total_mb = Some(tot);
                        }
                    }
                }
            }
            if let Some(cmd) = gpu_cmd {
                if let Some(g) = read_gpu(cmd).await {
                    s.gpu_pct = Some(g.util_pct);
                    s.vram_used_mb = Some(g.mem_used_mb);
                    s.vram_total_mb = Some(g.mem_total_mb);
                    s.gpu_temp_c = g.temp_c;
                    s.gpu_power_w = g.power_w;
                }
            }
            // provider runtimes are asked every other tick (it is an HTTP call per provider)
            if tick & 1 == 0 {
                runtime = sup.runtime_status().await;
            }
            tick += 1;
            s.loaded = runtime.loaded.clone();
            s.decode_tps = runtime.decode_tps;
            s.prompt_tps = runtime.prompt_tps;
            self.push(s);
            drop(sup);
            tokio::time::sleep(SAMPLE_EVERY).await;
        }
    }
}

// ---- host (CPU / RAM) ------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct HostReading {
    at: Instant,
    cpu_pct: f32,
    used_mb: u64,
    total_mb: u64,
}

enum HostProbe {
    /// Windows machine read through a persistent PowerShell (native Windows or via WSL).
    Windows {
        latest: Arc<Mutex<Option<HostReading>>>,
        via_wsl: bool,
        // killed when the probe is dropped; polled so an ended PowerShell is restarted
        child: Box<tokio::process::Child>,
    },
    /// This Linux machine through /proc.
    Proc {
        wsl_only: bool,
    },
    None,
}

// The loop ends by itself after 4 h (7200 x 2 s) so that a PowerShell left behind by a killed MAW
// cannot run forever; the sampler restarts it when it ends.
const HOST_SCRIPT: &str = "for($i=0;$i -lt 7200;$i++){$o=Get-CimInstance Win32_OperatingSystem;\
$c=(Get-CimInstance Win32_PerfFormattedData_PerfOS_Processor | Where-Object {$_.Name -eq '_Total'}).PercentProcessorTime;\
[Console]::Out.WriteLine([string]::Format('{0},{1},{2}',$c,$o.FreePhysicalMemory,$o.TotalVisibleMemorySize));\
Start-Sleep -Seconds 2}";

impl HostProbe {
    fn start() -> HostProbe {
        let wsl = is_wsl();
        if wsl || cfg!(windows) {
            if let Some(p) = spawn_powershell() {
                let (child, latest) = p;
                return HostProbe::Windows {
                    latest,
                    via_wsl: wsl,
                    child: Box::new(child),
                };
            }
        }
        if cfg!(target_os = "linux") {
            return HostProbe::Proc { wsl_only: wsl };
        }
        HostProbe::None
    }

    fn describe(&self) -> String {
        match self {
            HostProbe::Windows { via_wsl: true, .. } => {
                "Windows host (read through PowerShell from WSL)".into()
            }
            HostProbe::Windows { .. } => "this Windows machine (PowerShell)".into(),
            HostProbe::Proc { wsl_only: true } => {
                "WSL virtual machine only (PowerShell not found): Windows' own CPU/RAM are not visible".into()
            }
            HostProbe::Proc { .. } => "this Linux machine (/proc)".into(),
            HostProbe::None => "no CPU/RAM source on this platform".into(),
        }
    }
}

fn is_wsl() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var_os("WSL_DISTRO_NAME").is_some()
            || std::fs::read_to_string("/proc/version")
                .map(|v| v.to_ascii_lowercase().contains("microsoft"))
                .unwrap_or(false))
}

fn spawn_powershell() -> Option<(tokio::process::Child, Arc<Mutex<Option<HostReading>>>)> {
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", HOST_SCRIPT])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn().ok()?;
    let out = child.stdout.take()?;
    let latest: Arc<Mutex<Option<HostReading>>> = Arc::new(Mutex::new(None));
    let l2 = latest.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(out).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some((cpu, used, total)) = parse_host_line(&line) {
                if let Ok(mut g) = l2.lock() {
                    *g = Some(HostReading {
                        at: Instant::now(),
                        cpu_pct: cpu,
                        used_mb: used,
                        total_mb: total,
                    });
                }
            }
        }
    });
    Some((child, latest))
}

/// `cpu%,free_kb,total_kb` as printed by the PowerShell loop.
pub fn parse_host_line(line: &str) -> Option<(f32, u64, u64)> {
    let mut it = line.trim().split(',');
    let cpu: f32 = it.next()?.trim().parse().ok()?;
    let free_kb: u64 = it.next()?.trim().parse().ok()?;
    let total_kb: u64 = it.next()?.trim().parse().ok()?;
    if total_kb == 0 || free_kb > total_kb {
        return None;
    }
    Some((
        cpu.clamp(0.0, 100.0),
        (total_kb - free_kb) / 1024,
        total_kb / 1024,
    ))
}

// ---- Linux /proc -----------------------------------------------------------------------------

/// `(total, idle)` jiffies from the first line of `/proc/stat`.
pub fn parse_proc_stat_cpu(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().next()?;
    let mut f = line.split_whitespace();
    if f.next()? != "cpu" {
        return None;
    }
    let v: Vec<u64> = f.take(8).filter_map(|x| x.parse().ok()).collect();
    if v.len() < 5 {
        return None;
    }
    let total: u64 = v.iter().sum();
    let idle = v[3] + v.get(4).copied().unwrap_or(0);
    Some((total, idle))
}

pub fn cpu_percent(prev: Option<(u64, u64)>, cur: (u64, u64)) -> Option<f32> {
    let (pt, pi) = prev?;
    let dt = cur.0.checked_sub(pt)?;
    let di = cur.1.checked_sub(pi)?;
    if dt == 0 {
        return None;
    }
    Some((100.0 * (dt - di.min(dt)) as f32 / dt as f32).clamp(0.0, 100.0))
}

/// `(used_mb, total_mb)` where used = total - available.
pub fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let kb = |key: &str| -> Option<u64> {
        text.lines()
            .find(|l| l.starts_with(key))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    };
    let total = kb("MemTotal:")?;
    let avail = kb("MemAvailable:")?;
    Some((total.saturating_sub(avail) / 1024, total / 1024))
}

// ---- GPU -------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct GpuReading {
    pub util_pct: f32,
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
    pub temp_c: Option<f32>,
    pub power_w: Option<f32>,
}

const GPU_ARGS: [&str; 2] = [
    "--query-gpu=utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw",
    "--format=csv,noheader,nounits",
];

/// One `nvidia-smi` CSV line (first GPU). Fields may be `[N/A]`.
pub fn parse_nvidia_line(line: &str) -> Option<GpuReading> {
    let f: Vec<&str> = line.lines().next()?.split(',').map(str::trim).collect();
    if f.len() < 3 {
        return None;
    }
    let num = |i: usize| f.get(i).and_then(|s| s.parse::<f32>().ok());
    Some(GpuReading {
        util_pct: num(0)?.clamp(0.0, 100.0),
        mem_used_mb: num(1)? as u64,
        mem_total_mb: num(2)? as u64,
        temp_c: num(3),
        power_w: num(4),
    })
}

async fn run_gpu(cmd: &str) -> Option<GpuReading> {
    let out = tokio::time::timeout(
        Duration::from_secs(3),
        Command::new(cmd)
            .args(GPU_ARGS)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_nvidia_line(&String::from_utf8_lossy(&out.stdout))
}

async fn detect_gpu_tool() -> Option<&'static str> {
    for c in ["nvidia-smi", "nvidia-smi.exe"] {
        if run_gpu(c).await.is_some() {
            return Some(c);
        }
    }
    None
}

async fn read_gpu(cmd: &str) -> Option<GpuReading> {
    run_gpu(cmd).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_load_comes_from_jiffy_deltas() {
        let a = parse_proc_stat_cpu("cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 1 1 1 1\n").unwrap();
        let b = parse_proc_stat_cpu("cpu  200 0 200 1600 0 0 0 0 0 0\n").unwrap();
        assert_eq!(a, (1000, 800));
        let pct = cpu_percent(Some(a), b).unwrap();
        assert!((pct - 20.0).abs() < 0.01, "{pct}");
        assert!(
            cpu_percent(None, b).is_none(),
            "the first reading has nothing to compare with"
        );
        assert!(cpu_percent(Some(b), b).is_none(), "no time passed");
        assert!(parse_proc_stat_cpu("garbage").is_none());
    }

    #[test]
    fn memory_is_total_minus_available() {
        let t = "MemTotal:        8192000 kB\nMemFree: 1 kB\nMemAvailable:    2048000 kB\n";
        assert_eq!(parse_meminfo(t), Some((6000, 8000)));
        assert!(parse_meminfo("MemTotal: 5 kB\n").is_none());
    }

    #[test]
    fn nvidia_smi_lines_tolerate_missing_fields() {
        let g = parse_nvidia_line("37, 1234, 6144, 51, 22.40\n").unwrap();
        assert_eq!(
            (g.util_pct, g.mem_used_mb, g.mem_total_mb),
            (37.0, 1234, 6144)
        );
        assert_eq!((g.temp_c, g.power_w), (Some(51.0), Some(22.4)));
        let g = parse_nvidia_line("5, 300, 6144, [N/A], [N/A]").unwrap();
        assert_eq!((g.temp_c, g.power_w), (None, None));
        assert!(parse_nvidia_line("[N/A], 1, 2").is_none());
        assert!(parse_nvidia_line("").is_none());
    }

    #[test]
    fn host_lines_from_powershell_are_validated() {
        // 12 GB total, 3 GB free
        assert_eq!(
            parse_host_line("42,3145728,12582912"),
            Some((42.0, 9216, 12288))
        );
        assert_eq!(parse_host_line("150,0,1024").unwrap().0, 100.0);
        for bad in ["", "x,1,2", "1,2", "1,5000,1000", "1,1,0"] {
            assert!(parse_host_line(bad).is_none(), "{bad:?}");
        }
    }
}
