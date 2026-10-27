//! Saved connections.
//!
//! A bookmark stores **everything except the password**, which is the point of
//! difference from termscp: it keeps the local working directory and a sync
//! profile reference as well as the connection details, so opening a bookmark
//! restores both panes rather than just the remote one.
//!
//! The connection fields are declared here rather than flattened in from
//! [`Target`]. That is deliberate: `#[serde(flatten)]` is incompatible with
//! `deny_unknown_fields`, and this is a file people hand-edit to add a
//! `local_path`, so a silently ignored typo is worth avoiding more than the
//! duplication costs. [`Bookmark::target`] and [`Bookmark::set_target`] are
//! the only places the two representations meet, and a round-trip test pins
//! them together.

pub mod secrets;

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::connection::{AuthMethod, Protocol, Target};

/// One saved connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bookmark {
    /// What the bookmark list shows, and the only thing shown when opening it.
    pub name: String,

    // ── Connection, mirroring `Target` ───────────────────────────────────
    pub protocol: Protocol,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default)]
    pub auth: AuthMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<PathBuf>,

    // ── The parts termscp does not keep ──────────────────────────────────
    /// Local working directory, opened in the left pane.
    ///
    /// Also what zide auto-open matches the invocation directory against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<PathBuf>,

    /// Name of a `[profiles.<name>]` entry in `config.toml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_profile: Option<String>,

    /// Turn synchronized browsing on when this bookmark opens.
    #[serde(default)]
    pub sync_browse: bool,

    /// Whether the password may be kept in the OS keyring.
    ///
    /// Opt-in, and false by default: storing a secret is a decision the user
    /// should make per host, not a side effect of saving a bookmark.
    #[serde(default)]
    pub remember_password: bool,

    /// Supplied by lazysync rather than by the user, and never written to
    /// disk.
    ///
    /// `skip` rather than `skip_serializing_if`: it must not round-trip at
    /// all. If it were ever read back from `bookmarks.toml`, deleting the
    /// built-in entry would become possible by hand-editing a flag, and the
    /// file would carry an entry that the next version might define
    /// differently.
    #[serde(skip)]
    pub builtin: bool,
}

impl Bookmark {
    /// The name of the built-in local entry.
    ///
    /// A constant because three places need to agree on it: the entry itself,
    /// the check that stops a user's bookmark colliding with it, and the
    /// refusal to delete it.
    pub const LOCAL_TRANSFER: &'static str = "Local transfer";

    /// The built-in entry for syncing two directories on this machine.
    ///
    /// Esc on the connection list used to drop straight into a two-pane
    /// browser. That browser was useful -- `~/photos` to a mounted backup
    /// drive is a real sync, and needs no server -- but arriving there was
    /// not: nothing had been chosen, nothing said what the screen was, and
    /// the right-hand pane was captioned `remote` while listing local files.
    ///
    /// Making it an entry in the list fixes the category error. It is a thing
    /// you *open*, like every other row, so it can be read before it is used,
    /// it inherits the whole screen's conventions, and the way back out is
    /// the way back out of any connection.
    ///
    /// It is not written to `bookmarks.toml` and cannot be deleted, because
    /// "my machine" is not a connection someone configured -- it is always
    /// there, and an empty connection list should still be able to do
    /// something.
    pub fn local_transfer() -> Self {
        Self {
            name: Self::LOCAL_TRANSFER.to_string(),
            protocol: Protocol::Local,
            host: String::new(),
            port: None,
            username: None,
            auth: AuthMethod::default(),
            identity_file: None,
            // Both sides start at home. Not the invocation directory: this
            // entry is reached from the connection list, where the directory
            // lazysync happened to be launched in is not what you are asking
            // for -- and a sync tool opening somewhere arbitrary on both
            // sides is how the wrong pair of trees gets compared.
            remote_path: None,
            local_path: None,
            sync_profile: None,
            sync_browse: false,
            remember_password: false,
            builtin: true,
        }
    }

    /// A new bookmark for `target`.
    pub fn from_target(name: impl Into<String>, target: &Target) -> Self {
        let mut bookmark = Self {
            name: name.into(),
            protocol: target.protocol,
            host: String::new(),
            port: None,
            username: None,
            auth: AuthMethod::default(),
            identity_file: None,
            remote_path: None,
            local_path: None,
            sync_profile: None,
            sync_browse: false,
            remember_password: false,
            builtin: false,
        };
        bookmark.set_target(target);
        bookmark
    }

    /// The connection this bookmark describes.
    pub fn target(&self) -> Target {
        Target {
            protocol: self.protocol,
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            auth: self.auth,
            identity_file: self.identity_file.clone(),
            remote_path: self.remote_path.clone(),
        }
    }

    /// Replace the connection fields, leaving name, paths and profile alone.
    pub fn set_target(&mut self, target: &Target) {
        self.protocol = target.protocol;
        self.host = target.host.clone();
        self.port = target.port;
        self.username = target.username.clone();
        self.auth = target.auth;
        self.identity_file = target.identity_file.clone();
        self.remote_path = target.remote_path.clone();
    }

    /// The keyring entry this bookmark's password lives under.
    pub fn secret_key(&self) -> String {
        secrets::key_for(&self.name)
    }

    /// Whether opening this bookmark has to ask for anything.
    ///
    /// The answer is usually no, which is the whole reason the open screen is
    /// just a name: agent and key auth need no secret, and a remembered
    /// password comes from the keyring.
    pub fn needs_password_prompt(&self) -> bool {
        self.auth.needs_secret_upfront() && !self.remember_password
    }
}

/// `bookmarks.toml`.
///
/// Separate from `config.toml` because lazysync writes to this one from the
/// UI, and round-tripping a hand-commented config through a serializer would
/// eat the comments.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BookmarkStore {
    /// Saved connections, in display order.
    #[serde(rename = "bookmark")]
    pub bookmarks: Vec<Bookmark>,

    /// Connections opened by URL rather than saved, newest first.
    ///
    /// Kept separate from bookmarks, as termscp does -- but unlike termscp
    /// these keep their local path too, so promoting a recent to a bookmark
    /// does not lose it.
    #[serde(rename = "recent")]
    pub recents: Vec<Bookmark>,
}

/// How many recent connections to keep.
///
/// Enough to cover "the thing I was doing yesterday" without the list
/// becoming something you have to scroll.
pub const MAX_RECENTS: usize = 10;

impl BookmarkStore {
    /// Load from `path`, or return an empty store if it does not exist.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };

        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn load_default() -> Result<Self> {
        Self::load(&path())
    }

    /// Write the store back.
    ///
    /// Via a temporary file and a rename, so an interrupted write cannot leave
    /// a truncated `bookmarks.toml` -- which would lose every saved
    /// connection at once.
    ///
    /// **A symlink is written *through*, not replaced.** `rename(2)` onto a
    /// symlink replaces the link itself, which is wrong wherever something
    /// else owns this file: a dotfiles repo symlink would be silently turned
    /// into a regular file, and a home-manager symlink into the Nix store
    /// would be clobbered -- with the breakage surfacing later, as a failed
    /// `home-manager switch` complaining about an unexpected file.
    ///
    /// Resolving the link first means the write lands where the user pointed
    /// it, and where that target is read-only -- which is exactly the
    /// declaratively-managed case -- it fails here, with a message saying so,
    /// instead of appearing to work.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }

        // `read_link` rather than `canonicalize`: the target may not exist
        // yet, and canonicalize fails in that case.
        let target = match fs::read_link(path) {
            Ok(dest) if dest.is_absolute() => dest,
            // A relative symlink resolves against the link's own directory.
            Ok(dest) => path.parent().unwrap_or(Path::new(".")).join(dest),
            // Not a symlink, or does not exist yet.
            Err(_) => path.to_path_buf(),
        };

        if target != path && is_read_only(&target) {
            anyhow::bail!(
                "{} is a symlink to {}, which is not writable -- your bookmarks are managed \
                 declaratively (home-manager, or similar), so add or edit them there rather \
                 than here",
                path.display(),
                target.display()
            );
        }

        // Built-in entries are lazysync's, not the user's. Writing one out
        // would put an entry in their file that a later version might define
        // differently, and that they would then be unable to get rid of.
        let persisted = Self {
            bookmarks: self
                .bookmarks
                .iter()
                .filter(|b| !b.builtin)
                .cloned()
                .collect(),
            recents: self.recents.clone(),
        };
        let rendered = toml::to_string_pretty(&persisted).context("rendering bookmarks as TOML")?;

        // The temporary file goes beside the *target*, because a rename cannot
        // cross filesystems and the target may be somewhere else entirely.
        let temporary = target.with_extension("toml.tmp");
        fs::write(&temporary, &rendered)
            .with_context(|| format!("writing {}", temporary.display()))?;
        fs::rename(&temporary, &target)
            .with_context(|| format!("replacing {}", target.display()))?;

        Ok(())
    }

    pub fn save_default(&self) -> Result<()> {
        self.save(&path())
    }

    pub fn get(&self, name: &str) -> Option<&Bookmark> {
        self.bookmarks.iter().find(|b| b.name == name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Bookmark> {
        self.bookmarks.iter_mut().find(|b| b.name == name)
    }

    /// Add a bookmark, or replace one of the same name.
    ///
    /// Replacing rather than appending: two bookmarks with one name would make
    /// `lazysync open <name>` ambiguous and the list confusing.
    pub fn upsert(&mut self, bookmark: Bookmark) {
        match self.bookmarks.iter_mut().find(|b| b.name == bookmark.name) {
            Some(existing) => *existing = bookmark,
            None => self.bookmarks.push(bookmark),
        }
    }

    /// Remove a bookmark, returning it.
    ///
    /// Built-in entries are not removable, and this is the single place that
    /// enforces it rather than a check at the one call site that exists
    /// today: a store that can be emptied of the local entry by any future
    /// caller is a store where the invariant is a convention.
    pub fn remove(&mut self, name: &str) -> Option<Bookmark> {
        let index = self
            .bookmarks
            .iter()
            .position(|b| b.name == name && !b.builtin)?;
        Some(self.bookmarks.remove(index))
    }

    /// Put the built-in entries in place.
    ///
    /// Last, not first. The cursor opens on row 0, and the entry you least
    /// often want should not be the one sitting under it every launch --
    /// nor should adding a built-in row push everybody's real connections
    /// down by one.
    ///
    /// Called after loading, so a `bookmarks.toml` that happens to contain a
    /// connection named "Local transfer" -- written by hand, or by an older
    /// version -- cannot shadow or duplicate this one.
    pub fn add_builtins(&mut self) {
        self.bookmarks
            .retain(|b| b.name != Bookmark::LOCAL_TRANSFER);
        self.bookmarks.push(Bookmark::local_transfer());
    }

    /// Record a connection that was opened without being saved.
    pub fn push_recent(&mut self, bookmark: Bookmark) {
        // Dedupe on where it points, not on the generated name: connecting to
        // the same host twice should move one entry to the top rather than
        // filling the list with near-duplicates.
        let target = bookmark.target();
        self.recents.retain(|r| r.target() != target);
        self.recents.insert(0, bookmark);
        self.recents.truncate(MAX_RECENTS);
    }

    /// A name not already in use, derived from `base`.
    pub fn unique_name(&self, base: &str) -> String {
        if self.get(base).is_none() {
            return base.to_owned();
        }
        (2..)
            .map(|n| format!("{base} ({n})"))
            .find(|candidate| self.get(candidate).is_none())
            // The range is unbounded, so some candidate always succeeds.
            .unwrap_or_else(|| base.to_owned())
    }
}

/// `$XDG_CONFIG_HOME/lazysync/bookmarks.toml`.
pub fn path() -> PathBuf {
    crate::config::config_dir().join("bookmarks.toml")
}

/// Whether a path is somewhere lazysync cannot write.
///
/// Checked by permission rather than by looking for `/nix/store` in the path:
/// the Nix store is the common case but not the only one, and a hard-coded
/// path prefix would miss a read-only bind mount and would be wrong on a
/// system where the store lives elsewhere.
fn is_read_only(path: &Path) -> bool {
    // A missing file is not read-only -- it is about to be created, and what
    // matters then is the directory.
    let Some(parent) = path.parent() else {
        return false;
    };

    match std::fs::metadata(parent) {
        Ok(metadata) => metadata.permissions().readonly(),
        // Cannot tell. Treated as writable so the attempt goes ahead and
        // reports whatever the real failure turns out to be, rather than
        // refusing on a guess.
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sftp(name: &str, host: &str) -> Bookmark {
        let mut target = Target::new(Protocol::Sftp, host);
        target.username = Some("greg".into());
        target.remote_path = Some(PathBuf::from("/srv/www"));

        let mut bookmark = Bookmark::from_target(name, &target);
        bookmark.local_path = Some(PathBuf::from("/home/greg/proj"));
        bookmark
    }

    /// The headline difference from termscp, so it is asserted rather than
    /// assumed.
    #[test]
    fn a_bookmark_keeps_the_local_path() {
        let rendered = toml::to_string(&sftp("www", "box")).unwrap();
        assert!(rendered.contains("local_path"), "{rendered}");
        assert!(rendered.contains("/home/greg/proj"), "{rendered}");
    }

    /// And the thing it must never keep.
    #[test]
    fn a_bookmark_never_serializes_a_password() {
        let mut bookmark = sftp("www", "box");
        bookmark.auth = AuthMethod::Password;
        bookmark.remember_password = true;

        let rendered = toml::to_string(&bookmark).unwrap();

        // Matched per line rather than as a substring: `remember_password =
        // true` contains "password =", so a naive `contains` check here
        // passes for the wrong reason and would keep passing if a real
        // password field were ever added.
        let secret_field = rendered
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("password") || line.starts_with("secret"));
        assert_eq!(secret_field, None, "{rendered}");

        // The *flag* is stored; the secret is not.
        assert!(rendered.contains("remember_password = true"), "{rendered}");
    }

    #[test]
    fn target_round_trips_through_a_bookmark() {
        let mut target = Target::new(Protocol::Scp, "box.example.com");
        target.port = Some(2222);
        target.username = Some("deploy".into());
        target.auth = AuthMethod::Key;
        target.identity_file = Some(PathBuf::from("/home/greg/.ssh/id_ed25519"));
        target.remote_path = Some(PathBuf::from("/var/www"));

        let bookmark = Bookmark::from_target("prod", &target);
        assert_eq!(
            bookmark.target(),
            target,
            "set_target and target must stay in step, since the fields are declared twice"
        );
    }

    #[test]
    fn set_target_leaves_the_local_fields_alone() {
        let mut bookmark = sftp("www", "box");
        bookmark.sync_profile = Some("deploy".into());
        bookmark.sync_browse = true;

        let mut replacement = Target::new(Protocol::Sftp, "newbox");
        replacement.username = Some("root".into());
        bookmark.set_target(&replacement);

        assert_eq!(bookmark.host, "newbox");
        assert_eq!(bookmark.local_path, Some(PathBuf::from("/home/greg/proj")));
        assert_eq!(bookmark.sync_profile.as_deref(), Some("deploy"));
        assert!(bookmark.sync_browse);
    }

    #[test]
    fn a_store_round_trips_through_toml() {
        let mut store = BookmarkStore::default();
        store.upsert(sftp("www", "box"));
        store.upsert(sftp("backup", "nas"));

        let rendered = toml::to_string_pretty(&store).unwrap();
        let parsed: BookmarkStore = toml::from_str(&rendered).unwrap();

        assert_eq!(parsed, store);
        assert!(rendered.contains("[[bookmark]]"), "{rendered}");
    }

    #[test]
    fn a_missing_file_is_an_empty_store_not_an_error() {
        let store = BookmarkStore::load(Path::new("/definitely/not/here.toml")).unwrap();
        assert!(store.bookmarks.is_empty());
    }

    #[test]
    fn a_malformed_file_names_itself() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.toml");
        fs::write(&path, "this is not toml {{{").unwrap();

        let err = BookmarkStore::load(&path).unwrap_err();
        assert!(err.to_string().contains("bookmarks.toml"), "{err}");
    }

    #[test]
    fn a_typo_in_a_bookmark_is_reported() {
        let err = toml::from_str::<BookmarkStore>(
            r#"
            [[bookmark]]
            name = "www"
            protocol = "sftp"
            host = "box"
            local_pth = "/home/greg/proj"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("local_pth"), "{err}");
    }

    #[test]
    fn saving_and_loading_a_store_preserves_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("bookmarks.toml");

        let mut store = BookmarkStore::default();
        store.upsert(sftp("www", "box"));
        store.save(&path).unwrap();

        assert_eq!(BookmarkStore::load(&path).unwrap(), store);
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "the temporary file should have been renamed away"
        );
    }

    #[test]
    fn upsert_replaces_rather_than_duplicating_a_name() {
        let mut store = BookmarkStore::default();
        store.upsert(sftp("www", "box"));
        store.upsert(sftp("www", "newbox"));

        assert_eq!(store.bookmarks.len(), 1);
        assert_eq!(store.get("www").unwrap().host, "newbox");
    }

    #[test]
    fn remove_takes_the_bookmark_out() {
        let mut store = BookmarkStore::default();
        store.upsert(sftp("www", "box"));

        assert_eq!(store.remove("www").unwrap().host, "box");
        assert!(store.get("www").is_none());
        assert!(store.remove("www").is_none());
    }

    /// Reconnecting to the same host should move one entry up, not add a
    /// near-duplicate.
    #[test]
    fn recents_dedupe_on_the_connection_not_the_name() {
        let mut store = BookmarkStore::default();
        store.push_recent(sftp("one", "box"));
        store.push_recent(sftp("two", "nas"));
        store.push_recent(sftp("three", "box"));

        assert_eq!(store.recents.len(), 2);
        assert_eq!(store.recents[0].host, "box");
        assert_eq!(store.recents[1].host, "nas");
    }

    #[test]
    fn recents_are_capped() {
        let mut store = BookmarkStore::default();
        for i in 0..MAX_RECENTS + 5 {
            store.push_recent(sftp(&format!("n{i}"), &format!("host{i}")));
        }
        assert_eq!(store.recents.len(), MAX_RECENTS);
        // Newest first.
        assert_eq!(store.recents[0].host, format!("host{}", MAX_RECENTS + 4));
    }

    #[test]
    fn unique_name_avoids_collisions() {
        let mut store = BookmarkStore::default();
        assert_eq!(store.unique_name("box"), "box");

        store.upsert(sftp("box", "box"));
        assert_eq!(store.unique_name("box"), "box (2)");

        store.upsert(sftp("box (2)", "box"));
        assert_eq!(store.unique_name("box"), "box (3)");
    }

    /// The open screen shows only a name, which is only honest if agent and
    /// key auth really do skip the prompt.
    #[test]
    fn only_password_auth_prompts() {
        let mut bookmark = sftp("www", "box");

        bookmark.auth = AuthMethod::Agent;
        assert!(!bookmark.needs_password_prompt());

        bookmark.auth = AuthMethod::Key;
        assert!(!bookmark.needs_password_prompt());

        bookmark.auth = AuthMethod::Password;
        assert!(bookmark.needs_password_prompt());

        // ...unless it is in the keyring.
        bookmark.remember_password = true;
        assert!(!bookmark.needs_password_prompt());
    }
}
