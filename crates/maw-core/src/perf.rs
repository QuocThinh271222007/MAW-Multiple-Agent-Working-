//! Machine performance samples, for watching the cost of running local models.
//!
//! Plain data only; the sampling itself (reading `/proc`, `nvidia-smi`, the Windows host) lives
//! in `maw-orchestrator`. Every figure is optional: a source that is unavailable leaves its fields
//! empty rather than showing a made-up zero.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// A model a local server currently holds in memory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoadedModel {
    pub name: String,
    pub size_bytes: u64,
    /// The part of `size_bytes` that sits in GPU memory (the rest is in system RAM).
    pub vram_bytes: u64,
}

impl LoadedModel {
    /// Share of the model that runs on the GPU, 0.0..=1.0.
    pub fn gpu_share(&self) -> f32 {
        if self.size_bytes == 0 {
            0.0
        } else {
            (self.vram_bytes as f32 / self.size_bytes as f32).clamp(0.0, 1.0)
        }
    }
}

/// What a provider can say about its own runtime right now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub loaded: Vec<LoadedModel>,
    /// Generation speed of the most recent request, tokens per second.
    pub decode_tps: Option<f32>,
    /// Prompt-processing speed of the most recent request, tokens per second.
    pub prompt_tps: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PerfSample {
    pub at: DateTime<Utc>,
    /// Whole-machine CPU load, percent.
    pub cpu_pct: Option<f32>,
    pub ram_used_mb: Option<u64>,
    pub ram_total_mb: Option<u64>,
    pub gpu_pct: Option<f32>,
    pub vram_used_mb: Option<u64>,
    pub vram_total_mb: Option<u64>,
    pub gpu_temp_c: Option<f32>,
    pub gpu_power_w: Option<f32>,
    pub decode_tps: Option<f32>,
    pub prompt_tps: Option<f32>,
    pub loaded: Vec<LoadedModel>,
}

/// Where the figures come from, so the view can say what it is measuring.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PerfSources {
    /// e.g. "Windows host (PowerShell)", "this Linux machine", "WSL virtual machine only".
    pub system: String,
    /// e.g. "nvidia-smi", or empty when no GPU tool was found.
    pub gpu: String,
}

/// Bounded history of samples (oldest dropped first).
#[derive(Clone, Debug)]
pub struct PerfHistory {
    cap: usize,
    samples: VecDeque<PerfSample>,
    pub sources: PerfSources,
}

impl PerfHistory {
    pub fn new(cap: usize) -> Self {
        PerfHistory {
            cap: cap.max(1),
            samples: VecDeque::new(),
            sources: PerfSources::default(),
        }
    }

    pub fn push(&mut self, s: PerfSample) {
        if self.samples.len() == self.cap {
            self.samples.pop_front();
        }
        self.samples.push_back(s);
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn latest(&self) -> Option<&PerfSample> {
        self.samples.back()
    }

    /// Samples newer than `since`, oldest first.
    pub fn since(&self, since: DateTime<Utc>) -> Vec<PerfSample> {
        self.samples
            .iter()
            .filter(|s| s.at >= since)
            .cloned()
            .collect()
    }

    pub fn all(&self) -> Vec<PerfSample> {
        self.samples.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn s(at: DateTime<Utc>) -> PerfSample {
        PerfSample {
            at,
            cpu_pct: Some(1.0),
            ..Default::default()
        }
    }

    #[test]
    fn history_is_bounded_and_filters_by_age() {
        let now = Utc::now();
        let mut h = PerfHistory::new(3);
        for i in 0..5 {
            h.push(s(now - Duration::seconds(10 - i)));
        }
        assert_eq!(h.len(), 3, "oldest samples are dropped");
        assert_eq!(h.all().len(), 3);
        let recent = h.since(now - Duration::seconds(7));
        assert!(recent.iter().all(|x| x.at >= now - Duration::seconds(7)));
        assert!(h.latest().unwrap().at > h.all()[0].at);
    }

    #[test]
    fn gpu_share_is_a_fraction_even_for_odd_input() {
        let m = |size, vram| LoadedModel {
            name: "m".into(),
            size_bytes: size,
            vram_bytes: vram,
        };
        assert_eq!(m(0, 5).gpu_share(), 0.0);
        assert_eq!(m(100, 50).gpu_share(), 0.5);
        assert_eq!(m(100, 500).gpu_share(), 1.0);
    }
}
