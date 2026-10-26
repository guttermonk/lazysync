//! The browser's key hint bar.
//!
//! The bookmark list, the form and the confirmation window all tell you what
//! you can press. The browser -- the screen you spend the most time on, and
//! the only one where the interesting keys are two-key chords nobody guesses
//! -- told you nothing, which is how somebody can open a connection and have
//! no idea that `sp` plans a push.
//!
//! **What it offers depends on what is in front of you.** Pointed at two
//! local directories, the sync chords are still bound and still work, but the
//! thing you are most likely to want is navigation. With a remote pane open,
//! the sync actions are the entire reason the connection exists, so they come
//! first and the navigation keys are what drops off a narrow terminal.
//!
//! Everything here is generated from the resolved keymap by [`super::hint`],
//! so an action nobody bound is not advertised -- which also makes a `base`
//! table that forgot the sync chords visible at a glance instead of
//! presenting as "the sync keys do nothing".

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::hint;
use crate::{
    app::App,
    keys::{Action, Mode},
};

/// Draw the bar, most useful action first.
pub fn draw(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let keymap = app.keymaps.get(Mode::Browser);

    let text = hint::fitted(keymap, &items(app), area.width as usize);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(app.palette.dim),
        ))),
        area,
    );
}

/// Whether either pane is something other than the local filesystem.
///
/// Asked of the VFS rather than tracked as a flag on `App`, so it cannot go
/// stale: a pane that is showing a remote tree *is* connected, by definition.
fn connected(app: &App) -> bool {
    app.panes.iter().any(|pane| pane.vfs.scheme() != "local")
}

fn items(app: &App) -> Vec<(Action, &'static str)> {
    let sync = [
        (Action::SyncPush, "push"),
        (Action::SyncPull, "pull"),
        (Action::SyncBisync, "bisync"),
        (Action::SyncMirror, "mirror"),
        (Action::SyncProfile, "profile"),
    ];
    let browse = [
        (Action::ToggleSyncBrowse, "sync-browse"),
        (Action::ToggleMark, "mark"),
        (Action::NextPane, "pane"),
        (Action::Search, "find"),
        (Action::ToggleUnionView, "union"),
        (Action::ToggleWatch, "watch"),
        (Action::ShowLog, "log"),
    ];
    // `?` last but never dropped: it is the way out of not knowing, so it is
    // prepended to whichever group leads rather than left to the tail.
    let help = [(Action::Help, "help")];

    if connected(app) {
        help.into_iter().chain(sync).chain(browse).collect()
    } else {
        help.into_iter().chain(browse).chain(sync).collect()
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use crate::{app::App, config::Config};

    fn bar_for(width: u16) -> String {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().to_path_buf();
        let app = App::new(Config::default(), here.clone(), here).unwrap();

        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        terminal
            .draw(|frame| super::draw(frame, &app, frame.area()))
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>()
    }

    /// The point of the bar: the chords you would never guess are on screen.
    #[test]
    fn the_bar_offers_the_keys_that_are_actually_bound() {
        let bar = bar_for(200);
        assert!(bar.contains("sp push"), "{bar}");
        assert!(bar.contains("sl pull"), "{bar}");
        assert!(bar.contains("? help"), "{bar}");
    }

    /// A narrow terminal drops whole hints, never half of one.
    #[test]
    fn a_narrow_bar_truncates_between_items() {
        let bar = bar_for(24);
        let trimmed = bar.trim_end();

        assert!(!trimmed.is_empty(), "something must survive");
        assert!(
            trimmed.chars().count() <= 24,
            "the bar must fit: {trimmed:?}"
        );
        // Every surviving piece is a whole `key label` pair.
        for piece in trimmed.split('\u{b7}') {
            let piece = piece.trim();
            assert!(
                piece.split_whitespace().count() == 2,
                "{piece:?} is half a hint, from {trimmed:?}"
            );
        }
    }

    /// Help survives even the narrowest useful bar, because it is the way out
    /// of not knowing what anything else does.
    #[test]
    fn help_is_never_the_hint_that_gets_dropped() {
        assert!(bar_for(10).contains("? help"));
    }
}
