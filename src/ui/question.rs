//! The yes/no dialog.
//!
//! Two buttons rather than two letters. The footer used to read `y yes  n no`
//! as a string literal, which is the bug [`super::hint`] exists to prevent:
//! on a Colemak keymap `n` is a motion, so the dialog named a key that did
//! nothing and the only way out was Esc.
//!
//! So the answers are a *selection*. The arrows move between the buttons,
//! Enter takes the selected one, and the letters still answer directly for
//! anybody who knows the question -- each named from the live
//! [`Mode::Question`] keymap, so a rebinding shows up here too.
//!
//! **The selection starts on "no".** Every question lazysync asks guards
//! something it cannot take back, and the keystroke people press without
//! reading is Enter.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
    Frame,
};

use super::{hint, HitMap, Target};
use crate::{
    app::{Answer, App, Question},
    keys::{Action, Mode},
};

/// The width of the button row, which the window never draws narrower than.
const BUTTON_ROW: usize = 2 * BUTTON_WIDTH + GAP;
const BUTTON_WIDTH: usize = 7;
const GAP: usize = 2;

/// Which line of the window the buttons are on, counting from inside the
/// border: a blank, the question, a blank, and then them.
const BUTTON_LINE: u16 = 3;

pub fn draw(frame: &mut Frame<'_>, app: &App, question: &Question, hits: &mut HitMap) {
    let keymap = app.keymaps.get(Mode::Question);

    // `move` rather than `select`: there is nothing to select that is not
    // already in front of you, and what the key does is move between them.
    let keys = hint::line(
        keymap,
        &[
            (Action::PrevChoice, "move"),
            (Action::Submit, "choose"),
            (Action::Apply, "yes"),
            (Action::Cancel, "no"),
        ],
    );

    // Wide enough for whichever line is longest, so the keys are never the
    // thing that gets cut off.
    let content = question
        .text
        .chars()
        .count()
        .max(keys.chars().count())
        .max(BUTTON_ROW);
    let width = content as u16 + 6;
    let area = super::centered(frame.area(), width, 7);

    // Centred under the question rather than tucked against the left border,
    // because the two buttons are what the window is asking you to look at.
    let inner = area.width.saturating_sub(2) as usize;
    let pad = inner.saturating_sub(BUTTON_ROW) / 2;
    let buttons = Line::from(vec![
        Span::raw(" ".repeat(pad)),
        button(app, "yes", question.answer == Answer::Yes),
        Span::raw(" ".repeat(GAP)),
        button(app, "no", question.answer == Answer::No),
    ]);

    // The two buttons, in the same arithmetic that drew them. They are the
    // one place in lazysync that looks like something to click, so they had
    // better be.
    let left = area.x + 1 + pad as u16;
    for (offset, answer) in [
        (0, Answer::Yes),
        (BUTTON_WIDTH as u16 + GAP as u16, Answer::No),
    ] {
        hits.push(
            Rect {
                x: left + offset,
                y: area.y + 1 + BUTTON_LINE,
                width: BUTTON_WIDTH as u16,
                height: 1,
            },
            Target::Answer(answer),
        );
    }

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {}  ", question.text),
                Style::default().fg(app.palette.fg),
            )),
            Line::from(""),
            buttons,
            Line::from(Span::styled(keys, Style::default().fg(app.palette.dim))),
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

/// One button, bracketed so the selection survives a terminal that drops
/// colour -- which is also how it reads over ssh in a 16-colour TERM.
///
/// Every spelling is [`BUTTON_WIDTH`] wide, selected or not, so moving
/// between the two does not make the row jump.
fn button<'a>(app: &App, label: &str, selected: bool) -> Span<'a> {
    if selected {
        Span::styled(
            format!("[ {label:^3} ]"),
            Style::default()
                .fg(app.palette.warn)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )
    } else {
        Span::styled(
            format!("  {label:^3}  "),
            Style::default().fg(app.palette.dim),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ratatui::{backend::TestBackend, Terminal};

    use crate::{
        app::{Answer, App, Overlay, Pending, Question},
        config::Config,
        keys::{Action, KeyCombo, Keymap, KeymapSection, Mode},
    };

    /// Render the whole frame, so what is asserted is what a user sees.
    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(90, 12)).unwrap();
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

    fn app_with(config: Config) -> App {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().to_path_buf();
        let mut app = App::new(config, here.clone(), here).unwrap();
        app.overlay = Overlay::Question(Question::new(
            "Close the connection?",
            Pending::LeaveConnection,
            Pending::Nothing,
        ));
        app
    }

    #[test]
    fn the_dialog_opens_on_no() {
        let mut app = app_with(Config::default());
        let drawn = screen(&mut app);

        assert!(drawn.contains("[ no"), "{drawn}");
        assert!(!drawn.contains("[ yes"), "{drawn}");
    }

    /// Enter is the keystroke people press without reading, so it must take
    /// the selection -- which is "no" until they move.
    #[test]
    fn enter_declines_until_the_selection_moves() {
        let mut app = app_with(Config::default());

        app.handle(Action::Submit);
        assert!(
            matches!(app.overlay, Overlay::None),
            "the dialog should have closed"
        );
        assert!(
            app.panes[1].vfs.scheme() == "local",
            "nothing was connected, so nothing should have changed"
        );

        let mut app = app_with(Config::default());
        app.handle(Action::NextChoice);
        let drawn = screen(&mut app);
        assert!(drawn.contains("[ yes"), "{drawn}");
    }

    /// The bug this module was rewritten for: a keymap that moves `n` onto a
    /// motion must not leave the dialog advertising `n no`.
    #[test]
    fn the_footer_names_the_keys_the_user_actually_has() {
        let config: Config = toml::from_str(
            r#"
[keys.question]
set = [
    ["Cancel", "esc"],
    ["Apply", "u"],
]
"#,
        )
        .unwrap();

        let mut app = app_with(config);
        let drawn = screen(&mut app);

        assert!(drawn.contains("u yes"), "the rebound yes key: {drawn}");
        assert!(
            !drawn.contains("n no"),
            "`n` is not bound to anything here any more: {drawn}"
        );
        assert!(drawn.contains("esc no"), "{drawn}");
    }

    /// An action nobody bound is not offered, rather than offered with no
    /// key. The *button* stays -- it is what the selection points at -- but
    /// the footer names only keys that work.
    #[test]
    fn an_unbound_answer_is_left_out_of_the_footer() {
        let sections = BTreeMap::from([(
            Mode::Question,
            KeymapSection {
                base: Some(Keymap(vec![(
                    Action::Cancel,
                    KeyCombo::chord("esc".split('+').map(|k| k.parse().unwrap())),
                )])),
                ..Default::default()
            },
        )]);

        let mut app = app_with(Config {
            keys: sections,
            ..Default::default()
        });
        let drawn = screen(&mut app);

        let footer = drawn
            .lines()
            .find(|line| line.contains("esc no"))
            .unwrap_or_else(|| panic!("no footer in {drawn}"));
        assert!(
            !footer.contains("yes"),
            "nothing is bound to yes, so the footer must not offer it: {footer:?}"
        );
    }

    /// Both buttons are drawn whichever one is selected, so the dialog never
    /// looks like it has one answer.
    #[test]
    fn both_answers_are_always_on_screen() {
        let mut app = app_with(Config::default());
        for _ in 0..2 {
            let drawn = screen(&mut app);
            assert!(drawn.contains("yes"), "{drawn}");
            assert!(drawn.contains("no"), "{drawn}");
            app.handle(Action::NextChoice);
        }
    }

    #[test]
    fn selecting_yes_and_submitting_acts() {
        let mut app = app_with(Config::default());
        app.handle(Action::PrevChoice);

        if let Overlay::Question(question) = &app.overlay {
            assert_eq!(question.answer, Answer::Yes, "one move flips the selection");
        } else {
            panic!("the dialog closed on a move");
        }

        app.handle(Action::Submit);
        assert!(matches!(app.overlay, Overlay::None));
    }
}
