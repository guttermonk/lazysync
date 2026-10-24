//! The bookmark list: the screen lazysync opens on.
//!
//! What it shows is a name, a protocol badge, and the `local ↔ remote` pair of
//! paths. What it deliberately does *not* show is the connection
//! configuration -- host, port, username, key file. Those live in the form, on
//! their own page, because a list of hosts and usernames is a list of things
//! an onlooker can read, and because the thing you are picking between is
//! which *connection*, not which hostname.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};

use super::shorten_path;
use crate::app::App;

pub fn draw(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let [body, hint] = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(area);

    let palette = &app.palette;
    let visible = app.visible_bookmarks();

    let title = if app.bookmark_filter.is_empty() {
        " connections ".to_string()
    } else {
        format!(" connections matching {:?} ", app.bookmark_filter)
    };

    let block = Block::bordered()
        .border_style(Style::default().fg(palette.border_focused))
        .title_top(Line::from(Span::styled(
            title,
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(body);
    frame.render_widget(block, body);

    if visible.is_empty() {
        let message = if app.store.bookmarks.is_empty() {
            "  No saved connections yet.\n\n  Press n to add one. Connection details are \
             entered on that page -- this list only ever shows the name."
        } else {
            "  Nothing matches that filter. Press Esc to clear it."
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(palette.dim))
                .wrap(Wrap { trim: false }),
            inner,
        );
        draw_hints(frame, app, hint);
        return;
    }

    // Room for the paths after the fixed columns and the borders.
    let path_width = (inner.width as usize).saturating_sub(28) / 2;

    let rows: Vec<Row<'_>> = visible
        .iter()
        .map(|(_, bookmark)| {
            let local = bookmark.local_path.as_ref().map_or_else(
                || "-".to_string(),
                |p| shorten_path(&p.display().to_string(), path_width),
            );
            let remote = bookmark.remote_path.as_ref().map_or_else(
                || "~".to_string(),
                |p| shorten_path(&p.display().to_string(), path_width),
            );

            Row::new(vec![
                Cell::from(Line::from(Span::styled(
                    bookmark.name.clone(),
                    Style::default().fg(palette.fg).add_modifier(Modifier::BOLD),
                ))),
                Cell::from(Line::from(Span::styled(
                    bookmark.protocol.scheme().to_string(),
                    Style::default().fg(if bookmark.protocol.is_implemented() {
                        palette.accent
                    } else {
                        // A protocol that cannot connect yet is marked rather
                        // than hidden, so a bookmark for one is not a mystery
                        // failure at open time.
                        palette.warn
                    }),
                ))),
                Cell::from(Line::from(Span::styled(
                    local,
                    Style::default().fg(palette.dim),
                ))),
                Cell::from(Line::from(Span::styled(
                    "\u{2194}",
                    Style::default().fg(palette.dim),
                ))),
                Cell::from(Line::from(Span::styled(
                    remote,
                    Style::default().fg(palette.dim),
                ))),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Min(12),
            Constraint::Length(7),
            Constraint::Percentage(40),
            Constraint::Length(1),
            Constraint::Percentage(40),
        ],
    )
    .row_highlight_style(Style::default().fg(palette.cursor_fg).bg(palette.cursor_bg));

    let mut state = TableState::default().with_selected(Some(
        app.bookmark_cursor.min(visible.len().saturating_sub(1)),
    ));
    frame.render_stateful_widget(table, inner, &mut state);

    draw_hints(frame, app, hint);
}

/// A line of key hints, plus what opening the selected entry will do.
///
/// The second part matters: it is how the screen stays honest about the fact
/// that most connections open without asking for anything, while a
/// password-auth one will prompt.
fn draw_hints(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let palette = &app.palette;

    let mut lines = vec![Line::from(Span::styled(
        "  enter open \u{b7} n new \u{b7} e edit \u{b7} dd delete \u{b7} / filter \u{b7} \
         esc local browser \u{b7} ? help",
        Style::default().fg(palette.dim),
    ))];

    if let Some(bookmark) = app.selected_bookmark() {
        let what = if !bookmark.protocol.is_implemented() {
            format!(
                "{} arrives in phase {}",
                bookmark.protocol,
                bookmark.protocol.arrives_in_phase()
            )
        } else if bookmark.needs_password_prompt() {
            "will ask for a password".to_string()
        } else if bookmark.remember_password {
            "password from the keyring".to_string()
        } else {
            format!("{} auth, no password needed", bookmark.auth)
        };

        lines.push(Line::from(Span::styled(
            format!("  {what}"),
            Style::default().fg(palette.dim),
        )));
    }

    frame.render_widget(Paragraph::new(lines), area);
}
