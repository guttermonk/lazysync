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

use super::{hint, shorten_path};
use crate::{
    app::App,
    keys::{Action, Mode},
};

pub fn draw(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let [body, hints_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(area);

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
        let keymap = app.keymaps.get(Mode::Bookmarks);
        // Both sentences name a key, so both have to ask the keymap for it.
        // This empty state is the first thing a new user reads, and it was
        // telling them to press the default key rather than their own.
        let message = if app.store.bookmarks.is_empty() {
            let press = hint::key_for(keymap, Action::NewItem).map_or_else(
                || {
                    "  Nothing is bound to \"new connection\" in your keymap -- see `?`."
                        .to_string()
                },
                |key| format!("  Press {key} to add one."),
            );
            format!(
                "  No saved connections yet.\n\n{press} Connection details are entered on that \
                 page -- this list only ever shows the name."
            )
        } else {
            hint::key_for(keymap, Action::Cancel).map_or_else(
                || "  Nothing matches that filter.".to_string(),
                |key| format!("  Nothing matches that filter. Press {key} to clear it."),
            )
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(palette.dim))
                .wrap(Wrap { trim: false }),
            inner,
        );
        draw_hints(frame, app, hints_area);
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

    draw_hints(frame, app, hints_area);
}

/// A line of key hints, plus what opening the selected entry will do.
///
/// The second part matters: it is how the screen stays honest about the fact
/// that most connections open without asking for anything, while a
/// password-auth one will prompt.
fn draw_hints(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let palette = &app.palette;

    let hints = hint::line(
        app.keymaps.get(Mode::Bookmarks),
        &[
            (Action::OpenItem, "open"),
            (Action::NewItem, "new"),
            (Action::EditItem, "edit"),
            (Action::DeleteItem, "delete"),
            (Action::Search, "filter"),
            (Action::Cancel, "local browser"),
            (Action::Help, "help"),
        ],
    );
    let mut lines = vec![Line::from(Span::styled(
        format!(" {hints}"),
        Style::default().fg(palette.dim),
    ))];

    if let Some(bookmark) = app.selected_bookmark() {
        // The built-in entry has no auth to describe, so it says what it is
        // for instead -- which is the question somebody looking at a row
        // called "Local transfer" in a list of servers is actually asking.
        let what = if bookmark.builtin {
            "two directories on this machine -- no connection needed".to_string()
        } else if !bookmark.protocol.is_implemented() {
            format!("{} is not supported yet", bookmark.protocol)
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

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use crate::{app::App, config::Config};

    /// Render the real screen and read the footer off the cells.
    ///
    /// The store is emptied after construction rather than by pointing
    /// `LAZYSYNC_DEV_CONFIG_DIR` at a temp directory: an env var is process
    /// global, so two of these running in parallel would fight over it, and
    /// a test whose result depends on which thread won is the kind of thing
    /// that only shows up in CI.
    fn rendered_with(config_toml: &str) -> String {
        let config: Config = toml::from_str(config_toml).expect("the test config must parse");

        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().to_path_buf();
        let mut app = App::new(config, here.clone(), here).unwrap();
        app.store = Default::default();

        let mut terminal = Terminal::new(TestBackend::new(120, 14)).unwrap();
        terminal
            .draw(|frame| super::draw(frame, &app, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The reported bug, end to end: the footer and the empty-state sentence
    /// both have to name the rebound key.
    ///
    /// The unit tests in `super::super::hint` cover the rendering rule; this
    /// one covers the wiring, which is where the bug actually was. The footer
    /// logic was correct in the sense that it compiled -- it was a literal
    /// that nothing connected to the keymap at all.
    #[test]
    fn a_rebound_new_connection_key_is_what_the_screen_says() {
        let screen = rendered_with(
            r#"
[keys.bookmarks]
base = [
    ["NewItem", "a"],
    ["OpenItem", "o"],
    ["Cancel", "esc"],
]
"#,
        );

        assert!(
            screen.contains("a new"),
            "the footer must offer the bound key.\n{screen}"
        );
        assert!(
            screen.contains("Press a to add one"),
            "the empty state must name the bound key -- this is the sentence that was \
             telling the user to press `n`.\n{screen}"
        );
        assert!(
            !screen.contains("n new"),
            "`n new` is the literal this replaced, and `n` is not bound here.\n{screen}"
        );
        assert!(
            !screen.contains("Press n to add"),
            "the old empty-state literal must be gone.\n{screen}"
        );
    }

    /// An action nobody bound is not advertised, since the whole point is that
    /// a footer lists keys that work.
    #[test]
    fn an_unbound_action_is_not_advertised() {
        let screen = rendered_with(
            r#"
[keys.bookmarks]
base = [["NewItem", "a"]]
"#,
        );

        assert!(screen.contains("a new"));
        assert!(
            !screen.contains("delete"),
            "DeleteItem is unbound in this map, so the footer must not offer it.\n{screen}"
        );
        assert!(
            !screen.contains("edit"),
            "EditItem is unbound in this map too.\n{screen}"
        );
    }
}
