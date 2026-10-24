//! The status line.
//!
//! Left: the state that changes what keys do -- synchronized browsing, union
//! view, whether the panes are in step. Those are modes you can forget you
//! are in, so they are always on screen rather than discoverable by
//! experiment. Right: the message from the last action.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{app::App, browser::sync_browse};

pub fn draw(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let palette = &app.palette;
    let mut spans = Vec::new();

    if app.sync_browse {
        let in_step = sync_browse::in_step(&app.panes);
        let (label, color) = if app.desynced || !in_step {
            (" SYNC \u{26a0} ", palette.warn)
        } else {
            (" SYNC ", palette.ok)
        };
        spans.push(Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));

        // The shared relative path is the thing that makes synchronized
        // browsing legible: it says what both panes are standing on, even
        // when one of them is a ghost.
        if let Some(relative) = sync_browse::common_relative(&app.panes) {
            let shown = if relative.as_os_str().is_empty() {
                ".".to_string()
            } else {
                relative.display().to_string()
            };
            spans.push(Span::styled(
                format!("{shown} "),
                Style::default().fg(palette.dim),
            ));
        }
    }

    if app.union_view {
        spans.push(Span::styled(
            " UNION ",
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ));
    }

    if app.panes[app.focus].show_hidden {
        spans.push(Span::styled(" .files ", Style::default().fg(palette.dim)));
    }

    let pane = &app.panes[app.focus];
    spans.push(Span::styled(
        format!(
            " {}/{} ",
            pane.cursor() + usize::from(!pane.is_empty()),
            pane.len()
        ),
        Style::default().fg(palette.dim),
    ));

    let marks = pane.marks().len();
    if marks > 0 {
        spans.push(Span::styled(
            format!(" {marks} marked "),
            Style::default().fg(palette.marked),
        ));
    }

    if !app.status.is_empty() {
        spans.push(Span::styled(
            format!(" {} ", app.status),
            Style::default().fg(palette.fg),
        ));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .style(Style::default().fg(palette.status_fg).bg(palette.status_bg)),
        area,
    );
}
