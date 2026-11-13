# Project Overview

Randori is a desktop client for Jujutsu (jj). The root package (`randori-core`,
library `gg_lib`) is the jj engine taken from GG: a worker thread per workspace
that owns jj-lib state and answers queries and mutations. The UI is
`crates/randori`, built on GPUI and gpui-kit. There is no webview, Tauri or
TypeScript.

## Development Commands

```bash
cargo run -- /path/to/workspace     # launch Randori (default member: crates/randori)
cargo run -- --check /path          # open a workspace through the worker, print a summary, no window
cargo check --workspace             # engine + UI
cargo test --workspace              # engine tests need git >= 2.42 on PATH
cargo clippy --workspace --all-targets
cargo clippy -p randori --features screenshot
cargo fmt --all
script/bundle-macos.sh [--dmg]      # release Randori.app (and .dmg) under target/release
```

**Before committing:** run `cargo fmt --all`, both clippy commands and `cargo test --workspace`.

## Branches and CI

`main` is what was merged, green on CI; nothing is committed to it directly.

1. Each piece of work gets a bookmark off `main`, named for its kind and topic:
   `feat/…`, `fix/…`, `refactor/…`, `docs/…`, `test/…` or `chore/…` (the
   commit types). `jj new main`, commit with `jj commit`, then
   `jj bookmark set <name> -r @-` (or `move`) after each commit.
2. Push the bookmark and open a pull request to `main`, as a draft while the
   work goes on. A bookmark pushed with `git push` comes back from
   `jj git fetch` untracked: `jj bookmark track <name>@origin`, or step 3's
   fetch leaves it.
3. Merge with **Rebase and merge**, so the commits stay as they were split.
   GitHub deletes the branch on merge (**Automatically delete head
   branches**), and the next `jj git fetch` deletes the bookmark here too.
   Kept, it would hold on to the commits from before the rebase, which GitHub
   rewrote, and the log would show them beside `main`'s.

CI (`.github/workflows/ci.yml`) runs on a pull request that is ready for
review (opened, pushed to, or marked ready) and on `main`, and by hand
(**Run workflow**, `workflow_dispatch`). It does not run on a draft, and not
for a change to Markdown or `doc/` alone. A new push to a pull request
cancels the run for the push before it. `main` takes changes through pull
requests only; CI is not a required check, since a documentation change runs
none.

The gpui-kit fork (`jeonsu0609/gpui-kit`, see
[`doc/randori/gpui-kit-patch.md`](doc/randori/gpui-kit-patch.md)) keeps
`main` as upstream's, one branch per upstream pull request (off upstream
`main`, rebased onto it and pushed with `--force-with-lease` to update, deleted
once merged), and `randori/v<version>`: that release with the patches Randori
needs, cherry-picked, whose commit `[patch.crates-io]` pins.

## Releases

A release is made when it is asked for, not on a schedule:

1. A pull request (`chore/release-<version>`) sets the version in
   `crates/randori/Cargo.toml`, which the app reports; merge it.
2. Tag that commit of `main` `randori-v<version>` and push the tag. The prefix
   keeps Randori's tags apart from GG's (`v0.46.0` and before).
3. `.github/workflows/release.yml` builds three packages and drafts a GitHub
   release with them and notes from the merged pull requests:
   `Randori-<version>-macos-arm64.dmg` (`script/bundle-macos.sh --dmg`),
   `-windows-x64.zip` (`randori.exe`) and `-linux-x64.tar.gz` (`randori`). It
   refuses a tag that is not the crate's version.
4. Check the packages, then publish the draft.

Each package is built on its runner's platform, untested there beyond CI's
build. The macOS image is for Apple silicon and signed ad hoc, so macOS opens
it on another Mac only through Open in the context menu; giving it out widely
needs a Developer ID and notarization. The Windows executable is unsigned
(SmartScreen warns), and a release build opens no console window. The Linux
one needs GPUI's libraries (Vulkan, X11 or Wayland). Run by hand
(`workflow_dispatch`), the workflow builds any branch's packages as artifacts
and drafts nothing.

## Architecture

Each tab has a dedicated worker thread owning a `Session` (jj-lib is not
thread-safe). The session progresses through states: `WorkerSession` →
`WorkspaceSession` → `QuerySession`.

### Key Boundaries

- **`src/worker/session.rs`**: `SessionEvent`, the requests a view sends to its worker.
- **`src/worker/mod.rs`**: worker state machine, the `Mutation` trait and the `EventSink` trait for push events.
- **`src/messages/`**: request/response types (`Serialize`/`Deserialize`).
- **`crates/randori/src/worker.rs`**: spawns a worker per tab, `Worker::call` sends a `SessionEvent` and blocks on the reply (call it from a background task, never the UI thread). Log queries take `log_lock` so paging is never interleaved.
- **`crates/randori/src/app_view.rs`**: window, tabs (a dot when the tab's `RepoView::working_copy_changed`), rail, command palette, saved session (`<config dir>/randori/session.json`). The appearance lives here: the Settings window asks for it (`request_theme`) and it is applied on the next frame, which has the window. After a theme change (which puts the theme's code font back) `theme_changed` records that font (`ThemeCodeFont`) and `apply_code_font` puts the chosen one on the theme, only when the lookup found it installed, since GPUI cannot lay out text in a family it does not find.
- **`crates/randori/src/settings_window/`**: the Settings window (`mod.rs`; the pages in `pages.rs`, the fields that write when the user is done in `fields.rs`) (`OpenSettings`, ⌘,), one at a time (`OpenWindow`), built on gpui-kit's `setting` components; every page reads its values when drawn and re-renders when `Settings` changes. jj keys are written with `settings::set_jj_string`, which bumps `Settings::jj_generation`; a `RepoView` with an older generation opens its workspace again (`reopen_pending`, once its own commands are done), since a worker keeps the config it loaded; a write of the value already set does nothing. Text that goes to the jj config is written on Enter or when the field loses focus (`committed_input`), not per keystroke; until then it waits in `PendingEdits`, which closing the window writes, since a closing window may not tell its fields they lost focus. `CodeFonts` is the installed code fonts, looked up once off the UI thread with the terminal's symbol fonts (`find_installed_fonts`, one font listing for both); the installed terminal applications are looked for once. Settings changes are saved to the session half a second after the last one (`save_session_soon`); quitting saves at once. Screenshot builds open it with `RANDORI_SETTINGS_PAGE=<n>` and capture it instead of the main window, and take display settings from `RANDORI_DISPLAY` (JSON); the capture draws a frame first, since macOS asks for none while the screen is locked.
- **`crates/randori/src/issue_links.rs`**: Settings' issue link rules, compiled once per change (a thread-local cache): `find` for the inspector's links, `linkify_markdown` for pull request text (not inside code or existing links).
- **`crates/randori/src/repo_view.rs`**: one workspace: `RepoView` state, opening, loading and paging, selection, worker events, actions and key bindings. The rest of the view is split into submodules that each `use super::*`:
  - `model.rs`: row, ref, file and diff data built from worker responses (pure, unit tested)
  - `log_list.rs`: graph log rows and header
  - `panels.rs`: toolbar, bookmarks panel, status bar and the body layout
  - `inspector.rs`: revision details, changed files and the diff
  - `ops.rs`: every jj mutation the UI runs, and how results are applied
  - `pick.rs`: pick mode, choosing a target revision from the keyboard
  - `pulls.rs`: the pull requests section, its background refresh, and the avatars it brings back. The list query (`github::query`) asks for the default branch's recent authors on the first refresh only. GitHub's avatars are used as they come, identicons included; `request_missing_avatars` (after each log load and page) asks about authors still without one by a commit each (`github::pull::fetch_commit_avatars`, aliased `object(oid:)` queries, 50 per request, sent together), once per email unless its batch failed (a failed batch leaves the others' pictures in place; a timeout asks every email again). Rows open their pull request by number, not by position. GraphQL answers go through `github::answered`: an error fails the answer only when the node asked for is missing, since GitHub nulls a field the token may not read and answers the rest. Every GitHub request goes through `github_access::within`, which gives up after 30 seconds on GPUI's timer: the HTTP client's own read timeout needs a Tokio runtime where the body is read, and the bodies are read on GPUI's executors (it panics there)
  - `pull_view.rs`: one pull request in the place of the revisions, the diff and the inspector (`PullView`; `h_resizable` takes the id `workspace-pull` so the dropped inspector does not resize the other panels). The detail and the diff load side by side: the list query carries `baseRefOid`, so the diff does not wait for the detail. The view holds both loads (`_detail_task`, `_diff_task`), so closing it, opening another pull request or reloading cancels them, GitHub's requests included; a diff thread already computing runs to its end and its answer is dropped. Overview: `github::pull::fetch_detail` (description as `TextView::markdown`, ids carry the pull request's number, timeline, reviewers, labels, checks). Files changed: `gg_lib::worker::commit_range_diff_apart` (merge base to head on a load of its own, as blame does, so a large pull request does not hold up the tab's worker; through `unified_patch` into `DiffFile`s) when both commits are in the repository, otherwise GitHub's diff (`fetch_diff`, `application/vnd.github.diff`). Every GitHub request runs with its parsing in `background_spawn`, the detail and the diff share one token lookup (`github_access::github_token`, a shared future), and each load carries a number so a reload's answer is not overtaken by an earlier one (`detail_load` / `diff_load`). The Overview is its own entity (`PullOverview`), embedded cached; review threads are `Diff::annotations` on their lines, read from the shared `Rc<PullDetail>`. A list refresh with a newer `updatedAt` reloads the open pull request (`refresh_open_pull`), and its diff when its head moved or its base branch changed (new commits on the base leave the merge base, and the diff, as they were). The detail takes the conversation's latest 100 items (`timelineItems(last: 100)`), the first 100 review threads and 50 checks, and their `totalCount`s, so the view says what is only on GitHub. `show_diff_mode` switches its diff too
  - The middle of a tab is one `Center`: `Revisions`, `File(FileFocus)` or `Pull(PullView)`; opening one replaces the other
  - `switcher.rs`: the toolbar's workspace picker (open tabs from the `app_view::OpenTabs` global, then `gg.ui.recent-workspaces`) and bookmark picker (`query_headers("bookmarks() | remote_bookmarks()")`; choosing one runs `CreateRevision`, i.e. `jj new <bookmark>`), and the conflict count. Both pickers are `Command` dialogs like the palette
  - `picked_up.rs`: after New, Edit or a bookmark switch (`check_picked_up_after`), one snapshot looks for files the new working copy picked up because the revision it moved to does not ignore them (jj #5596), and offers `IgnoreNewFiles`: `.git/info/exclude` lines (the top-most new folder of each path) and an untrack that resets the working copy instead of checking out, so the files stay on disk
  - `focus.rs`: the full view of one file (`FileFocus`), in place of the revisions and the diff: Diff with per-hunk `CopyHunk` (discard) and `MoveHunk` (squash) under every hunk via `Diff::annotations`, File (`DiffFile::unchanged`), Blame, and History in the side panel's place. After a reload it follows the shown change to its rewritten commit (`refresh_focus`)
  - `split.rs`: the divider of split diffs (`split_diff` wraps a `Diff`). Split diffs soft-wrap so each side fits its column; the ratio is `DisplaySettings::split_ratio` (clamped by `clamp_ratio`), held in `split_drag` while dragged. The keyboard path is `WidenOriginalSide` / `WidenModifiedSide` / `EvenSplitSides` in the palette and the View menu. Both the changes panel and the full file view go through it, and `show_diff_mode` switches both diff states
  - `terminal_panel.rs`: the terminal at the bottom of a tab (`ToggleTerminal`, ⌃`), started in `WorkspaceInfo::root` the first time it opens. `TerminalEvent::RepositoryChanged { snapshot }` runs `refresh_from_disk(snapshot)` (or sets `refresh_after_ops: Option<bool>` while Randori's own mutation runs); `snapshot` is true when only the colocated Git repository moved, so the load imports it, and every `reload` tells the terminal the heads it now shows (`note_repository_seen`)
  - `github_access.rs`: the token GitHub requests carry (`github_token`) and their time limit (`within`), for the section and the pull request view
  - `dnd.rs`, `menus.rs`: drag payloads, context menus
- **`crates/randori/src/graph.rs`**: draws the worker's graph layout per virtual-list row. `branch_colors` gives each branch one palette colour (a node inherits the child above it in its lane; a lane change takes the right-hand endpoint's colour); nodes are author avatars (`graph::avatar`).
- **`crates/randori/src/settings.rs`**: the `Settings` GPUI global every view reads and observes (`cx.observe_global::<Settings>`), so changes apply to open tabs at once. Display settings are saved in the session; `gg.queries.auto-snapshot` and `gg.ui.mark-unpushed-bookmarks` are written to the jj user config with `gg_lib::config::write_user_setting`, and strings (`user.*`, `ui.merge-editor`, `ui.diff-formatter`) with `write_user_string`. Workers keep the config they were loaded with, so the UI passes the current choice along (`Worker::snapshot(force)`) instead of relying on the worker's copy.
- **`crates/randori/src/github.rs`**: GitHub remotes (`GitHubRepo::pick`), the token lookup (`env_token`: `GH_TOKEN` / `GITHUB_TOKEN` for github.com, enterprise tokens only for the host `GH_HOST` names; then `gh auth token` with the token variables removed from its environment, since `gh` hands `GH_ENTERPRISE_TOKEN` to any non-github.com host; never stored or logged), the one GraphQL query and its parser; `graphql` and `send` are the shared request helpers. `github/pull.rs`: a pull request in detail (`PullDetail`), its diff and commit avatars, each parser unit tested. HTTP goes through GPUI's client, set in `main.rs` (`reqwest_client`), which also loads avatar images.
- **`crates/randori/src/terminal/`**: `TerminalView`, the user's shell in a PTY. `alacritty_terminal` runs the PTY thread and keeps the screen; Randori draws its grid (`layout_grid`: backgrounds, one shaped line per run of narrow cells forced onto the cell width with neighbouring cells styled alike in one `TextRun`, wide characters alone, Powerline arrows as paths; the cell-to-run split is `lay_out_cells`, which needs no window and is unit tested, and is the only part done under the screen's lock: shaping runs after the lock is let go, so the PTY thread is not kept waiting; symbol fonts are picked from the one font listing at startup, `remember_symbol_fonts`; the terminal font of Settings is resolved when it or the font listing changes, not per frame), turns keys into xterm bytes (`keys.rs`, unit tested), sends text through `EntityInputHandler` so input methods compose, and handles selection, the wheel, the clipboard and resizing (in the canvas prepaint, as Zed does). After output it compares the repository with what it saw (`schedule_repository_check`, `RepoWatch`): `op_heads/heads` for jj, and in a colocated repository `.git/HEAD`, `packed-refs` and the times of the directories under `.git/refs` for `git commit`/`git checkout`. The `Listener` sends one `Wakeup` until the view takes it (`wakeup_pending`), since the PTY thread sends one per 64 KB read. Keys the app or gpui-kit's root binds that the shell needs (`Esc`, `Tab` and `Shift+Tab`, which the root uses to move focus, and on Linux/Windows the `Ctrl` shortcuts with `Ctrl+C`, the root's copy) are bound to `SendKey` in `TERMINAL_CONTEXT`, which wins over the tab's, the app's and the root's bindings; `terminal::bind_keys` runs after the others. The panel embeds the view cached (`AnyView::cached`), so the tab's redraws skip the grid and output redraws only the terminal; the view notifies itself on focus changes for the cursor.
- **`crates/randori/src/assets.rs`**: the icons the app embeds. Only listed icons ship; a new `IconName::Foo` must be added to `icon_assets!` or it draws nothing (the `every_used_icon_is_embedded` test catches this, and needs new submodules added to its file list).

## File Queries

`src/worker/queries.rs` answers three per-file queries for the full view:
`query_file` (contents at a revision, conflicts with git markers),
`query_annotation` (jj-lib's `FileAnnotator` over `all()`, like `jj file
annotate`, grouped into blocks) and `query_file_history` (`::<rev> &
files(root-file:"…")`, newest first). Files over 8 MiB are refused. The UI
runs blame through `gg_lib::worker::annotate_apart`, which loads the
workspace on the calling thread without snapshotting, so the tab's worker is
not held up by a long annotate. The app calls it on a thread of its own
(`worker::on_own_thread`), not on GPUI's background executor, whose few
threads the tab's other work shares. `query_headers` evaluates `immutable_heads()`
once for all the headers it returns.

## Adding New Mutations

1. Define the struct in `src/messages/mutations.rs`.
2. Implement the `Mutation` trait in the matching file under `src/worker/mutations/` (`revision.rs`, `change.rs`, `ref.rs`; others in `mod.rs`):
   - Start the transaction first
   - Use `from`/`to` variable names (not `source`/`target`)
   - Check immutability immediately after resolving commits
   - Use the `precondition!` macro for user-facing validation errors
3. Add a method in `repo_view/ops.rs` that builds it and calls `run_mutation`; refuse immutable selections up front with `refuse_if_immutable`.
4. Expose it as an action in `repo_view::actions`, bind it in a menu, the palette (`app_view::open_palette`) and, if it is frequent, a key.

## Testing Mutations

The test repository (`res/test-repo.zip`) contains pre-defined commits. Key test commits (helpers in the `revs` module of `src/worker/tests/mod.rs`):
- `working_copy()` - empty, child of main
- `main_bookmark()` - renamed c.txt
- `conflict_bookmark()` - has conflict in b.txt
- `resolve_conflict()` - resolved the conflict

Tests point `XDG_CONFIG_HOME` at a per-process temporary directory
(`isolate_user_config`, called by `mkrepo`): jj 0.46 migrates the test
repository's legacy repo config to `<config>/jj/repos/<id>`, which otherwise
left one directory in the developer's own jj config per test. Anything that
changes `XDG_CONFIG_HOME` must put the previous value back (`SetVarGuard`).

Test patterns:
- `mkid("change_id", "commit_id")` to reference commits
- `mutation.execute_unboxed(&mut ws).await?` to run mutations
- `assert_matches!(result, MutationResult::Updated { .. })` for success
- Immutable commits fail with `PreconditionError`

## Verifying the UI

Build with `--features screenshot` to render offscreen and drive the real key
bindings (no screen-recording permission needed):

```bash
cargo run -p randori --features screenshot -- \
  --revset 'all()' --select-row 1 --keys 'down,cmd-p,n,e,w,enter' \
  --screenshot /tmp/shot.png /path/to/workspace
```

`--keys` takes keystrokes, `action:randori::Name` (runs an action the way a
menu item does) and pointer steps in window points: `click:X;Y`,
`dblclick:X;Y` and `drag:X1;Y1;X2;Y2` (`;` because `,` separates the steps).
Screenshots are at 2x, so a point is half a screenshot pixel. Screenshot runs never read or write the saved session. Use
throwaway workspaces under a placeholder identity (`JJ_USER`, `JJ_EMAIL`).

## JJ Version Coupling

jj-lib and jj-cli are pinned to specific versions (see `Cargo.toml`). The app
embeds jj functionality - users don't need the jj CLI installed. Rust is
pinned in `rust-toolchain.toml`.

## Key Patterns

### Tree Merge Semantics

jj-lib's `MergedTree::merge()` performs 3-way merges:
- **Apply changes**: `target.merge(&base, &modified)` - applies diff base→modified to target
- **Remove changes**: `modified.merge(&base, &added)` - removes diff base→added from modified
- **Backout**: `working.merge(&to_revert, &parent_of_to_revert)` - reverses changes

### Direct Manipulation

Drag payloads are in `repo_view/dnd.rs` (`DraggedRevision`, `DraggedRef`,
`DraggedFile`); what a drop does is in `repo_view/ops.rs` (`rebase_onto`,
`move_ref_to`, `move_file_to`). Every drag has a keyboard path too: pick mode
(`repo_view/pick.rs`, started by `R`, the context menus or the palette). While
a pick is open, `run_mutation` and the dialogs refuse other commands
(`blocked_by_pick`), so nothing changes the repository underneath an open pick.

### Pull Request Diffs

`queries::query_commit_range_diff(base, head)` (full commit ids) returns the
changes from the best common ancestor of the two to the head, or `None` when
either commit is not in the repository, so the caller can ask the host.
Unrelated histories diff from the base itself. The app calls it through
`worker::commit_range_diff_apart`, on a load of the repository of its own
and a thread of its own (`on_own_thread`).

### Loading the Latest Operation

The worker keeps the operation it last loaded; queries (`QueryLog`, …) read
that one. `ExecuteSnapshot` and `LoadHead` load the latest first, so what the
jj CLI or the terminal recorded shows up: `RepoView::refresh_from_disk` uses
`LoadHead` when snapshots are off or not wanted and `ExecuteSnapshot`
otherwise, then reloads. Reload (`⌘R`) and the toolbar's reload snapshot; the
terminal's reload does not, since the jj command it follows snapshotted.

`ExecuteSnapshot` answers with a status when the working copy changed, the
head moved, or importing Git (colocated) recorded an operation: a `git
commit` in a terminal changes nothing in the working copy, yet the client
needs the new operation.

### Log Order

`SessionEvent::QueryLog { by_date }` picks the row order. `false` is jj's
`TopoGroupedGraph` (one branch at a time, as `jj log`); `true` streams the
revset in index order, newest first across branches. The index is
topological (a commit is added after its parents), so the graph layout works
unchanged. The order lives in `QueryState`, so the next pages keep it.

### Log Scope

"Mine" is the workspace's `query_choices["default"]` (jj's `revsets.log`) and
"All branches" is `repo_view::ALL_BRANCHES_QUERY` (`all()`). The selected tab is
derived from the current revset (`RepoView::log_scope`), so a typed revset
selects neither. The last pick is saved in the session (`all_branches`) and
applies to tabs opened later, unless `--revset` is given.

### GG Hunk Ranges

`ChangeRange` lines are already 1-based (GG's copy of jj's hunker starts at
`1..1`). Only an empty side changes in a unified header (`-0,0`).

### UI Rules

Follow the gpui-kit Design Guides: colours from `cx.theme()` tokens only,
sizes in `rem` (graph geometry scales by the window's rem), every action
reachable from the keyboard, visible hover/focus/selection/disabled states.

## Configuration

Settings are loaded via `jj config`. Engine settings live in `src/config/gg.toml`:
- `gg.queries.log-page-size` - paging size (default 1000)
- `gg.queries.large-repo-heuristic` - disables features when the repo is too large (default 100k)

## Comment Style

Lowercase, minimal. Don't describe *what* code does. Explain *why* when non-obvious, or warn about gotchas:
```rust
// GOOD: explains why
// must clone before the builder chain - settings moves into .manage()

// BAD: describes what
// Read recent workspaces for initial menu build
```

## Import Style

Prefer `use` for structs/types, but qualify function calls using the module they're in:
```rust
use anyhow::{Context, Error, Result};
use jj_cli::git_util;

// GOOD: struct via use, function qualified by containing module
let err = Error::new(e.error);
let options = git_util::load_git_import_options(&ui, &settings);

// BAD: qualifying struct, qualifying function by crate
let err = anyhow::Error::new(e.error);
let options = jj_cli::git_util::load_git_import_options(&ui, &settings);
```

## Key Files Reference

- `doc/gg/DESIGN.md` - GG's core metaphors, bookmark state machine
- `src/worker/mutations/` - all mutation implementations, by category
- `src/config/gg.toml` - default engine configuration with inline docs
- `crates/randori/src/repo_view/ops.rs` - every mutation the UI performs
