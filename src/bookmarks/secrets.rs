//! Passwords, in the platform's own credential store.
//!
//! Three rules shape this module:
//!
//! 1. **Opt in, per bookmark.** Saving a connection does not save its
//!    password; `remember_password` does, and it is false by default.
//! 2. **Prefer not needing it.** Agent and key auth store nothing at all,
//!    which is why the bookmark screen is usually just a name.
//! 3. **A missing keyring is not an error.** On a headless box with no Secret
//!    Service running, lazysync has to keep working by prompting rather than
//!    refusing to start. Every function here degrades to "no stored password".

use keyring::Entry;

use crate::connection::Secret;

/// The service name every entry is filed under.
///
/// Stable, because changing it would orphan every password already saved.
const SERVICE: &str = "lazysync";

/// The keyring account name for a bookmark.
pub fn key_for(bookmark_name: &str) -> String {
    // Prefixed rather than bare, so entries are recognisable in a keyring
    // browser like seahorse and cannot collide with another tool's.
    format!("bookmark:{bookmark_name}")
}

/// Whether a credential store is reachable at all.
///
/// Used to explain *why* a password cannot be remembered, rather than letting
/// the checkbox silently do nothing.
pub fn available() -> bool {
    // There is no "ping" in the API, so this asks for an entry that will not
    // exist. A `NoEntry` answer means the store responded; a platform failure
    // means it did not.
    match Entry::new(SERVICE, "lazysync:probe") {
        Ok(entry) => !matches!(
            entry.get_password(),
            Err(keyring::Error::PlatformFailure(_)) | Err(keyring::Error::NoStorageAccess(_))
        ),
        Err(_) => false,
    }
}

/// Fetch a stored password.
///
/// `None` covers both "nothing stored" and "no store available", because the
/// caller does the same thing either way: prompt.
pub fn get(bookmark_name: &str) -> Option<Secret> {
    let key = key_for(bookmark_name);
    let entry = Entry::new(SERVICE, &key).ok()?;

    match entry.get_password() {
        Ok(password) => Some(Secret::new(password)),
        Err(keyring::Error::NoEntry) => None,
        Err(e) => {
            // Logged, not surfaced: the fallback is a prompt, which is a
            // working path rather than a failure.
            tracing::debug!("no stored password for {bookmark_name}: {e}");
            None
        }
    }
}

/// Store a password.
///
/// Returns an error here rather than swallowing it, because this one is called
/// in response to the user ticking "remember" -- and a silent failure would
/// leave them believing it was saved.
pub fn set(bookmark_name: &str, secret: &Secret) -> Result<(), SecretError> {
    let key = key_for(bookmark_name);
    let entry = Entry::new(SERVICE, &key).map_err(|e| SecretError::Store(e.to_string()))?;
    entry
        .set_password(secret.expose())
        .map_err(|e| SecretError::Store(e.to_string()))
}

/// Forget a stored password.
///
/// A bookmark that is deleted or switched away from password auth should not
/// leave its secret behind.
///
/// Everything that amounts to "there is no stored password" is success,
/// because that is the state the caller asked for: an entry that was never
/// there, and a machine with no credential store at all. Only a store that
/// exists and refuses the deletion is an error -- that one leaves a secret
/// behind, which the caller needs to know about.
pub fn forget(bookmark_name: &str) -> Result<(), SecretError> {
    let key = key_for(bookmark_name);
    let entry = match Entry::new(SERVICE, &key) {
        Ok(entry) => entry,
        // No store means nothing is stored, which is the requested end state.
        Err(_) => return Ok(()),
    };

    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),

        // No reachable credential store: a headless box, a build sandbox, a
        // session with no Secret Service running. Nothing is stored there, so
        // nothing was left behind.
        Err(e @ (keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_))) => {
            tracing::debug!("no credential store, so nothing to forget for {bookmark_name}: {e}");
            Ok(())
        }

        Err(e) => Err(SecretError::Store(e.to_string())),
    }
}

/// Why a password could not be stored or removed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SecretError {
    #[error(
        "could not reach the system keyring ({0}). The password was not saved; lazysync will \
         ask for it instead"
    )]
    Store(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key format is part of the on-disk contract: changing it orphans
    /// every password already saved, so it is pinned.
    #[test]
    fn keys_are_namespaced_by_bookmark_name() {
        assert_eq!(key_for("www-prod"), "bookmark:www-prod");
        assert_eq!(key_for(""), "bookmark:");
    }

    #[test]
    fn distinct_bookmarks_get_distinct_keys() {
        assert_ne!(key_for("a"), key_for("b"));
    }

    /// Reading on a machine with no credential store must answer "nothing
    /// stored" rather than failing -- a headless box has to stay usable.
    ///
    /// This runs against the real keyring, so it asserts only that the call
    /// returns rather than what it returns: CI has no Secret Service, a
    /// desktop does, and both outcomes are correct.
    #[test]
    fn reading_a_missing_entry_does_not_panic_or_error() {
        let fetched = get("lazysync-test-definitely-not-saved");
        assert!(
            fetched.is_none(),
            "a name that was never stored should not come back with a password"
        );
    }

    /// Must hold on a desktop with a keyring *and* in a build sandbox with no
    /// D-Bus at all. The second case is not hypothetical: it is where `nix
    /// build` runs the test suite, and treating an unreachable store as a
    /// failure made the package build fail while `cargo test` on the same
    /// commit passed.
    #[test]
    fn forgetting_something_that_was_never_stored_succeeds() {
        assert!(
            forget("lazysync-test-definitely-not-saved").is_ok(),
            "the requested end state -- no stored password -- already holds, whether that is \
             because the entry was absent or because there is no credential store here at all"
        );
    }

    /// Same contract, read the other way: nothing in this module may refuse to
    /// work merely because no keyring is present.
    #[test]
    fn a_machine_with_no_keyring_still_works() {
        // `available()` answers the question rather than asserting either way,
        // since CI has no Secret Service and a desktop does.
        let _ = available();

        // Whatever it answered, reading and forgetting must not fail.
        assert!(get("lazysync-test-no-such-entry").is_none());
        assert!(forget("lazysync-test-no-such-entry").is_ok());
    }

    /// The error message has to say what happens next, because the user is
    /// standing at a checkbox that just failed.
    #[test]
    fn the_store_error_explains_the_fallback() {
        let message = SecretError::Store("no dbus".into()).to_string();
        assert!(message.contains("not saved"), "{message}");
        assert!(message.contains("ask for it"), "{message}");
    }
}
