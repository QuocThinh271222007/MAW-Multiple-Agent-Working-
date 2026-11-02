//! Colors and styles. A monochrome theme (explicit option or `NO_COLOR`) relies on text
//! labels plus modifiers (bold/dim/reverse) only, so state is never conveyed by color alone.

use maw_core::{AgentState, Role, TaskStatus};
use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub mono: bool,
}

impl Theme {
    pub fn new(mono: bool) -> Self {
        Theme { mono }
    }

    /// `force_mono` or a non-empty `NO_COLOR` environment variable selects the monochrome theme.
    pub fn from_env(force_mono: bool) -> Self {
        let no_color = std::env::var("NO_COLOR")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        Theme {
            mono: force_mono || no_color,
        }
    }

    fn fg(&self, c: Color) -> Style {
        if self.mono {
            Style::default()
        } else {
            Style::default().fg(c)
        }
    }

    pub fn base(&self) -> Style {
        Style::default()
    }
    pub fn dim(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    }
    pub fn bold(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }
    pub fn accent(&self) -> Style {
        self.fg(Color::Cyan).add_modifier(Modifier::BOLD)
    }
    pub fn ok(&self) -> Style {
        self.fg(Color::Green)
    }
    pub fn warn(&self) -> Style {
        self.fg(Color::Yellow)
    }
    pub fn err(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
    }
    pub fn info(&self) -> Style {
        self.fg(Color::Blue)
    }
    pub fn magenta(&self) -> Style {
        self.fg(Color::Magenta)
    }
    pub fn selected(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        }
    }
    pub fn header(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        }
    }
    pub fn footer(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(Color::Gray)
        }
    }
    pub fn border(&self, focused: bool) -> Style {
        if focused {
            self.accent()
        } else {
            self.dim()
        }
    }
    pub fn highlight(&self) -> Style {
        if self.mono {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().bg(Color::Yellow).fg(Color::Black)
        }
    }

    pub fn agent_state(&self, s: AgentState) -> Style {
        use AgentState::*;
        match s {
            Working => self.fg(Color::Green).add_modifier(Modifier::BOLD),
            Thinking => self.fg(Color::Cyan).add_modifier(Modifier::BOLD),
            Idle | Completed => self.fg(Color::Green),
            WaitingApproval | RateLimited | AuthRequired => {
                self.fg(Color::Yellow).add_modifier(Modifier::BOLD)
            }
            WaitingDependency | Connecting | Recovering => self.fg(Color::Yellow),
            Blocked | Failed => self.err(),
            Disconnected | Stopped => self.dim(),
        }
    }

    pub fn task_status(&self, s: TaskStatus) -> Style {
        use TaskStatus::*;
        match s {
            Backlog => self.dim(),
            Ready => self.fg(Color::Blue),
            Assigned => self.fg(Color::Cyan),
            Running => self.fg(Color::Green).add_modifier(Modifier::BOLD),
            Blocked => self.fg(Color::Red).add_modifier(Modifier::BOLD),
            InReview => self.fg(Color::Magenta),
            Verifying => self.fg(Color::Yellow),
            Done => self.fg(Color::Green),
            Failed => self.err(),
            Canceled => self.dim(),
        }
    }

    pub fn role(&self, r: Role) -> Style {
        match r {
            Role::Leader => self.fg(Color::Yellow).add_modifier(Modifier::BOLD),
            Role::Worker => self.fg(Color::Cyan),
            Role::Reviewer => self.fg(Color::Magenta),
            Role::Specialist => self.fg(Color::Blue),
        }
    }
}

/// Short fixed-width tag for a task status (used by compact lists).
pub fn status_tag(s: TaskStatus) -> &'static str {
    match s {
        TaskStatus::Backlog => "BACK ",
        TaskStatus::Ready => "READY",
        TaskStatus::Assigned => "ASGN ",
        TaskStatus::Running => "RUN  ",
        TaskStatus::Blocked => "BLOCK",
        TaskStatus::InReview => "REVW ",
        TaskStatus::Verifying => "VERIF",
        TaskStatus::Done => "DONE ",
        TaskStatus::Failed => "FAIL ",
        TaskStatus::Canceled => "CANC ",
    }
}

pub fn state_dot(s: AgentState) -> &'static str {
    match s {
        AgentState::Working | AgentState::Thinking => "●",
        AgentState::Idle | AgentState::Completed => "○",
        AgentState::WaitingApproval | AgentState::RateLimited | AgentState::AuthRequired => "◐",
        AgentState::Blocked | AgentState::Failed => "✗",
        _ => "·",
    }
}
