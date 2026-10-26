//! The confirmation window.
//!
//! The screen this project exists for. Every row shows **both sides' size and
//! timestamp**, so "overwrite" is never an abstract word: you can see the 8.1
//! KB from this morning that is about to replace the 7.9 KB from Monday, and
//! the delta between them.
//!
//! Three rendering choices carry most of the weight:
//!
//! 1. **A newer destination is red.** Size and date in two columns is
//!    information; `DESTINATION IS NEWER` in red is a warning. Overwriting
//!    newer data with older is the one outcome of a sync that destroys work,
//!    and it must not look like the other rows.
//! 2. **Deletions sort above the skipped list.** A fold of two hundred
//!    identical files must never sit between you and the destructive part of
//!    a plan.
//! 3. **The totals recompute live.** Excluding a row with `space` updates the
//!    footer immediately, because a plan you can edit is one you will actually
//!    read.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, Paragraph, Row as TableRow, Table, TableState, Wrap},
    Frame,
};

use super::{format_size, hint, shorten_path};
use crate::{
    app::App,
    config::Palette,
    // `Action` is already the plan's action in this file, so the keymap's is
    // aliased rather than shadowing it.
    keys::{Action as KeyAction, Mode},
    sync::{
        plan::{Action, Plan, Resolution, Row, Section, Side},
        safety::DeletePolicy,
    },
};

/// A line of the window: a heading, or a row of the plan.
///
/// Flattened into one list so that one cursor can move over headings and rows
/// alike, and the table widget can scroll it.
pub enum Entry<'a> {
    Heading {
        section: Section,
        count: usize,
        folded: bool,
    },
    Row {
        index: usize,
        row: &'a Row,
    },
}

/// Build the display list from a plan and the set of folded sections.
pub fn entries<'a>(plan: &'a Plan, folded: &[Section]) -> Vec<Entry<'a>> {
    let mut out = Vec::new();

    for section in Section::ALL {
        let count = plan.count(section);

        // An empty section is still shown when it is one of the ones that
        // matter, so a missing "Delete" heading can never be mistaken for a
        // check that was not performed.
        let always = matches!(
            section,
            Section::Create | Section::Overwrite | Section::Delete
        );
        if count == 0 && !always {
            continue;
        }

        let is_folded = folded.contains(&section);
        out.push(Entry::Heading {
            section,
            count,
            folded: is_folded,
        });

        if is_folded {
            continue;
        }

        for (index, row) in plan.rows.iter().enumerate() {
            // Directory creations are not listed: nothing to decide, and a
            // list padded with mkdir rows is one nobody reads. The footer
            // says how many there are. Skipped-identical rows *are* listed --
            // that section exists to show them.
            if row.action.section() == section && row.action != Action::CreateDir {
                out.push(Entry::Row { index, row });
            }
        }
    }

    out
}

/// Which plan row the cursor is on, if it is on a row at all.
pub fn row_at(entries: &[Entry<'_>], cursor: usize) -> Option<usize> {
    match entries.get(cursor) {
        Some(Entry::Row { index, .. }) => Some(*index),
        _ => None,
    }
}

/// The section the cursor is in, for folding.
pub fn section_at(entries: &[Entry<'_>], cursor: usize) -> Option<Section> {
    match entries.get(cursor) {
        Some(Entry::Heading { section, .. }) => Some(*section),
        Some(Entry::Row { row, .. }) => Some(row.action.section()),
        None => None,
    }
}

pub fn draw(
    frame: &mut Frame<'_>,
    app: &App,
    plan: &Plan,
    cursor: usize,
    folded: &[Section],
    area: Rect,
) {
    let palette = &app.palette;
    let policy = DeletePolicy::from_config(&app.config.safety);

    let [header, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(5),
        Constraint::Length(4),
    ])
    .areas(area);

    draw_header(frame, plan, &policy, palette, header);

    let block = Block::bordered().border_style(Style::default().fg(
        // A plan containing deletions gets a warning-coloured frame, so the
        // destructive case is distinguishable before you read a word of it.
        if plan.totals().deletes > 0 || plan.totals().clobbers_newer > 0 {
            palette.warn
        } else {
            palette.border_focused
        },
    ));
    let inner = block.inner(body);
    frame.render_widget(block, body);

    let list = entries(plan, folded);
    let rows: Vec<TableRow<'_>> = list
        .iter()
        .map(|entry| match entry {
            Entry::Heading {
                section,
                count,
                folded,
            } => heading_row(*section, *count, *folded, palette),
            Entry::Row { row, .. } => plan_row(row, palette, inner.width),
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(3), // marker + glyph
            Constraint::Min(20),   // path
            // 26 = 8 for the size, two spaces, 16 for "YYYY-MM-DD HH:MM".
            // Anything narrower truncates the time to "2026-10-24 1", which
            // defeats the point of showing both sides' timestamps at all.
            Constraint::Length(26), // source size + time
            Constraint::Length(26), // destination size + time
            Constraint::Length(24), // delta / reason
        ],
    )
    .row_highlight_style(Style::default().fg(palette.cursor_fg).bg(palette.cursor_bg));

    let mut state =
        TableState::default().with_selected(Some(cursor.min(list.len().saturating_sub(1))));
    frame.render_stateful_widget(table, inner, &mut state);

    draw_footer(frame, app, plan, palette, footer);
}

fn draw_header(
    frame: &mut Frame<'_>,
    plan: &Plan,
    policy: &DeletePolicy,
    palette: &Palette,
    area: Rect,
) {
    let width = (area.width as usize).saturating_sub(30) / 2;

    let lines = vec![
        Line::from(vec![
            Span::styled(
                " Sync  ",
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                shorten_path(&plan.source_root.display().to_string(), width),
                Style::default().fg(palette.fg),
            ),
            // A two-way arrow for a bisync. "local -> remote" would be a
            // false description of a plan whose rows go both ways, and the
            // header is the first thing read.
            Span::styled(
                if plan.bidirectional {
                    "  \u{21c4}  "
                } else {
                    "  \u{2192}  "
                },
                Style::default().fg(palette.accent),
            ),
            Span::styled(
                shorten_path(&plan.dest_root.display().to_string(), width),
                Style::default().fg(palette.fg),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format!(
                    " compare: {}  tolerance: {}s  ",
                    plan.method.describe(),
                    plan.tolerance_secs
                ),
                Style::default().fg(palette.dim),
            ),
            // Said out loud, because agreeing to "delete" when it means
            // "moved aside" and when it means "gone" are different decisions.
            Span::styled(
                policy.describe(),
                Style::default().fg(if matches!(policy, DeletePolicy::Remove) {
                    palette.warn
                } else {
                    palette.dim
                }),
            ),
        ]),
    ];

    frame.render_widget(Paragraph::new(lines), area);
}

fn heading_row<'a>(
    section: Section,
    count: usize,
    folded: bool,
    palette: &Palette,
) -> TableRow<'a> {
    let color = match section {
        // Conflicts are the same red as deletions, because that is what they
        // can cost: a decision made carelessly here discards one side's work.
        Section::Delete | Section::Conflict => palette.danger,
        Section::Overwrite => palette.warn,
        Section::Create => palette.ok,
        Section::Skipped => palette.dim,
    };

    let marker = if folded { "\u{25b8}" } else { "\u{25be}" };
    let text = format!("{marker} {} ({count})", section.title());

    TableRow::new(vec![
        Cell::from(""),
        Cell::from(Line::from(Span::styled(
            text,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))),
        Cell::from(""),
        Cell::from(""),
        Cell::from(Line::from(Span::styled(
            if folded { "zo to open" } else { "" },
            Style::default().fg(palette.dim),
        ))),
    ])
}

fn plan_row<'a>(row: &Row, palette: &Palette, width: u16) -> TableRow<'a> {
    // Red for the one case that destroys work, whatever else the row says.
    let danger = row.clobbers_newer();

    let glyph_color = match row.action {
        Action::Delete => palette.danger,
        Action::Overwrite if danger => palette.danger,
        Action::Overwrite => palette.warn,
        Action::Create | Action::CreateDir => palette.ok,
        Action::Skip => palette.dim,
        // An undecided conflict is the thing stopping the plan, so it stays
        // red until it is settled; once settled it is an ordinary overwrite.
        Action::Conflict if row.is_undecided() => palette.danger,
        Action::Conflict => palette.warn,
    };

    // Direction is in the glyph: an arrow down means the thing replacing is
    // the older of the two.
    let glyph = if danger {
        "\u{2193}"
    } else if matches!(row.action, Action::Conflict) {
        // Once resolved, show which way it will move -- the arrow is the only
        // thing on the row that distinguishes keep-local from keep-remote at a
        // glance.
        match row.resolution {
            Some(Resolution::KeepLocal) => "\u{2192}",
            Some(Resolution::KeepRemote) => "\u{2190}",
            Some(Resolution::KeepBoth) => "\u{2194}",
            Some(Resolution::Skip) => "\u{00b7}",
            _ => row.action.glyph(),
        }
    } else {
        row.action.glyph()
    };

    let marker = if row.included { " " } else { "\u{2500}" };

    // The fixed columns plus ratatui's one-column gap between each of the
    // five: 3 + 26 + 26 + 24 + 4.
    let path_width = (width as usize).saturating_sub(83).max(12);
    let path_style = if !row.included {
        Style::default()
            .fg(palette.ghost)
            .add_modifier(Modifier::CROSSED_OUT)
    } else if danger {
        Style::default()
            .fg(palette.danger)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.fg)
    };

    let mut cells = vec![
        Cell::from(Line::from(vec![
            Span::styled(marker, Style::default().fg(palette.dim)),
            Span::styled(glyph, Style::default().fg(glyph_color)),
        ])),
        Cell::from(Line::from(Span::styled(
            shorten_path(&row.path.display().to_string(), path_width),
            path_style,
        ))),
        // By *pane*, not by role. The columns are "local" and "remote", and a
        // bisync plan has rows pointing both ways -- rendering `source` then
        // `dest` would put the remote file in the left-hand column for every
        // row that happens to be pulling, so the two columns would mean
        // different things on different lines.
        Cell::from(side(row.side_of(0), palette, row.included)),
        Cell::from(side(row.side_of(1), palette, row.included)),
    ];

    // The rightmost column: why, and by how much.
    let mut detail = Vec::new();
    if let Some(resolution) = row.resolution {
        // A conflict shows its decision, or the lack of one. The reason it is
        // a conflict is already in the row's glyph and section; what the user
        // needs here is what they have chosen.
        let (text, color) = match resolution {
            Resolution::Pending => (
                format!("{} \u{2014} L/R/B/X", row.reason.describe()),
                palette.danger,
            ),
            settled => (settled.label().to_string(), palette.ok),
        };
        detail.push(Span::styled(
            text,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    } else if danger {
        detail.push(Span::styled(
            row.reason.describe().to_string(),
            Style::default()
                .fg(palette.danger)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        let delta = row.size_delta();
        if delta != 0 && matches!(row.action, Action::Create | Action::Overwrite) {
            detail.push(Span::styled(
                format!(
                    "{}{}",
                    if delta > 0 { "+" } else { "-" },
                    format_size(delta.unsigned_abs() as u64)
                ),
                Style::default().fg(if delta < 0 {
                    // Shrinking is not wrong, but it is worth noticing.
                    palette.warn
                } else {
                    palette.dim
                }),
            ));
        } else {
            detail.push(Span::styled(
                row.reason.describe().to_string(),
                Style::default().fg(palette.dim),
            ));
        }
    }

    if let Some(caveat) = row.caveat {
        detail.push(Span::styled(
            format!("  {caveat}"),
            Style::default().fg(palette.warn),
        ));
    }

    cells.push(Cell::from(Line::from(detail)));
    TableRow::new(cells)
}

/// One side's size and time, or a dash where there is nothing.
fn side<'a>(value: Option<Side>, palette: &Palette, included: bool) -> Line<'a> {
    let color = if included { palette.dim } else { palette.ghost };

    match value {
        Some(side) => {
            let time = side.mtime.map_or_else(
                // A dash, never a fabricated date: on S3 there is no mtime,
                // and 1970 would read as a real timestamp.
                || "\u{2014}".to_string(),
                |t| t.format("%Y-%m-%d %H:%M").to_string(),
            );
            Line::from(Span::styled(
                format!("{:>8}  {time}", format_size(side.size)),
                Style::default().fg(color),
            ))
        }
        None => Line::from(Span::styled(
            "       \u{2014}",
            Style::default().fg(palette.ghost),
        )),
    }
}

fn draw_footer(frame: &mut Frame<'_>, app: &App, plan: &Plan, palette: &Palette, area: Rect) {
    let totals = plan.totals();
    let mut lines = Vec::new();

    // A refusal takes the whole footer: it is the only thing that matters.
    if let Some(refusal) = &plan.refusal {
        lines.push(Line::from(Span::styled(
            format!(" {refusal}"),
            Style::default()
                .fg(palette.danger)
                .add_modifier(Modifier::BOLD),
        )));
    } else {
        let mut parts = vec![Span::styled(
            format!(" {} ops", totals.operations()),
            Style::default().fg(palette.fg).add_modifier(Modifier::BOLD),
        )];

        if totals.bytes_out > 0 {
            parts.push(Span::styled(
                format!(" \u{b7} {} out", format_size(totals.bytes_out)),
                Style::default().fg(palette.dim),
            ));
        }
        if totals.deletes > 0 {
            parts.push(Span::styled(
                format!(
                    " \u{b7} {} deleted ({})",
                    totals.deletes,
                    format_size(totals.bytes_deleted)
                ),
                Style::default().fg(palette.danger),
            ));
        }
        if totals.directories > 0 {
            parts.push(Span::styled(
                format!(" \u{b7} {} dirs", totals.directories),
                Style::default().fg(palette.dim),
            ));
        }
        if totals.skipped > 0 {
            parts.push(Span::styled(
                format!(" \u{b7} {} unchanged", totals.skipped),
                Style::default().fg(palette.dim),
            ));
        }
        if totals.conflicts > 0 {
            parts.push(Span::styled(
                format!(" \u{b7} {} conflict(s)", totals.conflicts),
                Style::default().fg(palette.danger),
            ));
        }
        if totals.undecided > 0 {
            // The reason `y` will not work, said where the eye already is.
            parts.push(Span::styled(
                format!(" \u{b7} {} UNDECIDED", totals.undecided),
                Style::default()
                    .fg(palette.danger)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if totals.clobbers_newer > 0 {
            parts.push(Span::styled(
                format!(
                    " \u{b7} {} would overwrite NEWER data",
                    totals.clobbers_newer
                ),
                Style::default()
                    .fg(palette.danger)
                    .add_modifier(Modifier::BOLD),
            ));
        }

        lines.push(Line::from(parts));
    }

    // Directories that could not be read. Shown here rather than as rows,
    // because they are not things to include or exclude -- they are gaps in
    // what the plan could see, and a gap you do not know about is the
    // dangerous kind.
    if !plan.unreadable.is_empty() {
        let first = &plan.unreadable[0];
        lines.push(Line::from(Span::styled(
            format!(
                " {} unreadable: {} ({}){}",
                plan.unreadable.len(),
                first.0.display(),
                first.1,
                if plan.unreadable.len() > 1 {
                    ", ..."
                } else {
                    ""
                }
            ),
            Style::default().fg(palette.warn),
        )));
    }

    // A first bisync behaves differently, and the difference is a safety
    // property rather than a detail: with no snapshot there is no evidence
    // that anything was deleted rather than created, so nothing is.
    if plan.first_run {
        lines.push(Line::from(Span::styled(
            " first sync of this pair: nothing will be deleted, since a file on one side only              is as likely to be new as removed",
            Style::default().fg(palette.warn),
        )));
    }

    // The resolution keys are offered only when there is something to
    // resolve. A footer listing four keys that do nothing is a footer nobody
    // reads the useful half of.
    //
    // Built from the keymap, which on this screen is not a nicety. The
    // literal this replaced said `n cancel`, and a user who had rebound
    // `Cancel` to `esc`/`q` -- leaving `n` as `ParentDir` -- was being told
    // that the way out of a window about to overwrite their files was a key
    // that does not take it.
    let keymap = app.keymaps.get(Mode::Confirm);
    let keys = if totals.conflicts > 0 {
        hint::line(
            keymap,
            &[
                (KeyAction::Apply, "apply"),
                (KeyAction::Cancel, "cancel"),
                (KeyAction::ToggleRow, "include/exclude"),
                (KeyAction::ResolveKeepLocal, "keep local"),
                (KeyAction::ResolveKeepRemote, "keep remote"),
                (KeyAction::ResolveKeepBoth, "keep both"),
                (KeyAction::ResolveSkip, "leave alone"),
                (KeyAction::Help, "help"),
            ],
        )
    } else {
        hint::line(
            keymap,
            &[
                (KeyAction::Apply, "apply"),
                (KeyAction::Cancel, "cancel"),
                (KeyAction::ToggleRow, "include/exclude"),
                (KeyAction::AllRows, "all"),
                (KeyAction::InvertRows, "invert"),
                (KeyAction::OnlyRow, "only"),
                (KeyAction::FoldSection, "fold"),
                (KeyAction::UnfoldSection, "unfold"),
                (KeyAction::Help, "help"),
            ],
        )
    };
    lines.push(Line::from(Span::styled(
        keys,
        Style::default().fg(palette.dim),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::{
        sync::{
            compare::{Method, Reason},
            plan,
        },
        vfs::{Entry as VfsEntry, EntryKind},
    };

    fn entry(size: u64, epoch: Option<i64>) -> VfsEntry {
        use chrono::TimeZone;
        VfsEntry {
            name: "f".into(),
            kind: EntryKind::File,
            size,
            mtime: epoch.map(|s| chrono::Utc.timestamp_opt(s, 0).unwrap()),
            mode: None,
            symlink_target: None,
            symlink_to_dir: false,
        }
    }

    fn sample() -> Plan {
        let mut p = Plan {
            rows: vec![
                plan::create_row("new.txt".into(), &entry(100, Some(1)), Method::SizeMtime, 1),
                plan::compare_row(
                    "changed.txt".into(),
                    &entry(200, Some(2000)),
                    &entry(150, Some(1000)),
                    Reason::SizeDiffers,
                    Method::SizeMtime,
                    1,
                ),
                plan::compare_row(
                    "danger.txt".into(),
                    &entry(50, Some(1000)),
                    &entry(50, Some(9000)),
                    Reason::DestinationNewer,
                    Method::SizeMtime,
                    1,
                ),
                plan::delete_row("gone.txt".into(), &entry(30, Some(1)), Method::SizeMtime, 1),
                plan::compare_row(
                    "same.txt".into(),
                    &entry(10, Some(1)),
                    &entry(10, Some(1)),
                    Reason::SameSizeAndTime,
                    Method::SizeMtime,
                    1,
                ),
            ],
            ..Plan::default()
        };
        p.sort();
        p
    }

    #[test]
    fn the_display_list_has_a_heading_per_section() {
        let plan = sample();
        let list = entries(&plan, &[]);

        let headings: Vec<Section> = list
            .iter()
            .filter_map(|e| match e {
                Entry::Heading { section, .. } => Some(*section),
                _ => None,
            })
            .collect();

        assert_eq!(
            headings,
            vec![
                Section::Create,
                Section::Overwrite,
                Section::Delete,
                Section::Skipped
            ],
            "deletions must appear above the skipped list, never below a fold of it"
        );
    }

    /// An empty Delete heading still shows, so its absence can never be read
    /// as "the planner did not look".
    #[test]
    fn the_destructive_sections_are_shown_even_when_empty() {
        let mut plan = Plan {
            rows: vec![plan::create_row(
                "a".into(),
                &entry(1, Some(1)),
                Method::SizeMtime,
                1,
            )],
            ..Plan::default()
        };
        plan.sort();

        let list = entries(&plan, &[]);
        let headings: Vec<Section> = list
            .iter()
            .filter_map(|e| match e {
                Entry::Heading { section, .. } => Some(*section),
                _ => None,
            })
            .collect();

        assert!(headings.contains(&Section::Delete));
        assert!(headings.contains(&Section::Overwrite));
        assert!(
            !headings.contains(&Section::Skipped),
            "an empty skipped list is just noise"
        );
    }

    #[test]
    fn folding_a_section_hides_its_rows_but_keeps_the_heading() {
        let plan = sample();

        let open = entries(&plan, &[]);
        let folded = entries(&plan, &[Section::Skipped]);

        assert!(folded.len() < open.len());
        assert!(folded.iter().any(|e| matches!(
            e,
            Entry::Heading {
                section: Section::Skipped,
                folded: true,
                ..
            }
        )));
        assert!(!folded.iter().any(|e| match e {
            Entry::Row { row, .. } => row.action == Action::Skip,
            _ => false,
        }));
    }

    #[test]
    fn the_cursor_maps_to_the_right_plan_row() {
        let plan = sample();
        let list = entries(&plan, &[]);

        // Position 0 is the Create heading, so it is not a row.
        assert_eq!(row_at(&list, 0), None);
        assert_eq!(section_at(&list, 0), Some(Section::Create));

        // Position 1 is the first create.
        let index = row_at(&list, 1).expect("a row");
        assert_eq!(plan.rows[index].path, PathBuf::from("new.txt"));
        assert_eq!(section_at(&list, 1), Some(Section::Create));
    }

    #[test]
    fn a_cursor_past_the_end_maps_to_nothing() {
        let plan = sample();
        let list = entries(&plan, &[]);
        assert_eq!(row_at(&list, 9999), None);
        assert_eq!(section_at(&list, 9999), None);
    }

    /// Every row must render without panicking at a realistic width,
    /// including the dangerous, excluded and caveat paths.
    #[test]
    fn every_row_renders() {
        let palette = crate::config::Theme::default().palette().unwrap();
        let mut plan = sample();

        // Exclude one and add a caveat to another, to cover those branches.
        plan.toggle(0);
        plan.rows[1].caveat = Some("the destination cannot set timestamps");

        for row in &plan.rows {
            let rendered = plan_row(row, &palette, 120);
            let _ = rendered;
        }

        // And at an absurdly narrow width, where the path budget underflows.
        for row in &plan.rows {
            let _ = plan_row(row, &palette, 10);
        }
    }

    #[test]
    fn a_missing_side_renders_as_a_dash_not_a_date() {
        let palette = crate::config::Theme::default().palette().unwrap();

        let line = side(None, &palette, true);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('\u{2014}'), "{text:?}");

        // And a side that exists but has no timestamp.
        let line = side(
            Some(Side {
                size: 10,
                mtime: None,
            }),
            &palette,
            true,
        );
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("10 B"), "{text:?}");
        assert!(
            text.contains('\u{2014}') && !text.contains("1970"),
            "a backend with no mtime must not show a fabricated date: {text:?}"
        );
    }

    #[test]
    fn both_sides_are_shown_for_an_overwrite() {
        let palette = crate::config::Theme::default().palette().unwrap();
        let plan = sample();
        let row = plan
            .rows
            .iter()
            .find(|r| r.path == Path::new("changed.txt"))
            .unwrap();

        let source: String = side(row.source, &palette, true)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        let dest: String = side(row.dest, &palette, true)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();

        assert!(source.contains("200 B"), "{source:?}");
        assert!(
            dest.contains("150 B"),
            "the window must show what is being replaced: {dest:?}"
        );
    }
}
