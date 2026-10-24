//! Host key verification.
//!
//! This exists because `remotefs-ssh`'s libssh2 backend does not do it:
//! nothing in that crate consults `known_hosts`, so connecting through it
//! alone would trust whatever key the far end presents. For a tool whose whole
//! job is to copy your files somewhere, that is not acceptable -- a
//! man-in-the-middle would be handed both your credentials and your data.
//!
//! So the key is checked in a pre-flight pass: open the TCP connection, do the
//! SSH handshake, read the host key, compare it against `known_hosts`, and
//! only then let the real client connect. The cost is one extra handshake per
//! session. That is a few hundred milliseconds once, against silently
//! trusting an unknown server, which is not a close call.
//!
//! The pre-flight sends no credential of any kind -- it stops after the
//! handshake -- so a failed check cannot have leaked anything.

use std::{
    fmt,
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    time::Duration,
};

use ssh2::{CheckResult, KnownHostFileKind, KnownHostKeyFormat, Session};

/// What `known_hosts` says about the key a server presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The key is recorded and matches. Carry on.
    Trusted,
    /// The host is not in `known_hosts`. Worth asking about once.
    Unknown { fingerprint: String },
    /// The host *is* recorded, and the key is different.
    ///
    /// Separate from `Unknown` because the two mean completely different
    /// things: one is a first visit, the other is either a rebuilt server or
    /// an attack, and lazysync must not offer to paper over it with the same
    /// casual prompt.
    Changed {
        fingerprint: String,
        known_hosts: PathBuf,
    },
}

/// A host key read from a server, with enough context to record it later.
pub struct HostKey {
    host: String,
    port: u16,
    key: Vec<u8>,
    key_type: ssh2::HostKeyType,
    /// SHA256 fingerprint, formatted the way `ssh-keygen -l` shows it.
    fingerprint: String,
}

impl HostKey {
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn host_port(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

impl fmt::Debug for HostKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostKey")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// Check a server's host key against `known_hosts` without authenticating.
pub fn verify(
    host: &str,
    port: u16,
    timeout: Duration,
    known_hosts: Option<&Path>,
) -> Result<(Verdict, HostKey), HostKeyError> {
    let stream = connect(host, port, timeout)?;

    let mut session = Session::new().map_err(|e| HostKeyError::Ssh {
        context: "creating an SSH session".into(),
        source: e,
    })?;
    session.set_tcp_stream(stream);
    session.set_timeout(u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX));
    session.handshake().map_err(|e| HostKeyError::Ssh {
        context: format!("SSH handshake with {host}:{port}"),
        source: e,
    })?;

    let (key, key_type) = session
        .host_key()
        .ok_or_else(|| HostKeyError::NoHostKey(format!("{host}:{port}")))?;

    let fingerprint = sha256_fingerprint(&session, key);

    let host_key = HostKey {
        host: host.to_owned(),
        port,
        key: key.to_vec(),
        key_type,
        fingerprint,
    };

    let path = match known_hosts {
        Some(path) => path.to_path_buf(),
        None => default_known_hosts()?,
    };

    let mut store = session.known_hosts().map_err(|e| HostKeyError::Ssh {
        context: "opening the known_hosts store".into(),
        source: e,
    })?;

    // A missing file is not an error: it is a first run, and every host is
    // then simply unknown.
    if path.exists() {
        store
            .read_file(&path, KnownHostFileKind::OpenSSH)
            .map_err(|e| HostKeyError::Ssh {
                context: format!("reading {}", path.display()),
                source: e,
            })?;
    }

    let verdict = match store.check_port(host, port, key) {
        CheckResult::Match => Verdict::Trusted,
        CheckResult::NotFound => Verdict::Unknown {
            fingerprint: host_key.fingerprint.clone(),
        },
        CheckResult::Mismatch => Verdict::Changed {
            fingerprint: host_key.fingerprint.clone(),
            known_hosts: path,
        },
        // libssh2 reports a failure to check as its own result. Treating it as
        // "unknown" would downgrade an error into a prompt the user would
        // click through, so it stays an error.
        CheckResult::Failure => return Err(HostKeyError::CheckFailed(format!("{host}:{port}"))),
    };

    Ok((verdict, host_key))
}

/// Append a key to `known_hosts`, after the user agreed to trust it.
///
/// Writes through libssh2's own writer so the line is formatted exactly as
/// OpenSSH expects, rather than hand-assembling a format that has to stay
/// compatible with `ssh` itself.
pub fn remember(key: &HostKey, known_hosts: Option<&Path>) -> Result<PathBuf, HostKeyError> {
    let path = match known_hosts {
        Some(path) => path.to_path_buf(),
        None => default_known_hosts()?,
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| HostKeyError::Io {
            context: format!("creating {}", parent.display()),
            source: e,
        })?;
    }

    let session = Session::new().map_err(|e| HostKeyError::Ssh {
        context: "creating an SSH session".into(),
        source: e,
    })?;
    let mut store = session.known_hosts().map_err(|e| HostKeyError::Ssh {
        context: "opening the known_hosts store".into(),
        source: e,
    })?;

    // Read the existing file first, so writing back does not drop every other
    // host. libssh2's writer emits the whole store, not just the addition.
    if path.exists() {
        store
            .read_file(&path, KnownHostFileKind::OpenSSH)
            .map_err(|e| HostKeyError::Ssh {
                context: format!("reading {}", path.display()),
                source: e,
            })?;
    }

    let format = key_format(key.key_type)
        .ok_or_else(|| HostKeyError::UnknownKeyType(format!("{:?}", key.key_type)))?;

    store
        .add(
            &known_hosts_host(&key.host, key.port),
            &key.key,
            "added by lazysync",
            format,
        )
        .map_err(|e| HostKeyError::Ssh {
            context: "recording the host key".into(),
            source: e,
        })?;

    // Write to a temporary file and rename, so an interrupted write cannot
    // leave a truncated known_hosts -- which would lock you out of every host
    // at once. libssh2 opens the path itself, so there is nothing to create
    // or flush here.
    let temporary = path.with_extension("lazysync-tmp");
    store
        .write_file(&temporary, KnownHostFileKind::OpenSSH)
        .map_err(|e| HostKeyError::Ssh {
            context: format!("writing {}", temporary.display()),
            source: e,
        })?;
    std::fs::rename(&temporary, &path).map_err(|e| HostKeyError::Io {
        context: format!("replacing {}", path.display()),
        source: e,
    })?;

    Ok(path)
}

/// How a host is spelled in `known_hosts`.
///
/// OpenSSH brackets the host and appends the port only when it is not 22, and
/// libssh2's `check_port` builds the same string when looking one up. Writing
/// a bare `host:port` instead produces a line that nothing -- not `ssh`, not
/// lazysync's own next check -- will ever match, so the prompt would come back
/// every single time.
fn known_hosts_host(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    }
}

/// The `known_hosts` format constant for a key type.
///
/// `None` for a type libssh2 does not recognise. Guessing -- writing it as RSA
/// -- would record a line `ssh` cannot verify, so this refuses instead: a
/// repeated prompt is better than a silently broken entry.
const fn key_format(key_type: ssh2::HostKeyType) -> Option<KnownHostKeyFormat> {
    match key_type {
        ssh2::HostKeyType::Rsa => Some(KnownHostKeyFormat::SshRsa),
        ssh2::HostKeyType::Dss => Some(KnownHostKeyFormat::SshDss),
        ssh2::HostKeyType::Ecdsa256 => Some(KnownHostKeyFormat::Ecdsa256),
        ssh2::HostKeyType::Ecdsa384 => Some(KnownHostKeyFormat::Ecdsa384),
        ssh2::HostKeyType::Ecdsa521 => Some(KnownHostKeyFormat::Ecdsa521),
        ssh2::HostKeyType::Ed25519 => Some(KnownHostKeyFormat::Ed25519),
        ssh2::HostKeyType::Unknown => None,
    }
}

/// `SHA256:…`, matching what `ssh` prints when it asks you to confirm a key.
///
/// The point of matching is that you can check it against `ssh-keygen -lf`
/// output, which is the only way a fingerprint prompt is any use at all.
fn sha256_fingerprint(session: &Session, key: &[u8]) -> String {
    // libssh2 exposes its own hashes of the key, but only MD5 and SHA1 are
    // guaranteed, and MD5 fingerprints are not what modern ssh shows. Hashing
    // the key bytes directly gives the same value ssh-keygen reports.
    let digest = sha256(key);
    let _ = session;
    format!("SHA256:{}", base64_nopad(&digest))
}

/// `~/.ssh/known_hosts`.
fn default_known_hosts() -> Result<PathBuf, HostKeyError> {
    dirs::home_dir()
        .map(|home| home.join(".ssh").join("known_hosts"))
        .ok_or(HostKeyError::NoHomeDirectory)
}

fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, HostKeyError> {
    // Resolve explicitly so a DNS failure reads as one, and so each address of
    // a dual-stack host is tried rather than only the first.
    let addresses: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| HostKeyError::Resolve {
            host: host.to_owned(),
            source: e,
        })?
        .collect();

    if addresses.is_empty() {
        return Err(HostKeyError::Resolve {
            host: host.to_owned(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no addresses for this host"),
        });
    }

    let mut last = None;
    for address in &addresses {
        match TcpStream::connect_timeout(address, timeout) {
            Ok(stream) => {
                // Also bound the read side: a server that accepts the
                // connection and then says nothing would otherwise hang the
                // handshake, and with a blocking Vfs that hangs the UI.
                let _ = stream.set_read_timeout(Some(timeout));
                let _ = stream.set_write_timeout(Some(timeout));
                return Ok(stream);
            }
            Err(e) => last = Some(e),
        }
    }

    Err(HostKeyError::Connect {
        host: format!("{host}:{port}"),
        source: last.unwrap_or_else(|| std::io::Error::other("no address could be reached")),
    })
}

/// Why host key verification could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum HostKeyError {
    #[error("cannot resolve {host}: {source}")]
    Resolve {
        host: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot reach {host}: {source}")]
    Connect {
        host: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0} presented no host key")]
    NoHostKey(String),
    #[error("could not check the host key for {0}")]
    CheckFailed(String),
    #[error("no home directory, so known_hosts cannot be found")]
    NoHomeDirectory,
    #[error("unrecognised host key type {0}, so it cannot be recorded safely")]
    UnknownKeyType(String),
    #[error("{context}: {source}")]
    Ssh {
        context: String,
        #[source]
        source: ssh2::Error,
    },
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

// ── Small crypto helpers ────────────────────────────────────────────────────
//
// SHA-256 and base64 are implemented here rather than pulled in as
// dependencies: they are needed for exactly one 32-byte digest per connection,
// and both are short enough to read in full. The checksum crates arrive in
// phase 3 for file hashing, where throughput actually matters.

/// SHA-256, per FIPS 180-4.
fn sha256(input: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // Pad to a multiple of 64 bytes: 0x80, zeroes, then the bit length.
    let mut message = input.to_vec();
    let bit_length = (input.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    // `as_chunks` rather than `chunks_exact`: the block and word sizes are
    // constants, so this hands back fixed-size arrays and the indexing below
    // needs no bounds checks or slice-to-array conversions.
    for chunk in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);

            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }

        for (dst, src) in h.iter_mut().zip(v) {
            *dst = dst.wrapping_add(src);
        }
    }

    let mut out = [0u8; 32];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *chunk = word.to_be_bytes();
    }
    out
}

/// Standard base64 with the `=` padding stripped, which is how OpenSSH prints
/// a SHA256 fingerprint.
fn base64_nopad(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let triple = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let indices = [
            (triple >> 18) & 0x3f,
            (triple >> 12) & 0x3f,
            (triple >> 6) & 0x3f,
            triple & 0x3f,
        ];
        // One output character per 6 bits of actual input.
        let keep = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for &index in indices.iter().take(keep) {
            out.push(ALPHABET[index as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checked against `printf '' | sha256sum` and friends, because a wrong
    /// fingerprint is worse than none: it would teach the user to ignore the
    /// prompt.
    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(b"hello world")),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    /// Exercises the padding path where the message length lands just either
    /// side of a block boundary.
    #[test]
    fn sha256_handles_block_boundaries() {
        assert_eq!(
            hex(&sha256(&[b'a'; 55])),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            hex(&sha256(&[b'a'; 56])),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            hex(&sha256(&[b'a'; 64])),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_nopad(b""), "");
        assert_eq!(base64_nopad(b"f"), "Zg");
        assert_eq!(base64_nopad(b"fo"), "Zm8");
        assert_eq!(base64_nopad(b"foo"), "Zm9v");
        assert_eq!(base64_nopad(b"foob"), "Zm9vYg");
        assert_eq!(base64_nopad(b"fooba"), "Zm9vYmE");
        assert_eq!(base64_nopad(b"foobar"), "Zm9vYmFy");
    }

    /// The whole point of showing a fingerprint is that it can be compared
    /// with `ssh-keygen -lf`, so the shape has to match.
    #[test]
    fn a_fingerprint_looks_like_what_ssh_prints() {
        let digest = sha256(b"a pretend host key");
        let rendered = format!("SHA256:{}", base64_nopad(&digest));

        assert!(rendered.starts_with("SHA256:"));
        assert_eq!(rendered.len(), 7 + 43, "{rendered}");
        assert!(
            !rendered.contains('='),
            "ssh strips the padding: {rendered}"
        );
    }

    #[test]
    fn a_changed_key_is_a_different_verdict_from_an_unknown_one() {
        // The distinction is the point of the enum, so it is pinned here:
        // collapsing them would turn an attack into a routine prompt.
        let unknown = Verdict::Unknown {
            fingerprint: "SHA256:aaa".into(),
        };
        let changed = Verdict::Changed {
            fingerprint: "SHA256:aaa".into(),
            known_hosts: PathBuf::from("/home/greg/.ssh/known_hosts"),
        };
        assert_ne!(unknown, changed);
    }

    /// The spelling has to match OpenSSH's, or a recorded key never matches
    /// again and the prompt returns on every connection.
    #[test]
    fn a_non_default_port_is_bracketed_the_way_openssh_writes_it() {
        assert_eq!(known_hosts_host("box", 22), "box");
        assert_eq!(known_hosts_host("box", 2222), "[box]:2222");
        assert_eq!(known_hosts_host("127.0.0.1", 50954), "[127.0.0.1]:50954");
    }

    #[test]
    fn an_unrecognised_key_type_is_refused_rather_than_guessed() {
        assert!(
            key_format(ssh2::HostKeyType::Unknown).is_none(),
            "recording an unknown type as RSA would write a line ssh cannot verify"
        );
        assert!(key_format(ssh2::HostKeyType::Ed25519).is_some());
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
