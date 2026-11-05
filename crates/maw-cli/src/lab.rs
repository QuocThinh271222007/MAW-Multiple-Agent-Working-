//! `maw lab`: a tiny language model to train on your own text and take apart (see the `maw-lab` crate).

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use maw_lab::probe::{param_groups, rope_frequencies};
use maw_lab::sample::{generate, SampleOpts};
use maw_lab::train::{load_checkpoint, save_checkpoint, Schedule, TrainConfig, Trainer};
use maw_lab::{Config, Gpt, Tokenizer};
use std::path::{Path, PathBuf};

#[derive(Subcommand, Debug)]
pub enum LabCmd {
    /// Train a tiny Transformer on a text file and watch it learn in 3D (no file: a built-in text)
    Train(TrainArgs),
    /// Write text with a model saved by `maw lab train`
    Sample(SampleArgs),
    /// Show how a model's tokenizer cuts a text into tokens
    Tokens {
        /// A model saved by `maw lab train`
        model: PathBuf,
        /// The text to cut
        text: String,
    },
    /// Describe a saved model: sizes, where the parameters are, how it was trained
    Info { model: PathBuf },
}

#[derive(Args, Debug, Clone)]
pub struct TrainArgs {
    /// Your text file (UTF-8). Without it a short built-in text is used, so you can try the Lab at once
    pub file: Option<PathBuf>,
    /// Built-in text when no file is given: en or vi
    #[arg(long, default_value = "en")]
    pub demo: String,
    /// Training steps
    #[arg(long, default_value_t = 1500)]
    pub steps: usize,
    /// Width of the model (embedding size); must be divisible by --heads
    #[arg(long, default_value_t = 32)]
    pub dim: usize,
    #[arg(long, default_value_t = 2)]
    pub layers: usize,
    #[arg(long, default_value_t = 4)]
    pub heads: usize,
    /// Context window in tokens
    #[arg(long, default_value_t = 32)]
    pub ctx: usize,
    /// Text windows per step
    #[arg(long, default_value_t = 8)]
    pub batch: usize,
    /// Peak learning rate
    #[arg(long, default_value_t = 0.003)]
    pub lr: f64,
    /// constant, cosine or exponential
    #[arg(long, default_value = "cosine")]
    pub schedule: String,
    /// Where the learning rate ends, as a fraction of the peak
    #[arg(long, default_value_t = 0.1)]
    pub min_lr: f64,
    #[arg(long, default_value_t = 50)]
    pub warmup: usize,
    /// Weight decay (AdamW)
    #[arg(long, default_value_t = 0.01)]
    pub wd: f64,
    /// Clip the gradient to this length
    #[arg(long, default_value_t = 1.0)]
    pub clip: f64,
    /// char, or bpe:N for N byte-pair merges (e.g. bpe:100)
    #[arg(long, default_value = "char")]
    pub tokenizer: String,
    /// Same seed, same run
    #[arg(long, default_value_t = 1)]
    pub seed: u64,
    /// Where to save the model
    #[arg(long, default_value = "lab-model.json")]
    pub out: PathBuf,
    /// No full-screen view: print progress lines and save at the end
    #[arg(long)]
    pub plain: bool,
    /// Start paused (press Space to begin)
    #[arg(long)]
    pub paused: bool,
}

impl Default for TrainArgs {
    fn default() -> Self {
        TrainArgs {
            file: None,
            demo: "en".into(),
            steps: 1500,
            dim: 32,
            layers: 2,
            heads: 4,
            ctx: 32,
            batch: 8,
            lr: 0.003,
            schedule: "cosine".into(),
            min_lr: 0.1,
            warmup: 50,
            wd: 0.01,
            clip: 1.0,
            tokenizer: "char".into(),
            seed: 1,
            out: PathBuf::from("lab-model.json"),
            plain: false,
            paused: false,
        }
    }
}

#[derive(Args, Debug)]
pub struct SampleArgs {
    /// A model saved by `maw lab train`
    pub model: PathBuf,
    #[arg(long, default_value = "")]
    pub prompt: String,
    /// How many tokens to write
    #[arg(long, default_value_t = 200)]
    pub tokens: usize,
    /// 0 = always the likeliest token; higher = more adventurous
    #[arg(long, default_value_t = 0.8)]
    pub temperature: f64,
    /// Only the k likeliest tokens may be chosen (0 = all)
    #[arg(long, default_value_t = 0)]
    pub top_k: usize,
    #[arg(long, default_value_t = 1)]
    pub seed: u64,
}

fn read_text(a: &TrainArgs) -> Result<(String, String)> {
    match &a.file {
        Some(p) => {
            let t = std::fs::read_to_string(p).with_context(|| {
                format!("cannot read {} (it must be a UTF-8 text file)", p.display())
            })?;
            Ok((t, p.display().to_string()))
        }
        None => match a.demo.as_str() {
            "en" => Ok((
                maw_lab::DEMO_EN.to_string(),
                "the built-in English text".into(),
            )),
            "vi" => Ok((
                maw_lab::DEMO_VI.to_string(),
                "the built-in Vietnamese text".into(),
            )),
            other => bail!("--demo must be en or vi, not '{other}'"),
        },
    }
}

fn make_tokenizer(spec: &str, text: &str) -> Result<(Tokenizer, String)> {
    if spec == "char" {
        return Ok((Tokenizer::char_level(text), "characters".into()));
    }
    if let Some(n) = spec.strip_prefix("bpe:") {
        let n: usize = n.parse().with_context(|| {
            format!("--tokenizer bpe:N needs a number, e.g. bpe:100 (got '{n}')")
        })?;
        if n == 0 || n > 2000 {
            bail!("--tokenizer bpe:N needs N between 1 and 2000");
        }
        let t = Tokenizer::bpe(text, n);
        let name = format!("BPE, {} merges", t.merges.len());
        return Ok((t, name));
    }
    bail!("--tokenizer is char or bpe:N (e.g. bpe:100), not '{spec}'")
}

fn setup(a: &TrainArgs) -> Result<(Trainer, Tokenizer, String, String, String)> {
    let (text, source) = read_text(a)?;
    let (tok, tok_name) = make_tokenizer(&a.tokenizer, &text)?;
    let schedule = Schedule::parse(&a.schedule).with_context(|| {
        format!(
            "--schedule is constant, cosine or exponential, not '{}'",
            a.schedule
        )
    })?;
    let config = Config {
        vocab: tok.vocab_size().max(2),
        dim: a.dim,
        heads: a.heads,
        layers: a.layers,
        ctx: a.ctx,
        ff_mult: 4,
    };
    let model = Gpt::new(config, a.seed).map_err(|e| anyhow::anyhow!("{e}"))?;
    let cfg = TrainConfig {
        steps: a.steps,
        batch: a.batch,
        seq: a.ctx,
        lr: a.lr,
        min_lr_frac: a.min_lr,
        warmup: a.warmup,
        schedule,
        weight_decay: a.wd,
        clip: a.clip,
        seed: a.seed,
        ..Default::default()
    };
    let ids = tok.encode(&text);
    let trainer = Trainer::new(model, ids, cfg).map_err(|e| anyhow::anyhow!("{e}"))?;
    let probe: String = text.chars().filter(|c| *c != '\n').take(24).collect();
    Ok((trainer, tok, tok_name, source, probe))
}

pub fn run(cmd: Option<LabCmd>) -> Result<()> {
    match cmd.unwrap_or(LabCmd::Train(TrainArgs::default())) {
        LabCmd::Train(a) => train(a),
        LabCmd::Sample(a) => sample(a),
        LabCmd::Tokens { model, text } => tokens(&model, &text),
        LabCmd::Info { model } => info(&model),
    }
}

fn train(a: TrainArgs) -> Result<()> {
    let (mut trainer, tok, tok_name, source, probe) = setup(&a)?;
    if a.plain {
        println!(
            "Training on {source}: {} tokens ({} kept aside), vocabulary {} ({tok_name}), {} parameters, {} steps",
            trainer.train_tokens() + trainer.val_tokens(),
            trainer.val_tokens(),
            tok.vocab_size(),
            trainer.model.n_params(),
            a.steps
        );
        let every = (a.steps / 20).max(1);
        trainer.run(|s| {
            if s.step % every == 0 || s.step == 1 || s.step == a.steps {
                let val = s
                    .val_loss
                    .map(|v| format!("  held-out {v:.3}"))
                    .unwrap_or_default();
                println!(
                    "step {:>5}  loss {:.3}  lr {:.5}  grad {:.2}{val}",
                    s.step, s.loss, s.lr, s.grad_norm
                );
            }
        });
        save_checkpoint(
            &a.out,
            &trainer.model,
            &tok,
            Some(&trainer.cfg),
            &trainer.history,
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        let ids = tok.encode(&probe);
        let prompt: Vec<u32> = ids.iter().take(4).cloned().collect();
        let g = generate(&trainer.model, &prompt, 160, &SampleOpts::default());
        println!(
            "\nThe model writes:\n{}\n\nSaved to {} (try: maw lab sample {})",
            tok.decode(&g.ids),
            a.out.display(),
            a.out.display()
        );
        return Ok(());
    }
    let session = maw_lab::LabSession::start(trainer, tok, tok_name, probe, a.paused);
    maw_tui::lab_ui::run_lab(session, a.out.clone())?;
    println!(
        "Lab closed. Press w inside the Lab to save the model (to {}).",
        a.out.display()
    );
    Ok(())
}

fn load(path: &Path) -> Result<(Gpt, Tokenizer, Vec<maw_lab::StepStats>, Option<TrainConfig>)> {
    load_checkpoint(path).map_err(|e| anyhow::anyhow!("{e}"))
}

fn sample(a: SampleArgs) -> Result<()> {
    let (model, tok, _, _) = load(&a.model)?;
    let unknown = tok.unknown_chars(&a.prompt);
    if !unknown.is_empty() {
        eprintln!(
            "(characters this model has never seen are ignored in the prompt: {})",
            unknown.iter().collect::<String>()
        );
    }
    let mut prompt = tok.encode(&a.prompt);
    if prompt.is_empty() {
        prompt = vec![0];
    }
    let g = generate(
        &model,
        &prompt,
        a.tokens,
        &SampleOpts {
            temperature: a.temperature,
            top_k: a.top_k,
            seed: a.seed,
        },
    );
    println!("{}", tok.decode(&g.ids));
    Ok(())
}

fn tokens(model: &Path, text: &str) -> Result<()> {
    let (_, tok, _, _) = load(model)?;
    let unknown = tok.unknown_chars(text);
    let pieces = tok.pieces(text);
    println!(
        "{} characters -> {} tokens (vocabulary {})",
        text.chars().count(),
        pieces.len(),
        tok.vocab_size()
    );
    for (p, id) in &pieces {
        println!("  {id:>5}  {:?}", p);
    }
    if !unknown.is_empty() {
        println!(
            "never seen in the training text, so skipped: {}",
            unknown.iter().collect::<String>()
        );
    }
    Ok(())
}

fn info(model: &Path) -> Result<()> {
    let (m, tok, history, cfg) = load(model)?;
    let c = &m.config;
    println!("{}", model.display());
    println!(
        "  {} layers x {} heads, width {}, context {} tokens, vocabulary {}",
        c.layers, c.heads, c.dim, c.ctx, c.vocab
    );
    println!(
        "  {} parameters ({:.1} KB as saved numbers); tokenizer: {} base characters + {} merges",
        m.n_params(),
        m.n_params() as f64 * 8.0 / 1024.0,
        tok.chars.len(),
        tok.merges.len()
    );
    println!("  where the parameters are:");
    for (name, n) in param_groups(&m) {
        println!(
            "    {name:<30} {n:>8}  {:>5.1}%",
            100.0 * n as f64 / m.n_params() as f64
        );
    }
    println!(
        "  key/value cache for a full window, 16-bit: {} bytes",
        m.kv_cache_bytes(c.ctx, 2)
    );
    println!(
        "  rotation speeds a RoPE model of this head size would use: {}",
        rope_frequencies(c.head_dim().max(2), 10000.0)
            .iter()
            .take(6)
            .map(|x| format!("{x:.4}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(t) = cfg {
        println!("  trained {} steps, batch {}, peak learning rate {} ({} schedule), weight decay {}, seed {}", t.steps, t.batch, t.lr, t.schedule.name(), t.weight_decay, t.seed);
    }
    if let (Some(first), Some(last)) = (history.first(), history.last()) {
        let val = history
            .iter()
            .rev()
            .find_map(|h| h.val_loss)
            .map(|v| format!(", held-out {v:.3}"))
            .unwrap_or_default();
        println!(
            "  loss {:.3} at step 1 -> {:.3} at step {}{val}",
            first.loss, last.loss, last.step
        );
    }
    Ok(())
}
