//! Training: batches of text windows, the AdamW optimizer, learning-rate warm-up and decay, weight
//! decay, gradient clipping and a held-out part of the text to notice over-fitting.

use crate::model::{Config, Gpt};
use crate::{LabError, Result, Rng, Tokenizer};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// How the learning rate falls after the warm-up.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Schedule {
    /// Stays at the peak.
    Constant,
    /// Follows half a cosine wave down to the floor.
    Cosine,
    /// Shrinks by the same factor every step (a straight line on a log scale).
    Exponential,
}

impl Schedule {
    pub fn parse(s: &str) -> Option<Schedule> {
        match s.to_ascii_lowercase().as_str() {
            "constant" | "const" => Some(Schedule::Constant),
            "cosine" | "cos" => Some(Schedule::Cosine),
            "exponential" | "exp" => Some(Schedule::Exponential),
            _ => None,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Schedule::Constant => "constant",
            Schedule::Cosine => "cosine",
            Schedule::Exponential => "exponential",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrainConfig {
    pub steps: usize,
    /// Text windows per step.
    pub batch: usize,
    /// Tokens per window (at most the model's context).
    pub seq: usize,
    /// Peak learning rate.
    pub lr: f64,
    /// The floor the decay reaches, as a fraction of the peak.
    pub min_lr_frac: f64,
    /// Steps over which the learning rate rises from nearly zero to the peak.
    pub warmup: usize,
    pub schedule: Schedule,
    /// Weight decay (AdamW style: it shrinks the matrices a little every step, separately from the gradient).
    pub weight_decay: f64,
    /// Gradients longer than this (as one vector) are scaled down to it.
    pub clip: f64,
    pub seed: u64,
    /// Share of the text kept aside to measure over-fitting.
    pub val_fraction: f64,
    pub eval_every: usize,
}

impl Default for TrainConfig {
    fn default() -> Self {
        TrainConfig {
            steps: 1000,
            batch: 8,
            seq: 32,
            lr: 3e-3,
            min_lr_frac: 0.1,
            warmup: 50,
            schedule: Schedule::Cosine,
            weight_decay: 0.01,
            clip: 1.0,
            seed: 1,
            val_fraction: 0.1,
            eval_every: 50,
        }
    }
}

impl TrainConfig {
    /// The learning rate used by update number `step` (0-based).
    pub fn lr_at(&self, step: usize) -> f64 {
        let floor = self.lr * self.min_lr_frac;
        if step < self.warmup {
            return self.lr * (step + 1) as f64 / self.warmup as f64;
        }
        let span = self.steps.saturating_sub(self.warmup).max(1) as f64;
        let p = ((step - self.warmup) as f64 / span).clamp(0.0, 1.0);
        match self.schedule {
            Schedule::Constant => self.lr,
            Schedule::Cosine => {
                floor + 0.5 * (self.lr - floor) * (1.0 + (std::f64::consts::PI * p).cos())
            }
            Schedule::Exponential => self.lr * self.min_lr_frac.max(1e-12).powf(p),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.steps == 0 || self.batch == 0 || self.seq < 2 {
            return Err(LabError::Invalid(
                "steps and batch must be at least 1 and seq at least 2".into(),
            ));
        }
        if !(self.lr > 0.0 && self.lr <= 1.0) {
            return Err(LabError::Invalid(
                "the learning rate must be between 0 and 1 (try 0.003)".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.min_lr_frac) || !(0.0..0.5).contains(&self.val_fraction) {
            return Err(LabError::Invalid(
                "min_lr_frac must be in [0, 1] and val_fraction in [0, 0.5)".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepStats {
    pub step: usize,
    /// Mean loss of this step's batch (how surprised the model was, in nats per token).
    pub loss: f64,
    pub lr: f64,
    /// Length of the gradient vector before clipping.
    pub grad_norm: f64,
    /// Loss on text the model never trained on, measured every `eval_every` steps.
    pub val_loss: Option<f64>,
}

pub struct Trainer {
    pub model: Gpt,
    pub cfg: TrainConfig,
    pub history: Vec<StepStats>,
    /// Number of updates done.
    pub step: usize,
    train: Vec<u32>,
    val: Vec<u32>,
    grad: Vec<f64>,
    m: Vec<f64>,
    v: Vec<f64>,
    decay: Vec<bool>,
    rng: Rng,
}

impl Trainer {
    pub fn new(model: Gpt, tokens: Vec<u32>, mut cfg: TrainConfig) -> Result<Trainer> {
        cfg.validate()?;
        cfg.seq = cfg.seq.min(model.config.ctx);
        let need = cfg.seq + 2;
        if tokens.len() < need * 2 {
            return Err(LabError::TooShort(format!(
                "{} tokens; at least {} are needed for windows of {} tokens (give more text, or a smaller seq)",
                tokens.len(),
                need * 2,
                cfg.seq
            )));
        }
        // the last part of the text is kept aside; too little of it and everything is used for training
        let mut cut = tokens.len() - (tokens.len() as f64 * cfg.val_fraction) as usize;
        if tokens.len() - cut < need || cut < need {
            cut = tokens.len();
        }
        let (train, val) = (tokens[..cut].to_vec(), tokens[cut..].to_vec());
        let n = model.params.len();
        let decay = model.layout.decay_mask(&model.config);
        let rng = Rng::new(cfg.seed ^ 0xA5A5);
        Ok(Trainer {
            model,
            cfg,
            history: vec![],
            step: 0,
            train,
            val,
            grad: vec![0.0; n],
            m: vec![0.0; n],
            v: vec![0.0; n],
            decay,
            rng,
        })
    }

    pub fn done(&self) -> bool {
        self.step >= self.cfg.steps
    }

    pub fn train_tokens(&self) -> usize {
        self.train.len()
    }

    pub fn val_tokens(&self) -> usize {
        self.val.len()
    }

    /// Average loss on up to 24 evenly spaced windows of `data`.
    pub fn eval(&self, data: &[u32]) -> Option<f64> {
        let seq = self.cfg.seq;
        if data.len() < seq + 2 {
            return None;
        }
        let last = data.len() - seq - 1;
        let n = 24.min(last + 1);
        let mut total = 0.0;
        for i in 0..n {
            let s = if n == 1 { 0 } else { i * last / (n - 1) };
            total += self
                .model
                .loss_and_grad(&data[s..s + seq], &data[s + 1..s + seq + 1], None);
        }
        Some(total / (n * seq) as f64)
    }

    pub fn train_loss_estimate(&self) -> Option<f64> {
        self.eval(&self.train)
    }

    /// One update: sample a batch, compute loss and gradient, clip, apply AdamW.
    pub fn train_step(&mut self) -> StepStats {
        let (b, seq) = (self.cfg.batch, self.cfg.seq);
        self.grad.iter_mut().for_each(|g| *g = 0.0);
        let scale = 1.0 / (b * seq) as f64;
        let mut loss_sum = 0.0;
        for _ in 0..b {
            let s = self.rng.below(self.train.len() - seq - 1);
            let (inp, tgt) = (&self.train[s..s + seq], &self.train[s + 1..s + seq + 1]);
            loss_sum += self
                .model
                .loss_and_grad(inp, tgt, Some((&mut self.grad, scale)));
        }
        let norm = self.grad.iter().map(|g| g * g).sum::<f64>().sqrt();
        if norm > self.cfg.clip && norm.is_finite() {
            let k = self.cfg.clip / norm;
            self.grad.iter_mut().for_each(|g| *g *= k);
        }
        let lr = self.cfg.lr_at(self.step);
        let (b1, b2, eps) = (0.9f64, 0.999f64, 1e-8f64);
        let t = (self.step + 1) as f64;
        let (c1, c2) = (1.0 - b1.powf(t), 1.0 - b2.powf(t));
        for i in 0..self.model.params.len() {
            let g = self.grad[i];
            self.m[i] = b1 * self.m[i] + (1.0 - b1) * g;
            self.v[i] = b2 * self.v[i] + (1.0 - b2) * g * g;
            let upd = (self.m[i] / c1) / ((self.v[i] / c2).sqrt() + eps);
            let p = &mut self.model.params[i];
            if self.decay[i] {
                *p -= lr * self.cfg.weight_decay * *p; // decoupled from the gradient
            }
            *p -= lr * upd;
        }
        self.step += 1;
        let val_loss = if self.cfg.eval_every > 0
            && (self.step.checked_rem(self.cfg.eval_every) == Some(0)
                || self.step == self.cfg.steps)
        {
            self.eval(&self.val)
        } else {
            None
        };
        let st = StepStats {
            step: self.step,
            loss: loss_sum * scale,
            lr,
            grad_norm: norm,
            val_loss,
        };
        self.history.push(st.clone());
        st
    }

    /// Train to the end, calling `on_step` after every update.
    pub fn run<F: FnMut(&StepStats)>(&mut self, mut on_step: F) {
        while !self.done() {
            let st = self.train_step();
            on_step(&st);
        }
    }
}

// ------------------------------------------------------------------ checkpoints

#[derive(Serialize, Deserialize)]
pub struct Checkpoint {
    pub config: Config,
    pub tokenizer: Tokenizer,
    pub params: Vec<f64>,
    pub train: Option<TrainConfig>,
    pub history: Vec<StepStats>,
}

pub fn save_checkpoint(
    path: &Path,
    model: &Gpt,
    tok: &Tokenizer,
    train: Option<&TrainConfig>,
    history: &[StepStats],
) -> Result<()> {
    let ck = Checkpoint {
        config: model.config,
        tokenizer: tok.clone(),
        params: model.params.clone(),
        train: train.cloned(),
        history: history.to_vec(),
    };
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| LabError::Io {
            path: dir.display().to_string(),
            why: e.to_string(),
        })?;
    }
    let text = serde_json::to_string(&ck).map_err(|e| LabError::Invalid(e.to_string()))?;
    std::fs::write(path, text).map_err(|e| LabError::Io {
        path: path.display().to_string(),
        why: e.to_string(),
    })
}

pub fn load_checkpoint(
    path: &Path,
) -> Result<(Gpt, Tokenizer, Vec<StepStats>, Option<TrainConfig>)> {
    let text = std::fs::read_to_string(path).map_err(|e| LabError::Io {
        path: path.display().to_string(),
        why: e.to_string(),
    })?;
    let mut ck: Checkpoint = serde_json::from_str(&text).map_err(|e| {
        LabError::Invalid(format!(
            "{} is not a model saved by `maw lab train` ({e})",
            path.display()
        ))
    })?;
    ck.tokenizer.rebuild();
    let mut model = Gpt::new(ck.config, 0)?;
    if model.params.len() != ck.params.len() {
        return Err(LabError::Invalid(
            "the saved model does not match its own configuration".into(),
        ));
    }
    model.params = ck.params;
    Ok((model, ck.tokenizer, ck.history, ck.train))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_small() -> Config {
        Config {
            vocab: 0,
            dim: 16,
            heads: 2,
            layers: 1,
            ctx: 8,
            ff_mult: 2,
        }
    }

    fn setup(text: &str, steps: usize) -> (Trainer, Tokenizer) {
        let tok = Tokenizer::char_level(text);
        let mut c = cfg_small();
        c.vocab = tok.vocab_size().max(2);
        let model = Gpt::new(c, 1).unwrap();
        let cfg = TrainConfig {
            steps,
            batch: 4,
            seq: 8,
            lr: 0.01,
            warmup: 10,
            eval_every: 20,
            ..Default::default()
        };
        (Trainer::new(model, tok.encode(text), cfg).unwrap(), tok)
    }

    #[test]
    fn the_learning_rate_rises_then_decays_to_the_floor_on_each_schedule() {
        let mut c = TrainConfig {
            steps: 100,
            warmup: 10,
            lr: 0.01,
            min_lr_frac: 0.1,
            ..Default::default()
        };
        for s in [Schedule::Cosine, Schedule::Exponential, Schedule::Constant] {
            c.schedule = s;
            assert!(
                c.lr_at(0) < c.lr_at(5) && c.lr_at(5) < c.lr_at(9),
                "warm-up rises"
            );
            assert!(
                (c.lr_at(10) - 0.01).abs() < 1e-9,
                "{s:?} starts the decay at the peak"
            );
            let end = c.lr_at(99);
            match s {
                Schedule::Constant => assert!((end - 0.01).abs() < 1e-9),
                _ => {
                    assert!(
                        (0.001 - 1e-9..0.0013).contains(&end),
                        "{s:?} ends near the floor: {end}"
                    );
                    assert!(c.lr_at(40) > c.lr_at(70), "and falls in between");
                }
            }
        }
        c.schedule = Schedule::Exponential;
        // the same ratio every step: a straight line on a log scale
        let r1 = c.lr_at(30) / c.lr_at(20);
        let r2 = c.lr_at(80) / c.lr_at(70);
        assert!((r1 - r2).abs() < 1e-9);
        assert_eq!(Schedule::parse("COS"), Some(Schedule::Cosine));
        assert_eq!(Schedule::parse("nope"), None);
    }

    #[test]
    fn training_makes_the_model_learn_a_repeating_pattern() {
        let text = "abcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabcabc".repeat(2);
        let (mut t, tok) = setup(&text, 150);
        let start = t.train_loss_estimate().unwrap();
        t.run(|_| {});
        let end = t.train_loss_estimate().unwrap();
        assert!(start > 0.9 && end < 0.15, "loss {start:.3} -> {end:.3}");
        // and it can now continue the pattern
        let p = t.model.next_token_probs(&tok.encode("abcab"));
        let best = p
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0 as u32;
        assert_eq!(tok.token_str(best), "c");
        assert!(
            t.history.iter().any(|h| h.val_loss.is_some()),
            "held-out loss is measured"
        );
        assert_eq!(t.history.len(), 150);
    }

    #[test]
    fn the_same_seed_gives_exactly_the_same_run_and_another_seed_does_not() {
        let text = "the quick brown fox jumps over the lazy dog. ".repeat(10);
        let run = |seed: u64| {
            let (mut t, _) = setup(&text, 15);
            t.cfg.seed = seed;
            t.rng = Rng::new(seed ^ 0xA5A5);
            t.run(|_| {});
            t.history.iter().map(|h| h.loss).collect::<Vec<f64>>()
        };
        assert_eq!(run(5), run(5));
        assert_ne!(run(5), run(6));
    }

    #[test]
    fn weight_decay_shrinks_matrices_but_not_gains_and_clipping_limits_the_step() {
        // two identical runs (same text, same seed, so the same first batch) that differ only in decay
        let text = "ab".repeat(60);
        let one_step = |wd: f64| {
            let (mut t, _) = setup(&text, 5);
            t.cfg.weight_decay = wd;
            t.cfg.lr = 0.05;
            t.cfg.warmup = 1;
            let st = t.train_step();
            assert!(st.grad_norm.is_finite());
            t
        };
        let (plain, decayed) = (one_step(0.0), one_step(0.9));
        let lay = &plain.model.layout;
        let wq = lay.layers[0].wq;
        let norm = |t: &Trainer| {
            t.model.params[wq..wq + 16 * 16]
                .iter()
                .map(|x| x * x)
                .sum::<f64>()
        };
        assert!(
            norm(&decayed) < norm(&plain),
            "decay pulls the matrices toward zero: {} vs {}",
            norm(&decayed),
            norm(&plain)
        );
        // gains and biases are exempt: they ended up exactly where they do without decay
        assert_eq!(
            decayed.model.params[lay.gf..lay.gf + 16],
            plain.model.params[lay.gf..lay.gf + 16]
        );
        assert_eq!(
            decayed.model.params[lay.layers[0].b1..lay.layers[0].b1 + 32],
            plain.model.params[lay.layers[0].b1..lay.layers[0].b1 + 32]
        );
        // clipping: a tiny limit makes the whole update tiny
        let mut clipped = one_step(0.0);
        clipped.cfg.clip = 1e-9;
        let before = clipped.model.params.clone();
        clipped.train_step();
        let moved: f64 = before
            .iter()
            .zip(&clipped.model.params)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f64::max);
        assert!(
            moved < 0.2,
            "even clipped, Adam's step is bounded by the learning rate: {moved}"
        );
    }

    #[test]
    fn too_little_text_or_nonsense_settings_are_explained() {
        let tok = Tokenizer::char_level("abc");
        let model = Gpt::new(
            Config {
                vocab: 3,
                dim: 8,
                heads: 2,
                layers: 1,
                ctx: 8,
                ff_mult: 2,
            },
            1,
        )
        .unwrap();
        let e = Trainer::new(model.clone(), tok.encode("abc"), TrainConfig::default())
            .err()
            .unwrap();
        assert!(e.to_string().contains("too short"), "{e}");
        let bad = TrainConfig {
            lr: 5.0,
            ..Default::default()
        };
        assert!(Trainer::new(model, tok.encode(&"abc".repeat(50)), bad)
            .err()
            .unwrap()
            .to_string()
            .contains("learning rate"));
    }

    #[test]
    fn a_saved_model_comes_back_identical() {
        let text = "hello world. ".repeat(20);
        let (mut t, tok) = setup(&text, 10);
        t.run(|_| {});
        let dir = std::env::temp_dir().join(format!("maw-lab-test-{}", std::process::id()));
        let path = dir.join("m.json");
        save_checkpoint(&path, &t.model, &tok, Some(&t.cfg), &t.history).unwrap();
        let (m2, tok2, hist, cfg) = load_checkpoint(&path).unwrap();
        assert_eq!(m2.params, t.model.params);
        assert_eq!(tok2.decode(&tok2.encode("hello")), "hello");
        assert_eq!(hist.len(), 10);
        assert_eq!(cfg.unwrap().steps, 10);
        let ids = tok.encode("hello");
        assert_eq!(t.model.forward(&ids).logits, m2.forward(&ids).logits);
        assert!(load_checkpoint(&dir.join("missing.json")).is_err());
        std::fs::write(dir.join("bad.json"), "{}").unwrap();
        assert!(load_checkpoint(&dir.join("bad.json"))
            .unwrap_err()
            .to_string()
            .contains("not a model"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
