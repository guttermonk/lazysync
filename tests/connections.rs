//! The bookmark list, the connection form, and zide auto-open.
//!
//! Driven through the real keymap, like `tests/browser.rs`: a key press goes
//! through `Dispatcher` and comes out as an action, so these cover the wiring
//! between keys, screens and the bookmark store. Nothing here touches the
//! network -- `tests/sftp.rs` does that against a real server.
//!
//! `LAZYSYNC_DEV_CONFIG_DIR` points the store at a temporary directory, so no
//! test can read or write the real `bookmarks.toml`.

use std::{fs, path::Path, sync::Mutex, time::Duration};

use lazysync::{
    app::{App, Overlay, Screen},
    bookmarks::{Bookmark, BookmarkStore},
    config::{Config, ZideMatch},
    connection::{AuthMethod, Protocol, Target},
    event::{Dispatch, Dispatcher},
    integrations::zide,
    keys::{Key, ModifiedKey},
};
use tempfile::TempDir;

/// `LAZYSYNC_DEV_CONFIG_DIR` is process-wide, so tests that set it take turns.
static CONFIG_DIR: Mutex<()> = Mutex::new(());

struct Harness {
    app: App,
    dispatcher: Dispatcher,
    _dir: TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Harness {
    /// An app on the bookmark screen, with `bookmarks` already saved.
    fn new(bookmarks: &[Bookmark]) -> Self {
        let guard = CONFIG_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();

        // SAFETY: the mutex above makes this the only thread touching the
        // variable for the lifetime of the harness.
        unsafe { std::env::set_var("LAZYSYNC_DEV_CONFIG_DIR", dir.path()) };
        unsafe { std::env::set_var("LAZYSYNC_DEV_STATE_DIR", dir.path().join("state")) };

        let mut store = BookmarkStore::default();
        for bookmark in bookmarks {
            store.upsert(bookmark.clone());
        }
        store.save_default().unwrap();

        let here = dir.path().to_path_buf();
        let mut app = App::new(Config::default(), here.clone(), here).unwrap();
        app.start_on_bookmarks();

        Self {
            dispatcher: Dispatcher::new(Duration::from_millis(500)),
            app,
            _dir: dir,
            _guard: guard,
        }
    }

    fn press(&mut self, keys: &str) {
        for ch in keys.chars() {
            self.key(ModifiedKey::plain(Key::Char(ch)));
        }
    }

    fn key(&mut self, key: ModifiedKey) {
        let mode = self.app.mode();
        match self.dispatcher.key(mode, &self.app.keymaps, key) {
            Dispatch::Act(action) => self.app.handle(action),
            Dispatch::Text(ch) => self.app.handle_text(ch),
            Dispatch::Backspace => self.app.handle_backspace(),
            Dispatch::Pending | Dispatch::Unbound => {}
        }
    }

    fn enter(&mut self) {
        self.key(ModifiedKey::plain(Key::Enter));
    }

    fn tab(&mut self) {
        self.key(ModifiedKey::plain(Key::Tab));
    }

    fn right(&mut self) {
        self.key(ModifiedKey::plain(Key::Right));
    }

    fn esc(&mut self) {
        self.key(ModifiedKey::plain(Key::Esc));
    }

    /// Move the form cursor onto a labelled field and type into it.
    fn fill(&mut self, label: &str, text: &str) {
        let index = self
            .app
            .form
            .as_ref()
            .expect("a form should be open")
            .fields
            .iter()
            .position(|f| f.label == label)
            .unwrap_or_else(|| panic!("no field labelled {label:?}"));

        // Tab down from the top, so the movement goes through the keymap.
        self.app.form.as_mut().unwrap().cursor = 0;
        for _ in 0..index {
            self.tab();
        }
        self.press(text);
    }

    /// What is on disk, which is what matters for a store that is written
    /// from the UI.
    fn saved(&self) -> BookmarkStore {
        BookmarkStore::load_default().unwrap()
    }
}

fn sftp(name: &str, host: &str) -> Bookmark {
    let mut target = Target::new(Protocol::Sftp, host);
    target.username = Some("greg".into());
    target.remote_path = Some("/srv/www".into());
    Bookmark::from_target(name, &target)
}

fn with_password(name: &str, host: &str) -> Bookmark {
    let mut bookmark = sftp(name, host);
    bookmark.auth = AuthMethod::Password;
    bookmark
}

#[test]
fn lazysync_can_open_on_the_bookmark_list() {
    let h = Harness::new(&[sftp("www", "box")]);
    assert_eq!(h.app.screen, Screen::Bookmarks);
    assert_eq!(h.app.visible_bookmarks().len(), 1);
}

#[test]
fn the_list_navigates_with_vim_keys() {
    let mut h = Harness::new(&[sftp("a", "box"), sftp("b", "nas"), sftp("c", "vps")]);

    h.press("j");
    assert_eq!(h.app.selected_bookmark().unwrap().name, "b");
    h.press("G");
    assert_eq!(h.app.selected_bookmark().unwrap().name, "c");
    h.press("gg");
    assert_eq!(h.app.selected_bookmark().unwrap().name, "a");
}

#[test]
fn n_opens_a_blank_form_and_esc_discards_it() {
    let mut h = Harness::new(&[]);

    h.press("n");
    assert_eq!(h.app.screen, Screen::Form);
    assert!(h.app.form.as_ref().unwrap().editing.is_none());

    h.esc();
    assert_eq!(h.app.screen, Screen::Bookmarks);
    assert!(h.app.form.is_none());
    assert!(h.saved().bookmarks.is_empty(), "discarding must not save");
}

/// The whole point of the form: connection details are entered there and
/// written to the store.
#[test]
fn a_new_connection_can_be_created_and_is_saved_to_disk() {
    let mut h = Harness::new(&[]);

    h.press("n");
    h.fill("Name", "www-prod");
    h.fill("Host", "box.example.com");
    h.fill("Port", "2222");
    h.fill("Username", "deploy");
    h.fill("Remote path", "/srv/www");
    h.fill("Local path", "/home/greg/proj");
    h.enter();

    assert_eq!(h.app.screen, Screen::Bookmarks);

    let saved = h.saved();
    let bookmark = saved.get("www-prod").expect("it should be on disk");
    assert_eq!(bookmark.host, "box.example.com");
    assert_eq!(bookmark.port, Some(2222));
    assert_eq!(bookmark.username.as_deref(), Some("deploy"));
    assert_eq!(bookmark.remote_path.as_deref(), Some(Path::new("/srv/www")));
    assert_eq!(
        bookmark.local_path.as_deref(),
        Some(Path::new("/home/greg/proj")),
        "the local path is the field termscp drops"
    );
}

#[test]
fn e_edits_the_selected_connection_and_saves_the_change() {
    let mut h = Harness::new(&[sftp("www", "box")]);

    h.press("e");
    assert_eq!(h.app.screen, Screen::Form);
    assert_eq!(h.app.form.as_ref().unwrap().editing.as_deref(), Some("www"));

    // Append to the host.
    h.fill("Host", ".example.com");
    h.enter();

    assert_eq!(h.saved().get("www").unwrap().host, "box.example.com");
    assert_eq!(h.saved().bookmarks.len(), 1, "editing must not duplicate");
}

/// A rename has to replace the entry, or the list grows a stale copy.
#[test]
fn renaming_a_connection_replaces_it_rather_than_duplicating() {
    let mut h = Harness::new(&[sftp("old", "box")]);

    h.press("e");
    // Clear the name, then type a new one.
    h.app.form.as_mut().unwrap().cursor = 0;
    for _ in 0..10 {
        h.key(ModifiedKey::plain(Key::Backspace));
    }
    h.press("new");
    h.enter();

    let saved = h.saved();
    assert_eq!(saved.bookmarks.len(), 1);
    assert!(saved.get("new").is_some());
    assert!(saved.get("old").is_none());
}

#[test]
fn the_form_refuses_to_save_without_a_name_or_host() {
    let mut h = Harness::new(&[]);

    h.press("n");
    h.enter();

    assert_eq!(h.app.screen, Screen::Form, "it should still be open");
    assert!(h.app.form.as_ref().unwrap().error.is_some());
    assert!(h.saved().bookmarks.is_empty());

    // Name alone is not enough either.
    h.fill("Name", "www");
    h.enter();
    assert_eq!(h.app.screen, Screen::Form);
    assert!(h
        .app
        .form
        .as_ref()
        .unwrap()
        .error
        .as_ref()
        .unwrap()
        .contains("host"));
}

/// Choice fields cycle with the arrow keys, because `h`/`l` and space have to
/// stay available for typing a path or a password.
#[test]
fn choice_fields_cycle_with_the_arrow_keys() {
    let mut h = Harness::new(&[]);

    h.press("n");
    h.fill("Name", "x");
    h.fill("Host", "box");

    // Move to Protocol and cycle it.
    h.app.form.as_mut().unwrap().cursor = 0;
    h.tab(); // Protocol
    h.right();

    h.enter();
    assert_eq!(h.saved().get("x").unwrap().protocol, Protocol::Scp);
}

/// Typing in a form must insert text rather than running browser commands.
#[test]
fn typed_characters_go_into_the_form_not_the_keymap() {
    let mut h = Harness::new(&[]);

    h.press("n");
    // `q` would quit in the browser and `S` would toggle sync browsing.
    h.fill("Name", "qS");

    assert!(!h.app.should_quit);
    assert_eq!(h.app.screen, Screen::Form);
    assert_eq!(
        h.app.form.as_ref().unwrap().fields[0].value,
        lazysync::ui::form::Value::Text("qS".into())
    );
}

#[test]
fn dd_asks_before_deleting_and_both_answers_act() {
    let mut h = Harness::new(&[sftp("www", "box"), sftp("nas", "nas")]);

    h.press("dd");
    assert!(
        matches!(h.app.overlay, Overlay::Question(_)),
        "deleting must ask: {:?}",
        h.app.overlay
    );

    // Decline.
    h.press("n");
    assert_eq!(h.saved().bookmarks.len(), 2, "no should not delete");

    // Accept.
    h.press("dd");
    h.press("y");
    let saved = h.saved();
    assert_eq!(saved.bookmarks.len(), 1);
    assert!(saved.get("www").is_none());
}

#[test]
fn slash_filters_the_list() {
    let mut h = Harness::new(&[sftp("www-prod", "box"), sftp("nas-backup", "nas")]);

    h.press("/");
    assert!(matches!(h.app.overlay, Overlay::Prompt(_)));
    h.press("nas");

    assert_eq!(h.app.visible_bookmarks().len(), 1);
    assert_eq!(h.app.selected_bookmark().unwrap().name, "nas-backup");

    h.enter(); // accept the filter
    assert_eq!(h.app.visible_bookmarks().len(), 1);

    // Esc with a filter set clears it rather than leaving the screen.
    h.esc();
    assert_eq!(h.app.visible_bookmarks().len(), 2);
    assert_eq!(h.app.screen, Screen::Bookmarks);
}

/// The filter matches hosts too, which is the only place a host is matched
/// without being displayed.
#[test]
fn the_filter_also_matches_the_host() {
    let mut h = Harness::new(&[sftp("one", "box.example.com"), sftp("two", "nas.lan")]);

    h.press("/");
    h.press("example");
    assert_eq!(h.app.visible_bookmarks().len(), 1);
    assert_eq!(h.app.selected_bookmark().unwrap().name, "one");
}

#[test]
fn esc_on_an_unfiltered_list_goes_to_the_local_browser() {
    let mut h = Harness::new(&[sftp("www", "box")]);

    h.esc();
    assert_eq!(
        h.app.screen,
        Screen::Browser,
        "with no connections usable, a local file manager is still useful"
    );
}

#[test]
fn an_empty_store_says_what_to_do() {
    let mut h = Harness::new(&[]);

    h.enter();
    assert!(
        h.app.status.contains('n'),
        "opening nothing should point at the key that adds one: {:?}",
        h.app.status
    );
}

/// Opening a password-auth bookmark must not prompt before the host key is
/// settled -- here the host does not resolve, so it fails at the check and
/// never asks.
#[test]
fn a_password_is_not_requested_before_the_host_is_verified() {
    let mut h = Harness::new(&[with_password("nas", "nonexistent.invalid")]);

    h.enter();

    assert!(
        !matches!(h.app.overlay, Overlay::Prompt(_)),
        "it must not ask for a password for a server it cannot verify: {:?}",
        h.app.overlay
    );
    assert!(h.app.status.contains("could not reach"), "{}", h.app.status);
}

/// A protocol that cannot connect yet must be refused by name.
#[test]
fn an_unimplemented_protocol_is_refused_with_its_phase() {
    let mut bookmark = sftp("bucket", "my-bucket");
    bookmark.protocol = Protocol::S3;

    let mut h = Harness::new(&[bookmark]);
    h.enter();

    assert!(h.app.status.contains("s3"), "{}", h.app.status);
    assert!(h.app.status.contains("phase 6"), "{}", h.app.status);
}

// ── zide auto-open ──────────────────────────────────────────────────────────

/// The popup case: lazysync is opened inside a bookmark's local path.
#[test]
fn the_working_directory_selects_a_bookmark() {
    let mut store = BookmarkStore::default();
    let mut bookmark = sftp("proj", "box");
    bookmark.local_path = Some("/home/greg/proj".into());
    store.upsert(bookmark);

    let (found, matched) =
        zide::single(&store, Path::new("/home/greg/proj"), ZideMatch::Ancestor).unwrap();

    assert_eq!(found.name, "proj");
    assert!(matched.is_exact());
}

/// And the reason ancestor matching exists: opening the popup from a
/// subdirectory should find the bookmark *and* say where to land.
#[test]
fn a_subdirectory_selects_the_bookmark_and_reports_the_subpath() {
    let mut store = BookmarkStore::default();
    let mut bookmark = sftp("proj", "box");
    bookmark.local_path = Some("/home/greg/proj".into());
    store.upsert(bookmark);

    let (found, matched) = zide::single(
        &store,
        Path::new("/home/greg/proj/src/api"),
        ZideMatch::Ancestor,
    )
    .unwrap();

    assert_eq!(found.name, "proj");
    assert_eq!(matched.relative, Path::new("src/api"));
    assert!(!matched.is_exact());
}

#[test]
fn auto_open_can_be_turned_off() {
    let mut store = BookmarkStore::default();
    let mut bookmark = sftp("proj", "box");
    bookmark.local_path = Some("/home/greg/proj".into());
    store.upsert(bookmark);

    assert!(zide::single(&store, Path::new("/home/greg/proj"), ZideMatch::Off).is_none());
}

/// A bookmarks file written by hand must load, since that is how someone adds
/// a `local_path` to an existing set.
#[test]
fn a_hand_written_bookmarks_file_loads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bookmarks.toml");
    fs::write(
        &path,
        r#"
        [[bookmark]]
        name = "www-prod"
        protocol = "sftp"
        host = "box.example.com"
        username = "greg"
        auth = "agent"
        remote_path = "/srv/www"
        local_path = "/home/greg/proj"
        sync_browse = true
        "#,
    )
    .unwrap();

    let store = BookmarkStore::load(&path).unwrap();
    let bookmark = store.get("www-prod").unwrap();

    assert_eq!(bookmark.host, "box.example.com");
    assert_eq!(
        bookmark.local_path.as_deref(),
        Some(Path::new("/home/greg/proj"))
    );
    assert!(bookmark.sync_browse);
    assert!(
        !bookmark.needs_password_prompt(),
        "agent auth should open without asking for anything"
    );
}
