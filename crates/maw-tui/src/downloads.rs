//! Model downloads as the TUI shows them: what is being fetched, how far it is, how fast, how long
//! it will take, and how it ended. Pure state (no I/O): the runtime feeds it progress and outcomes,
//! the views draw it, the keys turn into `Action::StopPull` / `Action::PullModel`.

use chrono::{DateTime, Utc};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DlState {
    Downloading,
    Done,
    Failed,
    /// Stopped by the user; what was fetched is kept and a new download continues from it.
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Done(String),
    Failed(String),
    Stopped,
}

#[derive(Clone, Debug)]
pub struct Download {
    pub name: String,
    pub state: DlState,
    /// What Ollama says it is doing ("pulling", "verifying", "writing manifest" ...).
    pub status: String,
    pub completed: u64,
    pub total: u64,
    pub started: DateTime<Utc>,
    pub ended: Option<DateTime<Utc>>,
    /// How it ended: what was installed, or why it failed. Empty while downloading.
    pub message: String,
    samples: VecDeque<(DateTime<Utc>, u64)>,
}

/// Window the speed is measured over.
const SPEED_WINDOW_SECS: i64 = 8;

impl Download {
    fn new(name: &str, now: DateTime<Utc>) -> Download {
        Download {
            name: name.to_string(),
            state: DlState::Downloading,
            status: "starting".into(),
            completed: 0,
            total: 0,
            started: now,
            ended: None,
            message: String::new(),
            samples: VecDeque::new(),
        }
    }

    /// 0.0 ..= 1.0, or `None` before the size is known.
    pub fn fraction(&self) -> Option<f64> {
        (self.total > 0).then(|| (self.completed as f64 / self.total as f64).clamp(0.0, 1.0))
    }

    pub fn percent(&self) -> Option<u64> {
        self.fraction().map(|f| (f * 100.0).floor() as u64)
    }

    /// Bytes per second over the last few seconds; `None` until there is enough to measure.
    pub fn speed(&self) -> Option<f64> {
        if self.state != DlState::Downloading {
            return None;
        }
        let (t1, c1) = *self.samples.back()?;
        let (t0, c0) = *self.samples.front()?;
        let dt = (t1 - t0).num_milliseconds() as f64 / 1000.0;
        (dt >= 1.0 && c1 >= c0).then(|| (c1 - c0) as f64 / dt)
    }

    pub fn eta_secs(&self) -> Option<u64> {
        let s = self.speed().filter(|s| *s > 1.0)?;
        (self.total > self.completed).then(|| ((self.total - self.completed) as f64 / s) as u64)
    }

    pub fn elapsed_secs(&self, now: DateTime<Utc>) -> i64 {
        (self.ended.unwrap_or(now) - self.started)
            .num_seconds()
            .max(0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Downloads {
    pub list: Vec<Download>,
    pub selected: usize,
}

impl Downloads {
    /// Entries kept (finished ones are dropped oldest first beyond this).
    pub const KEEP: usize = 20;

    fn find(&mut self, name: &str) -> Option<&mut Download> {
        self.list.iter_mut().find(|d| d.name == name)
    }

    /// A download is starting (or starting again after it failed or was stopped).
    pub fn begin(&mut self, name: &str, now: DateTime<Utc>) {
        match self.find(name) {
            Some(d) => *d = Download::new(name, now),
            None => {
                self.list.push(Download::new(name, now));
                self.trim();
            }
        }
        self.selected = self.list.iter().position(|d| d.name == name).unwrap_or(0);
    }

    fn trim(&mut self) {
        while self.list.len() > Self::KEEP {
            match self
                .list
                .iter()
                .position(|d| d.state != DlState::Downloading)
            {
                Some(i) => {
                    self.list.remove(i);
                }
                None => break,
            }
        }
        self.selected = self.selected.min(self.list.len().saturating_sub(1));
    }

    pub fn progress(
        &mut self,
        name: &str,
        status: &str,
        completed: u64,
        total: u64,
        now: DateTime<Utc>,
    ) {
        if self.find(name).is_none() {
            self.begin(name, now);
        }
        let Some(d) = self.find(name) else { return };
        if d.state != DlState::Downloading {
            return; // a late report of something already stopped
        }
        d.status = status.to_string();
        // the totals only grow as Ollama reveals more layers; never go backwards
        d.total = d.total.max(total);
        d.completed = completed.max(d.completed.min(d.total));
        let last = d.samples.back().map(|s| (s.0, s.1));
        if last.is_none_or(|(t, c)| c != d.completed || (now - t).num_milliseconds() >= 1000) {
            d.samples.push_back((now, d.completed));
        }
        while d.samples.len() > 2
            && d.samples
                .front()
                .is_some_and(|s| (now - s.0).num_seconds() > SPEED_WINDOW_SECS)
        {
            d.samples.pop_front();
        }
    }

    pub fn finish(&mut self, name: &str, outcome: Outcome, now: DateTime<Utc>) {
        if self.find(name).is_none() {
            self.begin(name, now);
        }
        let Some(d) = self.find(name) else { return };
        d.ended = Some(now);
        match outcome {
            Outcome::Done(m) => {
                d.state = DlState::Done;
                d.completed = d.total.max(d.completed);
                d.status = "done".into();
                d.message = m;
            }
            Outcome::Failed(m) => {
                d.state = DlState::Failed;
                d.status = "failed".into();
                d.message = m;
            }
            Outcome::Stopped => {
                d.state = DlState::Stopped;
                d.status = "stopped".into();
                d.message =
                    "Stopped. What was fetched is kept: resume (r) continues from there.".into();
            }
        }
    }

    pub fn active(&self) -> impl Iterator<Item = &Download> {
        self.list.iter().filter(|d| d.state == DlState::Downloading)
    }

    pub fn is_active(&self, name: &str) -> bool {
        self.active().any(|d| d.name == name)
    }

    pub fn selected_item(&self) -> Option<&Download> {
        self.list.get(self.selected)
    }

    pub fn move_selection(&mut self, delta: isize) {
        let n = self.list.len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).clamp(0, n as isize - 1) as usize;
    }

    /// Forget the highlighted entry unless it is still downloading.
    pub fn remove_selected(&mut self) -> bool {
        match self.list.get(self.selected) {
            Some(d) if d.state != DlState::Downloading => {
                self.list.remove(self.selected);
                self.selected = self.selected.min(self.list.len().saturating_sub(1));
                true
            }
            _ => false,
        }
    }

    pub fn clear_finished(&mut self) {
        self.list.retain(|d| d.state == DlState::Downloading);
        self.selected = self.selected.min(self.list.len().saturating_sub(1));
    }
}

// ---------------------------------------------------------------- formatting

pub fn fmt_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} kB", b / 1e3)
    } else {
        format!("{b:.0} B")
    }
}

pub fn fmt_speed(bps: f64) -> String {
    if bps >= 1e6 {
        format!("{:.1} MB/s", bps / 1e6)
    } else {
        format!("{:.0} kB/s", bps / 1e3)
    }
}

pub fn fmt_eta(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn progress_speed_and_eta_come_from_the_last_seconds() {
        let mut d = Downloads::default();
        d.begin("m", t(0));
        assert_eq!(d.list[0].percent(), None, "size unknown yet");
        for s in 0..=4 {
            d.progress("m", "pulling", s as u64 * 10_000_000, 100_000_000, t(s));
        }
        let x = &d.list[0];
        assert_eq!(x.percent(), Some(40));
        let speed = x.speed().unwrap();
        assert!((speed - 10e6).abs() < 1e5, "{speed}");
        assert_eq!(x.eta_secs(), Some(6));
        // an old burst falls out of the window
        d.progress("m", "pulling", 90_000_000, 100_000_000, t(60));
        d.progress("m", "pulling", 91_000_000, 100_000_000, t(61));
        let speed = d.list[0].speed().unwrap();
        assert!(speed < 2e6, "recent speed only: {speed}");
    }

    #[test]
    fn lifecycle_done_failed_stopped_and_restart() {
        let mut d = Downloads::default();
        d.progress("a", "pulling", 5, 10, t(0));
        assert!(d.is_active("a"));
        d.finish("a", Outcome::Failed("no space left".into()), t(3));
        assert_eq!(d.list[0].state, DlState::Failed);
        assert_eq!(d.list[0].message, "no space left");
        assert!(!d.is_active("a") && d.list[0].speed().is_none());
        // a late progress report does not revive a finished entry
        d.progress("a", "pulling", 6, 10, t(4));
        assert_eq!(d.list[0].state, DlState::Failed);
        // starting again resets it in place
        d.begin("a", t(10));
        assert_eq!(
            (d.list.len(), d.list[0].state, d.list[0].completed),
            (1, DlState::Downloading, 0)
        );
        d.finish("a", Outcome::Stopped, t(12));
        assert_eq!(d.list[0].state, DlState::Stopped);
        assert!(d.list[0].message.contains("continues"));
        d.finish("a", Outcome::Done("a downloaded".into()), t(20));
        assert_eq!(d.list[0].percent(), None, "no total was ever reported");
    }

    #[test]
    fn the_total_never_shrinks_and_the_list_stays_bounded() {
        let mut d = Downloads::default();
        d.progress("a", "pulling", 10, 100, t(0));
        d.progress("a", "pulling", 5, 50, t(1)); // another layer's report arrives out of order
        assert_eq!((d.list[0].total, d.list[0].completed), (100, 10));
        for i in 0..40 {
            let n = format!("m{i}");
            d.begin(&n, t(i));
            d.finish(&n, Outcome::Done("ok".into()), t(i));
        }
        assert!(d.list.len() <= Downloads::KEEP + 1, "{}", d.list.len());
        assert!(d.is_active("a"), "a running download is never dropped");
    }

    #[test]
    fn selection_removal_and_clearing() {
        let mut d = Downloads::default();
        for n in ["a", "b", "c"] {
            d.begin(n, t(0));
        }
        d.finish("a", Outcome::Done("x".into()), t(1));
        d.finish("b", Outcome::Stopped, t(1));
        d.selected = 2;
        assert!(!d.remove_selected(), "c is still downloading");
        d.selected = 0;
        assert!(d.remove_selected());
        assert_eq!(d.list.len(), 2);
        d.clear_finished();
        assert_eq!(d.list.len(), 1);
        assert_eq!(d.list[0].name, "c");
        d.move_selection(5);
        assert_eq!(d.selected, 0);
    }

    #[test]
    fn numbers_are_written_for_people() {
        assert_eq!(fmt_bytes(5_200_000_000), "5.20 GB");
        assert_eq!(fmt_bytes(120_000_000), "120 MB");
        assert_eq!(fmt_speed(11_400_000.0), "11.4 MB/s");
        assert_eq!(fmt_speed(900_000.0), "900 kB/s");
        assert_eq!(fmt_eta(45), "45s");
        assert_eq!(fmt_eta(190), "3m 10s");
        assert_eq!(fmt_eta(4000), "1h 06m");
        let _ = Duration::seconds(1);
    }
}
