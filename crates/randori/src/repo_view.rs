//! Main window: a jj workspace laid out like a professional desktop workstation.
//!
//! ```text
//! ┌ title bar: sidebar toggle · workspace · revset input · refresh ─────────────┐
//! ├ rail ┬ bookmarks ──┬ revisions (graph log) ──────────┬ inspector ──────────┤
//! │ logo │ filter pills│ column header                   │ change id           │
//! │ nav  │ ref rows    │ graph · change · description …  │ description, facts  │
//! │      │             ├ changes (diff of the selection) ┤ changed files       │
//! ├──────┴─────────────┴─────────────────────────────────┴─────────────────────┤
//! └ status bar: worker state · workspace · last operation · @ ──────────────────┘
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use chrono::{DateTime, Local};
use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gg_lib::messages::queries::{ChangeKind, LogPage, LogRow, RevChange, RevsResult};
use gg_lib::messages::{ChangeHunk, RepoConfig, RevHeader, RevId, RevSet, StoreRef, TreePath};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    diff::{Diff, DiffEvent, DiffFile, DiffLineRange, DiffMode, DiffState},
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    list::ListItem,
    menu::{ContextMenuExt as _, PopupMenu},
    resizable::{h_resizable, resizable_panel, v_resizable},
    scroll::{ScrollableElement as _, Scrollbar},
    spinner::Spinner,
    status_bar::StatusBar,
    tab::TabBar,
    tag::Tag,
    v_flex,
};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, UniformListScrollHandle,
    Window, div, prelude::FluentBuilder as _, uniform_list,
};

use crate::graph::{Graph, row_height};

mod dnd;
mod focus;
mod inspector;
mod log_list;
mod menus;
mod model;
mod ops;
mod panels;
mod pick;
mod picked_up;
mod pulls;
mod switcher;

use crate::settings::{self, DiffLayout, Settings};
use crate::worker::{Worker, WorkerEvent};
use dnd::*;
use focus::{FOCUS_CONTEXT, FocusTab};
use menus::*;
use model::*;
use pick::*;

/// Keyboard actions of the main window.
pub mod actions {
    gpui_kit::actions!(
        randori,
        [
            SelectPrevious,
            SelectNext,
            SelectFirst,
            SelectLast,
            FocusRevset,
            FocusLog,
            Reload,
            ToggleSidebar,
            ShowMyWork,
            ShowAllBranches,
            OrderByDate,
            GroupByBranch,
            RefreshPullRequests,
            // the full view of one file
            OpenFileDiff,
            OpenFileView,
            OpenFileBlame,
            OpenFileHistory,
            CloseFileFocus,
            NextChange,
            PreviousChange,
            NextFile,
            PreviousFile,
            // the toolbar's pickers
            SwitchWorkspace,
            SwitchBookmark,
            NextConflict,
            // revisions
            NewChild,
            EditRevision,
            DescribeRevision,
            SaveDescription,
            DuplicateRevision,
            AbandonRevision,
            BackoutRevision,
            CreateBookmark,
            // bookmarks (on the ref the context menu was opened on)
            PushBookmark,
            RenameBookmark,
            DeleteRef,
            ToggleTrackBookmark,
            // pick mode: choose the target revision from the keyboard
            PickRebase,
            PickMoveBookmark,
            PickMoveFile,
            ConfirmPick,
            CancelPick,
            // files and hunks (on the selected file in the inspector)
            SquashFile,
            RestoreFile,
            ResolveInTool,
            DiffInTool,
            SquashHunk,
            // repository
            InitRepository,
            Undo,
            Redo,
            Fetch,
            Push,
        ]
    );
}

/// Revset of the "All branches" scope: every visible commit, which in a git
/// repository is everything reachable from a branch, tag or workspace.
pub const ALL_BRANCHES_QUERY: &str = "all()";

/// How stale the age labels ("3 hours ago") may get before a window
/// activation rebuilds them.
const AGE_REFRESH: std::time::Duration = std::time::Duration::from_secs(60);

/// Key context of the revision list; arrow and vi keys apply only there.
const LOG_CONTEXT: &str = "RandoriLog";
/// Key context of one repository tab.
const REPO_CONTEXT: &str = "RandoriRepo";

/// `cmd-<key>` on macOS, `ctrl-<key>` elsewhere.
macro_rules! platform_key {
    ($key:literal) => {
        if cfg!(target_os = "macos") {
            concat!("cmd-", $key)
        } else {
            concat!("ctrl-", $key)
        }
    };
}

/// Binds the window's keys. Call once at startup.
pub fn bind_keys(cx: &mut App) {
    use actions::*;
    use gpui_kit::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, Some(LOG_CONTEXT)),
        KeyBinding::new("k", SelectPrevious, Some(LOG_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(LOG_CONTEXT)),
        KeyBinding::new("j", SelectNext, Some(LOG_CONTEXT)),
        KeyBinding::new("home", SelectFirst, Some(LOG_CONTEXT)),
        KeyBinding::new("end", SelectLast, Some(LOG_CONTEXT)),
        KeyBinding::new("/", FocusRevset, Some(LOG_CONTEXT)),
        KeyBinding::new("n", NewChild, Some(LOG_CONTEXT)),
        KeyBinding::new("e", EditRevision, Some(LOG_CONTEXT)),
        KeyBinding::new("d", DescribeRevision, Some(LOG_CONTEXT)),
        KeyBinding::new("b", CreateBookmark, Some(LOG_CONTEXT)),
        KeyBinding::new("backspace", AbandonRevision, Some(LOG_CONTEXT)),
        KeyBinding::new("r", PickRebase, Some(LOG_CONTEXT)),
        KeyBinding::new("enter", ConfirmPick, Some(LOG_CONTEXT)),
        KeyBinding::new("escape", CancelPick, Some(LOG_CONTEXT)),
        KeyBinding::new("delete", AbandonRevision, Some(LOG_CONTEXT)),
        KeyBinding::new(platform_key!("l"), FocusRevset, Some(REPO_CONTEXT)),
        KeyBinding::new("escape", FocusLog, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("r"), Reload, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("b"), ToggleSidebar, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("z"), Undo, Some(LOG_CONTEXT)),
        KeyBinding::new(platform_key!("shift-z"), Redo, Some(LOG_CONTEXT)),
        KeyBinding::new(platform_key!("shift-f"), Fetch, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("shift-p"), Push, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("shift-d"), OpenFileDiff, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("shift-b"), SwitchBookmark, Some(REPO_CONTEXT)),
        KeyBinding::new("escape", CloseFileFocus, Some(FOCUS_CONTEXT)),
        KeyBinding::new("alt-down", NextChange, Some(FOCUS_CONTEXT)),
        KeyBinding::new("alt-up", PreviousChange, Some(FOCUS_CONTEXT)),
        KeyBinding::new("]", NextFile, Some(FOCUS_CONTEXT)),
        KeyBinding::new("[", PreviousFile, Some(FOCUS_CONTEXT)),
    ]);
}

enum LoadState {
    Loading,
    Failed(SharedString),
    Ready,
}

struct WorkspaceInfo {
    path: SharedString,
    query: String,
    /// jj's log revset (`revsets.log`), the "Mine" scope.
    default_query: String,
    /// `(name, fetch URL)` of every git remote, for the GitHub section.
    remote_urls: Vec<(String, String)>,
    /// `gg.presets` (or GG's built-in ones) as `(title, revset)`.
    presets: Vec<(SharedString, String)>,
    operation: SharedString,
    /// Git remotes, for fetch.
    remotes: Vec<String>,
    /// Whether the last undo can be redone.
    can_redo: bool,
    /// `ui.diff-formatter` / `ui.merge-editor` are configured.
    has_diff_tool: bool,
    has_merge_tool: bool,
}

pub struct RepoView {
    worker: Worker,
    state: LoadState,
    workspace: Option<WorkspaceInfo>,
    rows: Vec<LogRow>,
    row_data: Rc<Vec<RowData>>,
    graph: Rc<Graph>,
    refs: Vec<RefItem>,
    ref_filter: usize,
    has_more: bool,
    loading_more: bool,
    /// Bumped whenever the rows are replaced, so a late "next page" is dropped.
    log_generation: u64,
    /// Directory and revset the tab was opened with (for Try again / init).
    wd: Option<PathBuf>,
    initial_query: Option<String>,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    detail: Option<Detail>,
    detail_loading: bool,
    selected_file: Option<usize>,
    diff: Entity<DiffState>,
    diff_mode: DiffMode,
    revset: Entity<InputState>,
    sidebar_visible: bool,
    /// Focus for the revision list; arrow keys and j/k move the selection.
    log_focus: FocusHandle,
    /// Focus for the tab itself while there is no list to focus.
    root_focus: FocusHandle,
    _events: Task<()>,
    _subscriptions: Vec<Subscription>,
    detail_task: Option<Task<()>>,
    /// Label of the mutation in flight, shown in the status bar.
    busy: Vec<SharedString>,
    /// A finished operation changed the repository; reload when none are left.
    reload_after_ops: bool,
    /// Latest progress message from a git fetch or push.
    progress: Option<SharedString>,
    /// Change to select after the next reload (from a mutation result).
    pending_select: Option<String>,
    /// Bookmark or tag the last context menu was opened on.
    ref_target: Option<StoreRef>,
    /// Inspector editor for the selected revision's description.
    description_input: Entity<TextareaState>,
    /// Keyboard alternative to drag and drop: the operation waiting for a
    /// target revision, chosen with the selection and confirmed with Enter.
    pick: Option<Pick>,
    /// Lines selected in the diff; picks the hunk for "Squash hunk into parent".
    diff_selection: Option<DiffLineRange>,
    /// Unsaved descriptions of changes the user moved away from, by change id.
    description_drafts: HashMap<String, String>,
    /// Keeps the open one-field dialog's Enter handler alive.
    dialog_subscription: Option<Subscription>,
    /// Change id and text last loaded into the description editor.
    description_for: Option<(String, String)>,
    /// Author avatars by email, shared with the list closure.
    avatars: Rc<HashMap<String, SharedString>>,
    /// Pull requests of the workspace's GitHub remote, if it has one.
    pulls: Option<pulls::Pulls>,
    /// Commit to select after the next reload (a pull request's head).
    pending_commit_select: Option<String>,
    /// The full view of one file, in place of the revisions and the diff.
    focus: Option<focus::FileFocus>,
    /// The revision list's column widths (shared through the settings).
    columns: settings::ColumnWidths,
    /// Mutable revisions with conflicts, in the whole repository.
    conflicts: Vec<RevHeader>,
    conflicts_task: Option<Task<()>>,
    conflicts_loading: bool,
    /// When rows, refs and age labels were last derived from `rows`.
    derived_at: std::time::Instant,
    /// A column being resized: which, where the drag started, its width then.
    column_drag: Option<(log_list::Column, gpui_kit::Pixels, f32)>,
    /// A move of the working copy asked to look for files it picked up.
    /// `Some(working copy before the move)` while a check is due.
    check_picked_up: Option<Option<String>>,
    picked_up_checking: bool,
    /// Files found, waiting to be shown in a notification.
    picked_up: Vec<TreePath>,
    /// The order the loaded rows were queried in (see `settings::log_by_date`).
    log_by_date: bool,
    /// The revision list's width in rems, measured when it paints.
    log_width: Rc<std::cell::Cell<f32>>,
    /// Where the pointer went down on a column handle.
    column_press: Option<gpui_kit::Pixels>,
}

impl RepoView {
    pub fn new(
        worker: Worker,
        mut events: UnboundedReceiver<WorkerEvent>,
        workspace: Option<PathBuf>,
        initial_query: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let _events = cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                if this
                    .update(cx, |view, cx| view.handle_worker_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        let revset =
            cx.new(|cx| InputState::new(window, cx).placeholder("Revset, e.g. ancestors(@, 50)"));
        let _subscriptions = vec![cx.subscribe_in(
            &revset,
            window,
            |view: &mut Self, state, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let query = state.read(cx).value().trim().to_owned();
                    if !query.is_empty() {
                        view.run_query(query, cx);
                    }
                }
            },
        )];

        let diff_mode = diff_mode_of(settings::get(cx).display.diff);
        let diff = cx.new(|cx| DiffState::new([], cx).with_mode(diff_mode));
        let diff_events = cx.subscribe(&diff, |view: &mut Self, _, event: &DiffEvent, cx| {
            match event {
                DiffEvent::SelectionEnded(range) => view.diff_selection = Some(range.clone()),
                DiffEvent::SelectionChanged(None) => view.diff_selection = None,
                _ => return,
            }
            cx.notify();
        });

        let description_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder("Describe this change")
        });
        let mut _subscriptions = _subscriptions;
        _subscriptions
            .push(cx.observe_global::<Settings>(|view: &mut Self, cx| view.apply_settings(cx)));
        _subscriptions.push(diff_events);
        _subscriptions.push(cx.subscribe_in(
            &description_input,
            window,
            |view: &mut Self, _, event: &InputEvent, window, cx| match event {
                // ⌘↩ / Ctrl+↩ saves; a plain Enter is a newline.
                InputEvent::PressEnter {
                    secondary: true, ..
                } => view.save_description(window, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            },
        ));

        let mut view = Self {
            worker,
            state: LoadState::Loading,
            workspace: None,
            rows: Vec::new(),
            row_data: Rc::default(),
            graph: Rc::default(),
            refs: Vec::new(),
            ref_filter: 0,
            has_more: false,
            loading_more: false,
            log_generation: 0,
            wd: None,
            initial_query: None,
            selected: None,
            scroll: UniformListScrollHandle::new(),
            detail: None,
            detail_loading: false,
            selected_file: None,
            diff,
            diff_mode,
            revset,
            sidebar_visible: true,
            log_focus: cx.focus_handle(),
            root_focus: cx.focus_handle(),
            _events,
            _subscriptions,
            detail_task: None,
            busy: Vec::new(),
            reload_after_ops: false,
            progress: None,
            pending_select: None,
            ref_target: None,
            description_input,
            description_for: None,
            description_drafts: HashMap::new(),
            diff_selection: None,
            pick: None,
            dialog_subscription: None,
            avatars: Rc::default(),
            pulls: None,
            pending_commit_select: None,
            focus: None,
            columns: settings::get(cx).display.columns,
            conflicts: Vec::new(),
            conflicts_task: None,
            conflicts_loading: false,
            derived_at: std::time::Instant::now(),
            column_drag: None,
            column_press: None,
            log_width: Rc::default(),
            log_by_date: settings::get(cx).display.log_by_date,
            check_picked_up: None,
            picked_up_checking: false,
            picked_up: Vec::new(),
        };
        view.wd = workspace.clone();
        view.initial_query = initial_query.clone();
        view.open(workspace, initial_query, window, cx);
        view
    }

    /// True once the workspace, its log and the selected revision are all on screen.
    pub fn is_settled(&self) -> bool {
        matches!(self.state, LoadState::Ready | LoadState::Failed(_))
            && !self.detail_loading
            && !self.loading_more
            && self.busy.is_empty()
            && !self.pulls_fetching()
            && !self.conflicts_loading
            && !self.picked_up_pending()
    }

    /// Moves keyboard focus to the revision list (e.g. when its tab is selected).
    /// Without a loaded list (opening, failed) the tab itself takes focus, so
    /// its keys (⌘R to try again, the palette) still work.
    pub fn focus_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.state, LoadState::Ready) {
            self.log_focus.focus(window, cx);
        } else {
            self.root_focus.focus(window, cx);
        }
    }

    /// Where context menu commands start from: the revision list, or the full
    /// view while it replaces the list (a handle that is not on screen
    /// would swallow the command).
    pub(super) fn command_focus(&self) -> FocusHandle {
        match &self.focus {
            Some(focus) => focus.handle.clone(),
            None => self.log_focus.clone(),
        }
    }

    /// Descriptions typed but not saved: drafts left on other changes plus the
    /// editor, when it differs from the selected revision.
    pub fn unsaved_descriptions(&self, cx: &App) -> usize {
        self.description_drafts.len() + usize::from(self.description_dirty(cx))
    }

    /// Index of the selected log row (reported by `--keys`).
    #[cfg(feature = "screenshot")]
    pub fn selected_row(&self) -> Option<usize> {
        self.selected
    }

    /// Selects a log row by index (used by `--select-row`).
    pub fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.rows.len() {
            self.select(ix, cx);
        }
    }

    // ----- loading -------------------------------------------------------------------------

    fn open(
        &mut self,
        wd: Option<PathBuf>,
        initial_query: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state = LoadState::Loading;
        let by_date = settings::get(cx).display.log_by_date;
        self.log_by_date = by_date;
        let worker = self.worker.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    match worker.open_workspace(wd)? {
                        RepoConfig::Workspace {
                            absolute_path,
                            latest_query,
                            query_choices,
                            status,
                            git_remotes,
                            git_remote_urls,
                            has_external_diff_tool,
                            has_external_merge_tool,
                            ..
                        } => {
                            let default_query = query_choices
                                .get("default")
                                .cloned()
                                .unwrap_or_else(|| latest_query.clone());
                            let query = initial_query.unwrap_or(latest_query);
                            let page = worker.query_log(query.clone(), by_date)?;
                            let path = absolute_path.0;
                            Ok((
                                WorkspaceInfo {
                                    path: display_path(&path).into(),
                                    query,
                                    default_query,
                                    remote_urls: git_remote_urls,
                                    presets: preset_list(&query_choices),
                                    operation: status.operation_description.into(),
                                    remotes: git_remotes,
                                    can_redo: status.can_redo,
                                    has_diff_tool: has_external_diff_tool,
                                    has_merge_tool: has_external_merge_tool,
                                },
                                page,
                            ))
                        }
                        RepoConfig::LoadError { message, .. }
                        | RepoConfig::WorkerError { message } => Err(anyhow::anyhow!(message)),
                        RepoConfig::Initial | RepoConfig::TimeoutError => {
                            Err(anyhow::anyhow!("the workspace did not load"))
                        }
                    }
                })
                .await;

            let _ = this.update_in(cx, |view, window, cx| {
                match result {
                    Ok((workspace, page)) => {
                        let query = workspace.query.clone();
                        view.revset
                            .update(cx, |state, cx| state.set_value(query, window, cx));
                        let remote_urls = workspace.remote_urls.clone();
                        view.workspace = Some(workspace);
                        view.replace_rows(page, true, cx);
                        view.start_pulls(&remote_urls, cx);
                        view.state = LoadState::Ready;
                        // only the tab that has the keyboard takes it to its list; a
                        // tab loading in the background must not steal it
                        if view.root_focus.is_focused(window) {
                            view.log_focus.focus(window, cx);
                        }
                        cx.emit(RepoViewEvent::Loaded);
                    }
                    Err(err) => {
                        view.state = LoadState::Failed(format!("{err:#}").into());
                        if view.log_focus.is_focused(window) {
                            view.root_focus.focus(window, cx);
                        }
                        cx.emit(RepoViewEvent::LoadFailed);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the workspace again, e.g. after the worker restarted or the
    /// folder was fixed outside Randori.
    fn retry_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (wd, query) = (self.wd.clone(), self.initial_query.clone());
        self.open(wd, query, window, cx);
        cx.notify();
    }

    /// `jj git init` in the tab's folder (colocated when it already is a git
    /// repository), then opens it.
    fn init_repository(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(wd) = self.wd.clone() else {
            return;
        };
        let colocated = wd.join(".git").exists();
        self.state = LoadState::Loading;
        cx.notify();
        let worker = self.worker.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { worker.init_workspace(wd, colocated) })
                .await;
            let _ = this.update_in(cx, |view, window, cx| match result {
                Ok(_) => view.retry_open(window, cx),
                Err(err) => {
                    view.state = LoadState::Failed(format!("{err:#}").into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Which scope the current revset is: `Some(false)` for jj's own log
    /// ("Mine"), `Some(true)` for every branch, `None` for a typed revset.
    pub(super) fn log_scope(&self) -> Option<bool> {
        let ws = self.workspace.as_ref()?;
        if ws.query == ALL_BRANCHES_QUERY {
            Some(true)
        } else if ws.query == ws.default_query {
            Some(false)
        } else {
            None
        }
    }

    /// Switches between jj's log ("Mine") and every branch, like the branch
    /// list of GitKraken or Fork. The window remembers the choice.
    pub(super) fn set_log_scope(&mut self, all: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = &self.workspace else {
            return;
        };
        let query = if all {
            ALL_BRANCHES_QUERY.to_owned()
        } else {
            ws.default_query.clone()
        };
        self.revset
            .update(cx, |state, cx| state.set_value(query.clone(), window, cx));
        self.run_query(query, cx);
        cx.emit(RepoViewEvent::ScopeChanged { all });
        cx.notify();
    }

    fn run_query(&mut self, query: String, cx: &mut Context<Self>) {
        if let Some(workspace) = &mut self.workspace {
            workspace.query = query;
        }
        self.reload(cx);
    }

    /// Re-runs the current query, e.g. after the repository changed.
    ///
    /// Pages loaded with "Load more" are fetched again so the list does not shrink
    /// under the user, and the scroll position is left alone.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(query) = self.workspace.as_ref().map(|ws| ws.query.clone()) else {
            return;
        };
        let keep = self.rows.len();
        let by_date = settings::get(cx).display.log_by_date;
        self.log_by_date = by_date;
        let worker = self.worker.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { worker.query_log_pages(query, by_date, keep) })
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(page) => {
                        view.state = LoadState::Ready;
                        view.replace_rows(page, false, cx);
                    }
                    Err(err) => view.state = LoadState::Failed(format!("{err:#}").into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Asks the worker to record working-copy changes and reloads if there were any.
    /// Called by the window for the selected tab when the window comes back to
    /// the front or the tab is selected, the moments GG snapshots too.
    pub fn snapshot(&mut self, cx: &mut Context<Self>) {
        if self.workspace.is_none() {
            return;
        }
        self.refresh_pulls(false, cx);
        // The setting as it is now; the worker holds the config it loaded with.
        let force = match settings::get(cx).auto_snapshot {
            Some(false) => return,
            Some(true) => true,
            None => false,
        };
        let worker = self.worker.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { worker.snapshot(force) })
                .await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(Some(status)) => {
                    if let Some(workspace) = &mut view.workspace {
                        workspace.operation = status.operation_description.into();
                    }
                    view.reload(cx);
                }
                // nothing changed, but the age labels move on with the clock
                Ok(None) if view.derived_at.elapsed() >= AGE_REFRESH => {
                    view.rebuild_derived();
                    cx.notify();
                }
                Ok(None) => {}
                Err(err) => log::warn!("snapshot failed: {err:#}"),
            });
        })
        .detach();
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        if !self.has_more || self.loading_more {
            return;
        }
        self.loading_more = true;
        cx.notify();

        let generation = self.log_generation;
        let worker = self.worker.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { worker.query_log_next_page() })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.loading_more = false;
                if view.log_generation != generation {
                    // The list was replaced meanwhile; this page continued the old one.
                    cx.notify();
                    return;
                }
                match result {
                    Ok(page) => {
                        view.has_more = page.has_more;
                        view.rows.extend(page.rows);
                        view.rebuild_derived();
                    }
                    Err(err) => log::warn!("next log page failed: {err:#}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn replace_rows(&mut self, page: LogPage, scroll: bool, cx: &mut Context<Self>) {
        self.log_generation += 1;
        // Keep the selection on the same change across reloads.
        let previous = self
            .selected
            .and_then(|ix| self.rows.get(ix))
            .map(|row| row.revision.id.change.hex.clone());
        self.has_more = page.has_more;
        self.rows = page.rows;
        self.rebuild_derived();

        let pull_head = self.pending_commit_select.take().and_then(|hex| {
            self.rows
                .iter()
                .position(|row| row.revision.id.commit.hex == hex)
        });
        let selected = pull_head
            .or_else(|| {
                self.pending_select.take().or(previous).and_then(|hex| {
                    self.rows
                        .iter()
                        .position(|row| row.revision.id.change.hex == hex)
                })
            })
            .or_else(|| {
                self.rows
                    .iter()
                    .position(|row| row.revision.is_working_copy)
            })
            .or(if self.rows.is_empty() { None } else { Some(0) });
        match selected {
            Some(ix) => self.set_selection(ix, scroll, cx),
            None => {
                // Nothing to inspect: drop any detail still on its way.
                self.selected = None;
                self.detail = None;
                self.detail_task = None;
                self.detail_loading = false;
                self.select_file(None, cx);
            }
        }
        self.refresh_focus(cx);
        self.refresh_conflicts(cx);
        self.run_picked_up_check(cx);
    }

    fn rebuild_derived(&mut self) {
        let now = Local::now();
        self.derived_at = std::time::Instant::now();
        self.graph = Rc::new(Graph::new(&self.rows));
        let mut row_data: Vec<RowData> =
            self.rows.iter().map(|row| RowData::new(row, now)).collect();
        if self.log_by_date {
            // the rows follow commit time there; grouping by author time would
            // jump back and forth wherever history was rewritten
            for (data, row) in row_data.iter_mut().zip(&self.rows) {
                let (bucket, label) = time_bucket(row.revision.committed_at, now);
                data.bucket = bucket;
                data.bucket_label = label.into();
            }
        }
        mark_bucket_starts(&mut row_data);
        self.row_data = Rc::new(row_data);
        self.refs = self
            .rows
            .iter()
            .enumerate()
            .flat_map(|(ix, row)| {
                let data = &self.row_data[ix];
                ref_chips(&row.revision.refs)
                    .into_iter()
                    .map(move |chip| RefItem {
                        chip,
                        row: ix,
                        change: data.change_prefix.clone(),
                        summary: data
                            .summary
                            .clone()
                            .unwrap_or_else(|| "(no description set)".into()),
                        time: data.time.clone(),
                    })
            })
            .collect();
    }

    /// Selects a row because the user asked for it: the list scrolls to it.
    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.set_selection(ix, true, cx);
    }

    fn set_selection(&mut self, ix: usize, scroll: bool, cx: &mut Context<Self>) {
        self.selected = Some(ix);
        if scroll {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        self.load_detail(cx);
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() - 1;
        let ix = match self.selected {
            Some(ix) => ix.saturating_add_signed(delta).min(last),
            None => 0,
        };
        if Some(ix) != self.selected {
            self.select(ix, cx);
        }
        // Reaching the end of the loaded rows pulls in the next page.
        if ix == last {
            self.load_more(cx);
        }
    }

    fn load_detail(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.selected.and_then(|ix| self.rows.get(ix)) else {
            return;
        };
        let set = RevSet {
            from: row.revision.id.clone(),
            to: row.revision.id.clone(),
        };
        self.detail_loading = true;
        let worker = self.worker.clone();
        self.detail_task = Some(cx.spawn(async move |this, cx| {
            // Parsing every file's patch can take a while on a large commit;
            // keep it off the UI thread with the query itself.
            let result = cx
                .background_spawn(async move { worker.query_revisions(set).map(build_detail) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.detail_loading = false;
                match result {
                    Ok(Some(detail)) => {
                        // after a reload, stay on the file that was selected
                        let previous = view
                            .selected_file
                            .and_then(|ix| view.detail.as_ref()?.files.get(ix))
                            .map(|file| file.path.clone());
                        let keep = previous.and_then(|path| {
                            detail.files.iter().position(|file| file.path == path)
                        });
                        let first = (!detail.files.is_empty()).then_some(0);
                        view.detail = Some(detail);
                        view.select_file(keep.or(first), cx);
                    }
                    Ok(None) => {
                        view.detail = None;
                        view.select_file(None, cx);
                    }
                    Err(err) => {
                        // Never leave the previous revision on screen for the new selection.
                        log::warn!("revision query failed: {err:#}");
                        view.detail = None;
                        view.select_file(None, cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    fn select_file(&mut self, ix: Option<usize>, cx: &mut Context<Self>) {
        self.selected_file = ix;
        self.diff_selection = None;
        let file = ix
            .and_then(|ix| self.detail.as_ref()?.files.get(ix))
            .map(|file| file.path.clone());
        let patch = file.and_then(|path| {
            self.detail
                .as_ref()?
                .patches
                .iter()
                .find(|patch| patch.path() == &path)
                .cloned()
        });
        self.diff.update(cx, |state, cx| state.set_files(patch, cx));
        cx.notify();
    }

    /// The diff header's switch: changes this tab and the layout new diffs open in.
    fn set_diff_mode(&mut self, mode: DiffMode, cx: &mut Context<Self>) {
        self.show_diff_mode(mode, cx);
        let layout = if mode == DiffMode::Split {
            DiffLayout::Split
        } else {
            DiffLayout::Unified
        };
        settings::update_display(cx, |display| display.diff = layout);
    }

    fn show_diff_mode(&mut self, mode: DiffMode, cx: &mut Context<Self>) {
        if self.diff_mode == mode {
            return;
        }
        self.diff_mode = mode;
        self.diff.update(cx, |state, cx| state.set_mode(mode, cx));
        cx.notify();
    }

    /// Follows a settings change made in this or another tab.
    fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let current = settings::get(cx);
        let mode = diff_mode_of(current.display.diff);
        let github = current.display.github;
        // a drag in this tab keeps its own widths until it is dropped
        if self.column_drag.is_none() {
            self.columns = current.display.columns;
        }
        let by_date_changed = current.display.log_by_date != self.log_by_date;
        self.show_diff_mode(mode, cx);
        if by_date_changed {
            self.reload(cx);
        }
        if !github {
            // no more requests to GitHub, and no pictures from it
            self.pulls = None;
            self.avatars = Rc::default();
        } else if self.pulls.is_none()
            && let Some(urls) = self.workspace.as_ref().map(|ws| ws.remote_urls.clone())
        {
            self.start_pulls(&urls, cx);
        }
        cx.notify();
    }

    /// Runs `query` from the presets menu.
    pub(super) fn set_query(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.revset
            .update(cx, |state, cx| state.set_value(query.clone(), window, cx));
        self.run_query(query, cx);
        cx.notify();
    }

    fn handle_worker_event(&mut self, event: WorkerEvent, cx: &mut Context<Self>) {
        // GG's message types only implement Serialize, so read the fields we need
        // straight from the JSON payload.
        let payload = &event.payload;
        match event.name.as_str() {
            "gg://repo/config" => {
                if payload.get("type").and_then(|value| value.as_str()) == Some("WorkerError") {
                    let message = payload
                        .get("message")
                        .and_then(|value| value.as_str())
                        .unwrap_or("the jj worker failed");
                    self.state = LoadState::Failed(message.to_owned().into());
                    cx.notify();
                }
            }
            "gg://progress" => {
                self.progress = match payload.get("type").and_then(|value| value.as_str()) {
                    Some("Progress") => payload
                        .get("overall_percent")
                        .and_then(|value| value.as_u64())
                        .map(|percent| format!("{percent}%").into()),
                    Some("Message") => payload
                        .get("text")
                        .and_then(|value| value.as_str())
                        .map(|text| text.trim().to_owned().into()),
                    _ => None,
                };
                cx.notify();
            }
            other => log::debug!("unhandled worker event {other}"),
        }
    }

    // ----- regions -------------------------------------------------------------------------
}

impl Render for RepoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_description(window, cx);
        if !self.picked_up.is_empty() {
            cx.defer_in(window, |view, window, cx| view.notify_picked_up(window, cx));
        }
        // a resize released outside the window never reaches the drop handler
        if self.column_drag.is_some() && !cx.has_active_drag() {
            cx.defer_in(window, |view, _, cx| view.finish_column_resize(cx));
        }

        v_flex()
            .id("repo")
            .key_context(REPO_CONTEXT)
            .track_focus(&self.root_focus)
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|view, _: &actions::FocusRevset, window, cx| {
                view.revset.update(cx, |state, cx| state.focus(window, cx));
            }))
            .on_action(cx.listener(|view, _: &actions::FocusLog, window, cx| {
                view.log_focus.focus(window, cx);
            }))
            .on_action(cx.listener(|view, _: &actions::Reload, window, cx| {
                if matches!(view.state, LoadState::Failed(_)) {
                    view.retry_open(window, cx)
                } else {
                    view.reload(cx)
                }
            }))
            .on_action(
                cx.listener(|view, _: &actions::InitRepository, window, cx| {
                    if matches!(view.state, LoadState::Failed(_)) {
                        view.init_repository(window, cx)
                    }
                }),
            )
            .on_action(cx.listener(|_, _: &actions::OrderByDate, _, cx| {
                settings::update_display(cx, |d| d.log_by_date = true)
            }))
            .on_action(cx.listener(|_, _: &actions::GroupByBranch, _, cx| {
                settings::update_display(cx, |d| d.log_by_date = false)
            }))
            .on_action(cx.listener(|view, _: &actions::ShowMyWork, window, cx| {
                view.set_log_scope(false, window, cx)
            }))
            .on_action(
                cx.listener(|view, _: &actions::ShowAllBranches, window, cx| {
                    view.set_log_scope(true, window, cx)
                }),
            )
            // a column resize ends wherever the pointer is let go in the tab
            .on_drop(
                cx.listener(|view, _: &log_list::ColumnResize, _, cx| {
                    view.finish_column_resize(cx)
                }),
            )
            .on_action(
                cx.listener(|view, _: &actions::SwitchWorkspace, window, cx| {
                    view.open_workspace_picker(window, cx)
                }),
            )
            .on_action(
                cx.listener(|view, _: &actions::SwitchBookmark, window, cx| {
                    view.open_bookmark_picker(window, cx)
                }),
            )
            .on_action(cx.listener(|view, _: &actions::NextConflict, window, cx| {
                view.select_next_conflict(window, cx)
            }))
            .on_action(cx.listener(|view, _: &actions::OpenFileDiff, window, cx| {
                view.open_focus(FocusTab::Diff, false, window, cx)
            }))
            .on_action(cx.listener(|view, _: &actions::OpenFileView, window, cx| {
                view.open_focus(FocusTab::File, false, window, cx)
            }))
            .on_action(cx.listener(|view, _: &actions::OpenFileBlame, window, cx| {
                view.open_focus(FocusTab::Blame, false, window, cx)
            }))
            .on_action(
                cx.listener(|view, _: &actions::OpenFileHistory, window, cx| {
                    view.open_focus(FocusTab::Diff, true, window, cx)
                }),
            )
            .on_action(
                cx.listener(|view, _: &actions::RefreshPullRequests, _, cx| {
                    view.refresh_pulls(true, cx)
                }),
            )
            .on_action(cx.listener(|view, _: &actions::ToggleSidebar, _, cx| {
                view.sidebar_visible = !view.sidebar_visible;
                cx.notify();
            }))
            .on_action(
                cx.listener(|view, _: &actions::NewChild, window, cx| view.new_child(window, cx)),
            )
            .on_action(cx.listener(|view, _: &actions::EditRevision, window, cx| {
                view.edit_selected(window, cx)
            }))
            .on_action(
                cx.listener(|view, _: &actions::DescribeRevision, window, cx| {
                    if view.blocked_by_pick(window, cx)
                        || view.refuse_if_immutable("Describe", window, cx)
                    {
                        return;
                    }
                    view.description_input
                        .update(cx, |state, cx| state.focus(window, cx));
                }),
            )
            .on_action(
                cx.listener(|view, _: &actions::SaveDescription, window, cx| {
                    view.save_description(window, cx)
                }),
            )
            .on_action(
                cx.listener(|view, _: &actions::DuplicateRevision, window, cx| {
                    view.duplicate_selected(window, cx)
                }),
            )
            .on_action(
                cx.listener(|view, _: &actions::AbandonRevision, window, cx| {
                    view.confirm_abandon(window, cx)
                }),
            )
            .on_action(
                cx.listener(|view, _: &actions::BackoutRevision, window, cx| {
                    view.backout_selected(window, cx)
                }),
            )
            .on_action(
                cx.listener(|view, _: &actions::CreateBookmark, window, cx| {
                    view.prompt_create_bookmark(window, cx)
                }),
            )
            .on_action(cx.listener(|view, _: &actions::PushBookmark, window, cx| {
                view.push_ref_target(window, cx)
            }))
            .on_action(
                cx.listener(|view, _: &actions::RenameBookmark, window, cx| {
                    view.prompt_rename_bookmark(window, cx)
                }),
            )
            .on_action(cx.listener(|view, _: &actions::DeleteRef, window, cx| {
                view.delete_ref_target(window, cx)
            }))
            .on_action(
                cx.listener(|view, _: &actions::ToggleTrackBookmark, window, cx| {
                    view.toggle_track_ref_target(window, cx)
                }),
            )
            .on_action(
                cx.listener(|view, _: &actions::PickRebase, window, cx| {
                    view.pick_rebase(window, cx)
                }),
            )
            .on_action(
                cx.listener(|view, _: &actions::PickMoveBookmark, window, cx| {
                    view.pick_move_bookmark(window, cx)
                }),
            )
            .on_action(cx.listener(|view, _: &actions::PickMoveFile, window, cx| {
                view.pick_move_file(window, cx)
            }))
            .on_action(cx.listener(|view, _: &actions::SquashFile, window, cx| {
                view.squash_file_into_parent(window, cx)
            }))
            .on_action(cx.listener(|view, _: &actions::RestoreFile, window, cx| {
                view.confirm_restore_file(window, cx)
            }))
            .on_action(cx.listener(|view, _: &actions::ResolveInTool, window, cx| {
                view.resolve_in_tool(window, cx)
            }))
            .on_action(cx.listener(|view, _: &actions::DiffInTool, window, cx| {
                view.diff_in_tool(window, cx)
            }))
            .on_action(cx.listener(|view, _: &actions::SquashHunk, window, cx| {
                view.squash_hunk_into_parent(window, cx)
            }))
            .on_action(cx.listener(|view, _: &actions::Undo, window, cx| view.undo(window, cx)))
            .on_action(cx.listener(|view, _: &actions::Redo, window, cx| view.redo(window, cx)))
            .on_action(
                cx.listener(|view, _: &actions::Fetch, window, cx| view.fetch_all(window, cx)),
            )
            .on_action(
                cx.listener(|view, _: &actions::Push, window, cx| view.push_selected(window, cx)),
            )
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .child(self.render_body(window, cx)),
            )
            .child(self.render_status_bar(cx))
    }
}

impl RepoView {
    /// Loads the selected revision's description into the editor when the
    /// selection changes. For the same change it follows jj's text (e.g. after
    /// a save or a snapshot) only while the user has no unsaved edits.
    fn sync_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(header) = self.detail.as_ref().map(|detail| &detail.header) else {
            return;
        };
        let change = header.id.change.hex.clone();
        let text = header.description.lines.join("\n").trim_end().to_owned();
        let current = self
            .description_input
            .read(cx)
            .value()
            .trim_end()
            .to_owned();

        if let Some((loaded_change, loaded_text)) = &self.description_for {
            if *loaded_change == change {
                if current == text {
                    // Saved (or changed to the same text elsewhere): track the new text.
                    self.description_for = Some((change, text));
                    return;
                }
                if current != *loaded_text {
                    // Unsaved edits on this change: leave them alone.
                    return;
                }
                // Not editing and jj's text changed (undo, the jj CLI): load it below.
            } else if current != *loaded_text {
                // Leaving a change with unsaved edits: keep them as a draft.
                self.description_drafts
                    .insert(loaded_change.clone(), current.clone());
            }
        }

        // A draft left on this change earlier comes back; otherwise jj's text.
        let value = self
            .description_drafts
            .remove(&change)
            .unwrap_or_else(|| text.clone());
        self.description_input
            .update(cx, |state, cx| state.set_value(value, window, cx));
        self.description_for = Some((change, text));
    }

    fn detail_is_immutable(&self) -> bool {
        self.detail
            .as_ref()
            .is_some_and(|detail| detail.header.is_immutable)
    }

    /// True when the editor differs from the selected revision's description.
    fn description_dirty(&self, cx: &App) -> bool {
        self.detail.as_ref().is_some_and(|detail| {
            let saved = detail.header.description.lines.join("\n");
            self.description_input.read(cx).value().trim_end() != saved.trim_end()
        })
    }
}

fn diff_mode_of(layout: DiffLayout) -> DiffMode {
    match layout {
        DiffLayout::Unified => DiffMode::Unified,
        DiffLayout::Split => DiffMode::Split,
    }
}

/// GG's query choices without the default (that is "Mine"), titled the way GG
/// shows them: `tracked-bookmarks` → "Tracked Bookmarks".
fn preset_list(choices: &HashMap<String, String>) -> Vec<(SharedString, String)> {
    let mut presets: Vec<(SharedString, String)> = choices
        .iter()
        .filter(|(name, _)| name.as_str() != "default")
        .map(|(name, revset)| {
            let title: Vec<String> = name
                .split('-')
                .filter(|word| !word.is_empty())
                .map(|word| {
                    let mut chars = word.chars();
                    chars
                        .next()
                        .map(|first| first.to_uppercase().chain(chars).collect())
                        .unwrap_or_default()
                })
                .collect();
            (title.join(" ").into(), revset.clone())
        })
        .collect();
    presets.sort();
    presets
}

/// What happened when the tab tried to open its workspace; the window uses it
/// to keep broken tabs out of the saved session.
pub enum RepoViewEvent {
    Loaded,
    LoadFailed,
    /// The user picked "Mine" or "All branches".
    ScopeChanged {
        all: bool,
    },
    /// The workspace picker chose another workspace (open or recent).
    OpenWorkspace(PathBuf),
}

impl gpui_kit::EventEmitter<RepoViewEvent> for RepoView {}
