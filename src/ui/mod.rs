//! Rendering.
//!
//! Widgets are drawn from [`App`] state and never read keys themselves; the
//! help overlay in particular is generated from the live keymap, so it cannot
//! advertise a binding that does not exist.

mod bookmarks;
pub mod confirm;
pub mod form;
mod form_view;
mod help;
pub mod log;
mod panes;
mod progress;
mod status;

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
    Frame,
};

use crate::app::{App, Overlay, Screen};

/// Draw a frame.
pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let [body, status_line] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());

    match app.screen {
        Screen::Bookmarks => bookmarks::draw(frame, app, body),
        Screen::Log => log::draw(frame, app, &app.audit_entries, app.log_cursor, body),
        Screen::Browser => {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(body);
            panes::draw(frame, app, [left, right]);
        }
        Screen::Form => {
            // Cloned because `app` is borrowed mutably by `draw`, and the form
            // renderer needs both it and the palette. A form is a dozen short
            // strings, so the copy costs nothing.
            if let Some(form) = app.form.clone() {
                form_view::draw(frame, app, &form, body);
            }
        }
        Screen::Transfer => {
            if let Some(transfer) = &app.transfer {
                progress::draw(frame, app, transfer, body);
            }
        }
        Screen::Confirm => {
            // Cloned for the same borrow reason as the form. A plan can be
            // large, so this is the one place worth revisiting if a sync of
            // tens of thousands of files ever feels slow to draw.
            if let Some(plan) = app.plan.clone() {
                let cursor = app.plan_cursor;
                let folded = app.folded.clone();
                confirm::draw(frame, app, &plan, cursor, &folded, body);
            }
        }
    }

    status::draw(frame, app, status_line);

    match &app.overlay {
        Overlay::None => {}
        Overlay::Help { scroll } => help::draw(frame, app, *scroll),
        Overlay::Prompt(prompt) => {
            // A password is never echoed. Dots rather than nothing, so you can
            // see that your keystrokes are arriving.
            let shown = if prompt.kind.is_secret() {
                "\u{2022}".repeat(prompt.input.chars().count())
            } else {
                prompt.input.clone()
            };
            let text = format!("{}{shown}", prompt.kind.label());
            draw_prompt_line(frame, app, status_line, &text);
        }
        Overlay::Question(question) => draw_question(frame, app, &question.text),
    }
}

/// The prompt replaces the status line rather than floating over the panes,
/// so typing a search does not hide the thing you are searching.
fn draw_prompt_line(frame: &mut Frame<'_>, app: &App, area: Rect, text: &str) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            // A trailing block stands in for a cursor; the real cursor stays
            // hidden so it cannot end up parked somewhere misleading.
            format!("{text}\u{2588}"),
            Style::default().fg(app.palette.fg),
        ))),
        area,
    );
}

fn draw_question(frame: &mut Frame<'_>, app: &App, text: &str) {
    let area = centered(frame.area(), text.len() as u16 + 6, 5);

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {text}  "),
                Style::default().fg(app.palette.fg),
            )),
            Line::from(Span::styled(
                "  y yes    n no  ",
                Style::default().fg(app.palette.dim),
            )),
        ])
        .block(
            Block::bordered()
                .border_style(Style::default().fg(app.palette.warn))
                .title_top(Line::from(Span::styled(
                    " confirm ",
                    Style::default()
                        .fg(app.palette.warn)
                        .add_modifier(Modifier::BOLD),
                ))),
        ),
        area,
    );
}

/// A box of at most `width` x `height`, centred in `area`.
///
/// Clamped to the available space so a long path in a narrow terminal shrinks
/// the dialog rather than drawing outside the frame.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Fit a path into `max` columns.
///
/// Two steps, in this order:
///
/// 1. Replace the home directory with `~`, which is both shorter and how you
///    think of the path.
/// 2. If it still does not fit, elide from the *left*. The tail of a path is
///    the part that tells you where you are -- truncating the other way leaves
///    you reading `/tmp/claude-1000/-home-greg-Programs-git` and none of the
///    directories you actually navigated into.
pub fn shorten_path(path: &str, max: usize) -> String {
    let shortened = match dirs::home_dir() {
        Some(home) => {
            let home = home.display().to_string();
            match path.strip_prefix(&home) {
                // Only when the next character is a separator, so `/home/greg`
                // does not swallow the `/home/gregory` of a different user.
                Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
                _ => path.to_owned(),
            }
        }
        None => path.to_owned(),
    };

    let width = shortened.chars().count();
    if width <= max {
        return shortened;
    }
    if max <= 1 {
        return "\u{2026}".into();
    }

    let tail: String = shortened.chars().skip(width - (max - 1)).collect();
    format!("\u{2026}{tail}")
}

/// Render a byte count as something a person reads at a glance.
///
/// Binary units, because this is file sizes and every file manager on the
/// platform shows KiB.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];

    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    // One decimal below 10 and none above, so the column stays narrow without
    // throwing away the distinction between 1.2M and 1.9M.
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 K");
        assert_eq!(format_size(1536), "1.5 K");
        assert_eq!(format_size(20 * 1024), "20 K");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 M");
        assert_eq!(format_size(1024_u64.pow(4)), "1.0 T");
    }

    #[test]
    fn the_largest_unit_does_not_overflow() {
        // u64::MAX is about 16 EiB; the unit list stops at P, so it should
        // report a large number of petabytes rather than panicking or
        // dividing forever.
        let rendered = format_size(u64::MAX);
        assert!(rendered.ends_with(" P"), "{rendered}");
    }

    /// The tail is the informative end of a path, so that is what survives.
    #[test]
    fn a_long_path_is_elided_from_the_left() {
        let shortened = shorten_path("/tmp/very/long/prefix/that/goes/on/src/api", 20);
        assert!(shortened.starts_with('\u{2026}'), "{shortened}");
        assert!(shortened.ends_with("src/api"), "{shortened}");
        assert_eq!(shortened.chars().count(), 20);
    }

    #[test]
    fn a_short_path_is_left_alone() {
        assert_eq!(shorten_path("/srv/www", 40), "/srv/www");
    }

    #[test]
    fn the_home_directory_becomes_a_tilde() {
        let home = dirs::home_dir().unwrap().display().to_string();
        assert_eq!(shorten_path(&format!("{home}/proj"), 40), "~/proj");
        assert_eq!(shorten_path(&home, 40), "~");
    }

    /// `/home/greg` must not be stripped out of `/home/gregory`.
    #[test]
    fn a_sibling_home_directory_is_not_abbreviated() {
        let home = dirs::home_dir().unwrap().display().to_string();
        let sibling = format!("{home}ory/proj");
        assert_eq!(shorten_path(&sibling, 80), sibling);
    }

    #[test]
    fn an_absurdly_narrow_column_does_not_panic() {
        assert_eq!(shorten_path("/srv/www/html", 1), "\u{2026}");
        assert_eq!(shorten_path("/srv/www/html", 0), "\u{2026}");
    }

    #[test]
    fn a_dialog_wider_than_the_terminal_is_clamped() {
        let area = Rect::new(0, 0, 20, 10);
        let box_ = centered(area, 100, 100);
        assert_eq!(box_.width, 20);
        assert_eq!(box_.height, 10);
        assert_eq!((box_.x, box_.y), (0, 0));
    }

    #[test]
    fn a_dialog_is_centred() {
        let area = Rect::new(0, 0, 20, 10);
        let box_ = centered(area, 10, 4);
        assert_eq!((box_.x, box_.y, box_.width, box_.height), (5, 3, 10, 4));
    }
}
