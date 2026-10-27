# lazysync

A dual-pane terminal file manager and sync tool. [termscp][termscp]'s feature
set, vim keys by default with every binding remappable, and a confirmation
window that shows you both sides of every overwrite — size and timestamp,
source and destination — before anything moves.

> **Status: feature-complete against the plan.** Plan a push, pull, mirror or
> two-way bisync, review every row, and watch it transfer — with progress,
> throughput, ETA, pause and cancel, resumable partial files, and
> gitignore-style exclude rules driven by named sync profiles. Over the local
> filesystem, SFTP/SCP, FTP/FTPS, or **any `rclone` remote** — which is how S3,
> WebDAV, B2 and Google Drive are reachable. Optionally delegate the transfer
> to `rsync` or `rclone`. Watch a tree and sync what changes. Every run is
> recorded in an audit log.
>
> What is not there: no inline diffs and no checksum re-verification from the
> confirmation window, and transfers are sequential — one file at a time.
> `s3://`, `smb://` and `webdav://` are still refused as *native* protocols
> (the `remotefs` backends for the first two need an older, incompatible
> version of the trait every other backend implements) — use an rclone remote
> instead. See [`docs/PLAN.md`](docs/PLAN.md).

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
- **No advanced sync.** lazysync has a native sync engine, and can hand the
  transfer to `rsync` or `rclone` when you ask it to — see
  [below](#delegating-to-rsync-or-rclone) for what that trades away.

## Features

Planned for 1.0, phased in the order below. See
[`docs/PLAN.md`](docs/PLAN.md) for the full breakdown.

- Dual-pane browser over SFTP ✅, SCP ✅, FTP/FTPS ✅, and any `rclone`
  remote ✅ — which covers S3, WebDAV, B2, Google Drive and some seventy more
- Sync modes: push/pull without deletes ✅, mirror with deletes ✅, two-way
  bisync with conflict resolution ✅, and a file watcher ✅
- Confirmation window with per-row exclusion ✅; inline diffs and checksum
  re-verification are not implemented
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
- Headless `lazysync sync <profile> --yes` for cron ✅, with an audit log ✅

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

#### home-manager

```nix
{
  imports = [ inputs.lazysync.homeManagerModules.lazysync ];

  programs.lazysync = {
    enable = true;

    settings = {
      show_hidden = false;
      sync_browse.enabled = true;
      compare.mtime_tolerance_secs = 2;

      profiles.deploy = {
        mode = "push";
        exclude = [ "/target/" "*.log" ];
      };
    };
  };
}
```

`settings` becomes `config.toml`, which lazysync only ever reads — so owning
it declaratively costs nothing, and an unknown key is a startup error rather
than silently ignored.

There is also a `bookmarks` option, deliberately off by default. lazysync
*writes* `bookmarks.toml` when you add (`n`), edit (`e`) or delete (`dd`) a
connection, and home-manager points that file at the read-only Nix store — so
declaring your connections means the editor has nowhere to save. lazysync
detects it and says so plainly rather than appearing to work, but the trade is
real: set it only if you want your connection list to be configuration. Never
put a password in it, since the Nix store is world-readable.

#### Overriding the package

Configure with `.override` rather than by copying files over the result — the
store is read-only, and anything copied in is clobbered on the next update:

```nix
inputs.lazysync.packages.${system}.default.override {
  # External tools put on lazysync's PATH. `rsync` is purely an optional
  # executor. `rclone` is also how `rclone://` remotes work at all -- and so
  # how S3, WebDAV, B2 and Google Drive are reached -- so dropping it loses
  # more than an executor.
  syncBackends = [ pkgs.rsync pkgs.rclone ];
}
```

### Cargo

```sh
cargo install --locked --git https://github.com/guttermonk/lazysync
```

`rsync` is optional: the native engine needs it for nothing, and is what runs
unless a profile names a tool. `rclone` is optional too, but `rclone://`
remotes need it -- which is how S3, WebDAV, B2 and Google Drive are reached.

## Usage

```sh
lazysync                              # the saved connections
lazysync open www-prod                # open one by name
lazysync sftp://user@host/srv/www     # connect directly
lazysync rclone://gdrive/Documents    # any remote in your rclone.conf
lazysync /mnt/backup/proj             # the working directory against that
lazysync ~/proj /mnt/backup/proj      # one directory in each pane
lazysync keys --mode browser          # dump the resolved keymap
lazysync config                       # dump the resolved config as TOML

lazysync sync deploy ~/proj /mnt/backup --dry-run   # print a plan, move nothing
lazysync sync deploy ~/proj /mnt/backup --yes      # headless, for cron

# Not implemented:
lazysync import termscp               # import termscp bookmarks
```

These are the **default** bindings. Every one of them is remappable (see
below), and `lazysync keys` prints what yours actually resolve to — which is
also what the on-screen hints are generated from, so the footers follow your
config rather than this page.

In the connection list: `enter` opens, `n` adds, `e` edits, `dd` deletes, `/`
filters, `esc` clears the filter.

The list always ends with a built-in entry, **Local transfer**, which opens
two local panes. Syncing `~/photos` to a mounted backup drive or a NAS share
is a real sync that needs no server, so it is a row you open like any other
rather than a mode you reach by pressing Escape. It cannot be deleted and is
never written to your `bookmarks.toml`.

From the browser, `c` goes back to the connection list. Each pane is
labelled with the backend it is actually showing — `local`, `sftp`, `ftp`,
`rclone` — so a pane never claims to be somewhere it isn't.

In the browser: `h`/`j`/`k`/`l` navigate, `tab` switches panes, `S` toggles
synchronized browsing, `u` toggles union view, `/` searches, `space` marks,
`o` cycles the sort, `?` shows every binding, `q` quits. `sp` plans a push,
`sl` a pull, `sm` a mirror, `sb` a two-way bisync, `ss` uses the connection's
own sync profile, `sw` watches the tree, `sL` shows the audit log. All of it
remappable — see below.

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

### Two-way sync

`sb` syncs both ways in one pass. The plan's header shows `⇄` rather than `→`,
and rows point in whichever direction the file moved:

```
 Sync  ~/proj  ⇄  box:/srv/www        compare: size+mtime  tolerance: 2s  deletions move to .lazysync-backup/
┌───────────────────────────────────────────────────────────────────────────────────────────────┐
│    ▾ Create (2)                                                                               │
│ +  notes/today.md                  2.1 K  2026-10-25 09:14        —                  +2.1 K   │
│ +  build/report.json                 —                          8.4 K  2026-10-24 22:03  ←     │
│    ▾ Delete (1)                                                                               │
│ ✗  old-draft.md                       —                          1.2 K  2026-10-20 11:40  deleted on one side │
│    ▾ Conflicts (1)                                                                            │
│ ⚠  index.html                      11 K  2026-10-25 08:12       9 K  2026-10-25 08:30  BOTH SIDES CHANGED — L/R/B/X │
└───────────────────────────────────────────────────────────────────────────────────────────────┘
 2 ops · 2.1 K out · 1 deleted (1.2 K) · 1 conflict(s) · 1 UNDECIDED
 y apply · n cancel · space include/exclude · L keep local · R keep remote · B keep both · X leave alone
```

Two-way sync needs to know what the trees looked like **last time**, because
"here on the left, missing on the right" has two readings that call for
opposite actions: it was created on the left, or it was deleted on the right.
Nothing in the current state of the two trees distinguishes them. lazysync
keeps a snapshot per pair under `$XDG_STATE_HOME/lazysync/bisync/` — not inside
the synced tree, where it would be part of what is being compared and would
conflict with itself on every run.

Two consequences worth knowing before you use it:

- **The first bisync of a pair never deletes anything.** With no snapshot there
  is no evidence that a one-sided file was removed rather than created, so both
  sides simply gain what they are missing. The window says so.
- **When both sides changed, lazysync will not guess.** The row becomes a
  conflict and *the whole plan refuses to run* until you settle it — `L` keeps
  the local copy, `R` the remote, `B` keeps both, `X` leaves it for next time.
  Picking the newer file automatically is the obvious shortcut and it is wrong
  twice over: it discards the other edit silently, and "newer" is a claim about
  two clocks that may disagree by more than the edits do.

`B` keeps the local version at the original name on *both* sides and renames
the displaced copy beside itself with `safety.conflict_suffix`. Leaving each
side its own version under the original name would be the obvious reading of
"keep both" and would mean the pair never converges — the same conflict would
come back on every run, for ever.

Bisync is interactive-only. `lazysync sync` refuses a bisync profile rather
than resolving conflicts on its own, because a cron job that silently picks a
side is one that works until the day it matters.

### Delegating to rsync or rclone

`executor` in a sync profile picks what moves the bytes:

```toml
[profiles.deploy]
mode = "push"
executor = "rsync"      # "auto" | "native" | "rsync" | "rclone"
```

**`auto` means the native engine.** That is not what the name suggests, so it
is worth saying why. A delegated transfer gives up three things:

- the destination is no longer re-checked immediately before it is written, so
  a file that changed *after* you reviewed the plan is overwritten rather than
  skipped;
- progress and cancellation become one aggregate line rather than per file;
- a conflict resolved "keep both" does not get its displaced copy preserved.

Those may be a fair trade for rsync's delta transfer on a large changed file.
They are not a fair trade to make *silently*, under a setting that promises
only "the best available" — so you get them by naming the tool. (Keep-both is
refused outright even then: that one is data loss rather than a trade.)

When a tool does run, it is used as a **transport, not a planner**. It is
handed the exact list of paths the plan contains, with the flags that stop it
deciding anything for itself — without `--ignore-times`, rsync's own
size-and-mtime check silently skips files lazysync decided had changed.
Deletions always stay with the native engine, so the backup directory and the
delete limits keep working unchanged.

### Watching a tree

```toml
[profiles.notes]
mode = "push"
watch = "batch"         # "off" | "batch" | "auto"
debounce_ms = 2000
```

`sw` starts and stops it. Changes are collected until the tree has been quiet
for `debounce_ms`, then `batch` opens one plan for review and `auto` applies
without asking. A watcher that stops for a modal on every save is useless,
and one that never confirms contradicts the rest of this program — so both
exist and the unattended one is opt-in.

Two details worth knowing. The batch decides *when* to sync, never what: the
planner walks the trees as usual, because syncing only the paths that fired
events would miss whatever inotify dropped. And lazysync's own partial and
backup files are ignored, or a transfer would trigger the next sync for ever.

### The audit log

`sL` shows what previous runs actually did — newest first, failures first
within a run, since "why is that file not there" is the question a log gets
opened for. Refusals are recorded too: a safety rail stopping a sync is worth
knowing about, and otherwise the only evidence is that nothing happened.

It lives at `$XDG_STATE_HOME/lazysync/audit.jsonl`, one JSON object per line,
so `jq` works on it and a crash mid-write can only ever truncate the last
record. Runs nobody reviewed — `--yes` from cron, or watch mode's `auto` — are
marked `unattended`.

The `compare:` in the header is the rule that was *actually* used, not the one
configured. Against a destination that cannot be made to keep a modification
time — FTP, whose `MFMT` is not part of the base protocol — `size+mtime` drops
to `size`, and rows read `same size, times not kept`. The alternative is worse
than it sounds: every uploaded file carries the time it was uploaded, so
comparing timestamps would call the entire tree changed on every single run.
Size alone is weaker than it sounds for such a destination — a same-size edit
slips through — and `compare.method = "checksum"` is not yet an answer:
nothing hashes anything, so it degrades too. The header always names the rule
that was actually applied.

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

`ftps://` is *explicit* FTPS — a connection on the ordinary FTP port upgraded
with `AUTH TLS` before authenticating — and its certificate is verified
against the webpki root store. There is no accept-invalid-certificate option,
deliberately: a session that trusted anything would be worse than a plaintext
one, because the scheme on screen would say it was safe. Plain `ftp://` is
never silently upgraded either, since an `ftp://` that was sometimes encrypted
and sometimes not would be indistinguishable on screen.

## Configuration

`$XDG_CONFIG_HOME/lazysync/config.toml`. Every option is documented with its
default in [`config.example.toml`](config.example.toml); an empty file behaves
the same as that file.

Bookmarks live separately in `bookmarks.toml`, because lazysync writes to them
from the UI and round-tripping a hand-commented file through a serializer
would eat the comments.

### Keybindings

Keymaps are per UI mode — `d` is delete in the browser and a literal `d` in a
text field. Within a mode there are four tables, applied in this order:

| Table | Means | Resolves collisions by |
| --- | --- | --- |
| `base` | start from nothing but this | — (it *is* everything) |
| `set` | these actions get exactly these keys | **action** |
| `extra` | these keys get these actions | **key** |
| `unbind` | these keys do nothing | — (it only removes) |

`set` and `extra` are the pair worth understanding, because the difference is
not obvious and picking the wrong one is quiet rather than loud:

```toml
[keys.browser]
# "move down is on e now" -- j stops moving down.
set = [["MoveDown", "e"]]

# "e also moves down" -- j still does too. Use this to ADD a key.
extra = [["MoveDown", "e"]]
```

`set` replaces the action's other keys, so list every one you want to keep —
**except the arrows, which are never retired**. Choosing between `j` and `e`
is a statement about a layout, and the arrows are not part of any layout:
they are what works for someone who has not learned yours. If you do want rid
of one, say so — `unbind = ["down"]` runs after `set`.

Giving one action several keys means several entries. An array is a *chord*,
not a list of alternatives:

```toml
set = [
    ["EnterDir", "o"],
    ["EnterDir", "enter"],      # o, or Enter
    ["SyncPush", ["s", "p"]],   # s THEN p
]
```

`unbind` is for keys something else needs to see, and is applied last so
nothing can reinstate them:

```toml
[keys.browser]
unbind = ["esc"]    # let zellij or tmux close its popup
```

It is prefix-based: `unbind = ["s"]` also removes the `sp`/`sl`/`sm` chords,
because otherwise `s` would still be swallowed while lazysync waited to see
whether a chord was coming.

A fuller example:

```toml
[keys.browser]
set = [
    ["ToggleSyncBrowse", "S"],
    ["SyncPush",         ["s", "p"]],
]
extra = [
    ["Refresh", { key = "r", mod = ["Ctrl"] }],
]
unbind = ["esc"]
```

**Esc is never the only way out.** Every mode binds `ctrl+c` to cancel as
well, because a terminal multiplexer can take Esc before lazysync sees it —
binding it to close a floating pane is a normal thing to do in zellij or
tmux. If you do that, `ctrl+c` is what still works; lazysync quits on `q` or
`ZZ`, so nothing here treats `ctrl+c` as "exit". A `base` table replaces that
second key along with everything else, so keep an escape hatch in yours.

**Reach for `base` only if you really mean "start from nothing".** It replaces
the mode's defaults *wholesale*, so any action you leave out ends up with no
key at all — and the symptom is a key that silently does nothing, which reads
like a bug rather than a gap in your config. A `base` for `[keys.browser]`
that rebinds the motions and forgets the `s`-prefixed sync chords leaves
lazysync unable to start a sync. If what you want is "my own keys for a
handful of actions", that is `set`, which leaves everything else alone.

Whichever you use, check it — `lazysync keys` lists what your config resolves
to, and then names any action that ended up with no key:

```console
$ lazysync keys --mode browser
[keys.browser]
  MoveDown             e
  ...
  # unbound in this mode, but bound by default:
  # CycleSort          Next sort key
```

Actions go missing from more than just `base`: a `set` or `extra` that takes a
key another action was using leaves that action stranded, and so does an
`unbind` aimed at a chord prefix. The report catches all of them.

`lazysync keys` prints the resolved keymap, the in-app help overlay (`?`) is
generated from it, and so is every on-screen key hint — so nothing in the UI
can claim a binding you do not have. If the footer does not offer an action,
nothing is bound to it.

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
