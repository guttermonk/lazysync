#!/usr/bin/env bash
# Start a throwaway FTP server, to test the FTP backend against a real one.
#
# Same reasoning as sshd.sh: a protocol backend tested only against a mock
# agrees with the mock and nothing else. FTP especially -- its listing format
# is not standardised, the working directory is stateful, data moves over a
# second connection negotiated by PASV, and whether MFMT exists varies by
# server. None of that shows up against a fake.
#
# pyftpdlib rather than pure-ftpd or vsftpd: both of those want root, for the
# chroot jail and the setuid, and a test fixture does not have it -- pure-ftpd
# exits 252 with nothing on stderr. pyftpdlib is a real server built to run as
# an ordinary user, which is why it is what most FTP *clients* are tested
# against.
#
# Usage:
#   eval "$(tests/fixtures/ftpd.sh start /path/to/workdir)"
#   tests/fixtures/ftpd.sh stop /path/to/workdir
#
# Exports on start:
#   LAZYSYNC_TEST_FTP_PORT  the port it is listening on
#   LAZYSYNC_TEST_FTP_USER  the user to log in as
#   LAZYSYNC_TEST_FTP_PASS  that user's password
#   LAZYSYNC_TEST_FTP_ROOT  the directory it serves, with a sample tree
#   LAZYSYNC_TEST_FTPS_PORT a second server on which TLS is *required*, with a
#                           self-signed certificate -- empty if openssl could
#                           not make one. Used to prove lazysync refuses an
#                           untrusted certificate rather than accepting it.

set -euo pipefail

action="${1:?usage: ftpd.sh start|stop <workdir>}"
work="${2:?usage: ftpd.sh start|stop <workdir>}"

pidfile="$work/ftpd.pid"
tls_pidfile="$work/ftpsd.pid"

case "$action" in
stop)
    for file in "$pidfile" "$tls_pidfile"; do
        if [[ -f "$file" ]]; then
            kill "$(cat "$file")" 2>/dev/null || true
            rm -f "$file"
        fi
    done
    exit 0
    ;;
start) ;;
*)
    echo "unknown action: $action" >&2
    exit 64
    ;;
esac

if ! command -v python3 >/dev/null 2>&1; then
    echo "python3 is not installed" >&2
    exit 1
fi
if ! python3 -c 'import pyftpdlib' >/dev/null 2>&1; then
    echo "the pyftpdlib module is not available to python3" >&2
    exit 1
fi

rm -rf "$work"
mkdir -p "$work" "$work/root/src/api" "$work/root/docs"

# Content with known sizes, so a listing can be checked against exact numbers
# rather than just "it returned something".
printf 'hello ftp' >"$work/root/README.md"      # 9 bytes
printf 'nested' >"$work/root/src/api/handler.rs" # 6 bytes
printf '.' >"$work/root/.hidden"                 # 1 byte

user="ftptester"
pass="lazysync-test-pass"

# A free port, found by probing. Same approach as sshd.sh, for the same
# reason: no language runtime needed for the search itself.
free_port() {
    local candidate
    for _ in $(seq 40); do
        candidate=$((20000 + RANDOM % 40000))
        if ! (exec 3<>/dev/tcp/127.0.0.1/"$candidate") 2>/dev/null; then
            printf '%s' "$candidate"
            return 0
        fi
        exec 3<&- 3>&- 2>/dev/null || true
    done
    return 1
}

port="$(free_port)" || {
    echo "could not find a free port" >&2
    exit 1
}

# --write grants write permission, which the transfer tests need. Passive data
# ports are left to the OS: it only hands out ones nothing holds, so it cannot
# collide with another fixture's control port.
#
# `nohup` and the redirect matter: without them the server inherits the
# script's stdout, and its startup banner would land in the `eval` the caller
# runs on our output.
nohup python3 -m pyftpdlib \
    --interface=127.0.0.1 \
    --port="$port" \
    --directory="$work/root" \
    --username="$user" \
    --password="$pass" \
    --write \
    >"$work/ftpd.log" 2>&1 &

echo $! >"$pidfile"

# Wait for it to accept connections rather than sleeping a guessed amount.
ready=""
for _ in $(seq 100); do
    if (exec 3<>/dev/tcp/127.0.0.1/"$port") 2>/dev/null; then
        exec 3<&- 3>&-
        ready="yes"
        break
    fi
    sleep 0.1
done
if [[ -z "$ready" ]]; then
    echo "the FTP server did not come up:" >&2
    cat "$work/ftpd.log" >&2 || true
    exit 1
fi

# A second server, this one requiring TLS, with a certificate signed by
# nobody. lazysync must *refuse* it: there is no accept-invalid-certs option,
# because an FTPS session that trusts anything is worse than a plaintext one.
# Proving a refusal needs an untrusted certificate, and a self-signed one is
# the only kind a test can produce -- so this fixture cannot test a successful
# FTPS handshake, only that an untrusted one fails.
#
# Best effort: if openssl is missing the variable comes back empty and the
# test skips, rather than failing the whole fixture for the plaintext tests.
tls_port=""
if command -v openssl >/dev/null 2>&1 && tls_port="$(free_port)"; then
    if openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
        -subj "/CN=127.0.0.1" \
        -keyout "$work/tls.key" -out "$work/tls.crt" 2>/dev/null; then

        nohup python3 -m pyftpdlib \
            --interface=127.0.0.1 \
            --port="$tls_port" \
            --directory="$work/root" \
            --username="$user" \
            --password="$pass" \
            --tls \
            --certfile="$work/tls.crt" \
            --keyfile="$work/tls.key" \
            --tls-control-required \
            >"$work/ftpsd.log" 2>&1 &

        echo $! >"$tls_pidfile"

        # The port is exported only once something is confirmed listening on
        # it. Exporting it unconditionally is how this fixture silently lied
        # once already: the server was failing to start, every TLS test got a
        # refused connection instead of a rejected certificate, and "the
        # certificate was refused" passed for entirely the wrong reason.
        tls_ready=""
        tls_pid="$(cat "$tls_pidfile")"
        for _ in $(seq 100); do
            if (exec 3<>/dev/tcp/127.0.0.1/"$tls_port") 2>/dev/null; then
                exec 3<&- 3>&-
                tls_ready="yes"
                break
            fi
            # Give up the moment the process is gone, rather than waiting out
            # the whole budget. A server that exited on a bad argument is not
            # going to start listening, and this loop runs on every single
            # fixture start -- it was costing ten seconds a test.
            kill -0 "$tls_pid" 2>/dev/null || break
            sleep 0.1
        done
        if [[ -z "$tls_ready" ]]; then
            echo "the FTPS server did not come up, so those tests will skip:" >&2
            cat "$work/ftpsd.log" >&2 || true
            tls_port=""
        fi
    else
        tls_port=""
    fi
fi

cat <<EOF
export LAZYSYNC_TEST_FTP_PORT=$port
export LAZYSYNC_TEST_FTPS_PORT=$tls_port
export LAZYSYNC_TEST_FTP_USER=$user
export LAZYSYNC_TEST_FTP_PASS=$pass
export LAZYSYNC_TEST_FTP_ROOT=$work/root
EOF
