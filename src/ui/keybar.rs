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
    let width = area.width as usize;

    // With a chord in progress the bar stops advertising the keys you could
    // have pressed and starts answering the question you now have: having
    // pressed `s`, what are the second keys? The alternative was a bar that
    // kept saying `sp push` while you were already halfway through typing it.
    let (text, style) = match completions(app, keymap) {
        Some(offer) => (
            hint::fitted_pairs(&offer, width),
            Style::default().fg(app.palette.accent),
        ),
        None => (
            hint::fitted(keymap, &items(app), width),
            Style::default().fg(app.palette.dim),
        ),
    };

    frame.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
}

/// What the chord in progress could still become, `sp push` style.
///
/// `None` when no chord is in progress, which is the usual case and the one
/// that leaves the bar alone. An empty `Some` cannot happen: a chord is only
/// pending because a longer binding starts with it.
fn completions(app: &App, keymap: &crate::keys::Keymap) -> Option<Vec<(String, &'static str)>> {
    if app.pending_keys.is_empty() {
        return None;
    }

    let mut offer: Vec<(String, &'static str)> = keymap
        .completions(&app.pending_keys)
        .into_iter()
        .map(|(action, combo)| (combo.to_string(), action.label()))
        .collect();

    if offer.is_empty() {
        return None;
    }

    // The way out, last. A chord you did not mean to start is the other half
    // of being shown one in progress, and the key that ends it is whatever
    // this mode cancels with -- so this reads `esc back` or `ctrl+c back`
    // depending on the keymap, and is left out entirely if Cancel is unbound.
    if let Some(key) = hint::key_for(keymap, Action::Cancel) {
        offer.push((key, "back"));
    }

    Some(offer)
}

/// Whether either pane is something other than the local filesystem.
///
/// Asked of the VFS rather than tracked as a flag on `App`, so it cannot go
/// stale: a pane that is showing a remote tree *is* connected, by definition.
fn connected(app: &App) -> bool {
    app.panes.iter().any(|pane| pane.vfs.scheme() != "local")
}

fn items(app: &App) -> Vec<(Action, &'static str)> {
    // `sp`, `sl`, `sm` and `sb` work against whatever two directories are
    // open. The profile-driven pair does not: with no `[profiles.<name>]` on
    // the connection, pressing either can only produce the message that says
    // so. An offer that cannot be taken up is the thing this bar is for
    // avoiding, so those two are dropped rather than greyed -- there is one
    // row, and a dimmed hint in a bar that is already dim reads as a hint.
    // The help overlay still lists both, with what they need.
    let sync: Vec<(Action, &'static str)> = [
        (Action::SyncPush, "push"),
        (Action::SyncPull, "pull"),
        (Action::SyncBisync, "bisync"),
        (Action::SyncMirror, "mirror"),
        (Action::SyncProfile, "profile"),
    ]
    .into_iter()
    .filter(|(action, _)| *action != Action::SyncProfile || app.profile_available())
    .collect();

    // `Hide` is dropped unless there is something to hide with, on the same
    // grounds as the profile pair above: with no `hide_command` configured
    // the key can only produce the message saying so, and a bar that offers
    // it is advertising a dead end. Unbound, it drops out anyway -- `hint`
    // omits what the keymap does not bind -- so this covers the other half,
    // where the key exists and the command behind it does not.
    let browse: Vec<(Action, &'static str)> = [
        (Action::ShowConnections, "connections"),
        (Action::ToggleSyncBrowse, "sync-browse"),
        (Action::ToggleMark, "mark"),
        (Action::NextPane, "pane"),
        (Action::Search, "find"),
        (Action::ToggleUnionView, "union"),
        (Action::ToggleWatch, "watch"),
        (Action::ShowLog, "log"),
        (Action::Hide, "hide"),
    ]
    .into_iter()
    .filter(|(action, _)| *action != Action::ToggleWatch || app.watch_available())
    .filter(|(action, _)| *action != Action::Hide || !app.config.zide.hide_command.is_empty())
    .collect();

    // **Always last, whichever way round the other two go.** These are the
    // rarest keys on the screen and the bar drops from the tail, so putting
    // them anywhere else costs a sync hint on a narrow terminal -- which is
    // what happened when they first went in beside the navigation keys:
    // `ss profile` fell off a 200-column bar. A sync tool may not push its
    // sync keys off the footer to advertise `mkdir`.
    let files: Vec<(Action, &'static str)> = [
        (Action::EditFile, "edit"),
        (Action::Rename, "rename"),
        (Action::NewFile, "new file"),
        (Action::NewDir, "new dir"),
        (Action::DeleteFile, "delete"),
        (Action::Chmod, "permissions"),
    ]
    .into_iter()
    // Permission bits are a Unix idea the backend may simply not have, and
    // `p` on an FTP pane can only report that. Same rule as the profile keys.
    .filter(|(action, _)| *action != Action::Chmod || app.focused().vfs.capabilities().mode)
    .collect();

    // `?` first but never dropped: it is the way out of not knowing, so it is
    // prepended to whichever group leads rather than left to the tail.
    let help = [(Action::Help, "help")];

    if connected(app) {
        help.into_iter()
            .chain(sync)
            .chain(browse)
            .chain(files)
            .collect()
    } else {
        help.into_iter()
            .chain(browse)
            .chain(sync)
            .chain(files)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use crate::{app::App, config::Config};

    fn bar_for(width: u16) -> String {
        render(&app_with(&[]), width)
    }

    /// An app with a chord part-way typed, as the event loop leaves it.
    fn app_with(pending: &[char]) -> App {
        use crate::keys::{Key, ModifiedKey};

        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().to_path_buf();
        let mut app = App::new(Config::default(), here.clone(), here).unwrap();
        app.pending_keys = pending
            .iter()
            .map(|c| ModifiedKey::plain(Key::Char(*c)))
            .collect();
        app
    }

    /// An app built from config text, for the cases where the bar's contents
    /// depend on what was configured rather than only on what is bound.
    fn app_from(config: &str) -> App {
        let parsed: Config = toml::from_str(config).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().to_path_buf();
        App::new(parsed, here.clone(), here).unwrap()
    }

    fn render(app: &App, width: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        terminal
            .draw(|frame| super::draw(frame, app, frame.area()))
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

    /// `Hide` needs a key *and* a command behind it, and the bar offers it
    /// only when it has both. Either half missing is a hint that cannot be
    /// taken up, which is the thing this bar exists to avoid.
    #[test]
    fn hide_is_offered_only_when_it_would_work() {
        let configured = r#"
[zide]
hide_command = ["true"]
[keys.browser]
set = [["Hide", "esc"]]
"#;

        assert!(
            render(&app_from(configured), 200).contains("esc hide"),
            "bound and configured, so it is on the bar"
        );

        // Bound, but with nothing to hide with: pressing it could only
        // produce the message saying so.
        let unconfigured = r#"
[keys.browser]
set = [["Hide", "esc"]]
"#;
        assert!(!render(&app_from(unconfigured), 200).contains("hide"));

        // Configured, but with no key -- `hint` drops it for us.
        let keyless = r#"
[zide]
hide_command = ["true"]
"#;
        assert!(!render(&app_from(keyless), 200).contains("hide"));
    }

    /// The reported problem: `s` is bound to nothing on its own, so pressing
    /// it and looking at the screen told you nothing at all -- not that the
    /// key had landed, and not what the second key could be.
    #[test]
    fn a_pending_chord_turns_the_bar_into_its_completions() {
        let bar = render(&app_with(&['s']), 200);

        assert!(bar.contains("sp push"), "{bar}");
        assert!(bar.contains("sl pull"), "{bar}");
        assert!(bar.contains("sm mirror"), "{bar}");
        assert!(
            !bar.contains("? help"),
            "the bar answers the question you now have, not the old one: {bar}"
        );
    }

    /// Being shown a chord in progress is only half of it; the other half is
    /// knowing how to abandon one you did not mean to start.
    #[test]
    fn the_completions_name_the_way_back_out() {
        let bar = render(&app_with(&['s']), 200);
        assert!(bar.contains("esc back"), "{bar}");
    }

    /// And it names *your* cancel key. Giving Esc up to a multiplexer is a
    /// documented thing to do, and this is the bar that would otherwise send
    /// you pressing a key that does nothing.
    #[test]
    fn the_way_out_is_named_from_the_keymap() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().to_path_buf();
        let config: Config = toml::from_str(
            r#"
[keys.browser]
unbind = ["esc"]
"#,
        )
        .unwrap();

        let mut app = App::new(config, here.clone(), here).unwrap();
        app.pending_keys = vec![crate::keys::ModifiedKey::plain(crate::keys::Key::Char('s'))];

        let bar = render(&app, 200);
        assert!(bar.contains("ctrl+c back"), "{bar}");
        assert!(!bar.contains("esc back"), "esc is unbound here: {bar}");
    }

    /// Only the chord being typed. `g` and `s` are both prefixes; offering
    /// both would be the help overlay with extra steps.
    #[test]
    fn the_completions_are_only_the_ones_that_continue_this_chord() {
        let bar = render(&app_with(&['g']), 200);

        assert!(bar.contains("gg to top"), "{bar}");
        assert!(!bar.contains("push"), "`sp` does not continue `g`: {bar}");
    }

    /// With no chord in progress nothing changes, which is almost always.
    #[test]
    fn an_idle_bar_is_the_normal_bar() {
        assert_eq!(render(&app_with(&[]), 200), bar_for(200));
    }

    /// `ss` and `sw` need a profile, so without one they are not offered.
    /// Advertising a key whose only outcome is an error message is how
    /// somebody concludes the feature is broken rather than unconfigured.
    #[test]
    fn the_profile_keys_are_only_offered_when_there_is_a_profile() {
        let bar = bar_for(200);
        assert!(!bar.contains("profile"), "no profile is set: {bar}");
        assert!(!bar.contains("watch"), "no profile is set: {bar}");

        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().to_path_buf();
        let config: Config = toml::from_str(
            r#"
[profiles.deploy]
mode = "push"
watch = "batch"
"#,
        )
        .unwrap();

        let mut app = App::new(config, here.clone(), here).unwrap();
        app.active_profile = Some("deploy".into());

        let bar = render(&app, 200);
        assert!(bar.contains("ss profile"), "{bar}");
        assert!(bar.contains("sw watch"), "{bar}");
    }

    /// A profile that does not ask to be watched does not get a watch hint
    /// either, since `sw` would only report that it is off.
    #[test]
    fn watch_is_left_out_when_the_profile_does_not_want_it() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().to_path_buf();
        let config: Config = toml::from_str(
            r#"
[profiles.deploy]
mode = "push"
"#,
        )
        .unwrap();

        let mut app = App::new(config, here.clone(), here).unwrap();
        app.active_profile = Some("deploy".into());

        let bar = render(&app, 200);
        assert!(bar.contains("ss profile"), "{bar}");
        assert!(!bar.contains("watch"), "watch is off for this one: {bar}");
    }
}
