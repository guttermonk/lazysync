# lazysync — design & implementation plan

A dual-pane terminal file manager and sync tool for local ↔ remote (and
local ↔ local, remote ↔ remote) trees. termscp's feature set, vim keys by
default, every binding remappable, and a confirmation window that shows you
exactly what is about to be overwritten before anything moves.

Status: **planning**. Nothing implemented yet.

---

## 1. Decisions already made

| Decision | Choice |
| --- | --- |
| Engine | Native Rust VFS core; `rsync`/`rclone` used as *optional* executors when present |
| Sync modes (v1) | push/pull without delete, mirror with delete, two-way bisync, watch mode |
| Secrets | OS keyring, opt-in per bookmark; key/agent auth preferred; prompt otherwise |
| Repo | Public `guttermonk/lazysync`, MIT OR Apache-2.0, Nix flake, GitHub Actions CI |
| TUI | `ratatui` + `crossterm` (hand-rolled component model, not `tui-realm`) |

## 2. Stack

Modelled on `jambi` for packaging/layout and `bluetui` for the ratatui + serde
config idiom; the keybinding engine is modelled on `inlyne`.

```
ratatui 0.30 + crossterm 0.29   TUI
tokio                           async I/O, job queue, cancellation
remotefs-ssh / -ftp / -aws-s3   protocol backends (the crates termscp publishes)
  / -smb / -webdav
serde + toml                    config, bookmarks, sync profiles, themes
keyring                         password storage (Secret Service / kwallet)
notify                          watch mode
blake3 + md5 + sha2             checksum verification (match provider hash)
clap                            CLI
tracing + tracing-subscriber    logs to $XDG_STATE_HOME/lazysync/
thiserror / anyhow              errors
```

Tree-wide conventions copied from `jambi`: `rust-toolchain.toml`,
`.rustfmt.toml`, `.cargo/config.toml`, `.envrc`, `flake.nix` + `package.nix`,
`config.example.toml`, dual `LICENSE-MIT` / `LICENSE-APACHE`.

## 3. Module layout

```
src/
  main.rs            entry, CLI dispatch, headless vs TUI
  cli.rs             clap definitions
  app.rs             App state machine + screen stack
  event.rs           input/tick/job-event multiplexer
  tui.rs             terminal init/restore, panic hook

  config/
    mod.rs           Config, load/merge/validate, XDG paths
    defaults.rs
    theme.rs         colors, presets (catppuccin/gruvbox/nord), terminal-default

  keys/              <- inlyne-shaped
    action.rs        Action enum (per-mode)
    combo.rs         Key, ModifiedKey, KeyCombo (chords: gg, dd, ZZ)
    defaults.rs      vim defaults per mode
    serialization.rs serde for "g"/{key,mod}/["g","g"]
    resolver.rs      base/extra merge, chord timeout, prefix matching
    tests.rs

  vfs/
    mod.rs           trait Vfs + Entry/Metadata/Capabilities
    local.rs  sftp.rs  scp.rs  ftp.rs  s3.rs  smb.rs  webdav.rs
    rclone.rs        rclone rcd JSON-RPC backend (any of ~70 remotes)
    cache.rs         listing cache + invalidation
    hostkey.rs       known_hosts / TOFU prompt plumbing

  bookmarks/
    mod.rs  store.rs (bookmarks.toml)  secrets.rs (keyring)  import.rs (termscp)

  browser/
    pane.rs  sync_browse.rs  union_view.rs  sort.rs  filter.rs  marks.rs

  sync/
    profile.rs       named reusable sync configs
    diff.rs          tree walk + comparison (mtime/size/checksum)
    plan.rs          Plan { creates, overwrites, deletes, conflicts, skipped }
    modes.rs         Push/Pull/Mirror/Bisync
    state.rs         bisync snapshot store
    filters.rs       gitignore-style include/exclude
    watch.rs         notify-driven continuous push
    executor/
      mod.rs         trait Executor + capability negotiation
      native.rs      always available
      rsync.rs       ssh remotes when rsync exists on both ends
      rclone.rs      sync/bisync/check via rcd

  ui/
    layout.rs  panes.rs  confirm.rs  progress.rs  queue.rs  help.rs  prompt.rs
    forms/  connection.rs  bookmark.rs  profile.rs
    log.rs           transfer/audit log viewer

  integrations/
    zide.rs          cwd → bookmark resolution
```

## 4. Keybindings

Directly borrowed from `inlyne/src/keybindings/`: an `Action` enum paired with
`KeyCombo(Vec<ModifiedKey>)`, `base` fully replaces the defaults, `extra`
layers on top and wins any collision with a `base` binding that shares its
prefix.

Extension inlyne doesn't need: **per-mode keymaps**, because `d` means delete in
the browser and a literal `d` in a text field.

```toml
[keys.browser]
extra = [
  ["ToggleSyncBrowse", "S"],
  ["SyncPush",         ["s", "p"]],
  ["Delete",           ["d", "d"]],
]

[keys.confirm]
extra = [["Apply", "y"], ["Cancel", "n"]]

# modes: browser, confirm, form, prompt, help, queue, log
```

Vim defaults: `h j k l`, `gg G`, `ctrl-d/u`, `0 $`, `/ n N`, `:`, `dd yy p`,
`v` visual select, `space` mark, `tab` switch pane, `S` sync-browse,
`u` union view, `q`/`ZZ` quit, `?` help.

- `lazysync keys [--mode browser]` prints the resolved keymap.
- The `?` overlay is generated from the live keymap, never a hardcoded string.
- Chord resolution has a configurable timeout (`chord_timeout_ms`, default 500).

## 5. Synchronized browsing

The thing that makes this better than termscp's version: sync-browse tracks a
**relative path from each pane's sync root**, not a mirrored absolute path.

```
local  root: /home/greg/proj          remote root: /srv/www/proj
         cd src/api/                           → /srv/www/proj/src/api/
```

Behaviour when the counterpart directory doesn't exist (`sync_browse.on_missing`):

| Value | Effect |
| --- | --- |
| `ghost` (default) | Pane shows `(does not exist on this side)` but **remembers the virtual path**, so `..` re-couples the panes. No permanent desync. |
| `stay` | Counterpart pane holds position; a `⚠ desynced` badge appears in the status bar. |
| `create` | Create the missing directory (prompt once per session unless `always`). |
| `ask` | Prompt per occurrence. |

Your asymmetric case — local has a handful of the remote's directories:

- Each pane always lists its **own** full contents. Sync-browse couples the
  *cwd*, not the listing, so the remote pane showing 400 entries against your
  local 3 is normal and fine.
- `u` toggles **union view**: merge both listings into one logical set, with
  per-row origin markers (`L` local-only, `R` remote-only, `LR` both) and
  only-one-side rows dimmed. Entering an `R`-only directory moves the remote
  pane and ghosts the local pane. This is what makes a sparse local checkout
  navigable against a full remote.
- `leader = "focused" | "local" | "remote"` (default `focused`) decides which
  pane's navigation drives the other.

Edge cases to handle explicitly: symlinks (follow vs show), case-insensitive
remotes, `\` vs `/` separators, permission-denied on one side only, and roots
that are themselves symlinks.

## 6. Confirmation window

Every sync — interactive or headless — produces a `Plan` first. The plan *is*
the dry run; applying is a separate step.

```
 Sync  ~/proj  →  sftp://box/srv/www/proj     mode: push (no delete)   compare: size+mtime
┌────────────────────────────────────────────────────────────────────────────────────────┐
│    path                       source                     destination           delta   │
│ ── Create (12) ────────────────────────────────────────────────────────────────────────│
│  + src/api/routes.rs          4.2 KB  2026-10-24 09:14   —                    +4.2 KB  │
│ ── Overwrite (3) ──────────────────────────────────────────────────────────────────────│
│  ↑ index.html                 8.1 KB  2026-10-24 08:02   7.9 KB  2026-10-21   +0.2 KB  │
│  ↓ config.toml                1.1 KB  2026-09-02 11:30   1.4 KB  2026-10-23   DEST IS  │
│                                                                              NEWER (!) │
│ ── Delete (0) ─────────────────────────────────────────────────────────────────────────│
│ ── Conflicts (1) ──────────────────────────────────────────────────────────────────────│
│  ⚠ notes.md    both sides changed since last sync      [L] keep local  [R] keep remote │
│ ── Skipped, identical (218) ───────────────────────────────────────── (zo to expand) ──│
└────────────────────────────────────────────────────────────────────────────────────────┘
 15 ops · 61.4 MB up · 0 B deleted · ~48s      y apply · n cancel · space exclude · c checksum
```

- Row glyphs: `+` create, `↑` overwrite with newer source, `↓` **overwrite with
  older source — rendered red**, `=` identical/skip, `✗` delete (red section),
  `⚠` conflict.
- Both sides' size **and** mtime on every overwrite row, plus the delta — the
  core thing you asked for. Red whenever the destination is newer than the
  source, yellow when the file shrinks.
- `space` excludes a row, `i` inverts, `o` only-this, `a` all; excluded rows
  grey out and the footer totals recompute live.
- `enter` on a directory op drills into its file-level ops.
- `D` on a text file shows an inline unified diff before you decide.
- `c` re-compares the selected (or all ambiguous) rows by checksum rather than
  size+mtime.
- `w` writes the plan to JSON/Markdown — useful for auditing `--yes` cron runs.

## 7. Bookmarks & connections

`$XDG_CONFIG_HOME/lazysync/bookmarks.toml` stores **everything but the
password**:

```toml
[[bookmark]]
name          = "www-prod"
protocol      = "sftp"
host          = "box.example.com"
port          = 22
username      = "greg"
auth          = "agent"              # agent | key | password
identity_file = "~/.ssh/id_ed25519"
remote_path   = "/srv/www/proj"
local_path    = "/home/greg/proj"    # termscp drops this; we keep it
sync_profile  = "deploy"
sync_browse   = true
remember_password = false            # true → keyring entry "lazysync:www-prod"
```

- **Main screen is the bookmark list** — name, protocol badge, host, and the
  `local ↔ remote` path pair. No connection form on it.
- **Opening a bookmark** shows only the connection *name* and a password
  prompt — and skips even that when agent/key auth works or a keyring entry
  exists. Host, port, username, paths are never shown here.
- **Connection details live only in the add/edit screen** (`n` new, `e` edit).
- Separate **recents** list (like termscp), with local paths retained.
- `lazysync import termscp` pulls existing termscp bookmarks over.

## 8. zide integration

On startup, when `[zide] auto_open_bookmark = true` (default):

1. Resolve the invocation directory — `$PWD`, preferring `ZIDE_*` env vars when
   zide exports one.
2. Match against each bookmark's `local_path`:
   - **exact match** → open that bookmark immediately;
   - else **nearest-ancestor match** (deepest `local_path` that is a prefix of
     cwd) → open it *and* pre-position the remote pane at the corresponding
     relative subpath, so popping the popup from `~/proj/src/api` lands both
     panes in `src/api`;
   - multiple equal-depth matches → bookmark list filtered to just those.
3. No match → normal bookmark list.

Config: `match = "exact" | "ancestor" | "off"`. CLI overrides:
`--auto-open` / `--no-auto-open`, `--open <bookmark>`.

Ship `layouts/default_lazysync.kdl` etc. mirroring zide's lazygit pane, plus a
`zide-sync` launcher in the README:

```kdl
pane_template name="lazysync" start_suspended=true {
    command "lazysync"
    x "10%"; y "5%"; width "80%"; height "80%"
}
```

Last-used bookmark and pane paths persist to
`$XDG_STATE_HOME/lazysync/session.toml`, so re-popping the popup resumes where
you left off.

## 9. Additions beyond what you listed

Ranked by how much I think they matter.

1. **Sync profiles** — named, reusable bundles of (direction, mode, filters,
   delete policy, compare policy, bandwidth cap, executor preference) attached
   to bookmarks. Without these, "advanced syncing" means re-entering options
   every run.
2. **mtime fidelity and clock skew** — the #1 cause of "why does it want to
   re-copy everything". Needs: per-backend mtime granularity (FAT 2s, some
   SFTP servers 1s), a configurable tolerance window, timezone-correct
   comparison, S3's lack of real mtime (fall back to ETag/metadata), and the
   confirmation window stating *which* comparison it actually used per row.
3. **Filters** — gitignore-syntax include/exclude per profile, `--exclude` on
   the CLI, and an optional "respect .gitignore" toggle.
4. **Safety rails** — `backup_dir` or trash instead of hard delete;
   `max_delete` threshold that aborts if more than N% / N files of the
   destination would be removed; double-confirm for deletes outside the sync
   root.
5. **Transfer queue + progress** — concurrent transfers, per-file and aggregate
   progress with throughput and ETA, pause/resume/cancel per job. `Ctrl-C`
   cancels the job, not the app.
6. **Resumable transfers** — `.lazysync-part` partial files, resume after
   reconnect, atomic rename on completion so a killed sync never leaves a
   truncated file in place.
7. **Bisync conflict resolution UI** — per-file keep-local / keep-remote /
   keep-both (suffixed) / skip, driven from the Conflicts section.
8. **Host-key verification UX** — explicit TOFU dialog on first connect,
   `known_hosts` read/write, loud warning on key change.
9. **Permission/ownership preservation** where the protocol supports it, and an
   honest "cannot preserve: <reason>" note where it doesn't.
10. **Local↔local and remote↔remote** syncs, not just local↔remote.
11. **Headless CLI** — `lazysync sync <profile> --dry-run|--yes` with no TUI,
    for cron/systemd timers; plus an append-only audit log of applied syncs.
12. **File ops parity with termscp** — open in `$EDITOR`, view, chmod, mkdir,
    rename, multi-select, `:!cmd` remote exec.
13. **Session keepalive + auto-reconnect** preserving pane state.
14. **Theme file** in inlyne's style plus a bluetui-ish `[colors]` section, with
    terminal-default, catppuccin, gruvbox and nord presets.
15. **Home-manager module** (like `tuimon` has) so bookmarks/config/keys are
    declarable; needs a mutable-overlay path since the store is read-only.
16. **Test fixtures** — a nixos-test or compose fixture with `sshd` + an FTP
    server + minio so the planner and executors get real integration tests. The
    diff planner itself gets heavy pure unit-test coverage; that's where
    correctness bugs will hide.
17. **Dir size on remote** is expensive — make `du`-style sizes lazy/opt-in.

## 10. Phases

| Phase | Deliverable |
| --- | --- |
| 0 | Repo scaffolding: flake, package.nix, CI, licenses, README skeleton, this doc |
| 1 | TUI shell, config loader, keymap engine, theme, help overlay — local↔local dual pane, no network |
| 2 | `Vfs` trait + Local + SFTP/SCP; connection form; bookmarks + keyring; host-key TOFU; zide auto-open |
| 3 | Diff planner + confirmation window + native executor (push/pull, mirror) |
| 4 | Transfer queue, progress, resume, filters, sync profiles, safety rails |
| 5 | rsync + rclone executors; `RcloneFs`; remaining native backends (FTP/S3/SMB/WebDAV) |
| 6 | Bisync + state snapshots + conflict UI |
| 7 | Watch mode, headless CLI + audit log, home-manager module, docs + demo cast |

Phases 1–3 are the usable milestone: dual-pane browsing over SFTP with
sync-browse and a reviewed push.

## 11. Open questions

1. **Protocol priority** — which do you actually use day to day? SFTP only, or
   do S3/SMB/WebDAV matter enough to pull into phase 5 vs. later?
2. **rclone's role** — executor only (sync/bisync/check), or also a *browse*
   backend so bookmarks can point at any rclone remote (gdrive, b2, …)?
3. **Compare default** — size+mtime (fast, default) vs. always-checksum
   (correct, slow). And the mtime tolerance window default: 1s or 2s?
4. **Watch mode confirmation** — auto-apply, or queue changes and confirm in
   batches? A watcher that stops for a modal every save is useless; a watcher
   that never confirms contradicts the whole review-first design.
5. **Bisync state location** — per-bookmark under `$XDG_STATE_HOME`, or
   alongside the synced tree (survives reinstalls, visible to both ends)?
6. **Single instance** — should a second zide popup reattach to the running
   instance over a socket, or just start fresh?
7. **Deletes in mirror mode** — default to `backup_dir` (recoverable) or real
   deletion with a `max_delete` guard?
8. **Crate name** — `lazysync` on crates.io is unverified (crates.io blocked my
   automated check); worth confirming before the first publish. The GitHub name
   `guttermonk/lazysync` is free.
