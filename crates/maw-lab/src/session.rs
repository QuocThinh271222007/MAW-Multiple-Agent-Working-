//! A training run on its own thread that a screen can watch while it happens: after every few steps
//! it publishes a snapshot (loss curves, the embeddings projected to 3D, the attention weights for a
//! sentence, a sample of generated text ...) that the screen reads without ever blocking training.

use crate::probe::{attention_maps, entropy_bits, param_groups, pca3};
use crate::sample::{generate, SampleOpts};
use crate::train::{save_checkpoint, StepStats, TrainConfig, Trainer};
use crate::{Config, Tokenizer};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    Paused,
    Done,
}

/// Everything a screen shows, copied out of the training thread.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub status: Status,
    pub step: usize,
    pub steps: usize,
    pub history: Vec<StepStats>,
    pub config: Config,
    pub train_cfg: TrainConfig,
    pub n_params: usize,
    pub groups: Vec<(String, usize)>,
    pub tokenizer_name: String,
    pub train_tokens: usize,
    pub val_tokens: usize,
    pub steps_per_sec: f64,
    /// One point per token, from the embedding matrix by principal component analysis.
    pub emb3: Vec<[f64; 3]>,
    pub labels: Vec<String>,
    /// The sentence the attention map and the next-token chart are about.
    pub probe_text: String,
    pub probe_pieces: Vec<String>,
    /// `[layer][head]` -> `T x T`.
    pub attention: Vec<Vec<Vec<f64>>>,
    /// The model's five likeliest next tokens after the probe sentence, with probabilities.
    pub next: Vec<(String, f64)>,
    pub next_entropy_bits: f64,
    pub sample: String,
    pub notice: String,
}

struct Shared {
    snap: Mutex<Snapshot>,
    paused: AtomicBool,
    stop: AtomicBool,
    probe: Mutex<Option<String>>,
    save_to: Mutex<Option<PathBuf>>,
    sample_now: AtomicBool,
}

pub struct LabSession {
    shared: Arc<Shared>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

fn lock_snap(s: &Shared) -> MutexGuard<'_, Snapshot> {
    s.snap.lock().unwrap_or_else(|e| e.into_inner())
}

fn labels_of(tok: &Tokenizer) -> Vec<String> {
    (0..tok.vocab_size() as u32)
        .map(|i| {
            tok.token_str(i)
                .chars()
                .map(|c| match c {
                    ' ' => '·',
                    '\n' => '↵',
                    '\t' => '→',
                    c => c,
                })
                .collect()
        })
        .collect()
}

impl LabSession {
    /// Start training in the background. `tokenizer_name` is only for display.
    pub fn start(
        mut trainer: Trainer,
        tok: Tokenizer,
        tokenizer_name: String,
        probe_text: String,
        paused: bool,
    ) -> LabSession {
        let labels = labels_of(&tok);
        let snap = Snapshot {
            status: if paused {
                Status::Paused
            } else {
                Status::Running
            },
            step: 0,
            steps: trainer.cfg.steps,
            history: vec![],
            config: trainer.model.config,
            train_cfg: trainer.cfg.clone(),
            n_params: trainer.model.n_params(),
            groups: param_groups(&trainer.model),
            tokenizer_name,
            train_tokens: trainer.train_tokens(),
            val_tokens: trainer.val_tokens(),
            steps_per_sec: 0.0,
            emb3: vec![],
            labels,
            probe_text: probe_text.clone(),
            probe_pieces: vec![],
            attention: vec![],
            next: vec![],
            next_entropy_bits: 0.0,
            sample: String::new(),
            notice: String::new(),
        };
        let shared = Arc::new(Shared {
            snap: Mutex::new(snap),
            paused: AtomicBool::new(paused),
            stop: AtomicBool::new(false),
            probe: Mutex::new(None),
            save_to: Mutex::new(None),
            sample_now: AtomicBool::new(true),
        });
        let sh = shared.clone();
        let handle = std::thread::spawn(move || {
            let started = Instant::now();
            let mut probe = probe_text;
            let mut last_publish = Instant::now() - Duration::from_secs(1);
            let mut last_sample_step = usize::MAX;
            let mut first = true;
            loop {
                if sh.stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Some(p) = sh.probe.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    probe = p;
                    first = true;
                }
                let paused = sh.paused.load(Ordering::SeqCst);
                if !paused && !trainer.done() {
                    trainer.train_step();
                }
                let finished = trainer.done();
                let want_publish = first
                    || last_publish.elapsed() > Duration::from_millis(120)
                    || finished
                    || paused;
                if want_publish
                    && (first
                        || paused && last_publish.elapsed() > Duration::from_millis(250)
                        || !paused)
                {
                    last_publish = Instant::now();
                    first = false;
                    let emb = trainer.model.embeddings();
                    let ids = tok.encode(&probe);
                    let ids: Vec<u32> = ids.into_iter().take(trainer.model.config.ctx).collect();
                    let attention = attention_maps(&trainer.model, &ids);
                    let probs = if ids.is_empty() {
                        vec![]
                    } else {
                        trainer.model.next_token_probs(&ids)
                    };
                    let mut ranked: Vec<(usize, f64)> = probs.iter().cloned().enumerate().collect();
                    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                    let labels = labels_of(&tok);
                    let next: Vec<(String, f64)> = ranked
                        .iter()
                        .take(5)
                        .map(|(i, p)| (labels[*i].clone(), *p))
                        .collect();
                    let sample_due = sh.sample_now.swap(false, Ordering::SeqCst)
                        || (trainer.step.checked_rem(25) == Some(0)
                            && trainer.step != last_sample_step)
                        || finished && last_sample_step != trainer.step;
                    let sample = if sample_due {
                        last_sample_step = trainer.step;
                        let prompt: Vec<u32> = ids.iter().take(4).cloned().collect();
                        let g = generate(
                            &trainer.model,
                            &prompt,
                            90,
                            &SampleOpts {
                                temperature: 0.8,
                                top_k: 0,
                                seed: 7,
                            },
                        );
                        Some(tok.decode(&g.ids))
                    } else {
                        None
                    };
                    let mut notice = None;
                    if let Some(path) = sh.save_to.lock().unwrap_or_else(|e| e.into_inner()).take()
                    {
                        notice = Some(
                            match save_checkpoint(
                                &path,
                                &trainer.model,
                                &tok,
                                Some(&trainer.cfg),
                                &trainer.history,
                            ) {
                                Ok(()) => format!("saved to {}", path.display()),
                                Err(e) => format!("could not save: {e}"),
                            },
                        );
                    }
                    let mut s = lock_snap(&sh);
                    s.status = if finished {
                        Status::Done
                    } else if paused {
                        Status::Paused
                    } else {
                        Status::Running
                    };
                    s.step = trainer.step;
                    s.history = trainer.history.clone();
                    s.emb3 = pca3(&emb);
                    s.probe_text = probe.clone();
                    s.probe_pieces = ids.iter().map(|&i| labels[i as usize].clone()).collect();
                    s.attention = attention;
                    s.next_entropy_bits = entropy_bits(&probs);
                    s.next = next;
                    s.steps_per_sec =
                        trainer.step as f64 / started.elapsed().as_secs_f64().max(1e-9);
                    if let Some(x) = sample {
                        s.sample = x;
                    }
                    if let Some(n) = notice {
                        s.notice = n;
                    }
                }
                if paused || finished {
                    std::thread::sleep(Duration::from_millis(40));
                }
            }
        });
        LabSession {
            shared,
            thread: Mutex::new(Some(handle)),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared
            .snap
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn toggle_pause(&self) {
        let p = self.shared.paused.load(Ordering::SeqCst);
        self.shared.paused.store(!p, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::SeqCst)
    }

    /// Change the sentence the attention map and the next-token chart are about.
    pub fn set_probe(&self, text: String) {
        *self.shared.probe.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
    }

    pub fn request_save(&self, path: PathBuf) {
        *self
            .shared
            .save_to
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(path);
    }

    pub fn request_sample(&self) {
        self.shared.sample_now.store(true, Ordering::SeqCst);
    }

    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
    }
}

impl Drop for LabSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Gpt;

    fn session(steps: usize, paused: bool) -> LabSession {
        let text = "abcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabc".repeat(2);
        let tok = Tokenizer::char_level(&text);
        let cfg = Config {
            vocab: tok.vocab_size(),
            dim: 16,
            heads: 2,
            layers: 1,
            ctx: 8,
            ff_mult: 2,
        };
        let model = Gpt::new(cfg, 1).unwrap();
        let tc = TrainConfig {
            steps,
            batch: 4,
            seq: 8,
            lr: 0.01,
            warmup: 5,
            eval_every: 10,
            ..Default::default()
        };
        let trainer = Trainer::new(model, tok.encode(&text), tc).unwrap();
        LabSession::start(trainer, tok, "characters".into(), "abcab".into(), paused)
    }

    fn wait_for(s: &LabSession, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
        for _ in 0..400 {
            let snap = s.snapshot();
            if f(&snap) {
                return snap;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out; last snapshot step {}", s.snapshot().step);
    }

    #[test]
    fn a_run_in_the_background_publishes_what_a_screen_needs() {
        let s = session(60, false);
        let done = wait_for(&s, |x| x.status == Status::Done);
        assert_eq!(done.step, 60);
        assert_eq!(done.history.len(), 60);
        assert_eq!(done.emb3.len(), done.config.vocab);
        assert_eq!(done.labels.len(), done.config.vocab);
        assert_eq!(done.probe_pieces, vec!["a", "b", "c", "a", "b"]);
        assert_eq!((done.attention.len(), done.attention[0].len()), (1, 2));
        assert_eq!(done.attention[0][0].len(), 25, "5 x 5 weights per head");
        assert_eq!(
            done.next.len(),
            3,
            "a vocabulary of a, b, c has three candidates"
        );
        assert!(
            done.next[0].1 > 0.5 && done.next[0].0 == "c",
            "after 'abcab' comes 'c': {:?}",
            done.next
        );
        assert!(done.history.last().unwrap().loss < done.history[0].loss);
        assert!(!done.sample.is_empty());
        assert!(done.groups.iter().map(|g| g.1).sum::<usize>() == done.n_params);
        s.stop();
    }

    #[test]
    fn pausing_stops_the_steps_and_resuming_continues() {
        let s = session(100_000, true);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(s.snapshot().step, 0, "started paused");
        assert_eq!(s.snapshot().status, Status::Paused);
        s.toggle_pause();
        let moved = wait_for(&s, |x| x.step > 5);
        assert_eq!(moved.status, Status::Running);
        s.toggle_pause();
        std::thread::sleep(Duration::from_millis(150));
        let a = s.snapshot().step;
        std::thread::sleep(Duration::from_millis(150));
        let b = s.snapshot().step;
        assert!(b - a <= 2, "paused again: {a} -> {b}");
        s.stop();
    }

    #[test]
    fn the_sentence_can_be_changed_and_a_model_saved_while_it_runs() {
        let s = session(100_000, false);
        wait_for(&s, |x| x.step > 3);
        s.set_probe("cab".into());
        let snap = wait_for(&s, |x| x.probe_text == "cab");
        assert_eq!(snap.probe_pieces, vec!["c", "a", "b"]);
        let path = std::env::temp_dir().join(format!("maw-lab-live-{}.json", std::process::id()));
        s.request_save(path.clone());
        wait_for(&s, |x| x.notice.starts_with("saved"));
        assert!(path.is_file());
        let _ = std::fs::remove_file(path);
        s.stop();
        s.stop(); // stopping twice is harmless
    }
}
