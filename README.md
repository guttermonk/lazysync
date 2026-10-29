# lazysync

A dual-pane terminal file manager and sync tool. [termscp][termscp]'s feature
set, vim keys by default with every binding remappable, and a confirmation
window that shows you both sides of every overwrite — size and timestamp,
source and destination — before anything moves.

**Status: feature-complete against the plan.** Push, pull, mirror and two-way
bisync over the local filesystem, SFTP/SCP, FTP/FTPS, or any `rclone` remote
(which is how S3, WebDAV, B2 and Google Drive are reached). Transfers resume,
report progress, and can be paused. Watch a tree and sync what changes. Every
run is recorded.

Not there yet: inline diffs, checksum re-verification, and parallel transfers
— files move one at a time. `s3://`, `smb://` and `webdav://` are refused as
*native* protocols; use an rclone remote. See [`docs/PLAN.md`](docs/PLAN.md).

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
  `..` re-couples the panes.
- **Bookmarks don't store the local path.** lazysync bookmarks store
  everything except the password — including the local working directory and a
  sync profile reference.
- **The connection form is on the main screen.** lazysync opens to the
  bookmark list; connection details live in the add/edit screen.
- **No advanced sync.** lazysync has a native sync engine, and can hand the
  transfer to `rsync` or `rclone` when you ask it to.

## Install

### Nix

```nix
{
  inputs.lazysync.url = "github:guttermonk/lazysync";

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
      profiles.deploy = {
        mode = "push";
        exclude = [ "/target/" "*.log" ];
      };
    };
  };
}
```

`settings` becomes `config.toml`, which lazysync only ever reads.

There is also a `bookmarks` option, **off by default**. lazysync *writes*
`bookmarks.toml` from the UI, and home-manager points that file at the
read-only Nix store — so declaring your connections means the editor has
nowhere to save. lazysync says so rather than appearing to work. Never put a
password in it; the Nix store is world-readable.

#### Overriding the package

Use `.override`, not by copying files over the result — the store is read-only
and anything copied in is clobbered on the next update:

```nix
inputs.lazysync.packages.${system}.default.override {
  syncBackends = [ pkgs.rsync pkgs.rclone ];
}
```

### Cargo

```sh
cargo install --locked --git https://github.com/guttermonk/lazysync
```

`rsync` is an optional executor. `rclone` is optional too, but `rclone://`
remotes need it — which is how S3, WebDAV, B2 and Google Drive are reached.

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
lazysync sync deploy ~/proj /mnt/backup --yes       # headless, for cron
```

### Keys

These are the **defaults**. Every one is remappable, and `lazysync keys`
prints what yours resolve to — which is also what every on-screen hint is
generated from, so the footers follow your config rather than this page.

| Where | Keys |
| --- | --- |
| Connection list | `enter` open · `n` add · `e` edit · `dd` delete · `/` filter |
| Browser | `h` `j` `k` `l` navigate · `tab` other pane · `/` search · `space` mark · `o` sort · `c` connections · `?` help · `q` quit |
| Toggles | `S` synchronized browsing · `u` union view · `.` hidden files |
| Sync | `sp` push · `sl` pull · `sm` mirror · `sb` bisync · `ss` profile · `sw` watch · `sL` audit log |
| Files | `e` edit · `a` new file · `A` new directory · `r` rename · `dd` delete · `p` permissions |
| On a file | `enter` default sync · `shift+enter` or `gx` open externally |

The connection list always ends with a built-in **Local transfer** entry,
which opens two local panes — syncing `~/photos` to a mounted backup drive
needs no server. It cannot be deleted and is never written to your
`bookmarks.toml`.

Each pane is labelled with the backend it is actually showing — `local`,
`sftp`, `ftp`, `rclone` — so a pane never claims to be somewhere it isn't.

## Syncing

### Which sync key does what

| Key | Direction | What it deletes |
| --- | --- | --- |
| `sp` | local → remote | nothing |
| `sl` | remote → local | nothing |
| `sm` | local → remote | whatever the remote has and the local tree does not |
| `sb` | both ways | a file deleted on either side, on the other — once a snapshot exists |
| `ss` | whatever the connection's profile says | — |

`sm` and `sb` are the pair worth care. A **mirror** has a source of truth:
run it the day you forgot which pane you were in and it removes the files you
meant to fetch. A **two-way sync** has none — each side's changes travel to
the other, and a stored snapshot is what distinguishes "created over there"
from "deleted over here".

Both open the same plan, and both route deletions to `.lazysync-backup/`
unless you say otherwise.

`ss` and `sw` need a `[profiles.<name>]` named in the connection's `Sync
profile` field. Without one they say so and are not offered in the key bar.
Every other sync key works against whatever two directories are open.

Planning walks both trees, which over SFTP takes as long as the network does.
The status line shows a spinner, a running count, and your cancel key; a
stopped scan opens no window, because half a tree is not a plan you could
review. The browser is frozen while it runs — making the walk yield is the
remaining piece.

### Marks scope the sync

`space` marks a row. With anything marked in the pane files are being sent
*from*, a sync plans only those names — including a mirror's deletions, so
marking one file and pressing `sm` will not plan the removal of everything
else. The window says `3 marked items only`, so a narrow plan can never be
mistaken for a complete one.

### Enter on a file

`Enter` on a directory enters it. On a file it runs the connection's
**default sync** on that file — or on the marked files, if any are marked.

Set it in the connection editor's `Enter syncs` row (`off`, `push`, `pull`,
`mirror-push`, `mirror-pull`, `bisync`), or in `bookmarks.toml`:

```toml
default_sync = "push"
```

**It is `off` until you set it.** Enter is the key people press without having
decided to, and a default that transferred files would make the most
reflexive keystroke in the program the one that writes to a server. With no
setting of its own, a connection falls back to the mode of its sync profile.

A `bisync` default is **refused for a selection** rather than run unnarrowed:
a two-way sync compares both whole trees against a snapshot, so it cannot be
scoped to one file, and the alternative is pressing Enter on one row and
syncing both trees. Use `sb` when you mean that.

### Opening a file

`shift+enter` hands the selected file to your desktop's handler — `xdg-open`,
`open` on macOS, `start` on Windows.

**`gx` does the same and always works.** Most terminals send Shift+Enter as a
plain Enter; only those speaking the kitty keyboard protocol distinguish them.
A key that opens a file on some terminals and *syncs* one on the rest is worse
than no key, so both are bound.

Local files only — opening a remote one means leaving a copy in `/tmp` that
nothing owns, so it says so instead. The handler is launched detached with no
terminal, so this is for GUI applications; a terminal-based handler fails
visibly rather than drawing over the UI.

### The confirmation window

`sp` transfers nothing. It builds a plan and shows it:

```
 Sync  ~/proj  →  box:/srv/www      compare: size+mtime  tolerance: 2s  deletions move to .lazysync-backup/
┌──────────────────────────────────────────────────────────────────────────────────────────┐
│    ▾ Create (1)                                                                          │
│ +  src/main.rs                11 B  2026-10-24 14:14        —                   +11 B    │
│    ▾ Overwrite (2)                                                                       │
│ ↓  config.toml                 5 B  2026-09-02 16:30      5 B  2026-10-23 19:02  DESTINATION IS NEWER │
│ ↑  index.html                 11 B  2026-10-24 13:02      5 B  2026-10-21 14:14  +6 B    │
│    ▾ Delete (1)                                                                          │
│ ✗  obsolete.txt                  —                        8 B  2026-10-24 18:22  not on the source   │
│    ▸ Skipped, identical (1)                                                              │
└──────────────────────────────────────────────────────────────────────────────────────────┘
 3 ops · 27 B out · 1 unchanged · 1 would overwrite NEWER data
```

Both sides' size and timestamp on every row, so "overwrite" is never an
abstract word. A **newer destination is red** — the one outcome of a sync that
destroys work. `space` excludes a row and the totals recompute immediately.
The plan *is* the dry run: `y` applies exactly these rows and nothing else has
a path to the executor.

Deletions move to `.lazysync-backup/` by default rather than vanishing, and a
mirror that would empty the destination is refused outright.

### Two-way sync

`sb` syncs both ways in one pass; the header shows `⇄` and rows point in
whichever direction the file moved. Two things to know before using it:

- **The first bisync of a pair never deletes.** Two-way sync needs to know
  what the trees looked like *last time*, because "here on the left, missing
  on the right" has two readings that call for opposite actions. lazysync
  keeps a snapshot per pair under `$XDG_STATE_HOME/lazysync/bisync/` — not
  inside the synced tree, where it would conflict with itself every run. With
  no snapshot there is no evidence of a deletion, so both sides simply gain
  what they are missing.
- **When both sides changed, lazysync will not guess.** The row becomes a
  conflict and the whole plan refuses to run until you settle it: `L` keeps
  local, `R` remote, `B` both, `X` leaves it for next time. Picking the newer
  file automatically discards an edit silently, and "newer" is a claim about
  two clocks that may disagree by more than the edits do.

`B` keeps the local version at the original name on *both* sides and renames
the displaced copy with `safety.conflict_suffix`. Leaving each side its own
version under the original name would mean the pair never converges.

Bisync is interactive-only: `lazysync sync` refuses a bisync profile rather
than resolving conflicts on its own.

### Transfers

`y` hands the plan to a queue and shows per-file and overall progress with
throughput and ETA; it does not block. `p` pauses between chunks, `esc`
cancels. A cancelled file leaves its partial behind when `transfer.resume` is
on. Files stream in 256 KiB chunks, so a 4 GB file costs 256 KiB of memory.

`transfer.parallel` and `transfer.bandwidth_limit` are in the schema and **not
yet honoured** — transfers are sequential and uncapped.

Two safety rails apply to every run: deletions move to `.lazysync-backup/`
rather than vanishing, and `safety.max_delete_fraction` /
`safety.max_delete_files` abort a sync that would delete more than you
plausibly meant — which is what a sync root with a typo in it looks like.

### Filters and profiles

A `[profiles.<name>]` bundles a direction with exclude rules, and a bookmark's
`sync_profile` points at one:

```toml
[profiles.deploy]
mode = "push"
exclude = ["/target/", "*.log", ".git/"]
respect_gitignore = true
```

Rules are gitignore syntax — `name`, `name/`, `/name`, `*`, `**`, `?`, and `!`
to re-include. Character classes aren't supported, and a pattern using one is
an error rather than a literal: a filter that silently does nothing is how
`target/` ends up on a web server.

### Delegating to rsync or rclone

```toml
[profiles.deploy]
executor = "rsync"      # "auto" | "native" | "rsync" | "rclone"
```

**`auto` means the native engine.** That is not what the name suggests, so:
a delegated transfer gives up three things —

- the destination is no longer re-checked immediately before it is written, so
  a file that changed *after* you reviewed the plan is overwritten;
- progress and cancellation become one aggregate line rather than per file;
- a conflict resolved "keep both" loses its displaced copy.

Those may be a fair trade for rsync's delta transfer on a large file. They are
not a fair trade to make *silently*, under a setting promising "the best
available" — so you get them by naming the tool. (Keep-both is refused even
then: that one is data loss rather than a trade.)

A delegated tool is a **transport, not a planner**. It gets the exact list of
paths the plan contains, with the flags that stop it deciding anything for
itself. Deletions always stay with the native engine, so the backup directory
and delete limits keep working.

### Watching a tree

```toml
[profiles.notes]
mode = "push"
watch = "batch"         # "off" | "batch" | "auto"
debounce_ms = 2000
```

`sw` starts and stops it, on whatever local directory the left pane is
showing. It needs a profile with `watch` set, because `auto` syncs without
confirming and that is not something a keystroke should be able to turn on by
itself.

Changes are collected until the tree is quiet for `debounce_ms`, then `batch`
opens one plan and `auto` applies without asking. The batch decides *when* to
sync, never what — the planner walks the trees as usual, because syncing only
the paths that fired events would miss whatever inotify dropped.

### The audit log

`sL` shows what previous runs actually did — newest first, failures first
within a run. Refusals are recorded too: a safety rail stopping a sync is
worth knowing about, and otherwise the only evidence is that nothing happened.

It lives at `$XDG_STATE_HOME/lazysync/audit.jsonl`, one JSON object per line,
so `jq` works on it. Runs nobody reviewed — `--yes` from cron, or watch mode's
`auto` — are marked `unattended`.

The `compare:` in a plan header is the rule *actually* used, not the one
configured. Against a destination that cannot keep a modification time — FTP,
whose `MFMT` is not part of the base protocol — `size+mtime` drops to `size`
and rows read `same size, times not kept`. Comparing timestamps there would
call the whole tree changed on every run; size alone lets a same-size edit
slip through. `compare.method = "checksum"` is not yet an answer, since
nothing hashes anything.

## Browsing

### Synchronized browsing

`S` couples the panes. Entering `src/api` on one side takes the other to its
own `src/api`, and scrolling one scrolls the other — row N beside row N, which
is what makes two directories side by side a comparison.

It follows by *ordinal position*, not by name; a shorter directory stops at its
own last entry. If you want equal names lined up, that is union view (`u`),
which merges both listings with `L`/`R`/`LR` markers and can show that a name
exists on one side only — the thing that makes a sparse local checkout
navigable against a full remote.

Only the leader pane propagates, and the wheel never leads.

### File operations

`e` opens a file in your editor, `a` creates an empty file, `A` a directory,
`r` renames, `dd` deletes, `p` changes permissions. They act on the **focused pane**, local or remote alike —
the same `Vfs` the sync engine uses.

Four rules worth knowing:

- **A name, not a path.** `a` then `../x` is refused rather than creating
  something one directory up. Nest by making the directory and entering it.
- **An existing name is refused** for both create and rename. POSIX `rename`
  replaces silently, and a file manager that loses a file because you typed a
  name already in use is not one you trust twice.
- **Delete asks first, and marks win over the cursor** — mark three things and
  `dd` means those three. Deleting a directory is recursive and the dialog
  says so, because it is the same keystroke as deleting one file.
- **Delete is a real delete.** Unlike a *sync's* deletions it does not move to
  `.lazysync-backup/`. The backup exists because a plan can remove files you
  never looked at individually; this one is the file under your cursor, named
  in a dialog that says `cannot be undone`.

`p` takes three or four octal digits and opens pre-filled with the current
mode, so the usual edit is one character. It is offered only where the backend
has permission bits — not on FTP or an rclone remote, which say so rather than
failing at the server.

### Editing a file

`e` opens the selected file in your editor — `$VISUAL`, then `$EDITOR`, or
`[editor] command` to override. A **remote** file is fetched first, edited
locally, and written back.

```toml
[editor]
command = []            # empty = $VISUAL, then $EDITOR
max_bytes = 10485760    # refuse anything larger; 0 disables
text_only = true        # a binary goes to the desktop handler instead
```

**A file that is not text is handed to the desktop**, not to your editor —
so `e` on a JPEG opens your image viewer rather than a screenful of binary
you might save back mangled. "Not text" means a NUL byte in the first 8000
bytes, which is the rule `git` uses; not an extension list, which would be
wrong about a `.log` full of protobuf and about a shell script with no
extension at all. Set `text_only = false` if your editor is one you *want*
binaries in.

That routing is local-only. A **remote** binary says so and stops, because
sending it to the desktop means leaving the fetched copy behind for a viewer
that outlives lazysync's cleanup. Pull it first, then `gx`.

- **A local file is edited in place.** Copying it to scratch and back would
  turn an edit into a replace, and inodes, hard links and anything watching
  the file would all see something that did not happen.
- **Nothing is uploaded unless the editor actually wrote.** Re-sending an
  identical file would give it a new mtime on the server and make the next
  sync think it had changed. Opening and quitting costs one download.
- **The server is re-checked immediately before the upload.** If somebody
  else wrote the file while you had it open, yours is not sent and it says
  so — the same discipline the native executor applies to every transfer.
- **The scratch copy is removed however the edit ends**, including when the
  upload is refused. It lives in a `0700` directory for the duration.
- The size limit is checked **before** the download, since the point of it is
  not spending the transfer.

Unlike `shift+enter`, which hands a file to the desktop and forgets it, this
one owns the terminal until the editor exits — so a terminal editor is the
normal case rather than the broken one.

### Connections

Saved connections live in `bookmarks.toml` and store everything except the
password — including the local working directory and a sync profile reference.
Opening one shows its *name* and, only if needed, a password prompt; host,
port, username and key path appear solely in the add/edit form. A password can
be kept in the OS keyring per connection, opt-in via `Remember password`.

**An idle session is kept alive**, and put back if it drops anyway:

```toml
[connection]
keepalive_secs = 60    # 0 turns it off
auto_reconnect = true
```

A remote pane doing nothing gets one `stat` a minute, which is comfortably
inside the usual 120–300 second server idle timeout and nothing next to a
directory listing. Nothing is sent while both panes are local, so lazysync
still blocks on input and wakes for nothing.

If the session has gone anyway, it is reopened and the pane keeps its
directory, cursor and marks — a reconnect you did not ask for has no business
moving your cursor.

**It is only ever silent when the credential can be had again without you**:
agent auth, key auth, or a password the connection chose to keep in the
keyring. A password typed once and deliberately not stored cannot be
replayed, and a password box appearing from a timer while you were reading
something else is worse than a message — so that case says so and leaves
reopening to you.

Host keys are verified against `~/.ssh/known_hosts` before any credential is
sent, because `remotefs-ssh`'s libssh2 backend does not check them at all. A
new host asks once, with a `SHA256:` fingerprint you can compare against
`ssh-keygen -lf`. A *changed* host key is refused outright rather than offered
as a prompt to click through.

`~/.ssh/config` is read, so a bookmark for a host you have already configured
picks up its `HostName`, `User`, `Port` and `IdentityFile`.

`ftps://` is *explicit* FTPS, verified against the webpki root store. There is
no accept-invalid-certificate option, deliberately: a session that trusted
anything would be worse than a plaintext one, because the scheme on screen
would say it was safe. Plain `ftp://` is never silently upgraded either.

## Configuration

`$XDG_CONFIG_HOME/lazysync/config.toml`. Every option is documented with its
default in [`config.example.toml`](config.example.toml); an empty file behaves
the same as that file, and an unknown key is a startup error rather than
silently ignored.

Bookmarks live separately in `bookmarks.toml`, because lazysync writes to them
from the UI and round-tripping a hand-commented file through a serializer
would eat the comments.

### Theme

```toml
[theme]
preset = "terminal"   # also: "catppuccin-mocha", "gruvbox-dark", "nord"
```

**`terminal` is the default and is not a palette.** Its `fg` and `bg` are
`Color::Reset` and the rest are the sixteen ANSI colours, so lazysync inherits
whatever your terminal already has — and a transparent or themed background
shows through. The named presets are for when you want lazysync to look the
same everywhere regardless.

Any individual colour can be overridden on top of a preset:

```toml
[theme.colors]
border_focused = "#fabd2f"
ghost = "dark-gray"      # one-sided rows in union view, and absent panes
danger = "bright-red"    # overwriting newer data with older
```

Values are hex (`"#89b4fa"` or `"0x89b4fa"`) or an ANSI name: `reset`,
`black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `gray`,
`dark-gray`, and `bright-red` through `bright-white`. An unknown slot or
colour name is an error naming it, not a silent fallback.

| Slot | Used for |
| --- | --- |
| `fg` `bg` | ordinary text and the background |
| `dim` | secondary text, and everything behind an open overlay |
| `border` `border_focused` | pane borders, unfocused and focused |
| `dir` `file` `symlink` `executable` | entries, by kind |
| `ghost` | one-sided union rows, absent panes, excluded plan rows |
| `marked` | marked rows, and the key column in `?` |
| `cursor_fg` `cursor_bg` | the selected row |
| `status_fg` `status_bg` | the status line |
| `accent` | answers to a keypress, the scan spinner, the password window, `?` headings |
| `ok` `warn` `danger` | success, caution, and anything that destroys work |

Two slots do more than their name suggests. `dim` is also what an overlay
dims the screen behind it to; `bg` is what that dimming restores cell
backgrounds to, which is why transparency survives on the `terminal` preset —
set a concrete `bg` and dimming will paint it.

### Mouse

On by default. **One click selects, a double click opens:**

| Where | One click | Double click |
| --- | --- | --- |
| A file pane | focus the pane, select the row | enter the directory |
| The connection list | select | open the connection |
| The connection form | focus the field | next value of a choice |
| The confirmation window | select the line | fold a section, include/exclude a row |
| The yes/no dialog | select the button | answer with it |

A double click never does anything the keyboard cannot — it runs the same
action the binding does, which is why a stray click cannot exclude a row from
a sync or spend a listing walking into a remote directory. The wheel scrolls
whatever is under the pointer and never changes focus.

```toml
mouse = true
```

One cost: while lazysync runs, the terminal hands it the mouse, so dragging to
select text selects nothing. Most terminals keep their own selection on
shift+drag; `mouse = false` gets it back unconditionally.

### Keybindings

Keymaps are per UI mode — `d` is delete in the browser and a literal `d` in a
text field. The modes are `bookmarks`, `browser`, `confirm`, `question`,
`form`, `prompt`, `help`, `queue` and `log`. Within a mode there are four
tables, applied in this order:

| Table | Means | Resolves collisions by |
| --- | --- | --- |
| `base` | start from nothing but this | — (it *is* everything) |
| `set` | these actions get exactly these keys | **action** |
| `extra` | these keys get these actions | **key** |
| `unbind` | these keys do nothing | — (it only removes) |

`set` and `extra` are the pair worth understanding, because picking the wrong
one is quiet rather than loud:

```toml
[keys.browser]
# "move down is on e now" -- j stops moving down.
set = [["MoveDown", "e"]]

# "e also moves down" -- j still does too. Use this to ADD a key.
extra = [["MoveDown", "e"]]
```

`set` replaces the action's other keys, so list every one you want to keep —
**except the arrows, which are never retired.** Choosing between `j` and `e`
is a statement about a layout, and the arrows are not part of any layout: they
are what works for someone who has not learned yours. To drop one, say so:
`unbind = ["down"]` runs after `set`.

Giving one action several keys means several entries. An array is a *chord*,
not a list of alternatives:

```toml
set = [
    ["EnterDir", "o"],
    ["EnterDir", "enter"],      # o, or Enter
    ["SyncPush", ["s", "p"]],   # s THEN p
]
extra = [
    ["Refresh", { key = "r", mod = ["Ctrl"] }],
]
unbind = ["esc"]                # let zellij or tmux close its popup
```

`unbind` is prefix-based: `unbind = ["s"]` also removes the `sp`/`sl`/`sm`
chords, because otherwise `s` would still be swallowed while lazysync waited
to see whether a chord was coming.

**Esc is never the only way out.** Every mode binds `ctrl+c` to cancel as
well, because a multiplexer can take Esc before lazysync sees it. lazysync
quits on `q` or `ZZ`, so nothing treats `ctrl+c` as "exit". Whatever you leave
bound to `Cancel` is also what backs out of a half-typed chord.

**Reach for `base` only if you mean "start from nothing".** It replaces the
mode's defaults *wholesale*, so any action you leave out ends up with no key —
and the symptom is a key that silently does nothing, which reads like a bug
rather than a gap in your config. For "my own keys for a handful of actions",
use `set`.

Whichever you use, check it. `lazysync keys` prints what your config resolves
to and then names any action left with no key:

```console
$ lazysync keys --mode browser
[keys.browser]
  MoveDown             e
  ...
  # unbound in this mode, but bound by default:
  # CycleSort          Next sort key
```

Actions go missing from more than `base`: a `set` or `extra` that takes a key
another action was using strands it, and so does an `unbind` aimed at a chord
prefix. The report catches all of them.

The help overlay (`?`) and every on-screen hint are generated from the same
resolved keymap, so nothing in the UI can claim a binding you do not have. `?`
has three columns — keys, action name, description — and the middle one is the
exact spelling `[keys.*]` accepts, so `SyncMirror` pastes straight into
`set = [["SyncMirror", "sm"]]`.

```
    sm      SyncMirror     One-way: remote ends up identical to local
```

**The yes/no dialog is its own mode.** `[keys.question]` is separate from
`[keys.confirm]` because the two disagree about `n`: a motion in a remapped
browser-style keymap, and the answer "no" in a dialog. It is two buttons —
`←`/`→`, `h`/`l` or Tab move between them, Enter takes the selected one, `y`
and `n` answer directly. **It opens on "no"**, so the keystroke you press
without reading declines.

### Chords

Press `s` and the key bar becomes the list of what it can still become, with
the keys so far echoed bottom-right, the way vim shows a half-typed command.

A chord **waits as long as you do**. `chord_timeout_ms` is not a clock on
typing one — it only applies where a chord has a *shorter* binding to fall
back on, like `d` alongside `dd`, which none of the defaults has. To back out,
press whatever your keymap binds to `Cancel`; it is spent on the chord, so
backing out of `sp` does not also close the screen you are on.

## zide integration

lazysync is built to be opened as a [zide][zide] floating pane the way
`lazygit` is. Starting in a directory matching a bookmark's `local_path` opens
that bookmark directly — exact match first, then the deepest `local_path` that
is a prefix of the working directory, pre-positioning the remote pane at the
corresponding subpath. So popping the popup in `~/proj/src/api` lands both
panes in `src/api`.

```toml
[zide]
auto_open_bookmark = true
match = "ancestor"   # "exact" | "ancestor" | "off"
```

### Hiding the popup without dropping the connection

Quitting closes the connection, which makes "I want my terminal back for a
minute" cost a reconnect. `Hide` asks the host to stop showing the pane and
keeps running behind it, so the connection, the marks, the cursor and a
transfer in flight are all still there when it comes back.

```toml
[zide]
hide_command = ["zellij", "action", "hide-floating-panes"]

[keys.browser]
set = [["Hide", "esc"]]
```

Both halves are needed and neither has a default: lazysync cannot know what is
hosting it, so nothing is guessed — and because the command is empty out of
the box, `Hide` starts with no key rather than a key that does nothing. The
command is a list because splitting a string takes a shell, and it is waited
for, so it has to return promptly as `zellij action` does.

**Bringing the pane back is the host's job** — `ToggleFloatingPanes`, `Alt+f`
by default in zellij. A keybind that *runs* lazysync starts a second one
beside the hidden pane rather than revealing it.

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
- [lazygit][lazygit] — the popup-in-a-pane workflow, and the password prompt

## License

MIT OR Apache-2.0, at your option.

[termscp]: https://github.com/veeso/termscp
[inlyne]: https://github.com/Inlyne-Project/inlyne
[zide]: https://github.com/guttermonk/zide
[rclone]: https://rclone.org
[lazygit]: https://github.com/jesseduffield/lazygit
