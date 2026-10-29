//! Rendering the connection form.
//!
//! Kept separate from [`super::form`], which holds the form's state and
//! validation: that part is pure and heavily tested, and mixing ratatui into
//! it would make it testable only through a terminal.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
    Frame,
};

use super::{
    form::{ConnectionForm, Value},
    hint, HitMap, Target,
};
use crate::{
    app::App,
    config::Palette,
    keys::{Action, Keymap, Mode},
};

pub fn draw(
    frame: &mut Frame<'_>,
    app: &App,
    form: &ConnectionForm,
    area: Rect,
    hits: &mut HitMap,
) {
    let palette = &app.palette;

    let [body, footer] = Layout::vertical([Constraint::Min(5), Constraint::Length(4)]).areas(area);

    let title = match &form.editing {
        Some(name) => format!(" edit {name} "),
        None => " new connection ".to_string(),
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

    let lines: Vec<Line<'_>> = form
        .fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let focused = index == form.cursor;
            let relevant = form.is_relevant(field.id);
            row(field, focused, relevant, palette)
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);

    // One field per line, in order, from the top of the box: the form does
    // not scroll, so there is no offset to account for. Whether a click on a
    // dimmed field does anything is `App`'s decision, not the renderer's.
    hits.rows(inner, 0, form.fields.len(), Target::Field);

    draw_footer(frame, form, palette, app.keymaps.get(Mode::Form), footer);
}

fn row<'a>(
    field: &super::form::Field,
    focused: bool,
    relevant: bool,
    palette: &Palette,
) -> Line<'a> {
    let label_style = if focused {
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else if relevant {
        Style::default().fg(palette.fg)
    } else {
        // Irrelevant for the chosen auth method: dimmed rather than removed,
        // so the rows below it do not jump as the choice changes.
        Style::default().fg(palette.ghost)
    };

    let text = value_text(field);
    let is_empty = text.is_empty();

    let value_style = if !relevant {
        Style::default().fg(palette.ghost)
    } else if is_empty {
        Style::default()
            .fg(palette.ghost)
            .add_modifier(Modifier::ITALIC)
    } else {
        Style::default().fg(palette.marked)
    };

    let shown = if is_empty {
        field.placeholder.to_string()
    } else {
        text
    };

    // A choice field advertises that the arrow keys change it, since nothing
    // else on the row suggests it and `h`/`l` are literal text here.
    let affordance = match &field.value {
        Value::Choice { .. } | Value::Flag(_) if focused => "  \u{2190}\u{2192}",
        _ => "",
    };

    let mut spans = vec![
        Span::styled(if focused { "  > " } else { "    " }, label_style),
        Span::styled(format!("{:<18}", field.label), label_style),
        Span::styled(shown, value_style),
    ];

    // The text cursor, only on an editable field: showing one on a choice
    // would imply you could type into it.
    if focused && matches!(field.value, Value::Text(_)) {
        spans.push(Span::styled(
            "\u{2588}",
            Style::default().fg(palette.accent),
        ));
    }

    spans.push(Span::styled(
        affordance.to_string(),
        Style::default().fg(palette.dim),
    ));

    Line::from(spans)
}

fn value_text(field: &super::form::Field) -> String {
    match &field.value {
        Value::Text(text) => text.clone(),
        Value::Choice { options, selected } => options.get(*selected).cloned().unwrap_or_default(),
        Value::Flag(true) => "yes".into(),
        Value::Flag(false) => "no".into(),
    }
}

fn draw_footer(
    frame: &mut Frame<'_>,
    form: &ConnectionForm,
    palette: &Palette,
    keymap: &Keymap,
    area: Rect,
) {
    let mut lines = Vec::new();

    // The hint for the focused field, which is where the explanation of each
    // option lives rather than in a manual.
    lines.push(Line::from(Span::styled(
        format!("  {}", form.focused().hint),
        Style::default().fg(palette.dim),
    )));

    if let Some(error) = &form.error {
        lines.push(Line::from(Span::styled(
            format!("  {error}"),
            Style::default()
                .fg(palette.danger)
                .add_modifier(Modifier::BOLD),
        )));
    }

    let hints = hint::line(
        keymap,
        &[
            (Action::MoveDown, "next field"),
            (Action::NextChoice, "change choice"),
            (Action::Submit, "save"),
            (Action::Cancel, "discard"),
        ],
    );
    lines.push(Line::from(Span::styled(
        format!(" {hints}"),
        Style::default().fg(palette.dim),
    )));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

#[cfg(test)]
mod tests {
    use super::{super::form::FieldId, *};

    /// Pins the exact field list.
    ///
    /// The point is not the names: it is that adding a field to this form has
    /// to be a deliberate act, because a password field here would be written
    /// to `bookmarks.toml` and rendered on screen. Secrets belong in the
    /// keyring or in a one-shot prompt, and this test is what makes someone
    /// stop and notice if that ever changes.
    #[test]
    fn the_form_holds_exactly_these_fields_and_no_secret() {
        let ids: Vec<FieldId> = ConnectionForm::new().fields.iter().map(|f| f.id).collect();

        assert_eq!(
            ids,
            vec![
                FieldId::Name,
                FieldId::Protocol,
                FieldId::Host,
                FieldId::Port,
                FieldId::Username,
                FieldId::Auth,
                FieldId::IdentityFile,
                FieldId::RemotePath,
                FieldId::LocalPath,
                FieldId::SyncProfile,
                FieldId::SyncBrowse,
                FieldId::RememberPassword,
            ],
            "the connection form's fields changed -- if a secret was added, it must not be \
             here: passwords are prompted for and optionally kept in the keyring"
        );
    }

    /// Every row has to render without panicking at a realistic width,
    /// including the dimmed-irrelevant and empty-placeholder paths.
    #[test]
    fn every_row_renders() {
        let palette = crate::config::Theme::default().palette().unwrap();
        let form = ConnectionForm::new();

        for (index, field) in form.fields.iter().enumerate() {
            let line = row(field, index == 0, form.is_relevant(field.id), &palette);
            assert!(!line.spans.is_empty(), "{:?} rendered nothing", field.id);
        }
    }
}
