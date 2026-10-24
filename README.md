# lazysync

A dual-pane terminal file manager and sync tool. [termscp][termscp]'s feature
set, vim keys by default with every binding remappable, and a confirmation
window that shows you both sides of every overwrite — size and timestamp,
source and destination — before anything moves.

> **Status: phase 0.** This is the scaffolding commit. The design is written
> up in [`docs/PLAN.md`](docs/PLAN.md); the code is not there yet. `lazysync`
> currently parses its CLI and tells you which phase each subcommand arrives
> in. Watch the repo if you want to know when it does something useful.

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
- Sync modes: push/pull without deletes, mirror with deletes, two-way bisync
  with conflict resolution, and a file watcher
- Confirmation window with per-row exclusion, inline diffs, and checksum
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
lazysync                              # bookmark list
lazysync sftp://user@host/srv/www     # connect directly
lazysync open www-prod                # open a saved bookmark
lazysync sync deploy --dry-run        # print a plan, transfer nothing
lazysync sync deploy --yes            # headless, for cron
lazysync keys --mode browser          # dump the resolved keymap
```

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
