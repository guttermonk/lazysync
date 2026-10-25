# Contributing

## Getting set up

```sh
nix develop        # or `direnv allow`, which does the same via .envrc
cargo test
```

Without Nix: a stable Rust toolchain (see `rust-toolchain.toml`), `pkg-config`
and `openssl`. `rsync` and `rclone` are optional at runtime and only needed to
exercise those executors.

## Before opening a PR

```sh
cargo fmt
cargo clippy --all-targets -- --deny warnings
cargo test
nix build          # what users actually install; CI runs it too
typos
```

CI runs the same set on stable and beta, Linux and macOS, plus `nix flake
check`.

**`nix build` only sees files git knows about.** A flake's `./.` is its *git*
source, so a new file you have not `git add`ed is invisible to the build — it
fails with `E0583 unresolved module` for a module whose file is plainly sitting
there. `git add` the new files first. This bites once per person and is
confusing every time.

## Things worth knowing

- **The planner is the part that must be right.** `sync::diff` and `sync::plan`
  decide what gets overwritten and deleted; a bug there costs someone their
  data. New comparison logic needs unit tests, and anything touching deletes
  needs a test that asserts the safety thresholds still fire.
- **Don't write to stderr while the TUI is up.** ratatui holds the alternate
  screen; a stray `println!` or an un-filtered `tracing` line corrupts the
  display. Log to the file sink instead.
- **Keybindings are data, not code.** Add an `Action` variant and a default
  binding; never match on a raw key event in a widget. The help overlay is
  generated from the live keymap, so a hardcoded key is a help entry that
  lies.
- **Every backend is allowed to be missing capabilities.** `Vfs` reports what
  it can do; code that assumes mtimes exist breaks on S3. When a plan can't
  preserve something, it has to say so in the row rather than silently
  proceeding.
- **Direction is per row, never per plan.** `Row::target_pane` names the side
  that changes, and bisync relies on one plan carrying rows that go both ways.
  Anything that re-derives a destination from a plan-wide root, or sorts the
  two endpoints into a "source" and a "destination" before the row is in hand,
  is a bug waiting to happen — both of phase 6's worst ones were exactly that,
  and both reported success while writing to the wrong side.
- **When both sides changed, do not guess.** Preferring the newer file is the
  tempting shortcut and it discards somebody's edit silently, on the strength
  of two clocks that may disagree by more than the edits do. It becomes a
  conflict, and the plan refuses to run until a human settles it.
- **Protocol work needs a real server.** `tests/fixtures/sshd.sh` and
  `tests/fixtures/ftpd.sh` each start one on loopback, as you, on a high port,
  serving a throwaway tree; the dev shell provides `openssh`, `pyftpdlib` and
  `garage`. The tests *skip* when a fixture cannot run, so check they actually
  ran before believing a green suite.

  A planner tested only against a mock agrees with the mock and nothing else,
  and both fixtures have earned their keep by proving it: the SSH one found
  that SFTP's rename refuses to replace an existing file, and the FTP one
  found that comparing mtimes against a destination which cannot set them
  marks every file changed, for ever. Neither is visible against a fake, which
  stores whatever it is handed and agrees.

## Commit messages

Imperative mood, one logical change per commit, and say *why* in the body when
the what isn't self-evident.
