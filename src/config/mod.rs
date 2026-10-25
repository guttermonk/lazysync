//! Configuration: the whole of `config.toml`, with defaults.
//!
//! Every section in `config.example.toml` is represented here even where the
//! phase that uses it has not landed, so that a config file written against
//! the documented schema loads cleanly today and the value is simply unused.
//!
//! Unknown fields are an error rather than a warning. A silently ignored
//! `max_delete_fraciton` is a safety rail that looks configured and is not,
//! and the same goes for a misspelled action name. The cost is that a config
//! written for a newer lazysync fails on an older one, which for a local tool
//! is the cheaper mistake.

pub mod theme;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::keys::{KeymapSection, Keymaps, Mode};

pub use theme::{Palette, Preset, Theme};

/// The parsed config file, before keymaps are resolved.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Which pane has focus at startup.
    pub default_pane: PaneSide,
    /// Show dotfiles.
    pub show_hidden: bool,
    /// Sort key, optionally prefixed with `-` to reverse.
    pub sort: SortSpec,
    /// Directories before files, regardless of sort key.
    pub dirs_first: bool,
    /// Compute recursive directory sizes. Off by default because it costs one
    /// listing per subdirectory, which over SFTP is slow enough to feel.
    pub dir_sizes: bool,
    /// How long a chord waits for its next key, in milliseconds.
    pub chord_timeout_ms: u64,

    pub sync_browse: SyncBrowse,
    pub zide: Zide,
    pub compare: Compare,
    pub transfer: Transfer,
    pub safety: Safety,
    pub confirm: Confirm,
    pub theme: Theme,

    /// Per-mode keymaps. Resolved into [`Keymaps`] by [`Config::keymaps`].
    pub keys: BTreeMap<Mode, KeymapSection>,

    /// Named sync profiles, referenced by bookmarks and `lazysync sync`.
    pub profiles: BTreeMap<String, Profile>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_pane: PaneSide::Local,
            show_hidden: false,
            sort: SortSpec::default(),
            dirs_first: true,
            dir_sizes: false,
            chord_timeout_ms: 500,
            sync_browse: SyncBrowse::default(),
            zide: Zide::default(),
            compare: Compare::default(),
            transfer: Transfer::default(),
            safety: Safety::default(),
            confirm: Confirm::default(),
            theme: Theme::default(),
            keys: BTreeMap::new(),
            profiles: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Load from `path`, or return the defaults if it does not exist.
    ///
    /// A missing file is not an error -- lazysync is usable with no config at
    /// all -- but a malformed one is, naming the file and the line.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!("no config at {}, using defaults", path.display());
                return Ok(Self::default());
            }
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", path.display()));
            }
        };

        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Load from the default location, or an explicit override.
    pub fn load_default(override_path: Option<&Path>) -> Result<Self> {
        match override_path {
            Some(path) => Self::load(path),
            None => Self::load(&config_path()),
        }
    }

    /// The keymaps this config resolves to.
    pub fn keymaps(&self) -> Keymaps {
        Keymaps::resolve(&self.keys)
    }

    pub fn chord_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.chord_timeout_ms)
    }
}

/// Which of the two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneSide {
    Local,
    Remote,
}

impl PaneSide {
    pub const fn index(self) -> usize {
        match self {
            Self::Local => 0,
            Self::Remote => 1,
        }
    }
}

/// A sort key with a direction, written `"name"` or `"-name"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortSpec {
    pub key: SortKey,
    pub reverse: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            reverse: false,
        }
    }
}

impl<'de> Deserialize<'de> for SortSpec {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        let (reverse, name) = match raw.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, raw.as_str()),
        };
        let key = match name {
            "name" => SortKey::Name,
            "size" => SortKey::Size,
            "modified" => SortKey::Modified,
            "extension" => SortKey::Extension,
            other => {
                return Err(serde::de::Error::custom(format!(
                    "unknown sort key {other:?} (expected name, size, modified or extension, \
                     optionally prefixed with - to reverse)"
                )))
            }
        };
        Ok(Self { key, reverse })
    }
}

impl Serialize for SortSpec {
    /// Back to the `"-modified"` form it was read from, so a dumped config can
    /// be pasted straight back into `config.toml`.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let prefix = if self.reverse { "-" } else { "" };
        serializer.serialize_str(&format!("{prefix}{}", self.key.name()))
    }
}

/// What a listing is ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Extension,
}

impl SortKey {
    pub const ALL: [Self; 4] = [Self::Name, Self::Size, Self::Modified, Self::Extension];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Size => "size",
            Self::Modified => "modified",
            Self::Extension => "extension",
        }
    }

    /// The next key, for [`crate::keys::Action::CycleSort`].
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

impl std::fmt::Display for SortKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// `[sync_browse]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SyncBrowse {
    pub enabled: bool,
    pub leader: Leader,
    pub on_missing: OnMissing,
    pub union_view: bool,
}

impl Default for SyncBrowse {
    fn default() -> Self {
        Self {
            enabled: false,
            leader: Leader::Focused,
            on_missing: OnMissing::Ghost,
            union_view: false,
        }
    }
}

/// Which pane's navigation drives the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Leader {
    Focused,
    Local,
    Remote,
}

/// What the follower pane does when the counterpart directory is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnMissing {
    /// Show a placeholder but remember the virtual path, so going back up
    /// re-couples the panes. The default, and the reason synchronized
    /// browsing here does not desync permanently.
    Ghost,
    /// Hold position and flag the panes as desynced.
    Stay,
    /// Create the directory.
    Create,
    /// Ask each time.
    Ask,
}

/// `[zide]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Zide {
    pub auto_open_bookmark: bool,
    pub r#match: ZideMatch,
}

impl Default for Zide {
    fn default() -> Self {
        Self {
            auto_open_bookmark: true,
            r#match: ZideMatch::Ancestor,
        }
    }
}

/// How the working directory is matched against bookmark local paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZideMatch {
    /// Exact match first, then the deepest `local_path` that is a prefix of
    /// the working directory.
    Ancestor,
    /// Only an exact match counts.
    Exact,
    Off,
}

/// `[compare]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Compare {
    pub method: CompareMethod,
    /// Timestamps differing by less than this are equal. Not zero by default:
    /// filesystems and protocols disagree about granularity (FAT stores 2s,
    /// some SFTP servers 1s), and a zero tolerance is the usual reason a sync
    /// wants to re-copy a tree that has not changed.
    pub mtime_tolerance_secs: u64,
    pub use_provider_hash: bool,
}

impl Default for Compare {
    fn default() -> Self {
        Self {
            method: CompareMethod::SizeMtime,
            mtime_tolerance_secs: 2,
            use_provider_hash: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareMethod {
    SizeMtime,
    Checksum,
    SizeOnly,
}

/// `[transfer]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Transfer {
    pub parallel: usize,
    /// KiB/s, 0 for unlimited.
    pub bandwidth_limit: u64,
    pub partial_suffix: String,
    pub resume: bool,
    pub preserve_mtime: bool,
    pub preserve_mode: bool,
}

impl Default for Transfer {
    fn default() -> Self {
        Self {
            parallel: 4,
            bandwidth_limit: 0,
            partial_suffix: ".lazysync-part".into(),
            resume: true,
            preserve_mtime: true,
            preserve_mode: true,
        }
    }
}

/// `[safety]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Safety {
    /// Where deletions go in mirror mode. Empty means real deletion.
    pub backup_dir: String,
    /// Abort rather than show a plan that would delete more than this
    /// fraction of the destination. The guard against a mistyped sync root.
    pub max_delete_fraction: f64,
    /// Abort above this many deletions. 0 disables.
    pub max_delete_files: usize,
    pub confirm_outside_root: bool,
    /// Suffix for the losing side of a conflict resolved with "keep both".
    ///
    /// The displaced file is renamed to `<name><suffix>` next to itself rather
    /// than deleted, so nothing is lost -- and the winner then propagates to
    /// both sides, which is what makes the pair *converge*. Keeping each
    /// side's own version at the original name would leave the two
    /// permanently different, and every later sync would raise the same
    /// conflict again.
    pub conflict_suffix: String,
}

impl Default for Safety {
    fn default() -> Self {
        Self {
            backup_dir: ".lazysync-backup".into(),
            max_delete_fraction: 0.25,
            max_delete_files: 0,
            confirm_outside_root: true,
            conflict_suffix: ".lazysync-conflict".into(),
        }
    }
}

/// `[confirm]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Confirm {
    pub fold_skipped: bool,
    pub warn_dest_newer: bool,
    pub always_show: Vec<String>,
}

impl Default for Confirm {
    fn default() -> Self {
        Self {
            fold_skipped: true,
            warn_dest_newer: true,
            always_show: ["create", "overwrite", "delete", "conflict"]
                .map(String::from)
                .to_vec(),
        }
    }
}

/// `[profiles.<name>]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Profile {
    pub mode: SyncMode,
    pub executor: Executor,
    pub exclude: Vec<String>,
    pub respect_gitignore: bool,
    pub auto_confirm: bool,
    pub watch: Watch,
    pub debounce_ms: u64,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            mode: SyncMode::Push,
            executor: Executor::Auto,
            exclude: Vec::new(),
            respect_gitignore: false,
            auto_confirm: false,
            watch: Watch::Off,
            debounce_ms: 2000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncMode {
    Push,
    Pull,
    MirrorPush,
    MirrorPull,
    Bisync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Executor {
    /// Pick the best available: rsync for ssh remotes when both ends have it,
    /// rclone for rclone remotes, native otherwise.
    Auto,
    Native,
    Rsync,
    Rclone,
}

impl Executor {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Native => "the native engine",
            Self::Rsync => "rsync",
            Self::Rclone => "rclone",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Watch {
    Off,
    /// Collect changes for `debounce_ms`, then show one plan.
    Batch,
    /// Apply without confirmation.
    Auto,
}

// ── Paths ───────────────────────────────────────────────────────────────────

/// `$XDG_CONFIG_HOME/lazysync`, or the dev override from `.envrc`.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LAZYSYNC_DEV_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lazysync")
}

/// `$XDG_STATE_HOME/lazysync`, for logs, session state and bisync snapshots.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LAZYSYNC_DEV_STATE_DIR") {
        return PathBuf::from(dir);
    }
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lazysync")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_equals_defaults() {
        let parsed: Config = toml::from_str("").unwrap();
        assert_eq!(parsed.chord_timeout_ms, Config::default().chord_timeout_ms);
        assert_eq!(parsed.sync_browse.on_missing, OnMissing::Ghost);
        assert_eq!(parsed.compare.mtime_tolerance_secs, 2);
    }

    /// The shipped example must stay loadable, or it documents a schema the
    /// parser does not accept.
    #[test]
    fn the_shipped_example_config_parses() {
        let text = include_str!("../../config.example.toml");
        let parsed: Config = toml::from_str(text)
            .expect("config.example.toml should parse against the current schema");

        // And it should describe the defaults it claims to.
        let defaults = Config::default();
        assert_eq!(parsed.chord_timeout_ms, defaults.chord_timeout_ms);
        assert_eq!(parsed.show_hidden, defaults.show_hidden);
        assert_eq!(parsed.dirs_first, defaults.dirs_first);
        assert_eq!(
            parsed.sync_browse.on_missing,
            defaults.sync_browse.on_missing
        );
        assert_eq!(parsed.zide.r#match, defaults.zide.r#match);
        assert_eq!(parsed.compare.method, defaults.compare.method);
        assert_eq!(
            parsed.compare.mtime_tolerance_secs,
            defaults.compare.mtime_tolerance_secs
        );
        assert_eq!(
            parsed.safety.max_delete_fraction,
            defaults.safety.max_delete_fraction
        );
        assert_eq!(
            parsed.transfer.partial_suffix,
            defaults.transfer.partial_suffix
        );
    }

    /// `lazysync config` exists so you can paste its output into
    /// `config.toml`. That is only true if it parses back to the same thing.
    #[test]
    fn a_dumped_config_parses_back_identically() {
        let original = Config::default();
        let rendered = toml::to_string_pretty(&original).unwrap();
        let round_tripped: Config = toml::from_str(&rendered)
            .unwrap_or_else(|e| panic!("dumped config did not parse: {e}\n{rendered}"));

        assert_eq!(round_tripped.sort, original.sort);
        assert_eq!(round_tripped.default_pane, original.default_pane);
        assert_eq!(round_tripped.chord_timeout_ms, original.chord_timeout_ms);
        assert_eq!(
            round_tripped.sync_browse.on_missing,
            original.sync_browse.on_missing
        );
        assert_eq!(round_tripped.compare.method, original.compare.method);
        assert_eq!(round_tripped.theme.preset, original.theme.preset);
    }

    /// And with a theme override in place, since colours have their own
    /// serializer.
    #[test]
    fn a_dumped_config_round_trips_theme_overrides() {
        let config: Config = toml::from_str(
            r##"
            [theme]
            preset = "nord"
            [theme.colors]
            border_focused = "#fabd2f"
            danger = "bright-red"
            "##,
        )
        .unwrap();

        let rendered = toml::to_string_pretty(&config).unwrap();
        let again: Config = toml::from_str(&rendered).unwrap();

        let before = config.theme.palette().unwrap();
        let after = again.theme.palette().unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn a_typo_in_a_key_is_an_error_rather_than_silence() {
        let err = toml::from_str::<Config>("max_delete_fraciton = 0.9")
            .expect_err("unknown keys must not be ignored");
        assert!(err.to_string().contains("max_delete_fraciton"), "{err}");
    }

    #[test]
    fn sort_spec_parses_a_reverse_prefix() {
        #[derive(Deserialize)]
        struct Wrapper {
            sort: SortSpec,
        }

        let w: Wrapper = toml::from_str(r#"sort = "-modified""#).unwrap();
        assert_eq!(w.sort.key, SortKey::Modified);
        assert!(w.sort.reverse);

        let w: Wrapper = toml::from_str(r#"sort = "size""#).unwrap();
        assert_eq!(w.sort.key, SortKey::Size);
        assert!(!w.sort.reverse);
    }

    #[test]
    fn an_unknown_sort_key_names_the_valid_ones() {
        let err = toml::from_str::<Config>(r#"sort = "mtime""#).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("mtime"), "{msg}");
        assert!(msg.contains("modified"), "{msg}");
    }

    #[test]
    fn keybindings_in_config_resolve_over_the_defaults() {
        let config: Config = toml::from_str(
            r#"
            [keys.browser]
            extra = [
                ["Quit", "x"],
                ["MoveTop", ["g", "h"]],
                ["Checksum", { key = "c", mod = ["Ctrl"] }],
            ]
            "#,
        )
        .unwrap();

        let maps = config.keymaps();
        let browser = maps.get(Mode::Browser);

        use crate::keys::{Action, Key, ModifiedKey, Resolution};
        assert_eq!(
            browser.resolve(&[ModifiedKey::plain(Key::Char('x'))]),
            Resolution::Exact(Action::Quit)
        );
        assert_eq!(
            browser.resolve(&[ModifiedKey::ctrl('c')]),
            Resolution::Exact(Action::Checksum)
        );
        // The default `j` survives an unrelated override.
        assert_eq!(
            browser.resolve(&[ModifiedKey::plain(Key::Char('j'))]),
            Resolution::Exact(Action::MoveDown)
        );
    }

    #[test]
    fn an_unknown_action_name_is_an_error() {
        let err = toml::from_str::<Config>(
            r#"
            [keys.browser]
            extra = [["Teleport", "t"]]
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Teleport"), "{err}");
    }

    #[test]
    fn an_unknown_mode_is_an_error() {
        let err = toml::from_str::<Config>(
            r#"
            [keys.browsr]
            extra = [["Quit", "q"]]
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("browsr"), "{err}");
    }
}
