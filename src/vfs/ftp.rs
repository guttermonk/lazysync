//! FTP and FTPS.
//!
//! Thin, because [`super::remote::RemoteAdapter`] does everything that is not
//! protocol-specific. What is left is the connection itself and one honest
//! admission about what FTP cannot do.
//!
//! **FTP cannot reliably set a modification time.** `MDTM` reads one and
//! `MFMT` writes one, but neither is in the base protocol and plenty of
//! servers implement only the first. [`Protocol::capabilities`] therefore
//! reports `set_mtime: false`.
//!
//! That one flag has more consequence than it looks. A file uploaded over FTP
//! carries the time it was *uploaded*, which never matches its source, so
//! comparing the two timestamps would mark the whole tree as changed on every
//! single run, for ever. `sync::compare` keys its rule off `set_mtime` for
//! exactly this reason: against a destination that cannot preserve times,
//! `size_mtime` degrades to a size comparison and the window says so per row
//! (`same size, times not kept`) rather than implying a check it did not make.
//! `tests/ftp.rs` pins both halves against a real server.
//!
//! **FTPS here is *explicit* FTPS**: connect in the clear on the ordinary FTP
//! port and send `AUTH TLS` before authenticating, which is what RFC 4217
//! specifies and what `suppaftp`'s `into_secure` performs. It is not implicit
//! FTPS on port 990, where TLS starts before a byte of FTP is spoken -- hence
//! [`Protocol::default_port`] giving 21 for both.
//!
//! Plain `ftp` is never silently upgraded. Probing for `AUTH TLS` and taking
//! it if offered would make `ftp://` sometimes encrypted and sometimes not,
//! with nothing on screen distinguishing the two. Ask for `ftps://` and you
//! get TLS or an error. Certificates are verified against the webpki root
//! store with no accept-invalid-certs escape hatch: an FTPS session that
//! trusts anything is worse than a plaintext one, because it looks safe.

use std::path::PathBuf;

use remotefs::RemoteFs;
use remotefs_ftp::FtpFs;

use super::{
    remote::{self, RemoteAdapter},
    VfsError,
};
use crate::connection::{Protocol, Secret, Target};

/// Connect to an FTP or FTPS server.
pub fn open(target: &Target, secret: Option<&Secret>) -> Result<RemoteAdapter, VfsError> {
    if !matches!(target.protocol, Protocol::Ftp | Protocol::Ftps) {
        return Err(VfsError::Unsupported(format!(
            "{} is not an FTP protocol",
            target.protocol
        )));
    }

    let mut client = FtpFs::new(&target.host, target.port_or_default());

    // Anonymous is the documented default when no username is given, and it
    // is what a bare `ftp://host/pub` is asking for.
    match &target.username {
        Some(username) => {
            client = client.username(username);
            if let Some(secret) = secret.filter(|s| !s.is_empty()) {
                client = client.password(secret.expose());
            }
        }
        None => {
            client = client.username("anonymous").password("anonymous@");
        }
    }

    // Passive mode: active requires the server to open a connection back to
    // the client, which fails behind almost any NAT or firewall.
    client = client.passive_mode();

    if matches!(target.protocol, Protocol::Ftps) {
        // Explicit FTPS: `AUTH TLS` on the ordinary port, verified against the
        // webpki root store. No accept-invalid-certs escape hatch is exposed
        // -- see the module docs.
        client = client.secure();
    }

    // Connected here rather than through the adapter, because the working
    // directory can only be read off the concrete `FtpFs`. `RemoteFs` has no
    // `pwd`, and `stream()` -- the only route to one -- needs a live client,
    // which the adapter has already boxed behind `dyn`.
    connect(&mut client, target)?;

    // FTP has a working directory, and a login usually lands in the account's
    // own. Asking for it means `ftp://host` opens somewhere useful rather than
    // at `/`, which on a shared server is often not even listable.
    //
    // The filter matters: a server that answers `PWD` with a relative path
    // would otherwise give a "home" interpreted against wherever the session
    // later wandered, and `GoHome` would land somewhere different each time.
    let home = client
        .stream()
        .and_then(|stream| stream.pwd().ok())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());

    Ok(RemoteAdapter::new(
        Box::new(client),
        target.protocol,
        target.clone(),
        home,
    ))
}

/// Open the session.
///
/// Split out so the error carries the host, which `remotefs`'s own message
/// does not. The context string is built here rather than being the bare word
/// "connecting" because every failure mode goes through it -- a refused
/// connection, a rejected certificate, a server that hung up -- and knowing
/// *which host* is the first thing you want in all of them.
fn connect(client: &mut FtpFs, target: &Target) -> Result<(), VfsError> {
    let context = format!("connecting to {}", target.host_port());

    client
        .connect()
        .map(|_| ())
        .map_err(|e| match remote::translate(e, &context) {
            // The only variant with no room for a reason of its own.
            VfsError::NotConnected => VfsError::Protocol {
                context,
                message: "the server refused the connection".into(),
            },
            other => other,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_ftp_protocol_is_refused() {
        let target = Target::new(Protocol::Sftp, "box");
        assert!(matches!(open(&target, None), Err(VfsError::Unsupported(_))));
    }

    /// The honest admission, pinned: if this ever flips to `true` without the
    /// protocol gaining a reliable `MFMT`, every FTP sync would re-copy its
    /// whole tree and the window would stop saying why.
    #[test]
    fn ftp_does_not_claim_it_can_set_timestamps() {
        for protocol in [Protocol::Ftp, Protocol::Ftps] {
            let caps = protocol.capabilities();
            assert!(caps.mtime, "MDTM gives a modification time");
            assert!(
                !caps.set_mtime,
                "MFMT is not in the base protocol and many servers lack it"
            );
            assert!(!caps.mode, "FTP has no Unix mode bits");
        }
    }

    /// FTPS must default to 21, not 990.
    ///
    /// `secure()` performs *explicit* FTPS -- a plaintext connect followed by
    /// `AUTH TLS`. 990 is the implicit-FTPS port, where the server expects a
    /// TLS handshake immediately, so defaulting to it would have `ftps://host`
    /// open a plaintext connection to a port that will never answer in
    /// plaintext.
    #[test]
    fn ftps_defaults_to_the_explicit_tls_port() {
        assert_eq!(Protocol::Ftps.default_port(), 21);
        assert_eq!(Protocol::Ftp.default_port(), 21);
    }
}
