//! A language model small enough to understand completely, written from scratch for teaching.
//!
//! Nothing here is borrowed from a framework: the tokenizer, the Transformer (embeddings, RMSNorm,
//! multi-head causal attention, GELU feed-forward, residual connections), back-propagation, the
//! AdamW optimizer with warm-up and learning-rate decay, weight decay, sampling and the probes that
//! show what happens inside are all in this crate, in plain Rust on the CPU. That is the point: a
//! student can train it on their own text in minutes and then look at the real embeddings, the real
//! attention weights, the real gradients and the real loss curve, because every number is ours.
//!
//! Everything is `f64` and deterministic for a given seed, so a result can be reproduced and the
//! hand-written gradients can be checked against numerical differentiation (see the tests).

pub mod model;
pub mod probe;
pub mod rng;
pub mod sample;
pub mod session;
pub mod tokenizer;
pub mod train;

pub use model::{Config, Gpt};
pub use rng::Rng;
pub use session::{LabSession, Snapshot, Status};
pub use tokenizer::Tokenizer;
pub use train::{Schedule, StepStats, TrainConfig, Trainer};

/// Errors a student can cause with their own data or files.
#[derive(Debug, thiserror::Error)]
pub enum LabError {
    #[error("the text is too short to train on: {0}")]
    TooShort(String),
    #[error("{0}")]
    Invalid(String),
    #[error("cannot read or write {path}: {why}")]
    Io { path: String, why: String },
}

pub type Result<T> = std::result::Result<T, LabError>;

/// Texts that ship with the program so a student can start before having any data of their own.
pub const DEMO_EN: &str = include_str!("../data/demo_en.txt");
pub const DEMO_VI: &str = include_str!("../data/demo_vi.txt");
