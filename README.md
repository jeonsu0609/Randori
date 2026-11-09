<p align="center">
  <img src="doc/randori/icon.png" width="112" alt="Randori icon" />
  <br>
  <strong>Randori</strong>
  <br>
  A GPU-accelerated desktop client for <a href="https://github.com/jj-vcs/jj">Jujutsu</a>
</p>

![Randori in the dark theme](doc/randori/first-screen.png)

Randori is a native desktop client for the Jujutsu version control system. It
pairs the jj engine from [GG](https://github.com/gulbanana/gg) with a UI built
on [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) and [GPUI Kit](https://github.com/longbridge/gpui-kit),
so the whole application is Rust, rendered on the GPU, with no webview.

The name comes from jujutsu practice: *randori* is free-form sparring, where
you move, split and recombine freely. That is how jj treats history.

> **Status:** early development. Randori reads and edits jj workspaces —
> tabs, graph log, diffs, describe, new, edit, abandon, bookmarks, rebase by
> drag and drop, file and hunk squashes, push, fetch, undo and redo, a command
> palette, appearance settings and the GitHub pull requests of the workspace.
> Conflicts open in your merge tool; an in-app conflict editor is not built yet.

## The window

| Region | What it shows |
| --- | --- |
| Tabs | One tab per jj workspace, like Fork or GitKraken. `+` (or `⌘O`) opens more; the open tabs come back on the next launch |
| Toolbar | Workspace path, **Mine / All branches**, a **revset** field (Enter to query), and New, Undo, Redo, Fetch, Push, Reload |
| Rail | Revisions, Open, the command palette (`⌘K`) and Settings (appearance: light, dark or follow the system; remembered) |
| Side panel | **Pull requests** of the workspace's GitHub remote (below), then the local and remote bookmarks and tags in the loaded revisions, filtered by kind. Click a bookmark to jump to its revision, right-click to push, rename, track or delete, drag onto a revision to move it there |
| Revisions | A branch / tag column, the commit graph and fixed columns: change id, description, author, relative time. Each branch keeps one colour from its tip down, and its rows are tinted in it. A revision is its author's avatar (GitHub picture or initials); rounded squares are immutable revisions, small dots are merges and the hollow ring is the working copy (`@`). Right-click for revision commands; drag a revision onto another to rebase it there |
| Changes | Diff of the selected file, unified or split, with line numbers and word-level highlights |
| Inspector | The selected revision: change id, an editable description (immutable history stays read-only), author, date, commit, parents and the changed files. Right-click a file to squash it into the parent, discard it, or open it in your diff or merge tool; drag it onto a revision to move it there |
| Status bar | Worker state, the running jj operation and its progress, the last operation, the working copy and the number of loaded revisions |

**Mine** is jj's own log (`revsets.log`): your changes that are not in trunk
yet, and trunk. Changes on other people's branches are immutable to jj and stay
out of it. **All branches** shows every visible commit (`all()`), every branch
and tag like GitKraken's graph. The last scope you pick is remembered, and a
revset you type selects neither.

Each tab runs its own jj worker thread (jj-lib is not thread-safe), so a slow
fetch in one workspace never blocks another. Edits made outside Randori, in an
editor or with the jj CLI, show up when the window comes back to the front:
Randori asks jj to snapshot the working copy and reloads, keeping the rows you
had loaded and your scroll position.

### Editing

Every command is one of GG's jj mutations, so the rules are jj's own:
immutable revisions are refused with a warning, `Undo` steps back through the
operation log, and git credentials are asked for when a remote needs them.

| Command | How |
| --- | --- |
| New change on the selection | `N`, toolbar `+`, or the context menu |
| Edit (make it the working copy) | `E` |
| Describe | `D` to focus the inspector editor, `⌘↩` to save |
| Create bookmark | `B`, then type a name and press Enter |
| Abandon | `⌫` / `Delete`, confirmed in a dialog |
| Duplicate, back out | Context menu |
| Rebase | Drag a revision onto its new parent, or press `R`, choose the parent with `↑` `↓` and press Enter |
| Move a bookmark | Drag it from the bookmarks panel onto a revision, or right-click → **Move to…** |
| Squash a file into the parent | Right-click it in the inspector |
| Move a file to any revision | Drag it from the inspector onto the revision, or right-click → **Move to revision…** |
| Discard a file's changes | Right-click it in the inspector, confirmed in a dialog |
| Squash one hunk into the parent | Select lines in the diff, then **Squash hunk into parent** |
| Resolve a conflict | Right-click the conflicted file → **Resolve in merge tool** (`ui.merge-editor`) |
| Anything else | `⌘K` opens the command palette |
| Push | `⌘⇧P` pushes the selection's bookmarks (an untracked bookmark goes to `origin`, or the only remote) |
| Fetch | `⌘⇧F` fetches every remote |
| Undo / redo | `⌘Z` / `⌘⇧Z` in the revision list |

### Pull requests

When a workspace has a GitHub remote (`origin`, or else the first remote on
github.com or a GitHub Enterprise host), the side panel lists its open pull
requests, newest activity first: author, number and title, head → base branch,
the review decision and the checks of the head commit. Click one to select its
head revision (switching to **All branches** if it is not loaded), or open it on
GitHub. The list refreshes every minute, when the window comes back to the front
(at most every 30 seconds) and from **Refresh pull requests** in the palette.
The same query brings the avatars of recent authors on the default branch and
on the pull requests' base branches.

Randori uses the token you already have: `GH_TOKEN` or `GITHUB_TOKEN`
(`GH_ENTERPRISE_TOKEN` for Enterprise hosts), or else the GitHub CLI's
(`gh auth token`). It is kept in memory, never written or logged, and sent only
to that host's GraphQL API over HTTPS. Without one the section shows how to
sign in (`gh auth login`); nothing else needs it.

![Creating a bookmark](doc/randori/bookmark-dialog.png)

![The command palette](doc/randori/command-palette.png)

### Keyboard

| Keys | Where | Action |
| --- | --- | --- |
| `↑` `↓` / `k` `j` | Revisions | Select the previous / next revision; the next page loads at the end |
| `Home` / `End` | Revisions | First / last loaded revision |
| `R` | Revisions | Pick a new parent for the selection (Enter applies, Esc cancels; other commands wait until you do) |
| `/` | Revisions | Edit the revset |
| `⌘L` (`Ctrl+L`) | Workspace | Edit the revset |
| `Esc` | Workspace | Back to the revision list |
| `⌘R` (`Ctrl+R`) | Workspace | Reload |
| `⌘B` (`Ctrl+B`) | Workspace | Show or hide the bookmarks panel |
| `⌘O` (`Ctrl+O`) | Window | Open a workspace in a new tab |
| `⌘W` (`Ctrl+W`) | Window | Close the tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Window | Next / previous tab |
| `⌘K` (`Ctrl+K`) | Window | Command palette |

On Linux and Windows, `⌘` is `Ctrl`.

The layout follows the professional workstation style of
[Longbridge Pro](https://longbridge.com/desktop), the application GPUI Kit was
extracted from: hairline-separated panels instead of cards, a narrow navigation
rail, dense rows with stable columns, and an inspector beside the work area.
Colors come only from theme tokens, so the light theme works unchanged.

<details>
<summary>Light theme</summary>

![Randori in the light theme](doc/randori/first-screen-light.png)

</details>

## Running

Randori needs Rust 1.99.1, pinned in `rust-toolchain.toml` (jj-lib 0.46 and
GPUI both require a recent compiler; rustup installs it on first build), and
git 2.42 or newer for jj's git operations. If an older git comes first on
`PATH`, point jj at a newer one; Randori reads the same setting:

```sh
jj config set --user git.executable-path /opt/homebrew/bin/git
```

### From source

```sh
cargo run -- /path/to/workspace /path/to/another   # each opens in a tab
```

This is the development build: unoptimized, and a bare executable. Don't
double-click `target/*/randori` in Finder; macOS runs a bare executable inside a
Terminal window. Build the app bundle below to launch Randori like any other
Mac app.

### As a Mac app

```sh
script/bundle-macos.sh           # target/release/Randori.app
script/bundle-macos.sh --dmg     # also target/release/Randori.dmg (drag to Applications)
open target/release/Randori.app
```

The script needs only cargo and the macOS command-line tools (`codesign`,
`hdiutil`): it builds the release binary, writes `Info.plist`, adds the icon
and signs the bundle ad hoc, which is enough to run it on your own Mac. Giving
the app to other people needs a Developer ID signature and notarization
instead; Gatekeeper refuses ad-hoc signed apps downloaded from the internet.

The release profile (`Cargo.toml`) uses thin LTO, a single codegen unit and
strips symbols, and the app embeds only the icons it draws (`assets.rs`)
instead of the whole Lucide set:

| Build | Size |
| --- | --- |
| Release, default profile, every icon | 47.7 MB executable |
| Release, used icons only | 46.7 MB executable |
| Release, LTO + `codegen-units = 1` + strip | 29.4 MB executable |
| The same, with the HTTP client for GitHub | 32.3 MB executable |
| `Randori.dmg` (compressed disk image) | 15.8 MB |

A release build takes about three minutes on an M-series Mac; `cargo run`
stays fast because the debug profile is unchanged.

Opening a folder that is not a jj repository offers **Create a jj repository
here** (colocated with git when the folder already is one) and **Try again**
(`⌘R`).

| Option | Effect |
| --- | --- |
| `--revset <query>` | Start with this revset instead of the workspace default |
| `--light` | Use the light theme |
| `--check` | Open the workspace through the worker, print a summary and exit (no window) |
| `--select-row <n>` | Select the n-th log row once loaded |
| `--screenshot <png>` | Render the first screen offscreen and exit; needs `--features screenshot` |
| `--keys <k1,k2,…>` | With `--screenshot`: press these keys first through the real key bindings, e.g. `down,j,end`; `action:randori::SquashFile` runs an action the way a menu item does |

The screenshots above were produced with:

```sh
cargo run -p randori --features screenshot -- \
  --revset 'ancestors(@ | spike/dense-rows, 40)' --select-row 1 \
  --screenshot doc/randori/first-screen.png /path/to/workspace
```

The sample workspaces are a clone of this repository with two extra changes and
a small notes repository, made under a placeholder identity
(`JJ_USER="Randori Demo"`). Screenshot runs never touch your saved tabs.

## How it is built

```text
crates/randori   app_view       tabs, rail, palette, settings, saved session
                 repo_view      one workspace: state, loading, selection, actions, keys
                   model        rows, refs, files and diffs built from worker data
                   log_list     graph log rows and header
                   panels       toolbar, bookmarks panel, status bar, layout
                   inspector    revision details, changed files, diff
                   ops          jj mutations (new, describe, abandon, push, rebase, …)
                   pick         choosing a target revision from the keyboard
                   pulls        the pull requests section
                   dnd, menus   drag payloads, context menus
                 graph          graph lanes, branch colours and avatars per virtual-list row
                 github         GitHub remotes, token lookup, pull request and avatar query
                 worker         one jj worker thread per tab
                 assets         embedded icons
      │  SessionEvent requests over a channel per tab, push events back through EventSink
      ▼
src/worker       GG's jj engine (jj-lib), one worker thread per tab
src/messages     request and response types shared with the UI
```

- jj-lib is not thread-safe, so each tab owns a worker thread that owns the
  repository. The UI sends requests and awaits replies on a background executor.
- GG's worker lays out the graph (lane and row of every node and edge). Randori
  draws it per row of a virtual list, clipped to the row, so long histories
  stay fast. Graph geometry and row height are in `rem`, so they scale with the
  interface.
- The root package, `randori-core`, is GG's jj engine (`src/worker`,
  `src/messages`, `src/config`, `src/askpass.rs`). GG's Tauri app, web mode and
  Svelte frontend are not part of Randori; GG's documentation is kept in
  [`doc/gg/`](doc/gg/).

## Based on GG

Randori starts from [GG — Gui for JJ](https://github.com/gulbanana/gg) by
Thomas Castiglione and contributors, and keeps its full history. The jj
engine in `src/` is theirs; Randori replaced GG's Tauri and Svelte frontend
with GPUI and removed it from this repository. GG's own documentation lives in
[`doc/gg/`](doc/gg/), including its [README](doc/gg/README.md),
[design notes](doc/gg/DESIGN.md), [changelog](doc/gg/CHANGELOG.md) and
[contributing guide](doc/gg/CONTRIBUTING.md). `AGENTS.md` is the guide for
working on Randori itself.

## License

Apache-2.0, the same as GG. See [LICENSE](LICENSE).
