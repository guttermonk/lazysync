//! The connection form.
//!
//! This is the *only* screen that shows a connection's details. The bookmark
//! list shows a name; opening a bookmark shows a name and, if it needs one, a
//! password prompt. Host, port, username, key path and remote path appear
//! here and nowhere else.
//!
//! The form is a flat list of typed fields rather than a widget tree, because
//! what it has to do is narrow -- edit one value at a time, validate on save
//! -- and a list keeps the keymap honest: there is one cursor, and `Tab` moves
//! it.

use std::path::PathBuf;

use crate::{
    bookmarks::Bookmark,
    connection::{AuthMethod, Protocol, Target},
};

/// Which field of the form a row edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldId {
    Name,
    Protocol,
    Host,
    Port,
    Username,
    Auth,
    IdentityFile,
    RemotePath,
    LocalPath,
    SyncProfile,
    SyncBrowse,
    RememberPassword,
}

/// What a field holds, which decides how keys reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Free text. Printable keys insert; backspace deletes.
    Text(String),
    /// One of a fixed set, cycled with the arrow keys.
    Choice {
        options: Vec<String>,
        selected: usize,
    },
    /// A flag, toggled with the arrow keys.
    Flag(bool),
}

impl Value {
    fn as_text(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::Choice { options, selected } => options.get(*selected).map_or("", String::as_str),
            Self::Flag(true) => "yes",
            Self::Flag(false) => "no",
        }
    }

    /// Whether typed characters go into this field.
    const fn takes_text(&self) -> bool {
        matches!(self, Self::Text(_))
    }

    fn cycle(&mut self, forward: bool) {
        match self {
            Self::Text(_) => {}
            Self::Choice { options, selected } => {
                if options.is_empty() {
                    return;
                }
                let len = options.len();
                *selected = if forward {
                    (*selected + 1) % len
                } else {
                    (*selected + len - 1) % len
                };
            }
            Self::Flag(flag) => *flag = !*flag,
        }
    }
}

/// One row of the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub id: FieldId,
    pub label: &'static str,
    pub value: Value,
    /// Shown dimmed when the field is empty, to say what would be used.
    pub placeholder: &'static str,
    /// One line of explanation under the cursor.
    pub hint: &'static str,
}

/// The form, as edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionForm {
    pub fields: Vec<Field>,
    pub cursor: usize,
    /// Name of the bookmark being edited, or `None` when adding.
    ///
    /// Kept so that renaming an existing bookmark replaces it rather than
    /// leaving the old name behind as a duplicate.
    pub editing: Option<String>,
    /// Validation message from the last failed save.
    pub error: Option<String>,
}

impl ConnectionForm {
    /// A blank form for a new connection.
    pub fn new() -> Self {
        Self::from_bookmark(&Bookmark::from_target("", &Target::new(Protocol::Sftp, "")))
            .with_editing(None)
    }

    /// A form populated from an existing bookmark.
    pub fn edit(bookmark: &Bookmark) -> Self {
        Self::from_bookmark(bookmark).with_editing(Some(bookmark.name.clone()))
    }

    fn with_editing(mut self, editing: Option<String>) -> Self {
        self.editing = editing;
        self
    }

    fn from_bookmark(bookmark: &Bookmark) -> Self {
        let protocols: Vec<String> = Protocol::ALL
            .iter()
            .map(|p| p.scheme().to_owned())
            .collect();
        let protocol_index = Protocol::ALL
            .iter()
            .position(|p| *p == bookmark.protocol)
            .unwrap_or(0);

        let auths: Vec<String> = AuthMethod::ALL
            .iter()
            .map(|a| a.name().to_owned())
            .collect();
        let auth_index = AuthMethod::ALL
            .iter()
            .position(|a| *a == bookmark.auth)
            .unwrap_or(0);

        let fields = vec![
            Field {
                id: FieldId::Name,
                label: "Name",
                value: Value::Text(bookmark.name.clone()),
                placeholder: "required",
                hint: "What the bookmark list shows. The only thing shown when you open it.",
            },
            Field {
                id: FieldId::Protocol,
                label: "Protocol",
                value: Value::Choice {
                    options: protocols,
                    selected: protocol_index,
                },
                placeholder: "",
                hint: "sftp and scp work today; the rest arrive in phase 5.",
            },
            Field {
                id: FieldId::Host,
                label: "Host",
                value: Value::Text(bookmark.host.clone()),
                placeholder: "required",
                hint: "A name from ~/.ssh/config works, and brings its settings with it.",
            },
            Field {
                id: FieldId::Port,
                label: "Port",
                value: Value::Text(bookmark.port.map(|p| p.to_string()).unwrap_or_default()),
                placeholder: "default for the protocol",
                hint: "Leave empty unless the server uses a non-standard port.",
            },
            Field {
                id: FieldId::Username,
                label: "Username",
                value: Value::Text(bookmark.username.clone().unwrap_or_default()),
                placeholder: "from ~/.ssh/config, then your login name",
                hint: "Leave empty to let ssh decide.",
            },
            Field {
                id: FieldId::Auth,
                label: "Auth",
                value: Value::Choice {
                    options: auths,
                    selected: auth_index,
                },
                placeholder: "",
                hint: "agent needs no secret at all, which is why it is the default.",
            },
            Field {
                id: FieldId::IdentityFile,
                label: "Key file",
                value: Value::Text(
                    bookmark
                        .identity_file
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                ),
                placeholder: "from ~/.ssh/config",
                hint: "Only for `key` auth. An agent-loaded key needs nothing here.",
            },
            Field {
                id: FieldId::RemotePath,
                label: "Remote path",
                value: Value::Text(
                    bookmark
                        .remote_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                ),
                placeholder: "your remote home directory",
                hint: "Opened in the right pane.",
            },
            Field {
                id: FieldId::LocalPath,
                label: "Local path",
                value: Value::Text(
                    bookmark
                        .local_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                ),
                placeholder: "the working directory",
                hint: "Opened in the left pane, and matched by zide auto-open.",
            },
            Field {
                id: FieldId::SyncProfile,
                label: "Sync profile",
                value: Value::Text(bookmark.sync_profile.clone().unwrap_or_default()),
                placeholder: "none",
                hint: "A [profiles.<name>] entry in config.toml.",
            },
            Field {
                id: FieldId::SyncBrowse,
                label: "Sync browsing",
                value: Value::Flag(bookmark.sync_browse),
                placeholder: "",
                hint: "Turn synchronized browsing on when this connection opens.",
            },
            Field {
                id: FieldId::RememberPassword,
                label: "Remember password",
                value: Value::Flag(bookmark.remember_password),
                placeholder: "",
                hint: "Store it in the system keyring. Only relevant for `password` auth.",
            },
        ];

        Self {
            fields,
            cursor: 0,
            editing: None,
            error: None,
        }
    }

    pub fn focused(&self) -> &Field {
        // The field list is never empty, and the cursor is clamped on every
        // move, so this cannot miss.
        &self.fields[self.cursor.min(self.fields.len() - 1)]
    }

    fn focused_mut(&mut self) -> &mut Field {
        let index = self.cursor.min(self.fields.len() - 1);
        &mut self.fields[index]
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.fields.len() - 1;
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    /// Whether typed characters currently go into the form.
    pub fn takes_text(&self) -> bool {
        self.focused().value.takes_text()
    }

    pub fn insert(&mut self, ch: char) {
        if let Value::Text(text) = &mut self.focused_mut().value {
            text.push(ch);
        }
    }

    pub fn backspace(&mut self) {
        if let Value::Text(text) = &mut self.focused_mut().value {
            text.pop();
        }
    }

    pub fn cycle(&mut self, forward: bool) {
        self.focused_mut().value.cycle(forward);
    }

    fn text(&self, id: FieldId) -> &str {
        self.fields
            .iter()
            .find(|f| f.id == id)
            .map_or("", |f| f.value.as_text())
    }

    fn flag(&self, id: FieldId) -> bool {
        self.fields
            .iter()
            .find(|f| f.id == id)
            .is_some_and(|f| matches!(f.value, Value::Flag(true)))
    }

    /// Whether a field is relevant given the chosen auth method.
    ///
    /// Irrelevant fields are dimmed rather than hidden: a form whose rows move
    /// around as you change a choice above them is disorienting, and the value
    /// is still there if you switch back.
    pub fn is_relevant(&self, id: FieldId) -> bool {
        let auth = AuthMethod::ALL
            .get(
                self.fields
                    .iter()
                    .find(|f| f.id == FieldId::Auth)
                    .and_then(|f| match &f.value {
                        Value::Choice { selected, .. } => Some(*selected),
                        _ => None,
                    })
                    .unwrap_or(0),
            )
            .copied()
            .unwrap_or_default();

        match id {
            FieldId::IdentityFile => matches!(auth, AuthMethod::Key),
            FieldId::RememberPassword => matches!(auth, AuthMethod::Password),
            _ => true,
        }
    }

    /// Validate and build a bookmark.
    pub fn to_bookmark(&self) -> Result<Bookmark, String> {
        let name = self.text(FieldId::Name).trim();
        if name.is_empty() {
            return Err("a connection needs a name".into());
        }

        let host = self.text(FieldId::Host).trim();
        if host.is_empty() {
            return Err("a connection needs a host".into());
        }

        let protocol = Protocol::from_scheme(self.text(FieldId::Protocol))
            .ok_or_else(|| "unknown protocol".to_string())?;

        let port = match self.text(FieldId::Port).trim() {
            "" => None,
            raw => Some(
                raw.parse::<u16>()
                    .map_err(|_| format!("{raw:?} is not a port number"))?,
            ),
        };

        let auth = AuthMethod::ALL
            .iter()
            .find(|a| a.name() == self.text(FieldId::Auth))
            .copied()
            .unwrap_or_default();

        if matches!(auth, AuthMethod::Key) && self.text(FieldId::IdentityFile).trim().is_empty() {
            // Not fatal: ~/.ssh/config or the agent may supply it. Saved as-is
            // and the connect path reports it if nothing works.
            tracing::debug!("{name}: key auth with no identity file; relying on ssh config");
        }

        let mut target = Target::new(protocol, host);
        target.port = port;
        target.username = non_empty(self.text(FieldId::Username));
        target.auth = auth;
        target.identity_file = non_empty(self.text(FieldId::IdentityFile)).map(PathBuf::from);
        target.remote_path = non_empty(self.text(FieldId::RemotePath)).map(PathBuf::from);

        let mut bookmark = Bookmark::from_target(name, &target);
        bookmark.local_path = non_empty(self.text(FieldId::LocalPath)).map(PathBuf::from);
        bookmark.sync_profile = non_empty(self.text(FieldId::SyncProfile));
        bookmark.sync_browse = self.flag(FieldId::SyncBrowse);
        bookmark.remember_password = self.flag(FieldId::RememberPassword);

        Ok(bookmark)
    }
}

impl Default for ConnectionForm {
    fn default() -> Self {
        Self::new()
    }
}

fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(form: &mut ConnectionForm, id: FieldId, text: &str) {
        let field = form.fields.iter_mut().find(|f| f.id == id).unwrap();
        field.value = Value::Text(text.into());
    }

    fn choose(form: &mut ConnectionForm, id: FieldId, wanted: &str) {
        let field = form.fields.iter_mut().find(|f| f.id == id).unwrap();
        if let Value::Choice { options, selected } = &mut field.value {
            *selected = options.iter().position(|o| o == wanted).unwrap();
        } else {
            panic!("{id:?} is not a choice field");
        }
    }

    #[test]
    fn a_new_form_defaults_to_sftp_and_agent_auth() {
        let form = ConnectionForm::new();
        let bookmark = {
            let mut f = form.clone();
            set(&mut f, FieldId::Name, "www");
            set(&mut f, FieldId::Host, "box");
            f.to_bookmark().unwrap()
        };

        assert_eq!(bookmark.protocol, Protocol::Sftp);
        assert_eq!(
            bookmark.auth,
            AuthMethod::Agent,
            "the default should be the method that needs no secret"
        );
        assert!(form.editing.is_none());
    }

    #[test]
    fn editing_a_bookmark_populates_every_field() {
        let mut target = Target::new(Protocol::Scp, "box.example.com");
        target.port = Some(2222);
        target.username = Some("deploy".into());
        target.auth = AuthMethod::Key;
        target.identity_file = Some(PathBuf::from("/home/greg/.ssh/id_ed25519"));
        target.remote_path = Some(PathBuf::from("/var/www"));

        let mut original = Bookmark::from_target("prod", &target);
        original.local_path = Some(PathBuf::from("/home/greg/proj"));
        original.sync_profile = Some("deploy".into());
        original.sync_browse = true;

        let form = ConnectionForm::edit(&original);
        assert_eq!(form.editing.as_deref(), Some("prod"));
        assert_eq!(
            form.to_bookmark().unwrap(),
            original,
            "a form opened on a bookmark and saved unchanged must round-trip"
        );
    }

    #[test]
    fn a_name_is_required() {
        let mut form = ConnectionForm::new();
        set(&mut form, FieldId::Host, "box");
        assert!(form.to_bookmark().unwrap_err().contains("name"));
    }

    #[test]
    fn a_host_is_required() {
        let mut form = ConnectionForm::new();
        set(&mut form, FieldId::Name, "www");
        assert!(form.to_bookmark().unwrap_err().contains("host"));
    }

    #[test]
    fn whitespace_only_values_count_as_empty() {
        let mut form = ConnectionForm::new();
        set(&mut form, FieldId::Name, "   ");
        set(&mut form, FieldId::Host, "box");
        assert!(form.to_bookmark().is_err());
    }

    #[test]
    fn a_bad_port_is_reported_rather_than_dropped() {
        let mut form = ConnectionForm::new();
        set(&mut form, FieldId::Name, "www");
        set(&mut form, FieldId::Host, "box");
        set(&mut form, FieldId::Port, "not-a-port");

        let err = form.to_bookmark().unwrap_err();
        assert!(err.contains("not-a-port"), "{err}");
    }

    #[test]
    fn an_empty_port_means_the_protocol_default() {
        let mut form = ConnectionForm::new();
        set(&mut form, FieldId::Name, "www");
        set(&mut form, FieldId::Host, "box");

        let bookmark = form.to_bookmark().unwrap();
        assert_eq!(bookmark.port, None);
        assert_eq!(bookmark.target().port_or_default(), 22);
    }

    #[test]
    fn text_fields_take_typed_characters() {
        let mut form = ConnectionForm::new();
        form.cursor = 0; // Name
        assert!(form.takes_text());

        for ch in "www".chars() {
            form.insert(ch);
        }
        assert_eq!(form.focused().value.as_text(), "www");

        form.backspace();
        assert_eq!(form.focused().value.as_text(), "ww");
    }

    /// A choice field must not swallow typed characters as text, or typing in
    /// one would silently do nothing.
    #[test]
    fn choice_fields_do_not_take_text() {
        let mut form = ConnectionForm::new();
        form.cursor = form
            .fields
            .iter()
            .position(|f| f.id == FieldId::Protocol)
            .unwrap();

        assert!(!form.takes_text());
        form.insert('x');
        assert_eq!(form.focused().value.as_text(), "sftp");
    }

    #[test]
    fn choices_cycle_both_ways_and_wrap() {
        let mut form = ConnectionForm::new();
        form.cursor = form
            .fields
            .iter()
            .position(|f| f.id == FieldId::Protocol)
            .unwrap();

        assert_eq!(form.focused().value.as_text(), "sftp");
        form.cycle(true);
        assert_eq!(form.focused().value.as_text(), "scp");

        form.cycle(false);
        assert_eq!(form.focused().value.as_text(), "sftp");

        // Wrapping backwards from the first lands on the last.
        form.cycle(false);
        assert_eq!(form.focused().value.as_text(), "webdav");
    }

    #[test]
    fn flags_toggle() {
        let mut form = ConnectionForm::new();
        form.cursor = form
            .fields
            .iter()
            .position(|f| f.id == FieldId::SyncBrowse)
            .unwrap();

        assert_eq!(form.focused().value.as_text(), "no");
        form.cycle(true);
        assert_eq!(form.focused().value.as_text(), "yes");
        form.cycle(true);
        assert_eq!(form.focused().value.as_text(), "no");
    }

    #[test]
    fn the_cursor_stops_at_the_ends() {
        let mut form = ConnectionForm::new();
        form.move_cursor(-5);
        assert_eq!(form.cursor, 0);
        form.move_cursor(999);
        assert_eq!(form.cursor, form.fields.len() - 1);
    }

    /// Fields that do not apply to the chosen auth method are dimmed rather
    /// than hidden, so rows do not jump around as you cycle the choice.
    #[test]
    fn relevance_follows_the_auth_method() {
        let mut form = ConnectionForm::new();

        choose(&mut form, FieldId::Auth, "agent");
        assert!(!form.is_relevant(FieldId::IdentityFile));
        assert!(!form.is_relevant(FieldId::RememberPassword));

        choose(&mut form, FieldId::Auth, "key");
        assert!(form.is_relevant(FieldId::IdentityFile));
        assert!(!form.is_relevant(FieldId::RememberPassword));

        choose(&mut form, FieldId::Auth, "password");
        assert!(!form.is_relevant(FieldId::IdentityFile));
        assert!(form.is_relevant(FieldId::RememberPassword));

        // Always-relevant fields stay so.
        assert!(form.is_relevant(FieldId::Host));
    }

    /// The local path is the field termscp does not have, so it is checked
    /// explicitly rather than only via the round-trip test.
    #[test]
    fn the_local_path_survives_a_save() {
        let mut form = ConnectionForm::new();
        set(&mut form, FieldId::Name, "www");
        set(&mut form, FieldId::Host, "box");
        set(&mut form, FieldId::LocalPath, "/home/greg/proj");

        let bookmark = form.to_bookmark().unwrap();
        assert_eq!(bookmark.local_path, Some(PathBuf::from("/home/greg/proj")));
    }

    #[test]
    fn every_field_has_a_hint() {
        for field in ConnectionForm::new().fields {
            assert!(
                !field.hint.is_empty(),
                "{:?} has no hint, so the form cannot explain it",
                field.id
            );
        }
    }
}
