//! MAW terminal UI (Ratatui).
//!
//! Architecture:
//! * [`app`]   — pure state + key reducer (`handle_key(&mut App, KeyEvent) -> Vec<Action>`).
//! * [`ui`]    — pure rendering of `&App`.
//! * [`flow`]  — data-flow graph derived from stored events.
//! * [`runtime`] — terminal lifecycle, async event loop, action execution.
//!
//! The TUI owns no provider processes or database connections: it reads
//! `Engine::snapshot()` and sends commands through `Engine`/`Supervisor`.

pub mod app;
pub mod canvas;
pub mod canvas_input;
pub mod downloads;
pub mod events;
pub mod flow;
pub mod input;
pub mod lab_ui;
pub mod picker;
pub mod runtime;
pub mod theme;
pub mod ui;
pub mod usage;

#[doc(hidden)]
pub mod testutil;

pub use app::{handle_key, Action, App, View};
pub use runtime::{restore_terminal, run, TuiOptions};
pub use theme::Theme;
