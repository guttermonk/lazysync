//! The transfer progress screen.
//!
//! Shown while a plan is being applied. Two bars -- the file in flight and
//! the plan as a whole -- plus throughput, ETA and a scrolling list of what
//! has finished.
//!
//! The ETA is absent until there is a measured rate to extrapolate from.
//! A number in the first second is always wrong and people watch it, so
//! showing nothing is the more honest option.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, Gauge, Paragraph, Row, Table, TableState},
    Frame,
};

use super::{format_size, shorten_path};
use crate::{
    app::App,
    config::Palette,
    sync::{
        executor::Outcome,
        transfer::{format_duration, State, Transfer},
    },
};

pub fn draw(frame: &mut Frame<'_>, app: &App, transfer: &Transfer, area: Rect) {
    let palette = &app.palette;

    let [header, bars, log, footer] = Layout::vertical([
        Constraint::Length(2),
        // 3 for the bordered per-file gauge plus 1 for the plan bar. A
        // two-row gauge puts its label on the second line and reads as a
        // rendering bug.
        Constraint::Length(4),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(area);

    draw_header(frame, transfer, palette, header);
    draw_bars(frame, transfer, palette, bars);
    draw_log(frame, transfer, palette, log);
    draw_footer(frame, transfer, palette, footer);
}

fn draw_header(frame: &mut Frame<'_>, transfer: &Transfer, palette: &Palette, area: Rect) {
    let plan = transfer.plan();
    let width = (area.width as usize).saturating_sub(30) / 2;

    let (label, colour) = match transfer.state() {
        State::Running => ("transferring", palette.accent),
        State::Paused => ("PAUSED", palette.warn),
        State::Cancelling => ("cancelling", palette.warn),
        State::Finished => ("done", palette.ok),
    };

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    format!(" {label}  "),
                    Style::default().fg(colour).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    shorten_path(&plan.source_root.display().to_string(), width),
                    Style::default().fg(palette.fg),
                ),
                Span::styled("  \u{2192}  ", Style::default().fg(palette.accent)),
                Span::styled(
                    shorten_path(&plan.dest_root.display().to_string(), width),
                    Style::default().fg(palette.fg),
                ),
            ]),
            Line::from(Span::styled(
                format!(
                    " {} of {}  \u{b7}  {} files left  \u{b7}  {}",
                    format_size(transfer.bytes_done()),
                    format_size(transfer.total_bytes()),
                    transfer.remaining_files(),
                    rate_and_eta(transfer),
                ),
                Style::default().fg(palette.dim),
            )),
        ]),
        area,
    );
}

/// `12 MB/s · 1m 23s left`, or just the elapsed time before a rate exists.
fn rate_and_eta(transfer: &Transfer) -> String {
    let rate = transfer.rate();
    if rate <= 0.0 {
        return format!("{} elapsed", format_duration(transfer.elapsed()));
    }

    match transfer.eta() {
        Some(eta) => format!(
            "{}/s \u{b7} {} left",
            format_size(rate as u64),
            format_duration(eta)
        ),
        None => format!("{}/s", format_size(rate as u64)),
    }
}

fn draw_bars(frame: &mut Frame<'_>, transfer: &Transfer, palette: &Palette, area: Rect) {
    let [current_area, total_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Length(1)]).areas(area);

    // The file in flight.
    let current = transfer.current();
    let title = current.as_ref().map_or_else(
        || " (between files) ".to_string(),
        |c| {
            format!(
                " {} ",
                shorten_path(
                    &c.path.display().to_string(),
                    (area.width as usize).saturating_sub(20)
                )
            )
        },
    );

    let fraction = current.as_ref().map_or(0.0, |c| c.fraction());

    frame.render_widget(
        Gauge::default()
            .block(
                Block::bordered()
                    .border_style(Style::default().fg(palette.border))
                    .title_top(Line::from(Span::styled(
                        title,
                        Style::default().fg(palette.fg),
                    ))),
            )
            .gauge_style(Style::default().fg(palette.accent))
            .ratio(fraction)
            .label(current.map_or_else(
                || "-".to_string(),
                |c| format!("{} / {}", format_size(c.done), format_size(c.total)),
            )),
        current_area,
    );

    // The whole plan.
    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(match transfer.state() {
                State::Paused | State::Cancelling => palette.warn,
                _ => palette.ok,
            }))
            .ratio(transfer.fraction())
            .label(format!("{:.0}% of the plan", transfer.fraction() * 100.0)),
        total_area,
    );
}

/// What has finished, newest last.
fn draw_log(frame: &mut Frame<'_>, transfer: &Transfer, palette: &Palette, area: Rect) {
    let applied = &transfer.report().applied;

    if applied.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  nothing finished yet",
                Style::default().fg(palette.dim),
            ))),
            area,
        );
        return;
    }

    let rows: Vec<Row<'_>> = applied
        .iter()
        .map(|entry| {
            let (glyph, colour, detail) = match &entry.outcome {
                Outcome::Transferred { bytes } => ("\u{2713}", palette.ok, format_size(*bytes)),
                Outcome::Deleted => ("\u{2717}", palette.danger, "deleted".into()),
                Outcome::MovedToBackup { .. } => {
                    ("\u{2717}", palette.warn, "moved to the backup dir".into())
                }
                Outcome::SkippedStale { why } => ("\u{2013}", palette.warn, why.clone()),
                Outcome::Cancelled { partial_bytes } => (
                    "\u{25a0}",
                    palette.warn,
                    format!("cancelled after {}", format_size(*partial_bytes)),
                ),
                Outcome::Failed { why } => ("!", palette.danger, why.clone()),
            };

            Row::new(vec![
                Cell::from(Line::from(Span::styled(glyph, Style::default().fg(colour)))),
                Cell::from(Line::from(Span::styled(
                    shorten_path(
                        &entry.path.display().to_string(),
                        (area.width as usize).saturating_sub(34).max(10),
                    ),
                    Style::default().fg(palette.fg),
                ))),
                Cell::from(Line::from(Span::styled(
                    detail,
                    Style::default().fg(palette.dim),
                ))),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Min(16),
            Constraint::Length(30),
        ],
    );

    // Scrolled to the end, so the newest line is always visible: a log that
    // stops updating once it fills the pane is worse than none.
    let mut state = TableState::default().with_selected(Some(applied.len().saturating_sub(1)));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_footer(frame: &mut Frame<'_>, transfer: &Transfer, palette: &Palette, area: Rect) {
    let report = transfer.report();
    let failures = report.failures().count();

    let mut lines = Vec::new();

    if failures > 0 {
        lines.push(Line::from(Span::styled(
            format!(" {failures} failed -- see the list above"),
            Style::default()
                .fg(palette.danger)
                .add_modifier(Modifier::BOLD),
        )));
    }

    lines.push(Line::from(Span::styled(
        match transfer.state() {
            State::Finished => " any key to return to the browser ".to_string(),
            State::Paused => " p resume \u{b7} esc cancel ".to_string(),
            _ => " p pause \u{b7} esc cancel ".to_string(),
        },
        Style::default().fg(palette.dim),
    )));

    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::sync::{plan::Plan, DeletePolicy};

    fn empty_transfer() -> Transfer {
        Transfer::new(
            Plan::default(),
            DeletePolicy::Remove,
            ".lazysync-part".into(),
            false,
        )
    }

    /// A rate of zero means no measurement yet, so the header shows elapsed
    /// time rather than inventing a throughput.
    #[test]
    fn before_a_rate_exists_the_header_shows_elapsed_time() {
        let transfer = empty_transfer();
        let text = rate_and_eta(&transfer);
        assert!(text.contains("elapsed"), "{text}");
        assert!(!text.contains("/s"), "{text}");
    }

    #[test]
    fn durations_are_readable() {
        assert_eq!(format_duration(Duration::from_secs(5)), "5s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2m 05s");
    }

    #[test]
    fn an_empty_transfer_is_finished_and_reports_a_full_bar() {
        let transfer = empty_transfer();
        assert!(transfer.is_finished());
        assert_eq!(transfer.fraction(), 1.0);
        assert!(
            transfer.eta().is_none(),
            "a finished transfer has no time remaining"
        );
    }
}
