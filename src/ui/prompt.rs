//! The password popup, in the shape lazygit asks for one.
//!
//! A secret used to be collected on the status line, in the same row and the
//! same colour as the mode badge and the cursor counter. It is the one moment
//! lazysync stops and waits for the user, and it looked like the furniture --
//! reported as "the password prompt isn't very noticeable", which it was not.
//!
//! So a secret gets a window: centred, bordered, titled with what is being
//! asked for, with the typed characters masked. That is lazygit's credentials
//! prompt, which is a centred popup `content + 2` rows tall, titled
//! `Password`, with the input masked by `*`.
//!
//! **Only secrets.** Search and the bookmark filter stay on the status line,
//! and the reason is in [`super::draw_prompt_line`]: a box in the middle of
//! the screen would cover the listing you are searching. A password has no
//! such tie to what is behind it, and wants the opposite treatment -- it
//! should stop you.
//!
//! Two deliberate departures from lazygit:
//!
//!   - **The keys are named in the window.** lazygit puts them in its global
//!     bottom bar, which lazysync does not have; and the keys come from the
//!     live [`Mode::Prompt`] keymap, since nothing in this UI is allowed to
//!     name a key the user may have rebound.
//!   - **The connection is named.** lazygit asks on behalf of one repository
//!     and does not need to say which. lazysync has a list of them, the open
//!     screen is just a name, and "which host is this password going to" is a
//!     question worth answering before somebody types a secret into it.

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
    Frame,
};

use super::hint;
use crate::{
    app::{App, Prompt},
    keys::{Action, Mode},
};

/// What a typed character is drawn as.
///
/// `*` is lazygit's, by way of gocui's view mask. It is also the one that
/// survives a terminal with no Unicode, which a bullet does not -- and this
/// is a window people see over ssh, in whatever `TERM` the far end offers.
const MASK: char = '*';

/// A block stands in for the cursor, as it does on the status-line prompt.
/// The real cursor stays hidden so it cannot be parked somewhere misleading.
const CURSOR: char = '\u{2588}';

/// The narrowest the window is drawn, however short the connection's name is.
///
/// Noticeability is the whole point: a box sized to `●●●●` is a box nobody
/// notices. Terminals narrower than this still shrink it -- [`super::centered`]
/// clamps -- so this is a floor on intent, not on the frame.
const MIN_CONTENT: usize = 44;

/// Left and right padding inside the border, plus the border itself.
const CHROME: usize = 6;

pub fn draw(frame: &mut Frame<'_>, app: &App, prompt: &Prompt) {
    let palette = &app.palette;
    let keys = hint::line(
        app.keymaps.get(Mode::Prompt),
        &[(Action::Submit, "accept"), (Action::Cancel, "cancel")],
    );

    let subject = prompt.kind.subject().unwrap_or_default();
    // The secret's *length* is all this needs, and all it is given. Nothing
    // below copies `prompt.input`, so the plaintext never gets a second
    // lifetime in a render buffer that outlives the keystroke.
    let typed = prompt.input.chars().count();

    let content = MIN_CONTENT
        .max(subject.chars().count())
        .max(keys.chars().count())
        .max(typed + 1);

    let height = if subject.is_empty() { 6 } else { 7 };
    let area = super::centered(frame.area(), (content + CHROME) as u16, height);

    // Measured against the width the window actually got, which a narrow
    // terminal or a long password can make smaller than the one asked for.
    // The tail is kept rather than the head: the cursor is the part that has
    // to stay on screen, and one `*` is indistinguishable from another.
    let room = (area.width as usize).saturating_sub(CHROME);
    let shown = typed.min(room.saturating_sub(1));
    let masked = format!("{}{CURSOR}", MASK.to_string().repeat(shown));

    let mut lines = vec![Line::from("")];
    if !subject.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  {subject}"),
            Style::default().fg(palette.dim),
        )));
    }
    lines.push(Line::from(Span::styled(
        format!("  {masked}"),
        Style::default().fg(palette.fg),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        keys,
        Style::default().fg(palette.dim),
    )));

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                // `accent`, not `warn`. The yes/no dialog is a warning because
                // it guards something destructive; this is a request for
                // input, and colouring it like a hazard would teach people to
                // read the hazard colour as "lazysync wants something".
                .border_style(Style::default().fg(palette.accent))
                .title_top(Line::from(Span::styled(
                    format!(" {} ", prompt.kind.title()),
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD),
                ))),
        ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use crate::{
        app::{App, Overlay, Prompt, PromptKind},
        config::Config,
    };

    const WIDTH: u16 = 90;
    const HEIGHT: u16 = 20;

    fn app_with(config: Config, prompt: Prompt) -> App {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().to_path_buf();
        let mut app = App::new(config, here.clone(), here).unwrap();
        app.overlay = Overlay::Prompt(prompt);
        app
    }

    fn password(input: &str) -> Prompt {
        Prompt {
            kind: PromptKind::Password {
                bookmark: "sandbox subdomain".into(),
            },
            input: input.into(),
        }
    }

    /// The whole frame, so what is asserted is what a user sees.
    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();

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

    /// The report: it was a line at the bottom of the screen. It is now a
    /// window in the middle of one.
    #[test]
    fn a_password_is_asked_for_in_a_window_not_on_the_status_line() {
        let mut app = app_with(Config::default(), password(""));
        let drawn = screen(&mut app);

        let row = drawn
            .lines()
            .position(|line| line.contains("password"))
            .expect("the prompt is on screen");

        assert!(
            row < (HEIGHT as usize) - 3,
            "the prompt should be in the middle of the frame, not on the last row: {drawn}"
        );
        assert!(
            drawn.contains('\u{250c}') && drawn.contains('\u{2518}'),
            "it should be a bordered box: {drawn}"
        );
    }

    /// lazygit's mask, and the count has to match what was typed or the
    /// feedback is a lie about how many keystrokes landed.
    #[test]
    fn what_is_typed_is_masked_one_star_per_character() {
        let mut app = app_with(Config::default(), password("hunter2"));
        let drawn = screen(&mut app);

        assert!(drawn.contains("*******\u{2588}"), "{drawn}");
        assert!(
            !drawn.contains("hunter2"),
            "the password must never reach the screen: {drawn}"
        );
    }

    /// An empty prompt shows the cursor and nothing else, so it is obvious
    /// that it is waiting rather than that it has already been filled in.
    #[test]
    fn an_empty_password_draws_no_stars() {
        let mut app = app_with(Config::default(), password(""));
        let drawn = screen(&mut app);

        assert!(drawn.contains('\u{2588}'), "the cursor is there: {drawn}");
        assert!(!drawn.contains('*'), "nothing typed yet: {drawn}");
    }

    /// Which connection is about to receive the secret. The open screen is
    /// just a name, so this is the only thing that says where it is going.
    #[test]
    fn the_window_names_the_connection_it_is_asking_for() {
        let mut app = app_with(Config::default(), password(""));
        assert!(screen(&mut app).contains("sandbox subdomain"));
    }

    /// Hardcoding `enter accept` would be the bug `ui::hint` exists to stop.
    #[test]
    fn the_keys_are_named_from_the_keymap() {
        let config: Config = toml::from_str(
            r#"
[keys.prompt]
set = [["Submit", "ctrl+s"]]
"#,
        )
        .unwrap();

        let mut app = app_with(config, password(""));
        let drawn = screen(&mut app);

        assert!(drawn.contains("ctrl+s accept"), "{drawn}");
        assert!(
            !drawn.contains("enter accept"),
            "enter is not bound to Submit here any more: {drawn}"
        );
    }

    /// A password longer than the window is clamped rather than drawn through
    /// the border and across the panes behind it.
    #[test]
    fn a_long_password_does_not_overflow_the_window() {
        let mut app = app_with(Config::default(), password(&"x".repeat(400)));
        let drawn = screen(&mut app);

        for line in drawn.lines() {
            assert_eq!(
                line.chars().count(),
                WIDTH as usize,
                "a row grew past the frame: {line:?}"
            );
        }
        // The cursor stays visible: it is the end of the line that matters.
        assert!(drawn.contains('\u{2588}'), "{drawn}");
    }

    /// And the same in a terminal too narrow for the window's preferred size,
    /// which is the case `centered` clamps and this has to survive.
    #[test]
    fn a_narrow_terminal_shrinks_the_window_rather_than_drawing_outside_it() {
        let mut app = app_with(Config::default(), password("hunter2"));

        let mut terminal = Terminal::new(TestBackend::new(24, 10)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        for y in 0..buffer.area.height {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            assert_eq!(row.chars().count(), 24, "{row:?}");
        }
    }

    /// Search is the other prompt and must not have moved: a box in the
    /// middle of the screen would cover the listing being searched.
    #[test]
    fn search_still_happens_on_the_status_line() {
        let mut app = app_with(
            Config::default(),
            Prompt {
                kind: PromptKind::Search,
                input: "arm".into(),
            },
        );
        let drawn = screen(&mut app);
        let lines: Vec<&str> = drawn.lines().collect();

        assert!(
            lines[lines.len() - 1].contains("/arm"),
            "the search prompt belongs on the last row: {drawn}"
        );
        assert!(
            !drawn.contains("accept"),
            "and it is not the window: {drawn}"
        );
    }
}
