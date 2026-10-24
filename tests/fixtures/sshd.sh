#!/usr/bin/env bash
# Start a throwaway sshd for testing against a real SSH server.
#
# A planner and a protocol backend tested only against a mock agree with the
# mock and nothing else. This brings up an actual OpenSSH server on a high
# port, as the invoking user, with its own host key and authorized_keys, and
# an isolated HOME so the test cannot read or write your real ~/.ssh.
#
# Usage:
#   eval "$(tests/fixtures/sshd.sh start /path/to/workdir)"   # exports vars
#   tests/fixtures/sshd.sh stop /path/to/workdir
#
# Exports on start:
#   LAZYSYNC_TEST_SSH_PORT  the port it is listening on
#   LAZYSYNC_TEST_SSH_USER  the user to authenticate as (you)
#   LAZYSYNC_TEST_HOME      isolated HOME holding .ssh/id_ed25519 and known_hosts
#   LAZYSYNC_TEST_SERVED    directory the server exposes, with a sample tree

set -euo pipefail

action="${1:?usage: sshd.sh start|stop <workdir>}"
work="${2:?usage: sshd.sh start|stop <workdir>}"

pidfile="$work/sshd.pid"

case "$action" in
stop)
    if [[ -f "$pidfile" ]]; then
        kill "$(cat "$pidfile")" 2>/dev/null || true
        rm -f "$pidfile"
    fi
    exit 0
    ;;
start) ;;
*)
    echo "unknown action: $action" >&2
    exit 64
    ;;
esac

rm -rf "$work"
mkdir -p "$work" "$work/home/.ssh" "$work/served/src/api" "$work/served/docs"

# Content with known sizes, so a listing can be checked rather than just
# "it returned something".
printf 'hello sftp' >"$work/served/README.md"          # 10 bytes
printf '#!/bin/sh\necho hi\n' >"$work/served/run.sh"
chmod 755 "$work/served/run.sh"
printf 'nested' >"$work/served/src/api/handler.rs"
printf '.' >"$work/served/.hidden"

home="$work/home"
chmod 700 "$home/.ssh"

# Host key, and the client key we will authenticate with.
ssh-keygen -q -t ed25519 -N '' -f "$work/host_ed25519" -C 'lazysync test host'
ssh-keygen -q -t ed25519 -N '' -f "$home/.ssh/id_ed25519" -C 'lazysync test client'
cp "$home/.ssh/id_ed25519.pub" "$home/.ssh/authorized_keys"
chmod 600 "$home/.ssh/authorized_keys" "$home/.ssh/id_ed25519"

# A free port. Found by probing rather than by binding-and-releasing, because
# the latter needs a language runtime the dev shell does not have to carry.
port=""
for _ in $(seq 40); do
    candidate=$((20000 + RANDOM % 40000))
    if ! (exec 3<>/dev/tcp/127.0.0.1/"$candidate") 2>/dev/null; then
        port="$candidate"
        break
    fi
    exec 3<&- 3>&- 2>/dev/null || true
done

if [[ -z "$port" ]]; then
    echo "could not find a free port" >&2
    exit 1
fi

# StrictModes off because the fixture's directories are not under the real
# HOME and sshd would otherwise refuse their ownership. Everything else is
# locked down: no password auth, no PAM, loopback only.
cat >"$work/sshd_config" <<EOF
Port $port
ListenAddress 127.0.0.1
HostKey $work/host_ed25519
PidFile $pidfile
AuthorizedKeysFile $home/.ssh/authorized_keys
StrictModes no
UsePAM no
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
PrintMotd no
Subsystem sftp internal-sftp
LogLevel ERROR
EOF

sshd_bin="$(command -v sshd || echo /usr/sbin/sshd)"
"$sshd_bin" -f "$work/sshd_config" -E "$work/sshd.log"

# Wait for it to accept connections rather than sleeping a guessed amount.
for _ in $(seq 50); do
    if (exec 3<>/dev/tcp/127.0.0.1/"$port") 2>/dev/null; then
        exec 3<&- 3>&-
        break
    fi
    sleep 0.1
done

cat <<EOF
export LAZYSYNC_TEST_SSH_PORT=$port
export LAZYSYNC_TEST_SSH_USER=$(id -un)
export LAZYSYNC_TEST_HOME=$home
export LAZYSYNC_TEST_SERVED=$work/served
EOF
