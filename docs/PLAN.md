# lazysync — design & implementation plan

A dual-pane terminal file manager and sync tool for local ↔ remote (and
local ↔ local, remote ↔ remote) trees. termscp's feature set, vim keys by
default, every binding remappable, and a confirmation window that shows you
exactly what is about to be overwritten before anything moves.

Status: **phase 5 of 7 complete**. Browsing, connections, syncing and the
transfer queue work over local, SFTP/SCP and FTP/FTPS; see the phase table in
§10 for what each one actually delivered, and what it turned out not to have
anticipated.

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
ratatui 0.30                    TUI (crossterm via ratatui::crossterm)
remotefs-ssh / -ftp / -aws-s3   protocol backends (the crates termscp publishes)
  / -smb / -webdav
serde + toml                    config, bookmarks, sync profiles, themes
keyring                         password storage (Secret Service / kwallet)
notify                          watch mode
blake3 + md5 + sha2             checksum verification (match provider hash)
clap                            CLI
tracing + tracing-appender      logs to $XDG_STATE_HOME/lazysync/
thiserror / anyhow              errors
```

**No async runtime.** The plan originally called for tokio. It is not there,
because the `remotefs-*` crates are themselves blocking: an async `Vfs` would
mean wrapping sync calls in `spawn_blocking` and paying async's complexity for
none of its benefit. The trait is blocking, and phase 2 keeps the UI responsive
by running each backend on its own worker thread and passing results to the UI
thread — which is also how termscp does it. If a later phase needs real
concurrency across many transfers, a thread pool is the smaller change.

**crossterm is not a direct dependency.** `ratatui-crossterm` can build against
either 0.28 or 0.29, so a second direct dependency risks resolving to the other
one — at which point `KeyEvent` from the re-export and `KeyEvent` from our own
dependency are different types. Everything goes through
`ratatui::crossterm`.

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

1. ~~**Sync profiles**~~ — done in phase 4. A bookmark's `sync_profile` picks
   the direction and the exclude rules, and `ss` runs it. Bandwidth caps and
   executor preference are declared in the schema but not yet honoured.
2. **mtime fidelity and clock skew** — the #1 cause of "why does it want to
   re-copy everything". Needs: per-backend mtime granularity (FAT 2s, some
   SFTP servers 1s), a configurable tolerance window, timezone-correct
   comparison, S3's lack of real mtime (fall back to ETag/metadata), and the
   confirmation window stating *which* comparison it actually used per row.

   Mostly done in phase 3, with one rule added in phase 5 that the original
   design missed. Comparing by timestamp needs more than both sides *reporting*
   one: the destination has to be able to **preserve** the source's. FTP
   reports mtimes but cannot set them (`MFMT` is not in the base protocol), so
   every file it receives carries the time it was uploaded — which never
   matches the source. An mtime comparison there marks the whole tree changed
   on every single run, for ever. `Comparator::new` therefore keys off
   `dest.set_mtime`, not just `dest.mtime`, and degrades to a size comparison
   with its own reason (`same size, times not kept`) so the window does not
   claim there are no timestamps while displaying two. Only the *destination's*
   limitation counts — a pull *from* FTP still compares by time.

   This is the one bug in the project so far that only a real server could have
   produced: a mock stores whatever mtime it is handed, so the second-sync
   comparison passes and the fault stays invisible. It is the argument for
   `tests/fixtures/` existing.

   Measuring against that server also corrected FTP's declared granularity from
   1 second to **a day**. `remotefs-ftp` lists with `LIST`, whose format is
   `ls -l` output: a recent file shows `Oct 24 22:52` and loses the seconds, and
   one older than about six months shows `Nov 14  2023` and loses the time
   altogether. Measured error was 52 seconds on a file from today and 80,017
   seconds on one from 2023. The 1-second claim only looked harmless because
   FTP destinations now degrade to a size comparison — on a *pull*, where FTP
   is the source and the local side the destination, timestamps are compared
   for real, and every remote one would have sat a day outside tolerance.
   `MDTM` returns true seconds and would allow a much tighter figure, at one
   round trip per file; that is a deliberate choice for later, not something to
   assume.
3. ~~**Filters**~~ — done in phase 4, bar the CLI flag. gitignore syntax per
   profile, applied during the walk so an excluded directory is never
   descended into, and on *both* sides so a mirror does not delete the
   destination's copy of something the user filtered out. `respect_gitignore`
   reads the sync root's own file; nested ones would need per-directory rule
   stacks and are not worth half-doing.
4. ~~**Safety rails**~~ — done in phase 3. `backup_dir` is the default, so a
   deletion is recoverable; a plan that would empty the destination is refused
   at any size; a fraction limit applies above a floor of ten destination
   files, because a percentage of a tiny tree is noise; and permanent deletion
   asks a second time. The backup directory is excluded from planning, or a
   mirror would delete its own backups on the next run.
5. ~~**Transfer queue + progress**~~ — done in phase 4, except for
   concurrency. Per-file and aggregate progress, throughput, ETA, pause and
   cancel. Transfers are sequential: parallelism within one SSH session needs
   multiple channels, which is its own piece of work, and the serial case is
   what a dual-pane tool spends its time on anyway.
6. ~~**Resumable transfers**~~ — done in phases 3 and 4. Partial files, an
   atomic rename (or a three-step swap where the protocol's rename will not
   overwrite), and resume from a partial left by a cancelled run. A partial
   longer than its source is discarded rather than appended to: it is debris
   from a different version of the file.
7. **Bisync conflict resolution UI** — per-file keep-local / keep-remote /
   keep-both (suffixed) / skip, driven from the Conflicts section.
8. ~~**Host-key verification UX**~~ — done in phase 2, and it turned out to be
   mandatory rather than a nicety: `remotefs-ssh`'s libssh2 backend does not
   consult `known_hosts` at all, so without this lazysync would hand both your
   credentials and your files to whatever answered. Implemented as a pre-flight
   handshake that reads the key and stops, *before* any password is collected —
   so you are never asked to type a secret for an unverified server. A changed
   key is refused outright rather than offered as a prompt.
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
16. ~~**Test fixtures**~~ — `sshd` done in phase 2 (`tests/fixtures/sshd.sh`),
    FTP in phase 5 (`tests/fixtures/ftpd.sh`). Both start a real server on
    loopback as the invoking user, and both have earned it: the SSH one caught
    SFTP's non-replacing rename, the FTP one caught the unpreservable-mtime
    comparison bug. Object storage is still outstanding — the dev shell carries
    `garage` for it (minio was removed from nixpkgs), waiting on an S3 backend
    in phase 6.

    One trap worth recording, because it is the failure mode a fixture is
    most prone to: the FTPS test passed for a while against *nothing
    listening*. The TLS server was dying on a bad argument, so "an untrusted
    certificate is refused" was really observing a refused connection. A
    fixture must not export an endpoint it has not confirmed is up, and a
    test that accepts any error will happily agree that an absent server
    rejected a certificate.

    A note on server choice, since it is not obvious: the FTP fixture runs
    `pyftpdlib`, not `pure-ftpd` or `vsftpd`. Both of those want root for their
    chroot jail and setuid, which a test does not have — `pure-ftpd` exits 252
    with nothing on stderr. `pyftpdlib` is a real server built to run as an
    ordinary user, which is why it is what most FTP *clients* are tested
    against.
17. **Dir size on remote** is expensive — make `du`-style sizes lazy/opt-in.

## 10. Phases

| Phase | Deliverable |
| --- | --- |
| 0 | ✅ Repo scaffolding: flake, package.nix, CI, licenses, README skeleton, this doc |
| 1 | ✅ TUI shell, config loader, keymap engine, theme, help overlay — local↔local dual pane, no network. Synchronized browsing with ghosting, and union view, landed here too: they are pure path logic and needed no network, and building them first meant the `Vfs` trait was shaped by a real caller. |
| 2 | ✅ `Vfs` + Local + SFTP/SCP; connection form; bookmarks + keyring; host-key TOFU; zide auto-open. Two things the underlying crates do not provide had to be written here: host key verification (`remotefs-ssh`'s libssh2 backend consults `known_hosts` nowhere at all), and an explicit `SshKeyStorage`, without which naming an identity file silently did nothing. `lazysync import termscp` slipped to phase 7 — its format carries an encrypted password blob, and an importer written against a format that cannot be tested here would be guesswork presented as a feature. |
| 3 | ✅ Diff planner + confirmation window + native executor (push/pull, mirror). Three things the design did not anticipate: **SFTP's rename does not replace an existing file** (OpenSSH answers `SSH_FX_PERMISSION_DENIED`), so overwrites go through a three-step swap — see `Capabilities::atomic_overwrite_rename`; a **percentage-only delete guard misfires on small trees**, where removing one of two files reads as 50%, so a floor was added with a separate all-or-nothing check that fires at any size; and the **backup directory had to be excluded from planning**, or the second mirror run deletes the backups the first one made. |
| 4 | ✅ Transfer queue, progress, resume, filters, sync profiles. (Safety rails landed early, in phase 3.) The queue is a state machine the main loop steps a chunk per frame rather than a worker thread: the panes own the `Vfs` values and an SFTP session cannot be cloned without asking for the password again, so a thread would mean the browser losing its backends for the duration. Stepping gives progress and cancellation with no shared state. Two things worth noting: `Vfs` gained streaming handles, so a 4 GB file no longer costs 4 GB of memory; and the glob matcher had to be memoised after `*a*a*a*a*a*a*b` against forty `a`s took **3.7 seconds** — the patterns come from the user's own config, so that was a filter line able to freeze the planner. |
| 5 | ✅ One generic `RemoteAdapter` for every `remotefs` backend; FTP + FTPS on top of it. The phase was planned as "all remaining backends plus the rsync/rclone executors" and was cut down deliberately: **WebDAV and S3 cannot go in at all right now** — `remotefs-webdav` and `remotefs-aws-s3` still depend on `remotefs 0.3`, which defines a *different, incompatible* `RemoteFs` trait, so adding either puts two versions of the trait in one binary. SMB needs `libsmbclient` and a share field the connection schema does not have. All three move to phase 6. What landed instead paid for itself twice over: generalising the adapter cut `ssh.rs` from 978 lines to 387, and the real-server fixture caught four bugs. One is a comparison bug affecting **every** destination that cannot set an mtime (see §9.2). The others were FTPS: the default port was 990, the *implicit* FTPS port, while `secure()` performs *explicit* FTPS — a plaintext connect then `AUTH TLS` — so `ftps://host` would have opened a plaintext connection to a port awaiting a TLS handshake; a rejected certificate surfaced as "the server refused the connection", because `remotefs` reports a failed handshake as `ConnectionError` with the cause in the source and `translate` was collapsing that into the payload-free `NotConnected`; and the certificate test itself passed for the wrong reason until the fixture stopped exporting a port before confirming something was listening on it. |
| 6 | Bisync + state snapshots + conflict UI; S3/SMB/WebDAV once `remotefs 1.0` ports exist; rsync + rclone executors; `RcloneFs` |
| 7 | Watch mode, headless CLI + audit log, home-manager module, docs + demo cast |

Phases 1–3 are the usable milestone: dual-pane browsing over SFTP with
sync-browse and a reviewed push.

## 11. Open questions

1. **Protocol priority** — which do you actually use day to day? SFTP and FTP
   are in; S3, SMB and WebDAV are blocked rather than deprioritised (see the
   phase 5 row), so the question is now whether any of them is worth the work
   their blockers need. S3 wants new connection fields (bucket, region,
   credentials) and either a `remotefs 1.0` port or a hand-written backend;
   SMB wants `libsmbclient` as a system dependency.
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
