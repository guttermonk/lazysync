//! Parsing `sftp://greg@box:2222/srv/www`.
//!
//! Hand-written rather than pulling in a URL crate, because what is accepted
//! here is narrower than a URL and the differences matter: there is no query
//! string, no fragment, and a password in the userinfo is *rejected* rather
//! than parsed. A URL on a command line ends up in shell history, so accepting
//! `sftp://greg:hunter2@box` would quietly teach people to leak passwords.

use std::path::PathBuf;

use super::{AuthMethod, Protocol, Target};

/// Parse a connection URL into a [`Target`].
pub fn parse(input: &str) -> Result<Target, ParseTargetError> {
    let (scheme, rest) = input
        .split_once("://")
        .ok_or_else(|| ParseTargetError::NoScheme(input.to_owned()))?;

    let protocol =
        Protocol::from_scheme(scheme).ok_or_else(|| ParseTargetError::UnknownScheme {
            scheme: scheme.to_owned(),
            known: Protocol::ALL
                .iter()
                .map(|p| p.scheme())
                .collect::<Vec<_>>()
                .join(", "),
        })?;

    // Split authority from path at the first `/`. Everything after it,
    // including that slash, is the remote path.
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], Some(&rest[i..])),
        None => (rest, None),
    };

    if authority.is_empty() {
        return Err(ParseTargetError::NoHost(input.to_owned()));
    }

    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((user, host)) => (Some(user), host),
        None => (None, authority),
    };

    if hostport.is_empty() {
        return Err(ParseTargetError::NoHost(input.to_owned()));
    }

    let username = match userinfo {
        Some(user) if user.contains(':') => {
            return Err(ParseTargetError::PasswordInUrl);
        }
        Some("") => None,
        Some(user) => Some(user.to_owned()),
        None => None,
    };

    let (host, port) = split_host_port(hostport, input)?;

    let mut target = Target::new(protocol, host);
    target.username = username;
    target.port = port;
    target.remote_path = path.map(PathBuf::from);

    // A URL says nothing about how to authenticate, so start from the method
    // that needs no secret and let the connect path fall back if it fails.
    target.auth = AuthMethod::Agent;

    Ok(target)
}

/// Split `host`, `host:port` or `[::1]:port`.
fn split_host_port(hostport: &str, input: &str) -> Result<(String, Option<u16>), ParseTargetError> {
    // Bracketed IPv6, where the colons inside are part of the address.
    if let Some(rest) = hostport.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| ParseTargetError::UnclosedBracket(input.to_owned()))?;
        let port = match tail.strip_prefix(':') {
            Some(port) => Some(parse_port(port)?),
            None if tail.is_empty() => None,
            None => return Err(ParseTargetError::TrailingGarbage(tail.to_owned())),
        };
        return Ok((host.to_owned(), port));
    }

    match hostport.rsplit_once(':') {
        // More than one colon and no brackets is a bare IPv6 address, where a
        // trailing `:1234` is ambiguous with the address itself. Treat the
        // whole thing as the host rather than silently guessing.
        Some(_) if hostport.matches(':').count() > 1 => Ok((hostport.to_owned(), None)),
        Some((host, port)) => Ok((host.to_owned(), Some(parse_port(port)?))),
        None => Ok((hostport.to_owned(), None)),
    }
}

fn parse_port(port: &str) -> Result<u16, ParseTargetError> {
    port.parse()
        .map_err(|_| ParseTargetError::BadPort(port.to_owned()))
}

/// Why a connection URL could not be understood.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseTargetError {
    #[error("{0:?} is not a connection URL (expected something like sftp://user@host/path)")]
    NoScheme(String),
    #[error("unknown scheme {scheme:?} (expected one of: {known})")]
    UnknownScheme { scheme: String, known: String },
    #[error("{0:?} has no host")]
    NoHost(String),
    #[error("{0:?} is not a valid port")]
    BadPort(String),
    #[error("unclosed [ in {0:?}")]
    UnclosedBracket(String),
    #[error("unexpected {0:?} after the host")]
    TrailingGarbage(String),
    #[error(
        "a password in a URL would be stored in your shell history. Leave it out -- lazysync \
         will prompt, or take it from the keyring"
    )]
    PasswordInUrl,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_url_parses() {
        let target = parse("sftp://greg@box.example.com:2222/srv/www").unwrap();
        assert_eq!(target.protocol, Protocol::Sftp);
        assert_eq!(target.username.as_deref(), Some("greg"));
        assert_eq!(target.host, "box.example.com");
        assert_eq!(target.port, Some(2222));
        assert_eq!(target.remote_path, Some(PathBuf::from("/srv/www")));
    }

    #[test]
    fn the_minimum_is_a_scheme_and_a_host() {
        let target = parse("sftp://box").unwrap();
        assert_eq!(target.host, "box");
        assert_eq!(target.port, None);
        assert_eq!(target.username, None);
        assert_eq!(target.remote_path, None);
        assert_eq!(target.port_or_default(), 22);
    }

    #[test]
    fn a_root_path_is_kept() {
        let target = parse("sftp://box/").unwrap();
        assert_eq!(target.remote_path, Some(PathBuf::from("/")));
    }

    /// A URL on a command line lands in shell history, so a password in one is
    /// refused rather than honoured.
    #[test]
    fn a_password_in_the_url_is_refused() {
        assert_eq!(
            parse("sftp://greg:hunter2@box").unwrap_err(),
            ParseTargetError::PasswordInUrl
        );
    }

    #[test]
    fn a_username_containing_an_at_sign_uses_the_last_one() {
        // Azure and some S3 gateways really do use user@domain as a username.
        let target = parse("sftp://greg@corp@box/srv").unwrap();
        assert_eq!(target.username.as_deref(), Some("greg@corp"));
        assert_eq!(target.host, "box");
    }

    #[test]
    fn bracketed_ipv6_keeps_its_colons() {
        let target = parse("sftp://[2001:db8::1]:2222/srv").unwrap();
        assert_eq!(target.host, "2001:db8::1");
        assert_eq!(target.port, Some(2222));

        let target = parse("sftp://[::1]/srv").unwrap();
        assert_eq!(target.host, "::1");
        assert_eq!(target.port, None);
    }

    /// A bare IPv6 address has colons of its own, so a trailing `:22` cannot
    /// be told from the address. Guessing either way would be wrong sometimes;
    /// treating it all as the host at least fails loudly at connect time.
    #[test]
    fn unbracketed_ipv6_is_treated_as_a_whole_host() {
        let target = parse("sftp://2001:db8::1/srv").unwrap();
        assert_eq!(target.host, "2001:db8::1");
        assert_eq!(target.port, None);
    }

    #[test]
    fn an_unknown_scheme_lists_the_known_ones() {
        let err = parse("gopher://box").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("gopher"), "{message}");
        assert!(message.contains("sftp"), "{message}");
        assert!(message.contains("webdav"), "{message}");
    }

    #[test]
    fn a_plain_path_is_not_a_url() {
        assert!(matches!(
            parse("/home/greg/proj"),
            Err(ParseTargetError::NoScheme(_))
        ));
    }

    #[test]
    fn a_missing_host_is_an_error() {
        assert!(matches!(parse("sftp://"), Err(ParseTargetError::NoHost(_))));
        assert!(matches!(
            parse("sftp://greg@"),
            Err(ParseTargetError::NoHost(_))
        ));
    }

    #[test]
    fn a_bad_port_names_itself() {
        let err = parse("sftp://box:not-a-port/srv").unwrap_err();
        assert!(err.to_string().contains("not-a-port"), "{err}");
    }

    #[test]
    fn a_port_above_the_range_is_rejected() {
        assert!(matches!(
            parse("sftp://box:70000"),
            Err(ParseTargetError::BadPort(_))
        ));
    }

    #[test]
    fn every_scheme_is_accepted_even_when_unimplemented() {
        // The bookmark store and the form have to round-trip all of them; the
        // connect path is what refuses the ones that are not ready.
        for protocol in Protocol::ALL {
            let target = parse(&format!("{}://box/path", protocol.scheme())).unwrap();
            assert_eq!(target.protocol, protocol);
        }
    }
}
