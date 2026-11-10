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

## Architecture

Each tab has a dedicated worker thread owning a `Session` (jj-lib is not
thread-safe). The session progresses through states: `WorkerSession` →
`WorkspaceSession` → `QuerySession`.

### Key Boundaries

- **`src/worker/session.rs`**: `SessionEvent`, the requests a view sends to its worker.
- **`src/worker/mod.rs`**: worker state machine, the `Mutation` trait and the `EventSink` trait for push events.
- **`src/messages/`**: request/response types (`Serialize`/`Deserialize`).
- **`crates/randori/src/worker.rs`**: spawns a worker per tab, `Worker::call` sends a `SessionEvent` and blocks on the reply (call it from a background task, never the UI thread). Log queries take `log_lock` so paging is never interleaved.
- **`crates/randori/src/app_view.rs`**: window, tabs, rail, command palette, settings, saved session (`<config dir>/randori/session.json`).
- **`crates/randori/src/repo_view.rs`**: one workspace: `RepoView` state, opening, loading and paging, selection, worker events, actions and key bindings. The rest of the view is split into submodules that each `use super::*`:
  - `model.rs`: row, ref, file and diff data built from worker responses (pure, unit tested)
  - `log_list.rs`: graph log rows and header
  - `panels.rs`: toolbar, bookmarks panel, status bar and the body layout
  - `inspector.rs`: revision details, changed files and the diff
  - `ops.rs`: every jj mutation the UI runs, and how results are applied
  - `pick.rs`: pick mode, choosing a target revision from the keyboard
  - `pulls.rs`: the pull requests section, its background refresh, and the avatars it brings back
  - `switcher.rs`: the toolbar's workspace picker (open tabs from the `app_view::OpenTabs` global, then `gg.ui.recent-workspaces`) and bookmark picker (`query_headers("bookmarks() | remote_bookmarks()")`; choosing one runs `CreateRevision`, i.e. `jj new <bookmark>`), and the conflict count. Both pickers are `Command` dialogs like the palette
  - `picked_up.rs`: after New, Edit or a bookmark switch (`check_picked_up_after`), one snapshot looks for files the new working copy picked up because the revision it moved to does not ignore them (jj #5596), and offers `IgnoreNewFiles`: `.git/info/exclude` lines (the top-most new folder of each path) and an untrack that resets the working copy instead of checking out, so the files stay on disk
  - `focus.rs`: the full view of one file (`FileFocus`), in place of the revisions and the diff: Diff with per-hunk `CopyHunk` (discard) and `MoveHunk` (squash) under every hunk via `Diff::annotations`, File (`DiffFile::unchanged`), Blame, and History in the side panel's place. After a reload it follows the shown change to its rewritten commit (`refresh_focus`)
  - `split.rs`: the divider of split diffs (`split_diff` wraps a `Diff`). Split diffs soft-wrap so each side fits its column; the ratio is `DisplaySettings::split_ratio` (clamped by `clamp_ratio`), held in `split_drag` while dragged. The keyboard path is `WidenOriginalSide` / `WidenModifiedSide` / `EvenSplitSides` in the palette and the View menu. Both the changes panel and the full file view go through it, and `show_diff_mode` switches both diff states
  - `dnd.rs`, `menus.rs`: drag payloads, context menus
- **`crates/randori/src/graph.rs`**: draws the worker's graph layout per virtual-list row. `branch_colors` gives each branch one palette colour (a node inherits the child above it in its lane; a lane change takes the right-hand endpoint's colour); nodes are author avatars (`graph::avatar`).
- **`crates/randori/src/settings.rs`**: the `Settings` GPUI global every view reads and observes (`cx.observe_global::<Settings>`), so changes apply to open tabs at once. Display settings are saved in the session; `gg.queries.auto-snapshot` and `gg.ui.mark-unpushed-bookmarks` are written to the jj user config with `gg_lib::config::write_user_setting`. Workers keep the config they were loaded with, so the UI passes the current choice along (`Worker::snapshot(force)`) instead of relying on the worker's copy.
- **`crates/randori/src/github.rs`**: GitHub remotes (`GitHubRepo::pick`), the token lookup (`GH_TOKEN` / `GITHUB_TOKEN`, then `gh auth token`; never stored or logged), the one GraphQL query and its parser. HTTP goes through GPUI's client, set in `main.rs` (`reqwest_client`), which also loads avatar images.
- **`crates/randori/src/assets.rs`**: the icons the app embeds. Only listed icons ship; a new `IconName::Foo` must be added to `icon_assets!` or it draws nothing (the `every_used_icon_is_embedded` test catches this, and needs new submodules added to its file list).

## File Queries

`src/worker/queries.rs` answers three per-file queries for the full view:
`query_file` (contents at a revision, conflicts with git markers),
`query_annotation` (jj-lib's `FileAnnotator` over `all()`, like `jj file
annotate`, grouped into blocks) and `query_file_history` (`::<rev> &
files(root-file:"…")`, newest first). Files over 8 MiB are refused. The UI
runs blame through `gg_lib::worker::annotate_apart`, which loads the
workspace on the calling thread without snapshotting, so the tab's worker is
not held up by a long annotate. `query_headers` evaluates `immutable_heads()`
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
  --revset 'all()' --select-row 1 --keys 'down,cmd-k,n,e,w,enter' \
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
