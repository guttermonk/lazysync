# lazysync — design & implementation plan

A dual-pane terminal file manager and sync tool for local ↔ remote and
local ↔ local trees (remote ↔ remote is still intended; see §9.10). termscp's
feature set, vim keys by default, every binding remappable, and a
confirmation window that shows you exactly what is about to be overwritten
before anything moves.

Status: **phase 7 of 7 complete**, plus the post-phase work in §10a. All four
sync modes, the transfer queue, watch mode and the audit log work over local,
SFTP/SCP, FTP/FTPS and any `rclone` remote — which is also how S3, WebDAV, B2
and Google Drive are reachable. The browser can now also create, rename,
delete and chmod, and an idle session is kept alive and reopened if it drops.
See the phase table in §10 for what each phase delivered, and what it turned
out not to have anticipated.

Sections below describe **what is built** unless they say otherwise. Anything
still outstanding is marked *(not built)* at the point it is described, rather
than left for a reader to discover by pressing the key.

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

What is actually depended on:

```
ratatui 0.30                    TUI (crossterm via ratatui::crossterm)
remotefs 1.0 + -ssh + -ftp      SFTP/SCP/FTP/FTPS, behind one adapter
ssh2                            the host key pre-check, which remotefs-ssh
                                  does not do at all
ssh2-config                     reading ~/.ssh/config
serde + toml                    config, bookmarks, profiles, themes
serde_json                      the bisync snapshot and the audit log
keyring                         password storage (Secret Service / kwallet)
notify                          watch mode
clap                            CLI
chrono                          both sides' timestamps on every row
tracing + tracing-appender      logs to $XDG_STATE_HOME/lazysync/
thiserror / anyhow              errors
```

Not depended on, and why:

- **`remotefs-aws-s3` / `-webdav` / `-smb`** — the first two still require
  `remotefs 0.3`, an incompatible version of the same trait, and SMB needs
  `libsmbclient`. All three are reached through `rclone` instead.
- **`blake3` / `md5` / `sha2`** — `compare.method = "checksum"` is parsed and
  shapes the plan's reported method, but no hashing is implemented, so there
  is nothing yet for a hash crate to do.
- **`notify-debouncer-*`** — the debounce window is a config option, a burst
  has to collapse into one *plan* rather than one event per file, and the
  debouncer crates bring their own runtime to avoid about twenty lines.

**No async runtime.** The plan originally called for tokio. It is not there,
because the `remotefs-*` crates are themselves blocking: an async `Vfs` would
mean wrapping sync calls in `spawn_blocking` and paying async's complexity for
none of its benefit. The trait is blocking, and the UI stays responsive by
*stepping* the transfer queue a chunk at a time on the main loop rather than
handing a backend to a worker thread — the panes own the `Vfs` values and
`Vfs` is `Send` but not `Sync`, so a thread would mean the browser losing its
backends for the duration of every sync. See the phase 4 row. If a later phase
needs real concurrency across many transfers, multiple channels within one SSH
session is the smaller change.

**crossterm is not a direct dependency.** `ratatui-crossterm` can build against
either 0.28 or 0.29, so a second direct dependency risks resolving to the other
one — at which point `KeyEvent` from the re-export and `KeyEvent` from our own
dependency are different types. Everything goes through
`ratatui::crossterm`.

Tree-wide conventions copied from `jambi`: `rust-toolchain.toml`,
`.rustfmt.toml`, `.cargo/config.toml`, `.envrc`, `flake.nix` + `package.nix`,
`config.example.toml`, dual `LICENSE-MIT` / `LICENSE-APACHE`.

## 3. Module layout

What is actually there, as built. The original sketch is in the git history;
three differences are worth noting because they were decisions rather than
drift:

- **`vfs/remote.rs` instead of one file per protocol.** `remotefs-ssh`,
  `-ftp` and the rest all implement the same trait, and everything on top of
  it is identical — so SFTP, SCP, FTP and FTPS share one adapter and each
  protocol module is only "build the client, hand it here" (phase 5).
- **No `sync/executor/` directory and no `Executor` trait.** Delegation turned
  out not to be a polymorphic choice of engine: it is one extra *stage* of the
  existing queue, because the tool only ever moves bytes and the native engine
  still performs the deletions (phase 7).
- **`vfs/rclone.rs` is a `Vfs`, not just an executor**, which is how S3,
  WebDAV, SMB and ~70 others are reached at all.

```
src/
  main.rs            entry, CLI dispatch, headless vs TUI
  cli.rs             clap definitions
  lib.rs
  app.rs             App state machine, screens, action dispatch
  event.rs           key/chord multiplexer
  tui.rs             terminal init/restore, panic hook, main loop

  config/
    mod.rs           Config, load/merge/validate, XDG paths
    theme.rs         palettes and presets

  keys/              <- inlyne-shaped
    action.rs  combo.rs  defaults.rs  keymap.rs  serialization.rs

  vfs/
    mod.rs           trait Vfs + Entry/Capabilities/WriteMetadata
    local.rs         the local filesystem
    remote.rs        one adapter for every `remotefs` backend
    ssh.rs  ftp.rs   per-protocol construction only
    rclone.rs        a Vfs over the rclone CLI: ~70 remotes
    hostkey.rs       known_hosts verification, written from scratch

  bookmarks/
    mod.rs           bookmarks.toml
    secrets.rs       keyring

  browser/
    pane.rs  sync_browse.rs  union.rs  sort.rs

  sync/
    compare.rs       what "the same file" means for a pair of backends
    diff.rs          the one-directional tree walk
    bisync.rs        the two-way classifier, plus its own symmetric walk
    state.rs         the bisync snapshot
    plan.rs          Plan/Row/Resolution/Section/Totals
    safety.rs        the delete limits and refusals
    filters.rs       gitignore-syntax excludes, memoised
    transfer.rs      the stepped queue, and `Settings`
    executor.rs      the blocking entry point, rename/delete primitives
    external.rs      how a backend names itself to an external tool
    delegate.rs      running rsync/rclone as a transport
    watch.rs         notify + debouncing
    audit.rs         the append-only run log
    progress.rs      where a tree walk reports to while it is running

  ui/
    mod.rs  panes.rs  confirm.rs  progress.rs  status.rs  help.rs
    bookmarks.rs  form.rs  form_view.rs  log.rs
    hint.rs          every on-screen key hint, from the resolved keymap
    keybar.rs        the browser's hint bar, and the which-key chord list
    hit.rs           what the last frame drew where, so a click can land
    question.rs      the two-button yes/no dialog
    prompt.rs        the password window

  integrations/
    zide.rs          cwd -> bookmark resolution
```

## 4. Keybindings

Directly borrowed from `inlyne/src/keybindings/`: an `Action` enum paired with
`KeyCombo(Vec<ModifiedKey>)`.

Extension inlyne doesn't need: **per-mode keymaps**, because `d` means delete in
the browser and a literal `d` in a text field.

**Four tables per mode, not two.** `base` and `extra` could not express the
ordinary case — change a handful of keys and have the ones they replaced stop
working — because `extra` resolves collisions by *key* and so can add a
binding but never move one. Applied in this order:

| Table | Means | Resolves collisions by |
| --- | --- | --- |
| `base` | start from nothing but this | — (it *is* everything) |
| `set` | these actions get exactly these keys | **action** |
| `extra` | these keys get these actions | **key** |
| `unbind` | these keys do nothing, prefix-wise | — (it only removes) |

```toml
[keys.browser]
set = [
  ["ToggleSyncBrowse", "S"],
  ["SyncPush",         ["s", "p"]],
  ["DeleteFile",       ["d", "d"]],
]
unbind = ["esc"]        # let a multiplexer have it

# modes: bookmarks, browser, confirm, question, form, prompt, help, queue, log
```

Vim-shaped defaults, as actually bound: `h j k l`, `gg G`, `ctrl+d/u`,
`ctrl+f/b`, `/ n N`, `space` mark with `ma`/`mc`/`mi`, `tab` switch pane,
`S` sync-browse, `u` union view, `o`/`O` sort, `.` dotfiles, `c`
connections, `a`/`A`/`r`/`dd`/`p` file operations, `s`-prefixed sync chords,
`q`/`ZZ` quit, `?` help. There is no visual-select mode, no `:` command line,
and no `yy`/`p` yank-and-put — marks do the work a visual selection would.

- `lazysync keys [--mode browser]` prints the resolved keymap, **and names
  any action that ended up with no key**. Every editing table can strand one:
  `set` and `extra` by taking a key something else was using, `unbind` by
  aiming at a chord prefix. At runtime that is indistinguishable from a
  broken feature, so it is reported rather than left to be found.
- The `?` overlay is generated from the live keymap, never a hardcoded
  string, and so is every footer and hint — see `ui/hint.rs`.
- `chord_timeout_ms` (default 500) is **not** a clock on typing a chord. It
  only applies where a chord has a shorter binding to fall back on, like `d`
  alongside `dd`; none of the defaults has one, so with a stock keymap a
  chord waits as long as you do and your cancel key is the way out.

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

Going **above** a sync root is the same idea pointed the other way. There is
no relative path to follow, so the follower holds — but the pairing is kept,
and walking back in re-couples the panes on its own.

It must not re-anchor. The two roots are not required to share a name (`sandbox
subdomain` paired with `www`), so once the original pair is discarded nothing
can reconstruct it: descending back into the directory you just left asks the
follower for a *sibling of its own root*, which does not exist, and the panes
appear broken with no way back short of reopening the bookmark. Re-pairing is
available deliberately — toggling sync-browse off and on anchors the two
directories you are looking at.

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

Edge cases, and where each stands:

- **Permission-denied on one side** — handled. The subtree is recorded in the
  plan's `unreadable` list, prefixed with which side refused, rather than
  silently reading as empty.
- **A counterpart that is a file, not a directory** — handled. The follower
  stats rather than merely checking existence, so it does not show an empty
  listing with no explanation.
- *(not handled)* **Symlinks** — not transferred at all, so the follow-vs-show
  question has not had to be answered yet. This is also why
  `safety.confirm_outside_root` is parsed and not honoured: nothing can
  currently escape the root this way.
- *(not handled)* **Case-insensitive remotes** — two names differing only in
  case would be two entries here and one on the server.
- *(not handled)* **`\` vs `/` separators**, and **roots that are themselves
  symlinks**.

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
- `zc`/`zo` fold and unfold a section. The heading of a folded one names the
  unfold key from the live keymap rather than as a literal.
- Conflicts are settled in place: `L` keep local, `R` keep remote, `B` keep
  both, `X` defer. An undecided conflict blocks the whole plan.

Still outstanding, and each says so when pressed rather than doing nothing:

- *(not built)* `dd` — an inline unified diff of the row before deciding.
- *(not built)* `c` — re-compare the selected rows by checksum rather than
  size+mtime. Blocked on there being any hashing at all; see §2.
- *(not built)* `enter` on a directory op, to drill into its file-level ops.
- *(not built)* writing the plan to JSON/Markdown for auditing `--yes` runs.
  The audit log covers most of what this was for.

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
default_sync  = "push"               # what Enter on a file runs; unset = off
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
- A built-in **Local transfer** row, last in the list, opening two local
  panes. It cannot be deleted and is never written to `bookmarks.toml`.
- *(not built)* `lazysync import termscp`. Its format carries an encrypted
  password blob, and an importer written against a format that cannot be
  tested here would be guesswork presented as a feature.

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

**Getting out of the way without dropping the connection.** Quitting closes
the session, which makes "I want my terminal back for a minute" cost a
reconnect. `Hide` runs `[zide] hide_command` and keeps running behind it, so
the connection, the marks, the cursor and a transfer in flight are all still
there when the pane comes back.

```toml
[zide]
hide_command = ["zellij", "action", "hide-floating-panes"]
```

Empty by default and unbound by default, and both halves matter: lazysync
cannot know what is hosting it, so nothing is detected or guessed — and a key
bound to a no-op reads as a broken feature where an unbound one sends you to
`?`. Bringing the pane back is the host's job (`ToggleFloatingPanes`, `Alt+f`
in zellij); a keybind that *runs* lazysync starts a second one beside the
hidden pane.

Two things from the original sketch that were **not built**:

- *(not built)* a shipped `layouts/default_lazysync.kdl`. There is no
  `layouts/` directory; the README documents the pane template instead, which
  is one copy of the information rather than two that can disagree.
- *(not built)* `$XDG_STATE_HOME/lazysync/session.toml` for last-used
  bookmark and pane paths. zide auto-open already resolves the working
  directory to a bookmark, which covers the case this was for without a
  second source of truth about where you were.

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
7. ~~**Bisync conflict resolution UI**~~ — done in phase 6. `L` keeps local,
   `R` keeps remote, `B` keeps both and `X` defers, on the row under the
   cursor, from the Conflicts section.

   Two decisions worth recording. An **undecided conflict blocks the whole
   plan**, rather than being skipped along with its own row: running the other
   ninety-nine rows and quietly leaving one behind would mean the next run
   compares against a half-synced tree with nothing on screen to say so.
   Exclude it with `space` to defer it deliberately.

   And **keep-both does not keep each side's own version at the original
   name**, which is the obvious reading and is wrong — the two sides would
   stay different and raise the same conflict on every later run, for ever.
   The winner lands at the original name on *both* sides and the displaced
   copy is renamed beside itself with `safety.conflict_suffix`. Nothing is
   lost and the pair converges.
8. ~~**Host-key verification UX**~~ — done in phase 2, and it turned out to be
   mandatory rather than a nicety: `remotefs-ssh`'s libssh2 backend does not
   consult `known_hosts` at all, so without this lazysync would hand both your
   credentials and your files to whatever answered. Implemented as a pre-flight
   handshake that reads the key and stops, *before* any password is collected —
   so you are never asked to type a secret for an unverified server. A changed
   key is refused outright rather than offered as a prompt.
9. **Permission/ownership preservation** — mode is preserved where the
   protocol has it, and the window says `the destination has no permission
   bits` where it does not. Ownership is not touched at all: setting a uid on
   a server you are not root on fails, and a sync that reports preserving
   something it did not is worse than one that never claimed to.
   `preserve_mtime` and `preserve_mode` are both always on; the flags that
   would turn them *off* are parsed and not honoured.
10. **Local↔local and remote↔remote** syncs, not just local↔remote.
    Local↔local is done, as the built-in `Local transfer` entry.
    Remote↔remote is **not**: pane 0 is always the local filesystem, and a
    connection opens into pane 1, so there is nowhere to put the second
    session. It needs the pane array to stop encoding which side is which.
11. ~~**Headless CLI**~~ — done in phase 7. `lazysync sync <profile>
    --dry-run|--yes` with no TUI, plus the append-only audit log. A bisync
    profile is refused rather than resolved unattended.
12. **File ops parity with termscp** — chmod, mkdir, rename, delete, create,
    multi-select and `$EDITOR` are all done; see §10a. What is left is a
    built-in *viewer* (a pager for a file you do not want to risk editing)
    and `:!cmd` remote exec, which needs a command line lazysync does not
    have — there is no `:` mode, by the same reasoning that left out visual
    select.
13. ~~**Session keepalive + auto-reconnect**~~ — done; see §10a.
14. ~~**Theme file**~~ — done. `[theme] preset` with terminal-default,
    catppuccin-mocha, gruvbox-dark and nord, plus per-slot `[theme.colors]`
    overrides across nineteen slots. The `terminal` preset is `Color::Reset`
    plus the sixteen ANSI colours, so a transparent terminal stays
    transparent — which an overlay's dimming had to be written to preserve.
15. ~~**Home-manager module**~~ — done in phase 7. `settings` owns
    `config.toml`, which lazysync only reads. `bookmarks` is offered and off
    by default, because lazysync *writes* that file from the UI and the store
    is read-only; see the phase 7 row for the symlink bug this exposed.
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
17. ~~**Dir size on remote**~~ — done. `dir_sizes` is off by default and `z`
    toggles it, because it costs one listing per subdirectory and over SFTP
    that is slow enough to feel. Directories show `-` when it is off, which
    is the honest rendering of a number nobody has measured.

## 10. Phases

| Phase | Deliverable |
| --- | --- |
| 0 | ✅ Repo scaffolding: flake, package.nix, CI, licenses, README skeleton, this doc |
| 1 | ✅ TUI shell, config loader, keymap engine, theme, help overlay — local↔local dual pane, no network. Synchronized browsing with ghosting, and union view, landed here too: they are pure path logic and needed no network, and building them first meant the `Vfs` trait was shaped by a real caller. |
| 2 | ✅ `Vfs` + Local + SFTP/SCP; connection form; bookmarks + keyring; host-key TOFU; zide auto-open. Two things the underlying crates do not provide had to be written here: host key verification (`remotefs-ssh`'s libssh2 backend consults `known_hosts` nowhere at all), and an explicit `SshKeyStorage`, without which naming an identity file silently did nothing. `lazysync import termscp` slipped to phase 7 — its format carries an encrypted password blob, and an importer written against a format that cannot be tested here would be guesswork presented as a feature. |
| 3 | ✅ Diff planner + confirmation window + native executor (push/pull, mirror). Three things the design did not anticipate: **SFTP's rename does not replace an existing file** (OpenSSH answers `SSH_FX_PERMISSION_DENIED`), so overwrites go through a three-step swap — see `Capabilities::atomic_overwrite_rename`; a **percentage-only delete guard misfires on small trees**, where removing one of two files reads as 50%, so a floor was added with a separate all-or-nothing check that fires at any size; and the **backup directory had to be excluded from planning**, or the second mirror run deletes the backups the first one made. |
| 4 | ✅ Transfer queue, progress, resume, filters, sync profiles. (Safety rails landed early, in phase 3.) The queue is a state machine the main loop steps a chunk per frame rather than a worker thread: the panes own the `Vfs` values and an SFTP session cannot be cloned without asking for the password again, so a thread would mean the browser losing its backends for the duration. Stepping gives progress and cancellation with no shared state. Two things worth noting: `Vfs` gained streaming handles, so a 4 GB file no longer costs 4 GB of memory; and the glob matcher had to be memoised after `*a*a*a*a*a*a*b` against forty `a`s took **3.7 seconds** — the patterns come from the user's own config, so that was a filter line able to freeze the planner. |
| 5 | ✅ One generic `RemoteAdapter` for every `remotefs` backend; FTP + FTPS on top of it. The phase was planned as "all remaining backends plus the rsync/rclone executors" and was cut down deliberately: **WebDAV and S3 cannot go in at all right now** — `remotefs-webdav` and `remotefs-aws-s3` still depend on `remotefs 0.3`, which defines a *different, incompatible* `RemoteFs` trait, so adding either puts two versions of the trait in one binary. SMB needs `libsmbclient` and a share field the connection schema does not have. All three move to phase 6. What landed instead paid for itself twice over: generalising the adapter cut `ssh.rs` from 978 lines to 387, and the real-server fixture caught four bugs. One is a comparison bug affecting **every** destination that cannot set an mtime (see §9.2). The others were FTPS: the default port was 990, the *implicit* FTPS port, while `secure()` performs *explicit* FTPS — a plaintext connect then `AUTH TLS` — so `ftps://host` would have opened a plaintext connection to a port awaiting a TLS handshake; a rejected certificate surfaced as "the server refused the connection", because `remotefs` reports a failed handshake as `ConnectionError` with the cause in the source and `translate` was collapsing that into the payload-free `NotConnected`; and the certificate test itself passed for the wrong reason until the fixture stopped exporting a port before confirming something was listening on it. |
| 6 | ✅ Bisync, state snapshots and the conflict UI. Scoped down again: S3/SMB/WebDAV are still blocked on `remotefs 1.0` ports, and the rsync/rclone executors move to phase 7 — bisync was the only one of the four sync modes still missing and it was worth doing properly rather than alongside two other large pieces. The structural change is that **a plan row now names the side it writes to**, so one plan can move files both ways and the plan-is-the-dry-run rule survives a two-way sync. That refactor is also where the phase's two worst bugs lived, both caught by existing tests: `executor::apply` took a source and a destination and so could not tell *which pane* its "source" was, which silently reversed every pull; and the queue re-derived each file's destination from a single plan-wide `dest_root` at completion time, so a bisync row writing locally had its partial file renamed onto the **remote** — reported as a successful transfer while landing on the wrong side. |
| 7 | ✅ rsync + rclone executors, `RcloneFs`, watch mode, audit log. The demo cast was dropped by agreement rather than attempted badly; the home-manager module followed shortly after, together with the `dirs` 5→7 bump that removed a duplicate copy of `dirs` and `dirs-sys` from the build (we pinned 5 while `ssh2-config` pulled 7).

  The module's shape is decided by which files lazysync *writes*. `settings` owns `config.toml`, which lazysync only reads — free to manage declaratively. `bookmarks` is offered but off by default, because lazysync writes `bookmarks.toml` from the UI and home-manager points it at the read-only store, so declaring connections means the editor has nowhere to save. Implementing it also exposed a real bug: `BookmarkStore::save` renamed *onto* its path, and `rename(2)` over a symlink replaces the link — so a dotfiles symlink would have been silently turned into a regular file and a home-manager symlink clobbered, surfacing later as a failed `home-manager switch`. It now resolves the link and writes through it, refusing with an explanation when the target is read-only. Two findings shaped the phase. First, **delegation must not make decisions**: `rsync --files-from` needs `--ignore-times` or rsync's own size-and-mtime check silently skips a row the window showed as an overwrite — verified by removing the flag and watching the test fail. Second, **`auto` had to stop delegating**: the moment it did, three existing tests failed, because a delegated transfer does not re-check the destination before writing (so a file changed after planning gets clobbered rather than skipped), has aggregate rather than per-file progress, and does not preserve a keep-both conflict's displaced copy. Those are trades worth offering to someone who types `executor = "rsync"`; making them silently under a setting that promises "the best available" is not. Keep-both is refused outright even when a tool is named — that one is data loss, not a trade. And `rclone moveto` turned out to delete the source while leaving the destination unchanged whenever the two were the same size, exit code 0: a finished partial file renamed into place is *always* the same size as what it replaces, so this would have silently reverted every rclone transfer. |

Phases 1–3 are the usable milestone: dual-pane browsing over SFTP with
sync-browse and a reviewed push.

## 10a. After phase 7

Not a phase. Everything here came from using the thing — mostly from running
it in a zellij popup against a real host — and is recorded because the
reasons are not recoverable from the diff.

**The UI was describing itself from memory.** Every footer was a string
literal listing the *default* keys, so a rebinding made lazysync confidently
name keys that did nothing — worst of all in the confirmation window, whose
footer said `n cancel` while `n` was bound to `ParentDir`. `ui/hint.rs`
renders every hint from the same `Keymap` the dispatcher resolves against,
which makes the two unable to disagree, and `lazysync keys` now reports any
action left with no key at all.

**A chord gave no evidence it had landed.** `s` is bound to nothing on its
own, so pressing it looked exactly like a key the terminal had swallowed.
The key bar now becomes the list of what the chord can still become, the
partial chord is echoed bottom-right the way vim's `showcmd` is, and the
cancel key backs out of one. `chord_timeout_ms` turned out to be actively
harmful: with no shorter binding to fall back on it could only ever *discard*
a chord mid-typing, which is what it had been doing.

**Planning froze the terminal.** `sync/progress.rs` is a `Progress` trait the
walk reports to, with a `Flow::Cancel` it can be stopped by; the TUI animates
a spinner with running counts and watches for the cancel key, and everything
headless uses `Silent`. A cancelled walk discards its plan rather than
offering a subset nobody chose. **This is a stopgap and should be read as
one:** the walk still owns the thread, so the browser underneath is frozen
rather than merely busy. The real fix is a planner that yields the way
`sync::transfer` already does.

**The mouse.** `ui/hit.rs` collects the rectangles each widget just drew,
rebuilt every frame, so a click is answered by the frame in front of the
user. One click selects and a double click runs the *same action the keyboard
binding does* — so the mouse can never do something the keyboard cannot, and
a stray click cannot exclude a row from a sync. Reporting is `?1000`+`?1006`
written out rather than crossterm's `EnableMouseCapture`, which also enables
motion reporting and would wake the loop for every cell the pointer crosses.

**Two screens were lying about their keys.** The yes/no dialog shared
`[keys.confirm]`, and the two disagree about `n` — a motion in a remapped
keymap, the answer "no" in a dialog — so `Mode::Question` is its own mode
with its own map. It is two buttons rather than two letters, and **opens on
"no"**, because every question lazysync asks guards something it cannot take
back and Enter is the key people press without reading.

**The password prompt was furniture.** Collected on the status line, in the
same row and colour as the mode badge and the cursor counter. It is now a
centred window in lazygit's shape — titled, bordered, `*`-masked, naming the
connection. Search and the bookmark filter stay on the status line, because
a box in the middle of the screen covers the listing you are searching.
Overlays also dim what is behind them and keep clear of the panes' bottom
border row, after a report that the help overlay's border and the pane
borders read as one shape with a seam in it.

**File operations**, the subject of §9.12: `a` new file, `A` new directory,
`r` rename, `dd` delete, `p` chmod, on either pane. The `Vfs` primitives had
existed since phase 3 and run on every sync; only the UI was missing.
`set_mode` is new, and is separate from `set_times` because that one is gated
on `set_mtime` — a backend that can chmod and cannot set timestamps was
unable to do the thing it *can* do. An existing name is refused for rename as
well as create, since POSIX `rename(2)` replaces silently and SFTP's refuses,
and leaving that to the backend makes the behaviour protocol-dependent.
Delete asks first, takes the marks over the cursor, and is a **real** delete:
the `.lazysync-backup/` path exists because a *plan* removes files nobody
looked at individually, which is not what the row under your cursor is.

**Keepalive and reconnect**, the subject of §9.13. `Vfs::has_session` decides
whether to schedule anything, so two local panes still block on input and
never wake. The ping is a `stat` — `remotefs` has no NOOP, and a stat proves
the session can still *do* something — and a missing path is explicitly not a
dead connection. A reconnect swaps the backend underneath the pane so the
directory, cursor and marks survive. It is only ever silent where the
credential can be had again without asking: agent auth, key auth, or a
password in the keyring. A password typed once and deliberately not stored
cannot be replayed, and a password box appearing from a timer is worse than a
message.

**`$EDITOR` integration**, the rest of §9.12. `e` opens the selected file;
a remote one is fetched to a scratch directory, edited, and written back.
The split is the interesting part: `App` cannot run the editor, because the
editor is a terminal program and lazysync is holding the terminal — so `App`
does the halves it can (resolve the command, fetch the file) and leaves an
`EditRequest` behind for `tui` to run, the same division as
`announce_work`/`act`. Everything that can fail happens *before* the terminal
is handed over, since discovering there is nothing to edit after the screen
has gone leaves the user looking at their shell.

Three rules it inherits from the rest of the program. Nothing is uploaded
unless the editor actually wrote, or an unchanged file would come back with
a new mtime and the next sync would call it changed. The destination is
re-checked immediately before the write, which is what the native executor
does and what a delegated transfer gives up. And the scratch directory is
removed however the edit ends, including when the upload is refused —
`tempfile` moved from a dev-dependency to a real one for this, because
`fs::create_dir` would leave a world-readable directory holding a file
fetched from somebody's server.

**Enter on a file** runs the connection's `default_sync` on that file, or on
the marked ones. Off until set, because Enter is the key people press without
having decided to. A `bisync` default is refused for a selection rather than
widened to the whole tree.

## 11. Open questions

1. **Protocol priority** — which do you actually use day to day? SFTP and FTP
   are in; S3, SMB and WebDAV are blocked rather than deprioritised (see the
   phase 5 row), so the question is now whether any of them is worth the work
   their blockers need. S3 wants new connection fields (bucket, region,
   credentials) and either a `remotefs 1.0` port or a hand-written backend;
   SMB wants `libsmbclient` as a system dependency.
2. ~~**rclone's role**~~ — decided in phase 7: **both**. `vfs::rclone` is a
   full `Vfs` over the `rclone` command line, so a bookmark can point at any
   remote in `rclone.conf` and the browser, planner, confirmation window and
   queue all work against it unchanged. That also routes around the
   `remotefs 0.3`/`1.0` trait split that had blocked S3, WebDAV and SMB for
   two phases: they are reachable as rclone remotes without waiting for
   upstream ports of any of them.

   A subprocess per operation rather than the `rcd` daemon. The daemon is
   faster, but it needs an HTTP client and a port to bind — and binding a port
   means answering what happens when it is taken, when the daemon dies
   mid-transfer, and who stops it if lazysync is killed. One `rclone lsjson`
   per directory entered is the same order as the round trips SFTP already
   makes, and reads and writes stream over a single process for the whole
   file.
3. **Compare default** — size+mtime (fast, default) vs. always-checksum
   (correct, slow). And the mtime tolerance window default: 1s or 2s?
4. ~~**Watch mode confirmation**~~ — decided in phase 7: both, per profile.
   `watch = "batch"` collects changes until the tree has been quiet for
   `debounce_ms` and then opens one plan; `watch = "auto"` applies without
   asking. The tension in the question was real, so it is resolved by making
   the unattended version opt-in and clearly labelled rather than by picking
   one. The audit log records which runs nobody saw.

   The batching is over *paths*, not events — a burst touching one file forty
   times is one path — and the batch decides *when* to plan, never what: the
   planner walks the trees as usual, because syncing only the paths that fired
   events would miss whatever inotify dropped and would make watch mode
   disagree with what `sp` does on the same trees.
5. ~~**Bisync state location**~~ — decided in phase 6:
   `$XDG_STATE_HOME/lazysync/bisync/<digest of the two roots>.json`. Beside the
   tree was tempting (survives a reinstall, visible from both ends) but does
   not work: a file inside the tree is *part of the tree being compared*, so it
   would be synced — it changes on every run, both machines would write their
   own, and the two would conflict with each other on every single sync.
   Excluding it by name fixes that and creates a worse problem, since the
   exclusion would have to hold for every profile and a user who overrode the
   filters would silently start syncing their own bookkeeping.
6. **Single instance** — should a second zide popup reattach to the running
   instance over a socket, or just start fresh?
7. ~~**Deletes in mirror mode**~~ — decided in phase 3: **both, in that
   order**. `backup_dir = ".lazysync-backup"` is the default, so a planned
   deletion is recoverable; setting it to `""` gives real deletion and asks a
   second time. The `max_delete` guards apply either way, since a backup
   directory does not make deleting four hundred files what you meant. The
   backup directory is excluded from planning, or the next mirror deletes the
   backups the last one made.

   A *manual* delete (`dd` in the browser) deliberately does not use it. The
   backup exists because a plan removes files nobody looked at individually;
   the row under your cursor is not that, and a safety net implied and not
   provided is worse than none.
8. **Crate name** — `lazysync` on crates.io is unverified (crates.io blocked my
   automated check); worth confirming before the first publish. The GitHub name
   `guttermonk/lazysync` is free.
