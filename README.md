# lazysync

A dual-pane terminal file manager and sync tool. [termscp][termscp]'s feature
set, vim keys by default with every binding remappable, and a confirmation
window that shows you both sides of every overwrite — size and timestamp,
source and destination — before anything moves.

> **Status: phase 4 of 7.** Plan a push, pull or mirror, review every row,
> and watch it transfer — with progress, throughput, ETA, pause and cancel,
> resumable partial files, and gitignore-style exclude rules driven by named
> sync profiles. Over the local filesystem or SFTP/SCP, with saved
> connections, host key verification, keyring passwords, zide auto-open,
> synchronized browsing with ghosting, union view, and vim keys throughout.
>
> What does not work yet: no bisync, no watch mode, no inline diffs, and no
> checksum re-verification from the window. Transfers are sequential — one
> file at a time. FTP, S3, SMB and WebDAV are refused by name. Each subcommand
> says which phase it arrives in. See [`docs/PLAN.md`](docs/PLAN.md).

## Why not termscp

termscp is good and lazysync owes it most of its feature list. These are the
things that sent me to write another one:

- **Overwrites are a yes/no prompt.** lazysync builds a plan first and shows
  it as a table: both sides' size and mtime, the delta, and a red row whenever
  the destination is *newer* than the source — the case where a sync destroys
  work. The plan is the dry run; applying it is a separate keystroke.
- **Synchronized browsing desyncs permanently** the moment you enter a
  directory the other side doesn't have. lazysync tracks a path *relative to
  each pane's sync root* and keeps a ghost position for the missing side, so
  `..` re-couples the panes. There is also a union view (`u`) that merges both
  listings with `L`/`R`/`LR` markers, which is what makes a sparse local
  checkout navigable against a full remote.
- **Bookmarks don't store the local path.** lazysync bookmarks store
  everything except the password — including the local working directory and a
  sync profile reference.
- **The connection form is on the main screen.** lazysync opens to the
  bookmark list. Connection details live in the add/edit screen; opening a
  bookmark shows its *name* and a password prompt, and skips even that when
  agent or key auth works.
- **No advanced sync.** lazysync has a native sync engine and will delegate to
  `rsync` or `rclone` when they are installed and the remote suits them.

## Features

Planned for 1.0, phased in the order below. See
[`docs/PLAN.md`](docs/PLAN.md) for the full breakdown.

- Dual-pane browser over SFTP, SCP, FTP/FTPS, S3, SMB, WebDAV, and any
  `rclone` remote
- Sync modes: push/pull without deletes ✅, mirror with deletes ✅, two-way
  bisync with conflict resolution, and a file watcher
- Confirmation window with per-row exclusion ✅, inline diffs, and checksum
  re-verification
- Synchronized browsing with ghost positions and a union view
- Vim keybindings by default; every binding remappable per UI mode, with chord
  support (`gg`, `dd`), in the style of [inlyne][inlyne]'s keymap config
- Bookmarks storing everything but the password; passwords optionally in the
  OS keyring
- Sync profiles: reusable bundles of direction, filters, delete policy and
  executor preference
- Resumable transfers, a transfer queue with progress and ETA, bandwidth caps
- Safety rails: backup-dir instead of hard delete, and a max-delete threshold
  that aborts on a mistyped sync root
- Headless `lazysync sync <profile> --yes` for cron, with an audit log

## Install

### Nix

```nix
{
  inputs.lazysync.url = "github:guttermonk/lazysync";

  # ...
  environment.systemPackages = [ inputs.lazysync.packages.${system}.default ];
  # or, with the overlay:
  nixpkgs.overlays = [ inputs.lazysync.overlays.default ];
}
```

Configure with `.override` rather than by copying files over the result — the
store is read-only, and anything copied in is clobbered on the next update:

```nix
inputs.lazysync.packages.${system}.default.override {
  # Optional sync executors put on lazysync's PATH. The native engine needs
  # neither. Pass [ ] for a native-only build.
  syncBackends = [ pkgs.rsync pkgs.rclone ];
}
```

### Cargo

```sh
cargo install --locked --git https://github.com/guttermonk/lazysync
```

`rsync` and `rclone` are optional; lazysync uses them when they're on `PATH`
and falls back to its own engine with a note in the plan when they're not.

## Usage

```sh
lazysync                              # the saved connections
lazysync open www-prod                # open one by name
lazysync sftp://user@host/srv/www     # connect directly
lazysync /mnt/backup/proj             # the working directory against that
lazysync ~/proj /mnt/backup/proj      # one directory in each pane
lazysync keys --mode browser          # dump the resolved keymap
lazysync config                       # dump the resolved config as TOML

lazysync sync deploy ~/proj /mnt/backup --dry-run   # print a plan, move nothing
lazysync sync deploy ~/proj /mnt/backup --yes      # headless, for cron

# Not yet implemented -- each says which phase it arrives in:
lazysync import termscp               # import bookmarks          (phase 7)
```

In the connection list: `enter` opens, `n` adds, `e` edits, `dd` deletes, `/`
filters, `esc` drops to the local browser.

In the browser: `h`/`j`/`k`/`l` navigate, `tab` switches panes, `S` toggles
synchronized browsing, `u` toggles union view, `/` searches, `space` marks,
`o` cycles the sort, `?` shows every binding, `q` quits. `sp` plans a push,
`sl` a pull, `sm` a mirror, `ss` uses the connection's own sync profile. All
of it remappable — see below.

### The confirmation window

`sp` does not transfer anything. It builds a plan and shows it:

```
 Sync  ~/proj  →  box:/srv/www        compare: size+mtime  tolerance: 2s  deletions move to .lazysync-backup/
┌───────────────────────────────────────────────────────────────────────────────────────────────┐
│    ▾ Create (1)                                                                               │
│ +  src/main.rs                     11 B  2026-10-24 14:14        —                   +11 B    │
│    ▾ Overwrite (2)                                                                            │
│ ↓  config.toml                      5 B  2026-09-02 16:30      5 B  2026-10-23 19:02  DESTINATION IS NEWER │
│ ↑  index.html                      11 B  2026-10-24 13:02      5 B  2026-10-21 14:14  +6 B     │
│    ▾ Delete (1)                                                                               │
│ ✗  obsolete.txt                       —                        8 B  2026-10-24 18:22  not on the source   │
│    ▸ Skipped, identical (1)                                                      zo to open    │
└───────────────────────────────────────────────────────────────────────────────────────────────┘
 3 ops · 27 B out · 1 unchanged · 1 would overwrite NEWER data
 y apply · n cancel · space include/exclude · a all · i invert · o only · zc/zo fold
```

Both sides' size and timestamp on every row, so "overwrite" is never an
abstract word. A **newer destination is red** — that is the one outcome of a
sync that destroys work. `space` excludes a row and the totals recompute
immediately. The plan *is* the dry run: `y` applies exactly these rows, and
nothing else has a path to the executor.

Deletions move to `.lazysync-backup/` by default rather than vanishing, and a
mirror that would empty the destination is refused outright — that is what a
mirror pointed at the wrong root looks like.

### Transfers

`y` hands the plan to a queue and shows progress; it does not block:

```
 transferring  ~/proj  →  box:/srv/www
 11 M of 69 M  ·  6 files left  ·  12 M/s · 4s left
┌ file0.bin ──────────────────────────────────────────────────────┐
│███████████████████████████ 11 M / 12 M █████████                │
└─────────────────────────────────────────────────────────────────┘
██████████████████ 17% of the plan
 ✓  index.html                                            11 M
 ✓  src/main.rs                                            2 M
 p pause · esc cancel
```

`p` pauses between chunks, `esc` cancels. A cancelled file leaves its partial
behind when `transfer.resume` is on, so the next run picks up where it
stopped rather than re-sending. Files are streamed in 256 KiB chunks, so a
4 GB file costs 256 KiB of memory rather than 4 GB.

### Filters and profiles

A `[profiles.<name>]` entry bundles a direction with exclude rules, and a
bookmark's `sync_profile` points at one. `ss` then syncs using it, so you do
not have to remember which way a given connection goes:

```toml
[profiles.deploy]
mode = "push"
exclude = ["/target/", "*.log", ".git/"]
respect_gitignore = true
```

Rules are gitignore syntax — `name`, `name/`, `/name`, `*`, `**`, `?`, and
`!` to re-include. Character classes aren't supported, and a pattern using
one is an error rather than a literal: a filter that silently does nothing is
how `target/` ends up on a web server.

### Connections

Saved connections live in `bookmarks.toml` and store everything except the
password — including the local working directory and a sync profile
reference. Opening one shows its *name* and, only if it needs one, a password
prompt; host, port, username and key path appear solely in the add/edit form.

Host keys are verified against `~/.ssh/known_hosts` before any credential is
sent, because `remotefs-ssh`'s libssh2 backend does not check them at all. A
new host asks once, with a `SHA256:` fingerprint you can compare against
`ssh-keygen -lf`. A *changed* host key is refused outright rather than offered
as a prompt to click through.

`~/.ssh/config` is read, so a bookmark for a host you have already configured
picks up its `HostName`, `User`, `Port` and `IdentityFile`.

## Configuration

`$XDG_CONFIG_HOME/lazysync/config.toml`. Every option is documented with its
default in [`config.example.toml`](config.example.toml); an empty file behaves
the same as that file.

Bookmarks live separately in `bookmarks.toml`, because lazysync writes to them
from the UI and round-tripping a hand-commented file through a serializer
would eat the comments.

### Keybindings

Keymaps are per UI mode — `d` is delete in the browser and a literal `d` in a
text field. Within a mode, `base` replaces the defaults entirely and `extra`
layers on top, winning any collision:

```toml
[keys.browser]
extra = [
    ["ToggleSyncBrowse", "S"],
    ["SyncPush",         ["s", "p"]],
    ["Delete",           ["d", "d"]],
    ["Copy",             { key = "c", mod = ["Ctrl"] }],
]
```

`lazysync keys` prints what a config actually resolves to, and the in-app help
overlay (`?`) is generated from the live keymap rather than a hardcoded list.

## zide integration

lazysync is built to be opened as a [zide][zide] floating pane the way
`lazygit` is. When it starts in a directory matching a bookmark's
`local_path`, it opens that bookmark directly — exact match first, then the
deepest `local_path` that is a prefix of the working directory, pre-positioning
the remote pane at the corresponding subpath. So popping the popup in
`~/proj/src/api` lands both panes in `src/api`.

```toml
[zide]
auto_open_bookmark = true
match = "ancestor"   # "exact" | "ancestor" | "off"
```

Turn it off with `match = "off"` or `--no-auto-open`.

## Development

```sh
nix develop          # or direnv allow
cargo test
cargo clippy --all-targets -- --deny warnings
cargo fmt --check
```

The dev shell carries `rsync`, `rclone`, and an `sshd`/`minio` pair for the
integration fixtures — a sync planner tested only against a mock agrees with
the mock and nothing else.

`.envrc` points `cargo run` at a scratch config tree under `.dev/`, so hacking
on the bookmark store doesn't edit the bookmarks you actually use.

## Prior art

- [termscp][termscp] — the feature set this starts from, and the source of the
  `remotefs-*` backend crates
- [inlyne][inlyne] — the keybinding config model
- [rclone][rclone] / rsync — the sync semantics worth copying
- [lazygit][lazygit] — the popup-in-a-pane workflow

## License

MIT OR Apache-2.0, at your option.

[termscp]: https://github.com/veeso/termscp
[inlyne]: https://github.com/Inlyne-Project/inlyne
[zide]: https://github.com/guttermonk/zide
[rclone]: https://rclone.org
[lazygit]: https://github.com/jesseduffield/lazygit
