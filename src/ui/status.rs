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

use crate::{
    app::{App, Screen, StatusKind},
    browser::sync_browse,
    config::Palette,
    keys::KeyCombo,
};

/// How a message is drawn, given what kind it is.
///
/// The whole reason this exists: a message rendered in `palette.fg` sits at
/// the end of a line of mode badges, paths and counters that are also
/// `palette.fg`. Pressing a sync key and getting "nothing to push" was
/// reported as the key doing nothing, while the sentence was on screen --
/// it simply did not read as a reply to anything.
///
/// An answer is therefore highlighted and bold, and a refusal is coloured
/// like the other things that stop a transfer. Ambient notes keep the old
/// styling, because making everything loud is the same as making nothing
/// loud.
fn message_style(kind: StatusKind, palette: &Palette) -> Style {
    match kind {
        StatusKind::Note => Style::default().fg(palette.fg),
        StatusKind::Answer => Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
        StatusKind::Problem => Style::default()
            .fg(palette.warn)
            .add_modifier(Modifier::BOLD),
    }
}

pub fn draw(frame: &mut Frame<'_>, app: &App, area: Rect) {
    draw_line(frame, app, area);

    // Last, over whatever the line ended with: a chord in progress is the
    // most perishable thing on screen and the one the user is waiting on.
    draw_pending(frame, app, area);
}

/// The chord in progress, in the bottom-right corner the way vim's `showcmd`
/// sits there.
///
/// This is the whole answer to "did that keypress land". The first key of a
/// chord is bound to nothing on its own, so without this, pressing `s` and
/// then thinking for a moment is indistinguishable from pressing a key the
/// terminal swallowed -- and the next thing you find out is whether you
/// guessed the second key right.
fn draw_pending(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if app.pending_keys.is_empty() {
        return;
    }

    // Spelled by `KeyCombo`, so the echo of what you pressed matches the way
    // every hint and `lazysync keys` writes the same chord.
    let text = format!(" {} ", KeyCombo::chord(app.pending_keys.iter().copied()));
    let width = text.chars().count() as u16;
    if width > area.width {
        return;
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default()
                .fg(app.palette.accent)
                .bg(app.palette.status_bg)
                .add_modifier(Modifier::BOLD),
        ))),
        Rect {
            x: area.x + area.width - width,
            width,
            ..area
        },
    );
}

fn draw_line(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let palette = &app.palette;
    let mut spans = Vec::new();

    // The browser indicators describe the panes, which the bookmark list and
    // the form do not have. Showing a pane cursor position on a screen with no
    // panes is just noise pretending to be information.
    if !matches!(app.screen, Screen::Browser) {
        if matches!(app.screen, Screen::Bookmarks) {
            let shown = app.visible_bookmarks().len();
            let total = app.store.bookmarks.len();
            spans.push(Span::styled(
                if shown == total {
                    format!(" {total} saved ")
                } else {
                    format!(" {shown}/{total} saved ")
                },
                Style::default().fg(palette.dim),
            ));
        }

        if !app.status.is_empty() {
            spans.push(Span::styled(
                format!(" {} ", app.status),
                message_style(app.status_kind, palette),
            ));
        }

        frame.render_widget(
            Paragraph::new(Line::from(spans))
                .style(Style::default().fg(palette.status_fg).bg(palette.status_bg)),
            area,
        );
        return;
    }

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
        // Styled by kind, like the other branch. It was plain `palette.fg`
        // here, which is the browser -- the one screen where the reported bug
        // actually happens. `sp` on two identical trees answers "nothing to
        // push", and an answer rendered in the same colour as the badges and
        // counters either side of it reads as part of the furniture rather
        // than as a reply to the key just pressed.
        spans.push(Span::styled(
            format!(" {} ", app.status),
            message_style(app.status_kind, palette),
        ));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .style(Style::default().fg(palette.status_fg).bg(palette.status_bg)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use crate::{
        app::{App, StatusKind},
        config::Config,
        keys::{Key, ModifiedKey},
    };

    fn app() -> App {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().to_path_buf();
        App::new(Config::default(), here.clone(), here).unwrap()
    }

    /// The status line as drawn, plus the style of the cell at `column`.
    fn line(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(60, 1)).unwrap();
        terminal
            .draw(|frame| super::draw(frame, app, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect()
    }

    /// The whole point: `s` on its own is bound to nothing, so without the
    /// echo, pressing it produced no evidence anywhere on screen.
    #[test]
    fn a_chord_in_progress_is_echoed() {
        let mut app = app();
        app.pending_keys = vec![ModifiedKey::plain(Key::Char('s'))];

        let drawn = line(&mut app);
        assert!(drawn.trim_end().ends_with('s'), "{drawn:?}");
    }

    #[test]
    fn a_two_key_chord_is_echoed_as_the_sequence_it_is() {
        let mut app = app();
        app.pending_keys = vec![
            ModifiedKey::plain(Key::Char('g')),
            ModifiedKey::plain(Key::Char('g')),
        ];

        assert!(line(&mut app).contains("gg"));
    }

    #[test]
    fn nothing_pending_means_nothing_in_the_corner() {
        let mut app = app();
        app.status.clear();

        let drawn = line(&mut app);
        assert!(
            drawn.trim_end().chars().last().unwrap_or(' ').is_numeric(),
            "the line should end in the cursor counter: {drawn:?}"
        );
    }

    /// The bug `message_style` was written for, in the screen it was reported
    /// on: the browser branch rendered every message in `palette.fg`, so an
    /// answer to a keypress looked like one more badge on a line of badges.
    #[test]
    fn a_browser_message_is_styled_by_its_kind() {
        let mut app = app();
        app.status = "nothing to push".into();
        app.status_kind = StatusKind::Answer;

        let mut terminal = Terminal::new(TestBackend::new(60, 1)).unwrap();
        terminal
            .draw(|frame| super::draw(frame, &app, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let column = (0..buffer.area.width)
            .find(|x| buffer[(*x, 0)].symbol() == "n")
            .expect("the message is on the line");
        let cell = &buffer[(column, 0)];

        assert_eq!(
            cell.fg, app.palette.accent,
            "an answer has to read as a reply, not as furniture"
        );
        assert!(cell.modifier.contains(ratatui::style::Modifier::BOLD));
    }
}
