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
    input::{InputEvent, InputState},
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
mod find;
mod focus;
mod github_access;
mod included;
mod inspector;
mod log_list;
mod menus;
mod message;
mod model;
mod ops;
mod pane;
mod panels;
mod pick;
mod picked_up;
mod pull_view;
mod pulls;
mod refs_panel;
mod split;
mod switcher;
pub use switcher::scan_source_folders;
mod terminal_panel;

use crate::settings::{self, DiffLayout, Settings};
use crate::worker::{Worker, WorkerEvent};
use dnd::*;
use focus::{FOCUS_CONTEXT, FocusTab};
use menus::*;
use model::*;
use pick::*;
use refs_panel::PanelSection;

/// What fills the middle of a tab; one at a time, so opening one closes the
/// other.
enum Center {
    Revisions,
    /// The full view of one file, in place of the revisions and the diff.
    File(Box<focus::FileFocus>),
    /// A pull request, in place of the revisions, the diff and the inspector.
    Pull(Box<pull_view::PullView>),
}

impl Center {
    fn file(&self) -> Option<&focus::FileFocus> {
        match self {
            Center::File(focus) => Some(focus),
            _ => None,
        }
    }

    fn file_mut(&mut self) -> Option<&mut focus::FileFocus> {
        match self {
            Center::File(focus) => Some(focus),
            _ => None,
        }
    }

    fn pull(&self) -> Option<&pull_view::PullView> {
        match self {
            Center::Pull(pull) => Some(pull),
            _ => None,
        }
    }

    fn pull_mut(&mut self) -> Option<&mut pull_view::PullView> {
        match self {
            Center::Pull(pull) => Some(pull),
            _ => None,
        }
    }
}

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
            ToggleTerminal,
            // the divider of split diffs
            WidenOriginalSide,
            WidenModifiedSide,
            EvenSplitSides,
            // the full view of one file
            OpenFileDiff,
            OpenFileView,
            OpenFileBlame,
            OpenFileHistory,
            CloseFileFocus,
            ClosePull,
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
            SplitFiles,
            RestoreFile,
            ResolveInTool,
            DiffInTool,
            SquashHunk,
            SplitLines,
            // what the next commit includes, in the working copy's inspector
            IncludeAllChanges,
            ExcludeAllChanges,
            // find a commit in the loaded revisions
            FindCommit,
            CloseFind,
            // the side panel's filter
            FocusPanelFilter,
            // repository
            InitRepository,
            Undo,
            Redo,
            Fetch,
            Push,
        ]
    );
}

/// jj's revset errors on one line: the parser draws the revset with a caret
/// under the error (` --> 1:3`, `1 | x(`, `  ^---`) and says what it
/// expected after `= `; the header has room for the first line and that.
fn one_line_error(error: &str) -> String {
    let mut lines = error.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or_default();
    let head = first
        .split_once(" -->")
        .map_or(first, |(head, _)| head)
        .trim_end();
    match lines.filter_map(|line| line.strip_prefix("= ")).next_back() {
        Some(expected) => format!("{head} {expected}"),
        None => head.to_owned(),
    }
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
        KeyBinding::new(platform_key!("f"), FindCommit, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("alt-f"), FocusPanelFilter, Some(REPO_CONTEXT)),
        KeyBinding::new("escape", CloseFind, Some("RandoriFind")),
        KeyBinding::new("escape", FocusLog, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("r"), Reload, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("b"), ToggleSidebar, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("shift-f"), Fetch, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("shift-p"), Push, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("shift-d"), OpenFileDiff, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("shift-b"), SwitchBookmark, Some(REPO_CONTEXT)),
        KeyBinding::new("ctrl-`", ToggleTerminal, Some(REPO_CONTEXT)),
        KeyBinding::new("escape", CloseFileFocus, Some(FOCUS_CONTEXT)),
        KeyBinding::new("escape", ClosePull, Some(pull_view::PULL_CONTEXT)),
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
    /// The workspace's name in the repository.
    name: SharedString,
    /// The workspace's directory, where the terminal starts.
    root: PathBuf,
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
    /// The parts drawn as views of their own (see `pane`).
    panes: Panes,
    /// The revisions loaded and what is drawn from them.
    log: LogData,
    /// The log loading a page at a time.
    paging: Paging,
    /// The side panel's items and how they show.
    panel: PanelState,
    /// The working copy's next commit (see `NextCommit`).
    next_commit: NextCommit,
    /// Lines selected in the diff, for Squash into parent and Split off
    /// (the full view's diff keeps its own: see `FileFocus`).
    line_selection: Option<DiffLineRange>,
    /// The log row whose bookmarks are spread out: the one whose chip, or
    /// whose spread-out list (it reaches over the rows below), is hovered.
    refs_hover: RefsHover,
    /// The find bar over the revision list, while it is open.
    find: Option<find::FindBar>,
    /// Authors (lowercase emails) the log is filtered by: the other
    /// revisions are dimmed, as GitKraken's author filter does.
    author_filter: Rc<std::collections::BTreeSet<SharedString>>,
    /// Why the revset last typed could not be used; the log still shows the
    /// one before it.
    query_error: Option<SharedString>,
    /// The local bookmark this tab last switched to: of bookmarks sharing a
    /// revision, the one marked current.
    switched_to: Option<String>,
    /// The bookmark standing in for a checked-out branch (see
    /// [`nearest_bookmark`]), found again whenever the rows change.
    current_bookmark: Option<SharedString>,
    /// Directory and revset the tab was opened with (for Try again / init).
    wd: Option<PathBuf>,
    initial_query: Option<String>,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    /// The revision inspected: its detail and the files picked in it.
    inspected: Inspected,
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
    /// The tab's terminal, started the first time it is shown.
    terminal: Option<Entity<crate::terminal::TerminalView>>,
    terminal_open: bool,
    /// Label of the mutation in flight, shown in the status bar.
    busy: Vec<SharedString>,
    /// A finished operation changed the repository; reload when none are left.
    reload_after_ops: bool,
    /// The terminal changed the repository meanwhile: load from disk, not
    /// just the tab's own operation, once none are left. `Some(true)` when
    /// only Git moved, so the load snapshots to import it.
    refresh_after_ops: Option<bool>,
    /// Latest progress message from a git fetch or push.
    progress: Option<SharedString>,
    /// Change to select after the next reload (from a mutation result).
    pending_select: Option<String>,
    /// Bookmark or tag the last context menu was opened on.
    ref_target: Option<StoreRef>,
    /// The description editor of the revision inspected.
    description: DescriptionEdit,
    /// Keyboard alternative to drag and drop: the operation waiting for a
    /// target revision, chosen with the selection and confirmed with Enter.
    pick: Option<Pick>,
    /// Keeps the open one-field dialog's Enter handler alive.
    dialog_subscription: Option<Subscription>,
    /// Author avatars by email, shared with the list closure.
    avatars: Rc<HashMap<String, SharedString>>,
    /// Pull requests of the workspace's GitHub remote, if it has one.
    pulls: Option<pulls::Pulls>,
    /// Commit to select after the next reload (a pull request's head).
    pending_commit_select: Option<String>,
    /// What fills the middle: the revisions, one file, or a pull request.
    center: Center,
    /// The Overview's "Show in the graph" waits for a frame with a window.
    show_head_pending: bool,
    /// The revision list's column widths (shared through the settings).
    columns: settings::ColumnWidths,
    /// The mutable revisions with conflicts, for the switcher's count.
    conflicts: Conflicts,
    /// When rows, refs and age labels were last derived from `rows`.
    derived_at: std::time::Instant,
    /// A column being resized: which, where the drag started, its width then.
    column_drag: Option<(log_list::Column, gpui_kit::Pixels, f32)>,
    /// The split diff's ratio while its divider is dragged.
    split_drag: Option<f32>,
    /// The working copy changes files, from the last status.
    working_copy_changed: bool,
    /// The jj settings generation this tab's workspace was loaded with; a
    /// newer one (the Settings window wrote jj's config) opens it again.
    jj_generation: u64,
    reopen_pending: bool,
    /// When the last automatic fetch started.
    last_auto_fetch: std::time::Instant,
    /// The minute-by-minute look whether a fetch is due, while Settings
    /// fetches automatically.
    auto_fetch: Option<Task<()>>,
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

/// The revision inspected: its detail and the files picked in it.
#[derive(Default)]
struct Inspected {
    /// The selected revision's header, files and patches, once loaded.
    detail: Option<Detail>,
    /// The detail is on its way.
    loading: bool,
    /// The query of the detail; a new selection drops the last one.
    task: Option<Task<()>>,
    /// The file whose diff shows.
    file: Option<usize>,
    /// Files picked in the inspector with ⌘- or ⇧-click, by path, for Split.
    /// They stay picked when the revision reloads, not when another is shown.
    marked: Vec<SharedString>,
}

/// The description editor of the revision inspected.
struct DescriptionEdit {
    /// Inspector editor for the selected revision's description.
    editor: message::MessageEditor,
    /// Change id and text last loaded into the description editor.
    loaded: Option<(String, String)>,
    /// Unsaved descriptions of changes the user moved away from, by change id.
    drafts: HashMap<String, String>,
}

/// The mutable revisions with conflicts, for the switcher's count.
#[derive(Default)]
struct Conflicts {
    /// Mutable revisions with conflicts, in the whole repository.
    revisions: Vec<RevHeader>,
    /// Their query; a newer one drops it.
    task: Option<Task<()>>,
    /// They are being looked for.
    loading: bool,
}

/// The revisions the log has loaded, and what is drawn from them.
#[derive(Default)]
struct LogData {
    /// The rows as the worker gave them, a page after another.
    rows: Vec<LogRow>,
    /// Each row's number by its commit id, added to as pages load.
    row_of_commit: HashMap<String, usize>,
    /// What each row draws, copied so the virtual list owns it.
    row_data: Rc<Vec<RowData>>,
    /// The rows' lanes and lines.
    graph: Rc<Graph>,
}

/// The log loading a page at a time (see `RepoView::load_more`).
#[derive(Default)]
struct Paging {
    /// The revset has rows after those loaded.
    has_more: bool,
    /// A next page is on its way.
    loading: bool,
    /// Why the last next page failed: no page is asked for again until the
    /// log reloads (the status bar's Retry), or scrolling to the end would
    /// ask every frame.
    error: Option<SharedString>,
    /// Bumped whenever the rows are replaced, so a late "next page" is dropped.
    generation: u64,
}

/// The side panel's refs and workspaces, and how they show (see `refs_panel`).
struct PanelState {
    /// The side panel's bookmarks, tags and workspaces: every one of the
    /// repository, from a query of their own (see `refs_panel`).
    refs: Vec<RefItem>,
    workspaces: Vec<WorkspaceItem>,
    /// The filter over every section.
    filter: Entity<InputState>,
    /// The sections folded.
    collapsed: std::collections::HashSet<PanelSection>,
    /// Folders of the panel's trees folded or unfolded against their
    /// default (see `refs_panel`).
    folders: HashMap<PanelSection, std::collections::HashSet<String>>,
    /// Bumped per panel query, so a late answer is dropped.
    generation: u64,
}

/// The working copy's next commit: its message, and what it takes.
struct NextCommit {
    /// The message, in the working copy's inspector.
    message: message::MessageEditor,
    /// The files and lines included in it; with nothing included, it takes
    /// every change, as `jj commit` does.
    included: included::Included,
    /// Included lines were left out when the working copy was read again:
    /// their file changed. Said in the commit box until the next change to
    /// what is included.
    lines_left_out: bool,
    /// A commit is on its way: its message is cleared when it is made.
    pending: bool,
    /// The working copy (change id) whose description the message box was
    /// filled with, once: a description it has is the commit's to keep.
    prefilled: Option<String>,
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

        let revset = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Filter with a revset, e.g. ancestors(@, 50)")
        });
        let panel_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter (⌘⌥F)"));
        let _subscriptions = vec![
            cx.subscribe(&panel_filter, |view, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    view.notify_pane(PaneId::Side, cx);
                }
            }),
            cx.subscribe_in(
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
            ),
        ];

        let diff_mode = diff_mode_of(settings::get(cx).display.diff);
        let diff = cx.new(|cx| DiffState::new([], cx).with_mode(diff_mode));
        let diff_events = cx.subscribe(&diff, |view: &mut Self, _, event: &DiffEvent, cx| {
            match event {
                DiffEvent::SelectionEnded(range) => view.line_selection = Some(range.clone()),
                DiffEvent::SelectionChanged(None) => view.line_selection = None,
                _ => return,
            }
            cx.notify();
        });

        let mut _subscriptions = _subscriptions;
        // ⌘↩ saves a description, or commits in the working copy's box
        let description_editor = message::MessageEditor::new(
            "Summary",
            "Description",
            |view, window, cx| view.save_description(window, cx),
            &mut _subscriptions,
            window,
            cx,
        );
        let commit_message = message::MessageEditor::new(
            "Commit summary",
            "Description",
            |view, window, cx| view.commit_changes(window, cx),
            &mut _subscriptions,
            window,
            cx,
        );
        _subscriptions
            .push(cx.observe_global::<Settings>(|view: &mut Self, cx| view.apply_settings(cx)));
        _subscriptions.push(diff_events);

        let panes = Panes::new(&cx.entity(), cx);
        let mut view = Self {
            panes,
            worker,
            state: LoadState::Loading,
            workspace: None,
            log: LogData::default(),
            paging: Paging::default(),
            panel: PanelState {
                refs: Vec::new(),
                workspaces: Vec::new(),
                filter: panel_filter,
                collapsed: refs_panel::default_collapsed(),
                folders: Default::default(),
                generation: 0,
            },
            next_commit: NextCommit {
                message: commit_message,
                included: Default::default(),
                lines_left_out: false,
                pending: false,
                prefilled: None,
            },
            line_selection: None,
            refs_hover: RefsHover::default(),
            find: None,
            author_filter: Rc::default(),
            query_error: None,
            switched_to: None,
            current_bookmark: None,
            wd: None,
            initial_query: None,
            selected: None,
            scroll: UniformListScrollHandle::new(),
            inspected: Inspected::default(),
            diff,
            diff_mode,
            revset,
            sidebar_visible: true,
            log_focus: cx.focus_handle(),
            root_focus: cx.focus_handle(),
            _events,
            _subscriptions,
            terminal: None,
            terminal_open: false,
            busy: Vec::new(),
            reload_after_ops: false,
            refresh_after_ops: None,
            progress: None,
            pending_select: None,
            ref_target: None,
            description: DescriptionEdit {
                editor: description_editor,
                loaded: None,
                drafts: HashMap::new(),
            },
            pick: None,
            dialog_subscription: None,
            avatars: Rc::default(),
            pulls: None,
            pending_commit_select: None,
            center: Center::Revisions,
            show_head_pending: false,
            columns: settings::get(cx).display.columns,
            conflicts: Conflicts::default(),
            derived_at: std::time::Instant::now(),
            column_drag: None,
            split_drag: None,
            working_copy_changed: false,
            jj_generation: settings::get(cx).jj_generation,
            reopen_pending: false,
            last_auto_fetch: std::time::Instant::now(),
            auto_fetch: None,
            column_press: None,
            log_width: Rc::default(),
            log_by_date: settings::get(cx).display.log_by_date,
            check_picked_up: None,
            picked_up_checking: false,
            picked_up: Vec::new(),
        };
        view.wd = workspace.clone();
        view.initial_query = initial_query.clone();
        view.follow_auto_fetch(cx);
        view.open(workspace, initial_query, window, cx);
        view
    }

    /// True once the workspace, its log and the selected revision are all on screen.
    pub fn is_settled(&self) -> bool {
        matches!(self.state, LoadState::Ready | LoadState::Failed(_))
            && !self.inspected.loading
            && !self.paging.loading
            && self.busy.is_empty()
            && !self.pulls_fetching()
            && !self.conflicts.loading
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
        match &self.center {
            Center::File(focus) => focus.handle.clone(),
            Center::Pull(pull) => pull.handle.clone(),
            Center::Revisions => self.log_focus.clone(),
        }
    }

    /// Descriptions typed but not saved: drafts left on other changes plus the
    /// editor, when it differs from the selected revision.
    pub fn unsaved_descriptions(&self, cx: &App) -> usize {
        self.description.drafts.len() + usize::from(self.description_dirty(cx))
    }

    /// Index of the selected log row (reported by `--keys`).
    #[cfg(feature = "screenshot")]
    pub fn selected_row(&self) -> Option<usize> {
        self.selected
    }

    /// Selects a log row by index (used by `--select-row`).
    pub fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.log.rows.len() {
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
                            workspace_name,
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
                            let changed = status.working_copy_changed;
                            Ok((
                                WorkspaceInfo {
                                    path: display_path(&path).into(),
                                    name: workspace_name.into(),
                                    root: PathBuf::from(&path),
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
                                changed,
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
                    Ok((workspace, page, changed)) => {
                        view.set_working_copy_changed(changed, cx);
                        let query = workspace.query.clone();
                        view.revset
                            .update(cx, |state, cx| state.set_value(query, window, cx));
                        let remote_urls = workspace.remote_urls.clone();
                        view.workspace = Some(workspace);
                        view.replace_rows(page, true, cx);
                        view.load_panel_refs(cx);
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

    /// What the worker reports after an operation: shown in the status bar,
    /// the redo button and the tab's dot.
    pub(super) fn apply_status(
        &mut self,
        status: &gg_lib::messages::RepoStatus,
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = &mut self.workspace {
            workspace.operation = status.operation_description.clone().into();
            workspace.can_redo = status.can_redo;
        }
        self.set_working_copy_changed(status.working_copy_changed, cx);
    }

    fn set_working_copy_changed(&mut self, changed: bool, cx: &mut Context<Self>) {
        if self.working_copy_changed != changed {
            self.working_copy_changed = changed;
            cx.emit(RepoViewEvent::WorkingCopyChanged);
        }
    }

    /// The working copy changes files (the tab shows a dot).
    pub fn working_copy_changed(&self) -> bool {
        self.working_copy_changed
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

    /// Shows the revisions of `query`. A revset that cannot be used leaves
    /// the log as it was, with the reason under the field.
    fn run_query(&mut self, query: String, cx: &mut Context<Self>) {
        let previous = self
            .workspace
            .as_mut()
            .map(|workspace| std::mem::replace(&mut workspace.query, query));
        self.reload_or_revert(previous, cx);
    }

    /// Re-runs the current query, e.g. after the repository changed.
    ///
    /// Pages loaded with "Load more" are fetched again so the list does not shrink
    /// under the user, and the scroll position is left alone.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.reload_or_revert(None, cx);
    }

    /// [`Self::reload`]; when it fails and `revert` is the query shown before,
    /// that query stays and the failure is the field's error.
    fn reload_or_revert(&mut self, revert: Option<String>, cx: &mut Context<Self>) {
        let Some(query) = self.workspace.as_ref().map(|ws| ws.query.clone()) else {
            return;
        };
        self.terminal_caught_up(cx);
        let keep = self.log.rows.len();
        let by_date = settings::get(cx).display.log_by_date;
        self.log_by_date = by_date;
        let worker = self.worker.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { worker.query_log_pages(query, by_date, keep) })
                .await;
            let _ = this.update(cx, |view, cx| {
                match (result, revert) {
                    (Ok(page), revert) => {
                        view.state = LoadState::Ready;
                        view.replace_rows(page, false, cx);
                        if revert.is_some() {
                            // a revset typed now replaces the error; the
                            // repository is as it was, and so the panel
                            view.query_error = None;
                        } else {
                            view.load_panel_refs(cx);
                        }
                    }
                    // the worker keeps paging the log shown, as the rows here stay
                    (Err(err), Some(previous)) if matches!(view.state, LoadState::Ready) => {
                        if let Some(workspace) = &mut view.workspace {
                            workspace.query = previous;
                        }
                        view.query_error = Some(one_line_error(&format!("{err:#}")).into());
                    }
                    (Err(err), _) => view.state = LoadState::Failed(format!("{err:#}").into()),
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
                    view.apply_status(&status, cx);
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

    /// Picks up what jj recorded outside this tab (the jj CLI, the terminal)
    /// and reloads: the worker otherwise keeps the operation it last loaded.
    /// With `snapshot` it snapshots first unless snapshots are off, as coming
    /// to the front does; without, as after a jj command, which snapshotted.
    pub(super) fn refresh_from_disk(&mut self, snapshot: bool, cx: &mut Context<Self>) {
        if self.workspace.is_none() {
            return;
        }
        let auto_snapshot = settings::get(cx).auto_snapshot;
        let worker = self.worker.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    match auto_snapshot {
                        _ if !snapshot => worker.load_head(),
                        Some(false) => worker.load_head(),
                        force => worker.snapshot(force == Some(true)),
                    }
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(Some(status)) => view.apply_status(&status, cx),
                    Ok(None) => {}
                    Err(err) => log::warn!("loading the latest operation: {err:#}"),
                }
                view.reload(cx);
            });
        })
        .detach();
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        if !self.paging.has_more || self.paging.loading || self.paging.error.is_some() {
            return;
        }
        self.paging.loading = true;
        cx.notify();

        let generation = self.paging.generation;
        let worker = self.worker.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { worker.query_log_next_page() })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.paging.loading = false;
                if view.paging.generation != generation {
                    // The list was replaced meanwhile; this page continued the old one.
                    cx.notify();
                    return;
                }
                match result {
                    Ok(page) => {
                        view.paging.has_more = page.has_more;
                        let from = view.log.rows.len();
                        view.log.rows.extend(page.rows);
                        view.extend_derived();
                        view.update_find(false, from, cx);
                        view.request_missing_avatars(cx);
                    }
                    Err(err) => {
                        log::warn!("next log page failed: {err:#}");
                        view.paging.error = Some(one_line_error(&format!("{err:#}")).into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn replace_rows(&mut self, page: LogPage, scroll: bool, cx: &mut Context<Self>) {
        self.paging.generation += 1;
        // Keep the selection on the same change across reloads.
        let previous = self
            .selected
            .and_then(|ix| self.log.rows.get(ix))
            .map(|row| row.revision.id.change.hex.clone());
        self.paging.has_more = page.has_more;
        self.paging.error = None;
        self.log.rows = page.rows;
        self.rebuild_derived();

        let pull_head = self.pending_commit_select.take().and_then(|hex| {
            self.log
                .rows
                .iter()
                .position(|row| row.revision.id.commit.hex == hex)
        });
        let selected = pull_head
            .or_else(|| {
                self.pending_select.take().or(previous).and_then(|hex| {
                    self.log
                        .rows
                        .iter()
                        .position(|row| row.revision.id.change.hex == hex)
                })
            })
            .or_else(|| {
                self.log
                    .rows
                    .iter()
                    .position(|row| is_this_working_copy(&row.revision))
            })
            .or(if self.log.rows.is_empty() {
                None
            } else {
                Some(0)
            });
        match selected {
            Some(ix) => self.set_selection(ix, scroll, cx),
            None => {
                // Nothing to inspect: drop any detail still on its way.
                self.selected = None;
                self.inspected.detail = None;
                self.inspected.marked.clear();
                self.inspected.task = None;
                self.inspected.loading = false;
                self.select_file(None, cx);
            }
        }
        self.update_find(false, 0, cx);
        self.refresh_focus(cx);
        self.refresh_conflicts(cx);
        self.run_picked_up_check(cx);
        self.request_missing_avatars(cx);
    }

    fn rebuild_derived(&mut self) {
        self.derived_at = std::time::Instant::now();
        self.log.row_data = Rc::new(Vec::new());
        self.log.row_of_commit.clear();
        // the rows may be others now; the pointer opens them again as it moves
        self.refs_hover = RefsHover::default();
        self.extend_derived();
    }

    /// The derived rows for the rows not derived yet (a page just loaded):
    /// the rows before it keep theirs, so each page costs its own rows only.
    /// The graph is drawn again whole, as a new page can join its lines.
    fn extend_derived(&mut self) {
        let now = Local::now();
        self.log.graph = Rc::new(Graph::new(&self.log.rows));
        // a copy only while a frame's list still holds the rows
        let row_data = Rc::make_mut(&mut self.log.row_data);
        let by_date = self.log_by_date;
        let from = row_data.len();
        self.log.row_of_commit.extend(
            self.log.rows[from..]
                .iter()
                .enumerate()
                .map(|(ix, row)| (row.revision.id.commit.hex.clone(), from + ix)),
        );
        row_data.extend(self.log.rows[from..].iter().map(|row| {
            let mut data = RowData::new(row, now);
            if by_date {
                // the rows follow commit time there; grouping by author time
                // would jump back and forth wherever history was rewritten
                let (bucket, label) = time_bucket(row.revision.committed_at, now);
                data.bucket = bucket;
                data.bucket_label = label.into();
            }
            data
        }));
        mark_bucket_starts(row_data);
        let current = nearest_bookmark(
            &self.log.rows,
            &self.log.row_of_commit,
            self.switched_to.as_deref(),
        );
        mark_current_ref(
            row_data,
            current.as_ref().map(|(ix, name)| (*ix, name.as_str())),
        );
        self.current_bookmark = current.map(|(_, name)| name.into());
        self.place_panel_items();
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
        if self.log.rows.is_empty() {
            return;
        }
        let last = self.log.rows.len() - 1;
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
        let Some(row) = self.selected.and_then(|ix| self.log.rows.get(ix)) else {
            return;
        };
        let set = RevSet {
            from: row.revision.id.clone(),
            to: row.revision.id.clone(),
        };
        self.inspected.loading = true;
        let worker = self.worker.clone();
        self.inspected.task = Some(cx.spawn(async move |this, cx| {
            // Parsing every file's patch can take a while on a large commit;
            // keep it off the UI thread with the query itself.
            let result = cx
                .background_spawn(async move { worker.query_revisions(set).map(build_detail) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.inspected.loading = false;
                match result {
                    Ok(Some(detail)) => {
                        // after a reload, stay on the file that was selected
                        let previous = view
                            .inspected
                            .file
                            .and_then(|ix| view.inspected.detail.as_ref()?.files.get(ix))
                            .map(|file| file.path.clone());
                        let keep = previous.and_then(|path| {
                            detail.files.iter().position(|file| file.path == path)
                        });
                        let first = (!detail.files.is_empty()).then_some(0);
                        let same_revision = view.inspected.detail.as_ref().is_some_and(|old| {
                            old.header.id.change.hex == detail.header.id.change.hex
                        });
                        if same_revision {
                            view.inspected
                                .marked
                                .retain(|path| detail.files.iter().any(|file| &file.path == path));
                        } else {
                            view.inspected.marked.clear();
                        }
                        // this tab's working copy: what was included of it
                        // stays while it is there as it was
                        if is_this_working_copy(&detail.header) {
                            let next = &mut view.next_commit;
                            next.included.follow(&detail.header.id.change.hex);
                            if next.included.keep_current(&detail.files) {
                                next.lines_left_out = true;
                            }
                        }
                        view.inspected.detail = Some(detail);
                        view.select_file(keep.or(first), cx);
                    }
                    Ok(None) => {
                        view.inspected.detail = None;
                        view.inspected.marked.clear();
                        view.select_file(None, cx);
                    }
                    Err(err) => {
                        // Never leave the previous revision on screen for the new selection.
                        log::warn!("revision query failed: {err:#}");
                        view.inspected.detail = None;
                        view.inspected.marked.clear();
                        view.select_file(None, cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    fn select_file(&mut self, ix: Option<usize>, cx: &mut Context<Self>) {
        self.inspected.file = ix;
        self.line_selection = None;
        let patch = ix.and_then(|ix| file_patch(self.inspected.detail.as_ref()?, ix));
        self.diff.update(cx, |state, cx| state.set_files(patch, cx));
        cx.notify();
    }

    /// The working copy is inspected as the next commit: a message, and its
    /// changes with those to commit included (see `included`). This tab's,
    /// mutable, on one parent.
    pub(super) fn commit_mode(&self) -> bool {
        self.inspected.detail.as_ref().is_some_and(|detail| {
            is_this_working_copy(&detail.header)
                && !detail.header.is_immutable
                && detail.parent_ids.len() == 1
        })
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
        // the full file view and the pull request keep diffs of their own
        match &self.center {
            Center::File(focus) => focus.diff.update(cx, |state, cx| state.set_mode(mode, cx)),
            Center::Pull(pull) => pull.diff.update(cx, |state, cx| state.set_mode(mode, cx)),
            Center::Revisions => {}
        }
        cx.notify();
    }

    /// Follows a settings change made in this or another tab.
    fn apply_settings(&mut self, cx: &mut Context<Self>) {
        self.follow_auto_fetch(cx);
        let current = settings::get(cx);
        let mode = diff_mode_of(current.display.diff);
        let github = current.display.github;
        // a drag in this tab keeps its own widths until it is dropped
        if self.column_drag.is_none() {
            self.columns = current.display.columns;
        }
        let by_date_changed = current.display.log_by_date != self.log_by_date;
        if current.jj_generation != self.jj_generation {
            // the workspace keeps the config it was loaded with
            self.jj_generation = current.jj_generation;
            self.reopen_pending = matches!(self.state, LoadState::Ready);
        }
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
        self.prefill_commit_message(window, cx);
        self.focus_find_if_new(window, cx);
        if !self.picked_up.is_empty() {
            cx.defer_in(window, |view, window, cx| view.notify_picked_up(window, cx));
        }
        // a resize released outside the window never reaches the drop handler
        if self.column_drag.is_some() && !cx.has_active_drag() {
            cx.defer_in(window, |view, _, cx| view.finish_column_resize(cx));
        }
        if self.split_drag.is_some() && !cx.has_active_drag() {
            cx.defer_in(window, |view, _, cx| view.finish_split_resize(cx));
        }
        if std::mem::take(&mut self.show_head_pending) {
            cx.defer_in(window, |view, window, cx| view.show_pull_head(window, cx));
        }
        // after the tab's own commands, whose answers belong to the old load
        if self.reopen_pending && self.busy.is_empty() {
            self.reopen_pending = false;
            cx.defer_in(window, |view, window, cx| view.retry_open(window, cx));
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
                    view.refresh_from_disk(true, cx)
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
            .on_drop(
                cx.listener(|view, _: &split::SplitResize, _, cx| view.finish_split_resize(cx)),
            )
            .on_action(
                cx.listener(|view, _: &actions::ToggleTerminal, window, cx| {
                    view.toggle_terminal(window, cx)
                }),
            )
            .on_action(cx.listener(|view, _: &actions::WidenOriginalSide, _, cx| {
                view.widen_original_side(cx)
            }))
            .on_action(cx.listener(|view, _: &actions::WidenModifiedSide, _, cx| {
                view.widen_modified_side(cx)
            }))
            .on_action(
                cx.listener(|view, _: &actions::EvenSplitSides, _, cx| view.nudge_split(None, cx)),
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
                    // the working copy is described by committing it
                    let editor = if view.commit_mode() {
                        &view.next_commit.message
                    } else {
                        &view.description.editor
                    };
                    editor.focus(window, cx);
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
            .on_action(cx.listener(|view, _: &actions::SplitFiles, window, cx| {
                view.prompt_split_files(window, cx)
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
            .on_action(cx.listener(|view, _: &actions::SplitLines, window, cx| {
                view.prompt_split_lines(window, cx)
            }))
            .on_action(
                cx.listener(|view, _: &actions::IncludeAllChanges, _, cx| {
                    view.include_all(true, cx)
                }),
            )
            .on_action(cx.listener(|view, _: &actions::ExcludeAllChanges, _, cx| {
                view.include_all(false, cx)
            }))
            .on_action(
                cx.listener(|view, _: &actions::FindCommit, window, cx| view.open_find(window, cx)),
            )
            .on_action(
                cx.listener(|view, _: &actions::FocusPanelFilter, window, cx| {
                    view.sidebar_visible = true;
                    view.panel
                        .filter
                        .update(cx, |state, cx| state.focus(window, cx));
                    cx.notify();
                }),
            )
            .on_action(cx.listener(|view, _: &actions::Undo, window, cx| view.undo(window, cx)))
            .on_action(cx.listener(|view, _: &actions::Redo, window, cx| view.redo(window, cx)))
            .on_action(
                cx.listener(|view, _: &actions::Fetch, window, cx| view.fetch_all(window, cx)),
            )
            .on_action(
                cx.listener(|view, _: &actions::Push, window, cx| view.push_selected(window, cx)),
            )
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().min_w_0().flex().child({
                let body = self.render_body(window, cx);
                self.with_terminal(body, window, cx)
            }))
            .child(self.render_status_bar(cx))
    }
}

impl RepoView {
    /// Fills the empty message box with the working copy's description, once
    /// per working copy: committing everything describes it with the box's
    /// message, which would drop a description typed before (`jj describe`).
    fn prefill_commit_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.commit_mode() {
            return;
        }
        let Some(header) = self.inspected.detail.as_ref().map(|detail| &detail.header) else {
            return;
        };
        if self.next_commit.prefilled.as_deref() == Some(header.id.change.hex.as_str()) {
            return;
        }
        self.next_commit.prefilled = Some(header.id.change.hex.clone());
        let description = header.description.lines.join("\n");
        if !description.trim().is_empty() && self.next_commit.message.text(cx).is_empty() {
            self.next_commit.message.set(&description, window, cx);
        }
    }

    /// Loads the selected revision's description into the editor when the
    /// selection changes. For the same change it follows jj's text (e.g. after
    /// a save or a snapshot) only while the user has no unsaved edits.
    fn sync_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(header) = self.inspected.detail.as_ref().map(|detail| &detail.header) else {
            return;
        };
        let change = header.id.change.hex.clone();
        // both as the fields read them back (see `message::normal_message`)
        let text = message::normal_message(&header.description.lines.join("\n"));
        let current = self.description.editor.text(cx);

        if let Some((loaded_change, loaded_text)) = &self.description.loaded {
            if *loaded_change == change {
                if current == text {
                    // Saved (or changed to the same text elsewhere): track the new text.
                    self.description.loaded = Some((change, text));
                    return;
                }
                if current != *loaded_text {
                    // Unsaved edits on this change: leave them alone.
                    return;
                }
                // Not editing and jj's text changed (undo, the jj CLI): load it below.
            } else if current != *loaded_text {
                // Leaving a change with unsaved edits: keep them as a draft.
                self.description
                    .drafts
                    .insert(loaded_change.clone(), current.clone());
            }
        }

        // A draft left on this change earlier comes back; otherwise jj's text.
        let value = self
            .description
            .drafts
            .remove(&change)
            .unwrap_or_else(|| text.clone());
        self.description.editor.set(&value, window, cx);
        self.description.loaded = Some((change, text));
    }

    fn detail_is_immutable(&self) -> bool {
        self.inspected
            .detail
            .as_ref()
            .is_some_and(|detail| detail.header.is_immutable)
    }

    /// True when the editor differs from the selected revision's description.
    fn description_dirty(&self, cx: &App) -> bool {
        self.inspected.detail.as_ref().is_some_and(|detail| {
            let saved = message::normal_message(&detail.header.description.lines.join("\n"));
            self.description.editor.text(cx) != saved
        })
    }
}

/// One of the tab's panes, for state only it shows (see
/// [`RepoView::notify_pane`]).
#[derive(Clone, Copy)]
enum PaneId {
    Log,
    Side,
    Inspector,
}

impl RepoView {
    /// Draws `pane` again, and the tab around it as GPUI marks it, but not
    /// the other panes: for state that pane alone shows, changed often (a
    /// hover, a key typed in its field). Anything else notifies the tab,
    /// and every pane draws again.
    fn notify_pane(&self, pane: PaneId, cx: &mut Context<Self>) {
        let pane = match pane {
            PaneId::Log => &self.panes.log,
            PaneId::Side => &self.panes.side,
            PaneId::Inspector => &self.panes.inspector,
        };
        pane.update(cx, |_, cx| cx.notify());
    }
}

/// The tab's panes (see `pane`): the parts a scroll of the revisions leaves
/// as they were, and the revisions, whose scroll draws only them.
struct Panes {
    log: Entity<pane::Pane>,
    side: Entity<pane::Pane>,
    inspector: Entity<pane::Pane>,
    changes: Entity<pane::Pane>,
}

impl Panes {
    fn new(owner: &Entity<RepoView>, cx: &mut Context<RepoView>) -> Self {
        let mut pane = |render: pane::PaneRender| cx.new(|cx| pane::Pane::new(owner, render, cx));
        Self {
            log: pane(|view, cx| view.render_log(cx)),
            // the full view's file history takes the side panel's place,
            // as GitKraken's does; the panel layout stays as it was
            side: pane(|view, cx| match view.center.file() {
                Some(focus) if focus.show_history => view.render_history(focus, cx),
                _ => view.render_refs_panel(cx),
            }),
            inspector: pane(|view, cx| view.render_inspector(cx)),
            changes: pane(|view, cx| view.render_changes(cx)),
        }
    }
}

/// The patch of `detail`'s file `ix`, matched by path: a patch that does not
/// parse leaves `patches` shorter than `files`.
fn file_patch(detail: &Detail, ix: usize) -> Option<DiffFile> {
    let path = &detail.files.get(ix)?.path;
    detail
        .patches
        .iter()
        .find(|patch| patch.path() == path)
        .cloned()
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
    /// The working copy started or stopped changing files.
    WorkingCopyChanged,
}

impl gpui_kit::EventEmitter<RepoViewEvent> for RepoView {}

#[cfg(test)]
mod tests {
    use super::one_line_error;

    #[test]
    fn a_revset_error_fits_on_one_line() {
        let parse = "evaluate revset: parse revset:  --> 1:3\n  |\n1 | x(\n  |   ^---\n  |\n  = expected <EOI> or <expression>";
        assert_eq!(
            one_line_error(parse),
            "evaluate revset: parse revset: expected <EOI> or <expression>"
        );
        assert_eq!(
            one_line_error("evaluate revset: Revision `nope` doesn't exist"),
            "evaluate revset: Revision `nope` doesn't exist"
        );
    }
}
