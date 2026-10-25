//! Deciding whether two files are the same.
//!
//! This is the single most consequential judgement in lazysync: everything the
//! confirmation window shows, and everything the executor then does, follows
//! from it. Getting it wrong in one direction re-copies a tree that has not
//! changed; getting it wrong in the other silently skips a file that has.
//!
//! The hard part is not the comparison, it is the *tolerance*. Filesystems and
//! protocols disagree about how precisely they record a modification time:
//!
//! - A local filesystem stores nanoseconds.
//! - The SFTP base attribute set carries whole seconds. Version 4 added
//!   nanoseconds; plenty of servers do not send them.
//! - FAT rounds to two seconds.
//! - S3 has no modification time of its own at all.
//!
//! So comparing a local file against the same file on an SFTP server with a
//! zero tolerance makes every file look changed, which is the usual reason a
//! sync tool wants to re-transfer everything. The tolerance is therefore the
//! *maximum* of the configured window and both sides' granularity -- a
//! configured value cannot promise precision the protocol does not carry.

use chrono::{DateTime, Utc};

use crate::{
    config::{Compare as CompareConfig, CompareMethod},
    vfs::{Capabilities, Entry},
};

/// Why two files were judged same or different.
///
/// Carried into the plan and shown in the confirmation window, because "this
/// will be overwritten" is far less useful than "this will be overwritten
/// because the sizes differ".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Sizes differ. Conclusive: no amount of timestamp agreement makes two
    /// files of different lengths the same.
    SizeDiffers,
    /// Same size, source is newer than the tolerance allows.
    SourceNewer,
    /// Same size, destination is newer. The dangerous case -- applying this
    /// overwrites newer data with older -- so the window colours it red.
    DestinationNewer,
    /// Checksums differ.
    ChecksumDiffers,
    /// Same size and the timestamps agree within tolerance.
    SameSizeAndTime,
    /// Same size, and checksums match.
    ChecksumMatches,
    /// Same size, and neither side offers a usable timestamp. Treated as
    /// equal, and the window says so -- on S3 this is all the information
    /// there is without reading both files.
    SameSizeNoTimestamps,
    /// Same size, and the destination reports timestamps but cannot be made
    /// to keep the source's.
    ///
    /// Distinct from `SameSizeNoTimestamps` because the window would otherwise
    /// claim there are no timestamps while displaying two of them. The
    /// timestamps exist; they just describe when each file was *written*, so
    /// comparing them says nothing about whether the contents match.
    SameSizeTimesNotPreserved,

    /// Present only on the source.
    ///
    /// Its own reason rather than reusing `SizeDiffers`: there is nothing to
    /// compare sizes with, and a confirmation window that says "size differs"
    /// about a file the destination does not have is describing a check it
    /// never made.
    NotOnDestination,
    /// Present only on the destination. What a mirror deletes.
    NotOnSource,
}

impl Reason {
    /// Whether this reason means the file needs transferring.
    pub const fn differs(self) -> bool {
        matches!(
            self,
            Self::SizeDiffers
                | Self::SourceNewer
                | Self::DestinationNewer
                | Self::ChecksumDiffers
                | Self::NotOnDestination
                | Self::NotOnSource
        )
    }

    /// Whether applying this would replace newer data with older.
    pub const fn clobbers_newer(self) -> bool {
        matches!(self, Self::DestinationNewer)
    }

    /// A short phrase for the confirmation window.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::SizeDiffers => "size differs",
            Self::SourceNewer => "source is newer",
            Self::DestinationNewer => "DESTINATION IS NEWER",
            Self::ChecksumDiffers => "checksum differs",
            Self::SameSizeAndTime => "same size and time",
            Self::ChecksumMatches => "checksum matches",
            Self::SameSizeNoTimestamps => "same size, no timestamps",
            Self::SameSizeTimesNotPreserved => "same size, times not kept",
            Self::NotOnDestination => "new",
            Self::NotOnSource => "not on the source",
        }
    }
}

/// How two files were compared, so the window can say which rule it used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Method {
    #[default]
    SizeMtime,
    SizeOnly,
    Checksum,
}

impl Method {
    pub const fn describe(self) -> &'static str {
        match self {
            Self::SizeMtime => "size+mtime",
            Self::SizeOnly => "size",
            Self::Checksum => "checksum",
        }
    }
}

/// The comparison rules for one pair of backends.
#[derive(Debug, Clone, Copy)]
pub struct Comparator {
    method: Method,
    /// Seconds within which two timestamps count as equal.
    tolerance: i64,
    /// Whether a timestamp comparison can mean anything here. See
    /// [`Comparator::new`].
    timestamps_available: bool,
    /// Whether the reason timestamps are unusable is that the destination
    /// cannot preserve them, rather than that nobody reports them. Only
    /// affects which [`Reason`] an equal pair gets, so the window does not
    /// claim there are no timestamps while displaying two.
    times_reported_but_not_preservable: bool,
}

impl Comparator {
    /// Build a comparator for a source and destination backend.
    ///
    /// The effective tolerance is the largest of the configured window and
    /// both sides' granularity. Taking the maximum rather than the configured
    /// value is the point: `mtime_tolerance_secs = 0` against a server that
    /// only records whole seconds cannot mean "compare exactly", because the
    /// information to do that was never transmitted. Promising it would just
    /// mark every file as changed.
    pub fn new(config: &CompareConfig, source: Capabilities, dest: Capabilities) -> Self {
        let configured = i64::try_from(config.mtime_tolerance_secs).unwrap_or(i64::MAX);
        let granularity = i64::try_from(
            source
                .mtime_granularity_secs
                .max(dest.mtime_granularity_secs),
        )
        .unwrap_or(i64::MAX);

        // Comparing by timestamp needs more than both sides reporting one: the
        // destination has to be able to *preserve* the source's. Where it
        // cannot -- FTP, whose `MFMT` is not in the base protocol -- every
        // file carries the time it was written, which never matches the
        // source, so an mtime comparison would call the whole tree changed on
        // every run, forever. That is not a theoretical worry: it is what the
        // FTP fixture test caught.
        //
        // Degrading to size is a real loss, since a same-size edit then slips
        // through, and it is why `compare.method = "checksum"` exists. But it
        // is the most this protocol allows, and the window names the method it
        // actually used rather than implying a precision it did not have.
        let times_reported = source.mtime && dest.mtime;
        let timestamps_available = times_reported && dest.set_mtime;
        let times_reported_but_not_preservable = times_reported && !dest.set_mtime;

        // A backend with no timestamps cannot be compared by timestamp, so
        // `size_mtime` degrades to `size` rather than silently treating a
        // missing mtime as a difference.
        let method = match config.method {
            CompareMethod::Checksum => Method::Checksum,
            CompareMethod::SizeOnly => Method::SizeOnly,
            CompareMethod::SizeMtime if timestamps_available => Method::SizeMtime,
            CompareMethod::SizeMtime => Method::SizeOnly,
        };

        Self {
            method,
            tolerance: configured.max(granularity),
            timestamps_available,
            times_reported_but_not_preservable,
        }
    }

    pub const fn method(&self) -> Method {
        self.method
    }

    pub const fn tolerance_secs(&self) -> i64 {
        self.tolerance
    }

    pub const fn has_timestamps(&self) -> bool {
        self.timestamps_available
    }

    /// Compare a source file against an existing destination file.
    ///
    /// `checksums` is consulted only for [`Method::Checksum`], and is a pair
    /// of already-computed digests -- hashing happens in the executor, where
    /// it can report progress, rather than inside a comparison that the UI
    /// calls while drawing.
    pub fn compare(&self, source: &Entry, dest: &Entry, checksums: Option<(&str, &str)>) -> Reason {
        // Size first, whatever the method: it is free, it is conclusive when
        // it differs, and it rules out the expensive checks.
        if source.size != dest.size {
            return Reason::SizeDiffers;
        }

        match self.method {
            // Timestamps are deliberately not consulted: size-only was asked
            // for, or one side has none, or the destination cannot be made to
            // keep the source's. Reporting `SameSizeAndTime` would claim a
            // check that was not made.
            Method::SizeOnly => self.same_size_reason(),

            Method::Checksum => match checksums {
                Some((a, b)) if a == b => Reason::ChecksumMatches,
                Some(_) => Reason::ChecksumDiffers,
                // Asked for a checksum and did not get one. Falling back to
                // timestamps quietly would report a stronger check than was
                // performed, so this reports the weaker answer honestly.
                None => self.by_time(source.mtime, dest.mtime),
            },

            Method::SizeMtime => self.by_time(source.mtime, dest.mtime),
        }
    }

    /// Which "equal as far as we can tell" reason applies, given *why* the
    /// timestamps went unused.
    const fn same_size_reason(&self) -> Reason {
        if self.times_reported_but_not_preservable {
            Reason::SameSizeTimesNotPreserved
        } else {
            Reason::SameSizeNoTimestamps
        }
    }

    /// Compare two timestamps within the tolerance.
    fn by_time(&self, source: Option<DateTime<Utc>>, dest: Option<DateTime<Utc>>) -> Reason {
        let (Some(source), Some(dest)) = (source, dest) else {
            // One side has no timestamp. Equal sizes is all the evidence
            // available, and inventing a difference would re-copy the file on
            // every run forever.
            return self.same_size_reason();
        };

        let difference = source.timestamp() - dest.timestamp();

        if difference.abs() <= self.tolerance {
            Reason::SameSizeAndTime
        } else if difference > 0 {
            Reason::SourceNewer
        } else {
            Reason::DestinationNewer
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::vfs::EntryKind;

    fn caps(mtime: bool, granularity: u64) -> Capabilities {
        Capabilities {
            mtime,
            set_mtime: true,
            mode: true,
            symlinks: true,
            ranged_writes: true,
            atomic_overwrite_rename: true,
            mtime_granularity_secs: granularity,
        }
    }

    fn local() -> Capabilities {
        caps(true, 1)
    }

    fn s3() -> Capabilities {
        caps(false, 1)
    }

    fn fat() -> Capabilities {
        caps(true, 2)
    }

    /// Reports timestamps, cannot be made to keep them. FTP.
    fn ftp() -> Capabilities {
        Capabilities {
            set_mtime: false,
            ..caps(true, 1)
        }
    }

    fn entry(size: u64, epoch: Option<i64>) -> Entry {
        Entry {
            name: "f".into(),
            kind: EntryKind::File,
            size,
            mtime: epoch.map(|s| Utc.timestamp_opt(s, 0).unwrap()),
            mode: None,
            symlink_target: None,
            symlink_to_dir: false,
        }
    }

    fn config(method: CompareMethod, tolerance: u64) -> CompareConfig {
        CompareConfig {
            method,
            mtime_tolerance_secs: tolerance,
            use_provider_hash: true,
        }
    }

    fn size_mtime(tolerance: u64) -> Comparator {
        Comparator::new(
            &config(CompareMethod::SizeMtime, tolerance),
            local(),
            local(),
        )
    }

    #[test]
    fn different_sizes_are_conclusive() {
        let c = size_mtime(2);
        // Identical timestamps must not override a size difference.
        assert_eq!(
            c.compare(&entry(10, Some(1000)), &entry(20, Some(1000)), None),
            Reason::SizeDiffers
        );
    }

    #[test]
    fn identical_files_are_equal() {
        let c = size_mtime(2);
        assert_eq!(
            c.compare(&entry(10, Some(1000)), &entry(10, Some(1000)), None),
            Reason::SameSizeAndTime
        );
    }

    #[test]
    fn a_newer_source_is_a_normal_overwrite() {
        let c = size_mtime(2);
        let reason = c.compare(&entry(10, Some(2000)), &entry(10, Some(1000)), None);
        assert_eq!(reason, Reason::SourceNewer);
        assert!(reason.differs());
        assert!(!reason.clobbers_newer());
    }

    /// The case the confirmation window colours red.
    #[test]
    fn a_newer_destination_is_flagged_as_clobbering() {
        let c = size_mtime(2);
        let reason = c.compare(&entry(10, Some(1000)), &entry(10, Some(2000)), None);
        assert_eq!(reason, Reason::DestinationNewer);
        assert!(reason.differs());
        assert!(
            reason.clobbers_newer(),
            "overwriting newer data with older is the case worth shouting about"
        );
    }

    /// The reason the tolerance exists at all.
    #[test]
    fn timestamps_within_the_tolerance_are_equal() {
        let c = size_mtime(2);
        for skew in [-2, -1, 0, 1, 2] {
            assert_eq!(
                c.compare(&entry(10, Some(1000 + skew)), &entry(10, Some(1000)), None),
                Reason::SameSizeAndTime,
                "a {skew}s difference is within a 2s tolerance"
            );
        }
        assert_eq!(
            c.compare(&entry(10, Some(1003)), &entry(10, Some(1000)), None),
            Reason::SourceNewer,
            "3s is outside it"
        );
    }

    /// A configured tolerance cannot promise precision the protocol does not
    /// carry: against a 2-second filesystem, `0` still means 2.
    #[test]
    fn the_tolerance_is_at_least_the_coarsest_granularity() {
        let c = Comparator::new(&config(CompareMethod::SizeMtime, 0), local(), fat());
        assert_eq!(c.tolerance_secs(), 2);

        assert_eq!(
            c.compare(&entry(10, Some(1001)), &entry(10, Some(1000)), None),
            Reason::SameSizeAndTime,
            "a 1s difference against a 2s-granularity filesystem is not evidence of change"
        );
    }

    /// And a configured tolerance larger than the granularity still wins.
    #[test]
    fn a_larger_configured_tolerance_is_respected() {
        let c = Comparator::new(&config(CompareMethod::SizeMtime, 60), local(), local());
        assert_eq!(c.tolerance_secs(), 60);
        assert_eq!(
            c.compare(&entry(10, Some(1030)), &entry(10, Some(1000)), None),
            Reason::SameSizeAndTime
        );
    }

    /// A backend with no timestamps cannot be compared by timestamp, so the
    /// method degrades rather than reporting every file as changed.
    #[test]
    fn size_mtime_degrades_to_size_only_without_timestamps() {
        let c = Comparator::new(&config(CompareMethod::SizeMtime, 2), local(), s3());

        assert_eq!(c.method(), Method::SizeOnly);
        assert!(!c.has_timestamps());
        assert_eq!(
            c.compare(&entry(10, Some(1000)), &entry(10, None), None),
            Reason::SameSizeNoTimestamps
        );
    }

    /// The same degradation for a destination that *reports* timestamps but
    /// cannot be made to keep them.
    ///
    /// This is FTP, and it is not a hypothetical: the file it receives carries
    /// the time it was uploaded, which never matches the source's. Comparing
    /// those two would mark every file changed on every run, for ever. Caught
    /// by `tests/ftp.rs` against a real server; a mock that stores whatever
    /// mtime it is handed makes the comparison pass and hides it.
    #[test]
    fn size_mtime_degrades_when_the_destination_cannot_preserve_times() {
        let c = Comparator::new(&config(CompareMethod::SizeMtime, 2), local(), ftp());

        assert_eq!(c.method(), Method::SizeOnly);
        assert!(!c.has_timestamps());

        // Years apart, and still equal -- because the timestamps carry no
        // information about whether the contents match.
        let reason = c.compare(&entry(10, Some(1_700_000_000)), &entry(10, Some(1)), None);
        assert!(
            !reason.differs(),
            "an unpreservable timestamp is not evidence of change: {reason:?}"
        );
        assert_eq!(
            reason,
            Reason::SameSizeTimesNotPreserved,
            "and the window must not claim there are no timestamps while showing two"
        );
    }

    /// The source being unable to set times is irrelevant: nothing is written
    /// there. Only the destination's limitation matters, and conflating the two
    /// would needlessly drop pulls *from* FTP down to a size comparison.
    #[test]
    fn a_source_that_cannot_set_times_still_compares_by_time() {
        let c = Comparator::new(&config(CompareMethod::SizeMtime, 2), ftp(), local());

        assert_eq!(c.method(), Method::SizeMtime);
        assert_eq!(
            c.compare(&entry(10, Some(2000)), &entry(10, Some(1000)), None),
            Reason::SourceNewer
        );
    }

    /// Equal sizes with a missing timestamp must read as equal. Treating the
    /// absence as a difference would re-copy the file on every single run.
    #[test]
    fn a_missing_timestamp_does_not_invent_a_difference() {
        let c = size_mtime(2);
        let reason = c.compare(&entry(10, Some(1000)), &entry(10, None), None);
        assert_eq!(reason, Reason::SameSizeNoTimestamps);
        assert!(!reason.differs());
    }

    #[test]
    fn checksums_decide_when_asked_for() {
        let c = Comparator::new(&config(CompareMethod::Checksum, 2), local(), local());
        assert_eq!(c.method(), Method::Checksum);

        // Equal sizes, wildly different timestamps, but matching digests.
        assert_eq!(
            c.compare(
                &entry(10, Some(9999)),
                &entry(10, Some(1000)),
                Some(("abc", "abc"))
            ),
            Reason::ChecksumMatches
        );
        assert_eq!(
            c.compare(
                &entry(10, Some(1000)),
                &entry(10, Some(1000)),
                Some(("abc", "def"))
            ),
            Reason::ChecksumDiffers
        );
    }

    /// Asking for a checksum and not getting one must report the weaker check
    /// that was actually made, not claim the stronger one.
    #[test]
    fn a_missing_checksum_falls_back_and_says_so() {
        let c = Comparator::new(&config(CompareMethod::Checksum, 2), local(), local());
        let reason = c.compare(&entry(10, Some(2000)), &entry(10, Some(1000)), None);

        assert_eq!(
            reason,
            Reason::SourceNewer,
            "it should report the timestamp comparison it fell back to"
        );
        assert_ne!(reason, Reason::ChecksumDiffers);
    }

    #[test]
    fn size_only_ignores_timestamps_on_purpose() {
        let c = Comparator::new(&config(CompareMethod::SizeOnly, 0), local(), local());
        assert_eq!(c.method(), Method::SizeOnly);
        assert_eq!(
            c.compare(&entry(10, Some(9999)), &entry(10, Some(1)), None),
            Reason::SameSizeNoTimestamps,
            "size-only was asked for, so a timestamp difference is not a difference"
        );
    }

    /// Every reason needs a description, since the window prints one per row.
    #[test]
    fn every_reason_describes_itself() {
        for reason in [
            Reason::SizeDiffers,
            Reason::SourceNewer,
            Reason::DestinationNewer,
            Reason::ChecksumDiffers,
            Reason::SameSizeAndTime,
            Reason::ChecksumMatches,
            Reason::SameSizeNoTimestamps,
            Reason::NotOnDestination,
            Reason::NotOnSource,
        ] {
            assert!(!reason.describe().is_empty());
        }
    }

    /// Zero-length files are equal when their timestamps agree, and must not
    /// be special-cased into always-copy.
    #[test]
    fn empty_files_compare_normally() {
        let c = size_mtime(2);
        assert_eq!(
            c.compare(&entry(0, Some(1000)), &entry(0, Some(1000)), None),
            Reason::SameSizeAndTime
        );
        assert_eq!(
            c.compare(&entry(0, Some(1000)), &entry(1, Some(1000)), None),
            Reason::SizeDiffers
        );
    }

    /// A tolerance so large it would overflow the arithmetic must not panic.
    #[test]
    fn an_absurd_tolerance_does_not_overflow() {
        let c = Comparator::new(
            &config(CompareMethod::SizeMtime, u64::MAX),
            local(),
            local(),
        );
        assert_eq!(
            c.compare(&entry(10, Some(i32::MAX.into())), &entry(10, Some(0)), None),
            Reason::SameSizeAndTime
        );
    }
}
