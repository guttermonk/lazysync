//! The audit log viewer.
//!
//! One line per run, newest first, with the selected run's files expanded
//! below. The ordering is the whole design: a log read from the top is a log
//! whose useful end is a thousand lines down.
//!
//! The question this screen answers is "why is that file not there", so a
//! failed item is red and carries its reason, and a refused run says so in
//! place of its counts -- those are the two cases somebody opens a log for.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use super::hint;
use crate::{
    app::App,
    config::Palette,
    keys::{Action, Keymap, Mode},
    sync::audit::Entry,
};

/// Draw the log over the whole area.
pub fn draw(frame: &mut Frame<'_>, app: &App, entries: &[Entry], cursor: usize, area: Rect) {
    let palette = &app.palette;

    let [header, body, detail, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(8),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " Audit log  ",
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                crate::sync::audit::path().display().to_string(),
                Style::default().fg(palette.dim),
            ),
        ])),
        header,
    );

    if entries.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Nothing recorded yet. A run is written here when it finishes, including \
                     the ones a safety rail refused.",
                    Style::default().fg(palette.dim),
                )),
            ])
            .wrap(Wrap { trim: false }),
            body,
        );
        draw_footer(frame, palette, app.keymaps.get(Mode::Log), footer);
        return;
    }

    let items: Vec<ListItem<'_>> = entries
        .iter()
        .map(|entry| {
            let color = if entry.refused.is_some() {
                palette.danger
            } else if entry.failed > 0 {
                palette.warn
            } else {
                palette.fg
            };
            ListItem::new(Line::from(Span::styled(
                format!(" {}", entry.summary()),
                Style::default().fg(color),
            )))
        })
        .collect();

    let block = Block::bordered().border_style(Style::default().fg(palette.border_focused));
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().fg(palette.cursor_fg).bg(palette.cursor_bg));

    let mut state = ListState::default().with_selected(Some(cursor.min(entries.len() - 1)));
    frame.render_stateful_widget(list, body, &mut state);

    draw_detail(frame, entries.get(cursor), palette, detail);
    draw_footer(frame, palette, app.keymaps.get(Mode::Log), footer);
}

/// The selected run's files.
fn draw_detail(frame: &mut Frame<'_>, entry: Option<&Entry>, palette: &Palette, area: Rect) {
    let Some(entry) = entry else {
        return;
    };

    let mut lines = vec![Line::from(Span::styled(
        format!(" {} \u{2192} {}", entry.source, entry.destination),
        Style::default().fg(palette.dim),
    ))];

    if let Some(why) = &entry.refused {
        lines.push(Line::from(Span::styled(
            format!(" {why}"),
            Style::default()
                .fg(palette.danger)
                .add_modifier(Modifier::BOLD),
        )));
    }

    // Failures first. This screen is opened to find out why something is
    // missing, so the answer must not be below a fold of successes.
    let mut items: Vec<_> = entry.items.iter().collect();
    items.sort_by_key(|item| item.outcome != "failed");

    for item in items.iter().take(5) {
        let color = match item.outcome.as_str() {
            "failed" => palette.danger,
            "skipped" | "cancelled" => palette.warn,
            _ => palette.dim,
        };
        let detail = item
            .detail
            .as_deref()
            .map(|d| format!(" -- {d}"))
            .unwrap_or_default();
        lines.push(Line::from(Span::styled(
            format!("  {} {}{}", item.outcome, item.path, detail),
            Style::default().fg(color),
        )));
    }

    if entry.items.len() > 5 {
        lines.push(Line::from(Span::styled(
            format!("  ... and {} more", entry.items.len() - 5),
            Style::default().fg(palette.dim),
        )));
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_footer(frame: &mut Frame<'_>, palette: &Palette, keymap: &Keymap, area: Rect) {
    // `MoveDown`/`MoveUp` are two actions presented as one hint, because
    // "move" is one idea to the reader. Both are named so a split binding --
    // down on one key, up on another that is not its pair -- still reads
    // correctly.
    let movement = [
        hint::key_for(keymap, Action::MoveDown),
        hint::key_for(keymap, Action::MoveUp),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("/");

    let mut parts = Vec::new();
    if !movement.is_empty() {
        parts.push(format!("{movement} move"));
    }
    let rest = hint::line(keymap, &[(Action::Cancel, "back"), (Action::Help, "help")]);
    if !rest.is_empty() {
        parts.push(rest.trim_start().to_string());
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {}", parts.join(" \u{b7} ")),
            Style::default().fg(palette.dim),
        ))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::sync::{
        audit,
        executor::{Applied, Outcome, Report},
        // `Action` is the keymap's in this module now, so the plan's is aliased.
        plan::Action as PlanAction,
    };

    fn entry_with_failure() -> Entry {
        let report = Report {
            applied: vec![
                Applied {
                    path: "ok.txt".into(),
                    action: PlanAction::Create,
                    outcome: Outcome::Transferred { bytes: 5 },
                },
                Applied {
                    path: "bad.txt".into(),
                    action: PlanAction::Overwrite,
                    outcome: Outcome::Failed {
                        why: "permission denied".into(),
                    },
                },
            ],
            bytes: 5,
        };
        audit::Entry::from_report(Utc::now(), "push", "native", "a", "b", true, &report)
    }

    /// The screen is opened to find out why a file is missing, so a failure
    /// must not sit below a fold of successes.
    #[test]
    fn failures_sort_above_successes_in_the_detail() {
        let entry = entry_with_failure();
        let mut items: Vec<_> = entry.items.iter().collect();
        items.sort_by_key(|item| item.outcome != "failed");

        assert_eq!(
            items[0].outcome, "failed",
            "the answer to the question must be the first thing shown"
        );
        assert_eq!(items[0].path, "bad.txt");
    }

    #[test]
    fn a_refused_run_is_distinguishable_from_a_failed_one() {
        let refused = audit::Entry::refusal(Utc::now(), "mirror push", "a", "b", "too many");
        assert!(refused.refused.is_some());
        assert_eq!(refused.failed, 0, "nothing ran, so nothing failed");

        let failed = entry_with_failure();
        assert!(failed.refused.is_none());
        assert_eq!(failed.failed, 1);
    }
}
