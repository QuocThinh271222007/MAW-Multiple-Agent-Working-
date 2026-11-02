//! Small layout/text helpers shared by all views. All functions are total (no panics on
//! zero-sized or tiny areas).

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;

/// Truncate to `max` display columns, adding an ellipsis when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// Truncate then right-pad with spaces to exactly `w` columns.
pub fn pad(s: &str, w: usize) -> String {
    let t = truncate(s, w);
    let tw = t.width();
    format!("{t}{}", " ".repeat(w.saturating_sub(tw)))
}

pub fn bar(width: usize, frac: f64) -> String {
    let f = if frac.is_finite() {
        frac.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (f * width as f64).round() as usize;
    format!(
        "{}{}",
        "█".repeat(filled.min(width)),
        "░".repeat(width.saturating_sub(filled))
    )
}

/// Greedy word wrap by display width. Never returns an empty vec.
pub fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for para in s.split('\n') {
        let mut line = String::new();
        let mut lw = 0;
        for word in para.split(' ') {
            let ww = word.width();
            if ww > width {
                // hard-split very long tokens
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                let mut chunk = String::new();
                let mut cw = 0;
                for c in word.chars() {
                    let w = c.width().unwrap_or(0);
                    if cw + w > width {
                        out.push(std::mem::take(&mut chunk));
                        cw = 0;
                    }
                    chunk.push(c);
                    cw += w;
                }
                line = chunk;
                lw = cw;
                continue;
            }
            let need = if line.is_empty() { ww } else { lw + 1 + ww };
            if need > width {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
                lw = ww;
            } else {
                if !line.is_empty() {
                    line.push(' ');
                    lw += 1;
                }
                line.push_str(word);
                lw += ww;
            }
        }
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

pub fn fmt_elapsed(secs: i64) -> String {
    let s = secs.max(0);
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

pub fn fmt_ago(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

/// Centered rectangle of at most `w`x`h` inside `area`.
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

pub fn panel<'a>(title: impl Into<Line<'a>>, focused: bool, theme: &Theme) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Plain
        })
        .border_style(theme.border(focused))
        .title(title)
}

pub fn spans<'a>(items: Vec<(String, Style)>) -> Line<'a> {
    Line::from(
        items
            .into_iter()
            .map(|(t, s)| Span::styled(t, s))
            .collect::<Vec<_>>(),
    )
}

/// Clamp a scroll offset so a window of `height` lines stays inside `total`.
pub fn clamp_scroll(offset: usize, total: usize, height: usize) -> usize {
    offset.min(total.saturating_sub(height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_display_width() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("日本語です", 5), "日本…");
        assert_eq!(truncate("x", 0), "");
        assert_eq!(pad("ab", 5), "ab   ");
        assert_eq!(pad("abcdefgh", 4).width(), 4);
    }

    #[test]
    fn wrap_handles_long_tokens_and_newlines() {
        let l = wrap_text("aaaa bbbb cccc", 9);
        assert_eq!(l, vec!["aaaa bbbb", "cccc"]);
        let l = wrap_text("abcdefghij", 4);
        assert!(l.iter().all(|x| x.width() <= 4) && l.concat() == "abcdefghij");
        assert_eq!(wrap_text("a\nb", 10), vec!["a", "b"]);
        assert_eq!(wrap_text("", 10), vec![""]);
    }

    #[test]
    fn bar_is_bounded() {
        assert_eq!(bar(10, 0.5), "█████░░░░░");
        assert_eq!(bar(4, 7.0), "████");
        assert_eq!(bar(4, f64::NAN), "░░░░");
        assert_eq!(bar(0, 0.5), "");
    }

    #[test]
    fn centered_never_exceeds_area() {
        let a = Rect::new(0, 0, 10, 4);
        let c = centered(a, 50, 50);
        assert_eq!((c.width, c.height), (10, 4));
    }

    #[test]
    fn elapsed_formats() {
        assert_eq!(fmt_elapsed(65), "01:05");
        assert_eq!(fmt_elapsed(3725), "1:02:05");
        assert_eq!(fmt_ago(5), "5s");
        assert_eq!(fmt_ago(300), "5m");
    }
}
