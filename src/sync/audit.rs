//! A durable record of what every run actually did.
//!
//! The confirmation window answers "what is about to happen". This answers
//! "what happened", which is a different question and the only one available
//! after the fact -- particularly for `lazysync sync --yes` from cron, where
//! nobody saw the plan and the status line scrolled past weeks ago.
//!
//! **One JSON object per line, appended.** Not a single JSON document: a log
//! is written to repeatedly and a document would have to be read, parsed,
//! extended and rewritten on every run, which turns a crash mid-write into a
//! corrupt *history* rather than one damaged line. Appending a line at a time
//! means the worst a crash can do is truncate the last record, and everything
//! before it is still readable -- by lazysync, by `jq`, or by eye.
//!
//! **Failures are recorded, not just successes.** A log that only lists what
//! worked cannot answer the question people actually bring to it, which is
//! why a file is missing.
//!
//! The log is never read back by the planner. It is for the user, and for the
//! log viewer screen; nothing in lazysync's behaviour depends on it, so a
//! write that fails is reported and then ignored rather than failing a sync
//! that has already happened.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::executor::{Outcome, Report};

/// How many bytes the log may reach before it is rotated.
///
/// One rotation, to `audit.jsonl.1`. Not a full rotation scheme, because the
/// alternative to a cap is a file that grows without limit on a watched tree
/// -- a per-save sync writes a record every few seconds -- and the alternative
/// to *one* old file is deciding how many generations matter, which nothing
/// here can answer better than the user can with a cron job.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// What one run did to one path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub path: String,
    /// `create`, `overwrite`, `delete`, `conflict`.
    pub action: String,
    /// `transferred`, `deleted`, `backed-up`, `skipped`, `cancelled`,
    /// `failed`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Why, for the outcomes that have a reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub at: DateTime<Utc>,
    /// `push`, `pull`, `mirror push`, `bisync`.
    pub mode: String,
    /// Which engine moved the bytes: `native`, `rsync`, `rclone`.
    pub engine: String,
    pub source: String,
    pub destination: String,

    pub applied: usize,
    pub failed: usize,
    pub skipped: usize,
    pub bytes: u64,

    /// Set when a safety rail refused the plan, in which case nothing ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,

    /// Whether a human saw the plan first.
    ///
    /// The distinction the log exists for: a reviewed sync and one applied by
    /// `--yes` or by watch mode are different events, and only one of them has
    /// somebody to ask about it.
    pub confirmed: bool,

    pub items: Vec<Item>,
}

impl Entry {
    /// Build an entry from a finished run.
    pub fn from_report(
        at: DateTime<Utc>,
        mode: &str,
        engine: &str,
        source: &str,
        destination: &str,
        confirmed: bool,
        report: &Report,
    ) -> Self {
        let items = report
            .applied
            .iter()
            .map(|applied| Item {
                path: applied.path.display().to_string(),
                action: format!("{:?}", applied.action).to_lowercase(),
                outcome: outcome_name(&applied.outcome).to_string(),
                bytes: match &applied.outcome {
                    Outcome::Transferred { bytes } => Some(*bytes),
                    Outcome::Cancelled { partial_bytes } => Some(*partial_bytes),
                    _ => None,
                },
                detail: match &applied.outcome {
                    Outcome::Failed { why } | Outcome::SkippedStale { why } => Some(why.clone()),
                    Outcome::MovedToBackup { to } => Some(to.display().to_string()),
                    _ => None,
                },
            })
            .collect();

        Self {
            at,
            mode: mode.to_owned(),
            engine: engine.to_owned(),
            source: source.to_owned(),
            destination: destination.to_owned(),
            applied: report.succeeded(),
            failed: report.failures().count(),
            skipped: report.skipped().count(),
            bytes: report.bytes,
            refused: None,
            confirmed,
            items,
        }
    }

    /// An entry for a plan that was refused and therefore never ran.
    ///
    /// Recorded because a refusal is the most interesting thing that can
    /// happen: it means a sync was about to do something the user would not
    /// have wanted, and the record is what lets them find out it was stopped
    /// rather than wondering why nothing synced.
    pub fn refusal(
        at: DateTime<Utc>,
        mode: &str,
        source: &str,
        destination: &str,
        why: &str,
    ) -> Self {
        Self {
            at,
            mode: mode.to_owned(),
            engine: "none".into(),
            source: source.to_owned(),
            destination: destination.to_owned(),
            applied: 0,
            failed: 0,
            skipped: 0,
            bytes: 0,
            refused: Some(why.to_owned()),
            confirmed: false,
            items: Vec::new(),
        }
    }

    /// One line for the log viewer.
    pub fn summary(&self) -> String {
        if let Some(why) = &self.refused {
            return format!(
                "{} {} REFUSED: {why}",
                self.at.format("%Y-%m-%d %H:%M"),
                self.mode
            );
        }

        let mut parts = vec![format!(
            "{} {} via {}",
            self.at.format("%Y-%m-%d %H:%M"),
            self.mode,
            self.engine
        )];
        parts.push(format!("{} applied", self.applied));
        if self.bytes > 0 {
            parts.push(crate::ui::format_size(self.bytes));
        }
        if self.failed > 0 {
            parts.push(format!("{} FAILED", self.failed));
        }
        if self.skipped > 0 {
            parts.push(format!("{} skipped", self.skipped));
        }
        if !self.confirmed {
            parts.push("unattended".into());
        }
        parts.join(" \u{b7} ")
    }
}

const fn outcome_name(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Transferred { .. } => "transferred",
        Outcome::Deleted => "deleted",
        Outcome::MovedToBackup { .. } => "backed-up",
        Outcome::SkippedStale { .. } => "skipped",
        Outcome::Cancelled { .. } => "cancelled",
        Outcome::Failed { .. } => "failed",
    }
}

/// Where the log lives.
pub fn path() -> PathBuf {
    crate::config::state_dir().join("audit.jsonl")
}

/// Append an entry.
///
/// Errors are returned rather than logged, so the caller can put a failure in
/// the status line -- but no caller should treat it as fatal: the sync has
/// already happened, and refusing to acknowledge it because the record could
/// not be written would be the wrong way round.
pub fn append(entry: &Entry) -> std::io::Result<()> {
    let path = path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    rotate_if_large(&path)?;

    let mut line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    line.push('\n');

    // Opened in append mode for every write rather than held open. A held
    // handle would be faster and would also mean a long-running lazysync
    // keeping a deleted or rotated file alive, writing into something nothing
    // can read.
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(line.as_bytes())?;
    file.flush()
}

/// Move the log aside if it has grown past [`MAX_BYTES`].
fn rotate_if_large(path: &Path) -> std::io::Result<()> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() < MAX_BYTES {
        return Ok(());
    }

    let old = path.with_extension("jsonl.1");
    // Replacing the previous generation, so exactly two files ever exist.
    std::fs::rename(path, &old)
}

/// Read the most recent entries, newest first.
///
/// A damaged line is skipped rather than failing the read. That is the point
/// of one object per line: a crash mid-write can truncate the last record, and
/// a log that refused to open because of it would be useless exactly when it
/// was needed.
pub fn recent(limit: usize) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path()) else {
        return Vec::new();
    };

    let mut entries: Vec<Entry> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    entries.reverse();
    entries.truncate(limit);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::{executor::Applied, plan::Action};

    fn report() -> Report {
        Report {
            applied: vec![
                Applied {
                    path: "a.txt".into(),
                    action: Action::Create,
                    outcome: Outcome::Transferred { bytes: 11 },
                },
                Applied {
                    path: "b.txt".into(),
                    action: Action::Overwrite,
                    outcome: Outcome::Failed {
                        why: "permission denied".into(),
                    },
                },
                Applied {
                    path: "c.txt".into(),
                    action: Action::Delete,
                    outcome: Outcome::Deleted,
                },
            ],
            bytes: 11,
        }
    }

    fn entry() -> Entry {
        Entry::from_report(
            Utc::now(),
            "push",
            "native",
            "/home/greg/proj",
            "box:/srv/www",
            true,
            &report(),
        )
    }

    #[test]
    fn an_entry_counts_what_happened() {
        let entry = entry();
        assert_eq!(entry.applied, 2, "the transfer and the deletion");
        assert_eq!(entry.failed, 1);
        assert_eq!(entry.bytes, 11);
        assert_eq!(entry.items.len(), 3);
    }

    /// The question people actually bring to a log is why a file is missing,
    /// so a failure has to be in it -- with its reason.
    #[test]
    fn a_failure_is_recorded_with_its_reason() {
        let entry = entry();
        let failed = entry
            .items
            .iter()
            .find(|item| item.outcome == "failed")
            .expect("the failure should be in the log");

        assert_eq!(failed.path, "b.txt");
        assert_eq!(failed.detail.as_deref(), Some("permission denied"));
    }

    #[test]
    fn an_entry_round_trips_through_one_line_of_json() {
        let entry = entry();
        let line = serde_json::to_string(&entry).unwrap();

        assert!(
            !line.contains('\n'),
            "a record has to be one line, or appending would corrupt the file"
        );

        let back: Entry = serde_json::from_str(&line).unwrap();
        assert_eq!(back, entry);
    }

    /// A refusal is the most interesting thing that can happen, and the record
    /// is what tells the user a sync was *stopped* rather than silently doing
    /// nothing.
    #[test]
    fn a_refusal_is_recorded_and_says_nothing_ran() {
        let entry = Entry::refusal(
            Utc::now(),
            "mirror push",
            "/home/greg/proj",
            "box:/srv/www",
            "REFUSED: this would delete every file",
        );

        assert_eq!(entry.applied, 0);
        assert_eq!(entry.bytes, 0);
        assert!(entry.items.is_empty());
        assert!(entry.refused.is_some());
        assert!(entry.summary().contains("REFUSED"), "{}", entry.summary());
    }

    /// An unattended run is a different event from a reviewed one, and the
    /// summary says which.
    #[test]
    fn an_unattended_run_is_marked_as_such() {
        let reviewed = entry();
        assert!(!reviewed.summary().contains("unattended"));

        let unattended =
            Entry::from_report(Utc::now(), "push", "native", "a", "b", false, &report());
        assert!(
            unattended.summary().contains("unattended"),
            "{}",
            unattended.summary()
        );
    }

    #[test]
    fn a_summary_names_the_engine_that_moved_the_bytes() {
        let delegated = Entry::from_report(Utc::now(), "push", "rsync", "a", "b", true, &report());
        assert!(
            delegated.summary().contains("rsync"),
            "{}",
            delegated.summary()
        );
    }

    /// The reason for one-object-per-line: a truncated last record must not
    /// make the whole history unreadable.
    #[test]
    fn a_truncated_last_line_does_not_lose_the_rest() {
        let good = serde_json::to_string(&entry()).unwrap();
        let text = format!("{good}\n{good}\n{{\"at\":\"2026-10-25T00:00:00Z\",\"mo");

        let parsed: Vec<Entry> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();

        assert_eq!(
            parsed.len(),
            2,
            "a crash mid-write truncates one record; the rest stay readable"
        );
    }

    #[test]
    fn every_outcome_has_a_name() {
        for outcome in [
            Outcome::Transferred { bytes: 1 },
            Outcome::Deleted,
            Outcome::MovedToBackup { to: "x".into() },
            Outcome::SkippedStale { why: "y".into() },
            Outcome::Cancelled { partial_bytes: 2 },
            Outcome::Failed { why: "z".into() },
        ] {
            let name = outcome_name(&outcome);
            assert!(!name.is_empty());
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "names are machine-readable, so they stay lowercase: {name}"
            );
        }
    }

    /// Writing and reading back, through the real state directory -- which the
    /// dev override points somewhere temporary.
    #[test]
    fn entries_can_be_appended_and_read_back() {
        let dir = tempfile::tempdir().unwrap();

        // SAFETY: this test is the only one touching the audit log, and the
        // variable is restored before it returns.
        let previous = std::env::var_os("LAZYSYNC_DEV_STATE_DIR");
        unsafe { std::env::set_var("LAZYSYNC_DEV_STATE_DIR", dir.path()) };

        append(&entry()).expect("appending");
        append(&Entry::refusal(Utc::now(), "mirror push", "a", "b", "nope"))
            .expect("appending a refusal");

        let read = recent(10);
        assert_eq!(read.len(), 2);
        assert!(
            read[0].refused.is_some(),
            "newest first, so the refusal comes back at the top"
        );

        match previous {
            Some(value) => unsafe { std::env::set_var("LAZYSYNC_DEV_STATE_DIR", value) },
            None => unsafe { std::env::remove_var("LAZYSYNC_DEV_STATE_DIR") },
        }
    }
}
