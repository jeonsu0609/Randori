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
use gg_lib::messages::{RepoConfig, RevHeader, RevId, RevSet, StoreRef};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    diff::{Diff, DiffFile, DiffMode, DiffState},
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

mod ops;
use crate::worker::{Worker, WorkerEvent};

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
            // repository
            Undo,
            Redo,
            Fetch,
            Push,
        ]
    );
}

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
        KeyBinding::new("delete", AbandonRevision, Some(LOG_CONTEXT)),
        KeyBinding::new(platform_key!("l"), FocusRevset, Some(REPO_CONTEXT)),
        KeyBinding::new("escape", FocusLog, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("r"), Reload, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("b"), ToggleSidebar, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("z"), Undo, Some(LOG_CONTEXT)),
        KeyBinding::new(platform_key!("shift-z"), Redo, Some(LOG_CONTEXT)),
        KeyBinding::new(platform_key!("shift-f"), Fetch, Some(REPO_CONTEXT)),
        KeyBinding::new(platform_key!("shift-p"), Push, Some(REPO_CONTEXT)),
    ]);
}

/// Ref chips shown inline in a log row; the rest collapse into a `+N` chip.
const MAX_ROW_REFS: usize = 1;

/// Fixed lanes shared by the log header and every row.
mod lane {
    pub const CHANGE: f32 = 6.5;
    pub const AUTHOR: f32 = 7.5;
    pub const TIME: f32 = 5.;
    pub const COMMIT: f32 = 5.;
}

enum LoadState {
    Loading,
    Failed(SharedString),
    Ready,
}

struct WorkspaceInfo {
    path: SharedString,
    query: String,
    operation: SharedString,
    /// Git remotes, for fetch.
    remotes: Vec<String>,
    /// Whether the last undo can be redone.
    can_redo: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum RefKind {
    Local,
    Remote,
    Tag,
}

#[derive(Clone)]
struct RefChip {
    name: SharedString,
    kind: RefKind,
    /// The ref as GG reports it, for bookmark operations.
    store: StoreRef,
}

/// One row of the bookmarks panel.
struct RefItem {
    chip: RefChip,
    row: usize,
    change: SharedString,
    summary: SharedString,
    time: SharedString,
}

/// The parts of a log row the virtual list needs, copied so the list closure owns them.
struct RowData {
    id: RevId,
    change_prefix: SharedString,
    change_rest: SharedString,
    commit: SharedString,
    summary: Option<SharedString>,
    author: SharedString,
    time: SharedString,
    refs: Vec<RefChip>,
    is_working_copy: bool,
}

struct FileItem {
    path: SharedString,
    name: SharedString,
    dir: SharedString,
    kind: ChangeKind,
    additions: usize,
    deletions: usize,
    has_conflict: bool,
}

struct Detail {
    header: RevHeader,
    parents: Vec<SharedString>,
    files: Vec<FileItem>,
    patches: Vec<DiffFile>,
    conflicts: usize,
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
    /// Unsaved descriptions of changes the user moved away from, by change id.
    description_drafts: HashMap<String, String>,
    /// Keeps the open one-field dialog's Enter handler alive.
    dialog_subscription: Option<Subscription>,
    /// Change id and text last loaded into the description editor.
    description_for: Option<(String, String)>,
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

        let diff = cx.new(|cx| DiffState::new([], cx).with_mode(DiffMode::Unified));

        let description_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder("Describe this change")
        });
        let mut _subscriptions = _subscriptions;
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
            selected: None,
            scroll: UniformListScrollHandle::new(),
            detail: None,
            detail_loading: false,
            selected_file: None,
            diff,
            diff_mode: DiffMode::Unified,
            revset,
            sidebar_visible: true,
            log_focus: cx.focus_handle(),
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
            dialog_subscription: None,
        };
        view.open(workspace, initial_query, window, cx);
        view
    }

    /// True once the workspace, its log and the selected revision are all on screen.
    pub fn is_settled(&self) -> bool {
        matches!(self.state, LoadState::Ready | LoadState::Failed(_))
            && !self.detail_loading
            && !self.loading_more
            && self.busy.is_empty()
    }

    /// Moves keyboard focus to the revision list (e.g. when its tab is selected).
    pub fn focus_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.log_focus.focus(window, cx);
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
        let worker = self.worker.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    match worker.open_workspace(wd)? {
                        RepoConfig::Workspace {
                            absolute_path,
                            latest_query,
                            status,
                            git_remotes,
                            ..
                        } => {
                            let query = initial_query.unwrap_or(latest_query);
                            let page = worker.query_log(query.clone())?;
                            let path = absolute_path.0;
                            Ok((
                                WorkspaceInfo {
                                    path: display_path(&path).into(),
                                    query,
                                    operation: status.operation_description.into(),
                                    remotes: git_remotes,
                                    can_redo: status.can_redo,
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
                        view.workspace = Some(workspace);
                        view.replace_rows(page, true, cx);
                        view.state = LoadState::Ready;
                        view.log_focus.focus(window, cx);
                    }
                    Err(err) => view.state = LoadState::Failed(format!("{err:#}").into()),
                }
                cx.notify();
            });
        })
        .detach();
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
        let worker = self.worker.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let mut page = worker.query_log(query)?;
                    while page.has_more && page.rows.len() < keep {
                        let next = worker.query_log_next_page()?;
                        page.has_more = next.has_more;
                        page.rows.extend(next.rows);
                    }
                    anyhow::Ok(page)
                })
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
        let worker = self.worker.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { worker.snapshot() }).await;
            let _ = this.update(cx, |view, cx| match result {
                Ok(Some(status)) => {
                    if let Some(workspace) = &mut view.workspace {
                        workspace.operation = status.operation_description.into();
                    }
                    view.reload(cx);
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

        let worker = self.worker.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { worker.query_log_next_page() })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.loading_more = false;
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
        // Keep the selection on the same change across reloads.
        let previous = self
            .selected
            .and_then(|ix| self.rows.get(ix))
            .map(|row| row.revision.id.change.hex.clone());
        self.has_more = page.has_more;
        self.rows = page.rows;
        self.rebuild_derived();

        let selected = self
            .pending_select
            .take()
            .or(previous)
            .and_then(|hex| {
                self.rows
                    .iter()
                    .position(|row| row.revision.id.change.hex == hex)
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
    }

    fn rebuild_derived(&mut self) {
        let now = Local::now();
        self.graph = Rc::new(Graph::new(&self.rows));
        self.row_data = Rc::new(self.rows.iter().map(|row| RowData::new(row, now)).collect());
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
            let result = cx
                .background_spawn(async move { worker.query_revisions(set) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.detail_loading = false;
                match result {
                    Ok(RevsResult::Detail {
                        headers,
                        parents,
                        changes,
                        conflicts,
                        ..
                    }) => {
                        if let Some(header) = headers.into_iter().last() {
                            view.install_detail(header, parents, changes, conflicts.len(), cx);
                        }
                    }
                    Ok(RevsResult::NotFound { .. }) => {
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

    fn install_detail(
        &mut self,
        header: RevHeader,
        parents: Vec<RevHeader>,
        changes: Vec<RevChange>,
        conflicts: usize,
        cx: &mut Context<Self>,
    ) {
        let files: Vec<FileItem> = changes.iter().map(FileItem::from).collect();
        let patches: Vec<DiffFile> = changes
            .iter()
            .filter_map(|change| DiffFile::parse(&unified_patch(change)).ok())
            .flatten()
            .collect();
        self.detail = Some(Detail {
            parents: parents
                .iter()
                .map(|parent| {
                    SharedString::from(short_id(
                        &parent.id.change.prefix,
                        &parent.id.change.rest,
                        8,
                    ))
                })
                .collect(),
            header,
            files,
            patches,
            conflicts,
        });
        self.select_file(if changes.is_empty() { None } else { Some(0) }, cx);
    }

    fn select_file(&mut self, ix: Option<usize>, cx: &mut Context<Self>) {
        self.selected_file = ix;
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

    fn set_diff_mode(&mut self, mode: DiffMode, cx: &mut Context<Self>) {
        self.diff_mode = mode;
        self.diff.update(cx, |state, cx| state.set_mode(mode, cx));
        cx.notify();
    }

    fn handle_worker_event(&mut self, event: WorkerEvent, cx: &mut Context<Self>) {
        // GG's message types only implement Serialize, so read the fields we need
        // straight from the JSON payload.
        let payload = &event.payload;
        match event.name.as_str() {
            "gg://repo/status" => {
                if let Some(workspace) = &mut self.workspace
                    && let Some(description) = payload
                        .get("operation_description")
                        .and_then(|value| value.as_str())
                {
                    workspace.operation = description.to_owned().into();
                }
                self.reload(cx);
            }
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

    /// The repository toolbar under the tab strip: panel toggle, workspace,
    /// revset query and the repository commands.
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let path = self
            .workspace
            .as_ref()
            .map(|ws| ws.path.clone())
            .unwrap_or_default();
        let ready = matches!(self.state, LoadState::Ready);
        let can_redo = self.workspace.as_ref().is_some_and(|ws| ws.can_redo);
        let has_selection = self.selected.is_some();
        let command = |id: &'static str, icon: IconName, tooltip: &'static str| {
            Button::new(id).ghost().small().icon(icon).tooltip(tooltip)
        };

        h_flex()
            .h_10()
            .flex_none()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("toggle-sidebar")
                    .ghost()
                    .small()
                    .icon(IconName::PanelLeft)
                    .selected(self.sidebar_visible)
                    .tooltip("Show or hide bookmarks")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.sidebar_visible = !view.sidebar_visible;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .min_w_0()
                    .px_1()
                    .text_xs()
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .child(path),
            )
            .child(div().flex_1())
            .child(
                div().w_72().child(
                    Input::new(&self.revset)
                        .small()
                        .prefix(gpui_kit::component::Icon::new(IconName::Search).xsmall())
                        .cleanable(true),
                ),
            )
            .child(div().w_2())
            .child(
                command(
                    "new-change",
                    IconName::Plus,
                    "New change on the selection (N)",
                )
                .disabled(!ready || !has_selection)
                .on_click(cx.listener(|view, _, window, cx| view.new_child(window, cx))),
            )
            .child(
                command("undo", IconName::Undo2, "Undo the last operation")
                    .disabled(!ready)
                    .on_click(cx.listener(|view, _, window, cx| view.undo(window, cx))),
            )
            .child(
                command("redo", IconName::Redo2, "Redo")
                    .disabled(!ready || !can_redo)
                    .on_click(cx.listener(|view, _, window, cx| view.redo(window, cx))),
            )
            .child(
                command("fetch", IconName::CloudDownload, "Fetch from all remotes")
                    .disabled(!ready)
                    .on_click(cx.listener(|view, _, window, cx| view.fetch_all(window, cx))),
            )
            .child(
                command("push", IconName::Upload, "Push the selection's bookmarks")
                    .disabled(!ready || !has_selection)
                    .on_click(cx.listener(|view, _, window, cx| view.push_selected(window, cx))),
            )
            .child(
                command("refresh", IconName::RefreshCw, "Reload revisions")
                    .on_click(cx.listener(|view, _, _, cx| view.reload(cx))),
            )
    }

    fn render_refs_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let filter = self.ref_filter;
        let visible: Vec<&RefItem> = self
            .refs
            .iter()
            .filter(|item| match filter {
                1 => item.chip.kind == RefKind::Local,
                2 => item.chip.kind == RefKind::Remote,
                3 => item.chip.kind == RefKind::Tag,
                _ => true,
            })
            .collect();
        let selected_row = self.selected;

        v_flex()
            .size_full()
            .min_h_0()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .h_10()
                    .px_4()
                    .gap_2()
                    .flex_none()
                    .child(div().text_sm().font_medium().child("Bookmarks"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(self.refs.len().to_string()),
                    ),
            )
            .child(
                div().px_3().pb_2().flex_none().child(
                    TabBar::new("ref-filter")
                        .pill()
                        .xsmall()
                        .selected_index(filter)
                        .on_click(cx.listener(|view, ix: &usize, _, cx| {
                            view.ref_filter = *ix;
                            cx.notify();
                        }))
                        .child("All")
                        .child("Local")
                        .child("Remote")
                        .child("Tags"),
                ),
            )
            .child(div().h_px().flex_none().bg(theme.border))
            .child(
                v_flex()
                    .id("refs")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .when(visible.is_empty(), |this| {
                        this.child(
                            div()
                                .p_4()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("No bookmarks in the loaded revisions."),
                        )
                    })
                    .children(visible.into_iter().enumerate().map(|(ix, item)| {
                        let row = item.row;
                        let store = item.chip.store.clone();
                        let menu_ref = item.chip.store.clone();
                        let icon = match item.chip.kind {
                            RefKind::Local => IconName::GitBranch,
                            RefKind::Remote => IconName::CloudDownload,
                            RefKind::Tag => IconName::Tag,
                        };
                        ListItem::new(("ref", ix))
                            .px_4()
                            .py_1p5()
                            .selected(selected_row == Some(row))
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap_3()
                                    .child(
                                        gpui_kit::component::Icon::new(icon)
                                            .small()
                                            .text_color(theme.muted_foreground),
                                    )
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_medium()
                                                    .truncate()
                                                    .child(item.chip.name.clone()),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_1()
                                                    .min_w_0()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(
                                                        div()
                                                            .flex_none()
                                                            .font_family(
                                                                theme.mono_font_family.clone(),
                                                            )
                                                            .text_color(theme.primary)
                                                            .child(item.change.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .child(item.summary.clone()),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(item.time.clone()),
                                    ),
                            )
                            .on_click(cx.listener(move |view, _, _, cx| view.select(row, cx)))
                            .on_mouse_down(
                                gpui_kit::MouseButton::Right,
                                cx.listener(move |view, _, window, cx| {
                                    view.ref_target = Some(store.clone());
                                    view.log_focus.focus(window, cx);
                                }),
                            )
                            .on_drag(
                                DraggedRef {
                                    store: item.chip.store.clone(),
                                    label: item.chip.name.clone(),
                                },
                                |dragged, _, _, cx| {
                                    cx.new(|_| DragChip {
                                        label: format!("Move {}", dragged.label).into(),
                                        icon: IconName::Bookmark,
                                    })
                                },
                            )
                            .context_menu({
                                let focus = self.log_focus.clone();
                                move |menu, _, _| ref_menu(menu, &menu_ref, focus.clone())
                            })
                    })),
            )
            .into_any_element()
    }

    fn render_log_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let query = self
            .workspace
            .as_ref()
            .map(|ws| SharedString::from(ws.query.clone()))
            .unwrap_or_default();
        v_flex()
            .flex_none()
            .child(
                h_flex()
                    .h_12()
                    .px_4()
                    .gap_3()
                    .child(
                        gpui_kit::component::Icon::new(IconName::GitGraph)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().text_base().font_medium().child("Revisions"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(query),
                    ),
            )
            .child(
                h_flex()
                    .h_8()
                    .px_3()
                    .gap_2()
                    .border_y_1()
                    .border_color(theme.border)
                    .bg(theme.muted.opacity(0.35))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().w(self.graph.width()).flex_none())
                    .child(
                        div()
                            .w(gpui_kit::rems(lane::CHANGE))
                            .flex_none()
                            .child("Change"),
                    )
                    .child(div().flex_1().min_w_0().child("Description"))
                    .child(
                        div()
                            .w(gpui_kit::rems(lane::AUTHOR))
                            .flex_none()
                            .child("Author"),
                    )
                    .child(
                        div()
                            .w(gpui_kit::rems(lane::TIME))
                            .flex_none()
                            .flex()
                            .justify_end()
                            .child("Updated"),
                    )
                    .child(
                        div()
                            .w(gpui_kit::rems(lane::COMMIT))
                            .flex_none()
                            .flex()
                            .justify_end()
                            .child("Commit"),
                    ),
            )
    }

    fn render_log(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.row_data.clone();
        let graph = self.graph.clone();
        let focus = self.log_focus.clone();
        let selected = self.selected;
        let owner = cx.entity().downgrade();

        v_flex()
            .size_full()
            .min_h_0()
            .child(self.render_log_header(cx))
            .child(
                div()
                    .id("log")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .track_focus(&self.log_focus)
                    .key_context(LOG_CONTEXT)
                    .role(gpui_kit::Role::List)
                    .aria_label("Revisions")
                    .on_mouse_down(
                        gpui_kit::MouseButton::Left,
                        cx.listener(|view, _, window, cx| view.log_focus.focus(window, cx)),
                    )
                    .on_action(cx.listener(|view, _: &actions::SelectPrevious, _, cx| {
                        view.move_selection(-1, cx)
                    }))
                    .on_action(cx.listener(|view, _: &actions::SelectNext, _, cx| {
                        view.move_selection(1, cx)
                    }))
                    .on_action(cx.listener(|view, _: &actions::SelectFirst, _, cx| {
                        view.move_selection(isize::MIN, cx)
                    }))
                    .on_action(cx.listener(|view, _: &actions::SelectLast, _, cx| {
                        view.move_selection(isize::MAX, cx)
                    }))
                    .child(
                        uniform_list("revisions", rows.len(), move |range, _, cx| {
                            range
                                .map(|ix| {
                                    let owner = owner.clone();
                                    let menu_owner = owner.clone();
                                    let drop_owner = owner.clone();
                                    let focus = focus.clone();
                                    render_row(&rows[ix], &graph, ix, selected == Some(ix), cx)
                                        .on_click(move |_, _, cx| {
                                            let _ =
                                                owner.update(cx, |view, cx| view.select(ix, cx));
                                        })
                                        // Right-click selects the row first, so the menu acts on it.
                                        .on_mouse_down(
                                            gpui_kit::MouseButton::Right,
                                            move |_, window, cx| {
                                                let _ = menu_owner.update(cx, |view, cx| {
                                                    view.select(ix, cx);
                                                    view.log_focus.focus(window, cx);
                                                });
                                            },
                                        )
                                        .on_drag(
                                            DraggedRevision {
                                                id: rows[ix].id.clone(),
                                                label: rows[ix].change_prefix.clone(),
                                            },
                                            |dragged, _, _, cx| {
                                                cx.new(|_| DragChip {
                                                    label: format!("Rebase {}", dragged.label)
                                                        .into(),
                                                    icon: IconName::GitCommitHorizontal,
                                                })
                                            },
                                        )
                                        .drag_over::<DraggedRevision>(|style, _, _, cx| {
                                            style.bg(cx.theme().primary.opacity(0.18))
                                        })
                                        .drag_over::<DraggedRef>(|style, _, _, cx| {
                                            style.bg(cx.theme().primary.opacity(0.18))
                                        })
                                        .on_drop({
                                            let owner = drop_owner.clone();
                                            move |dragged: &DraggedRevision, window, cx| {
                                                let _ = owner.update(cx, |view, cx| {
                                                    view.rebase_onto(
                                                        dragged.id.clone(),
                                                        ix,
                                                        window,
                                                        cx,
                                                    )
                                                });
                                            }
                                        })
                                        .on_drop({
                                            let owner = drop_owner.clone();
                                            move |dragged: &DraggedRef, window, cx| {
                                                let _ = owner.update(cx, |view, cx| {
                                                    view.move_ref_to(
                                                        dragged.store.clone(),
                                                        ix,
                                                        window,
                                                        cx,
                                                    )
                                                });
                                            }
                                        })
                                        .context_menu(move |menu, _, _| {
                                            revision_menu(menu, focus.clone())
                                        })
                                        .into_any_element()
                                })
                                .collect()
                        })
                        .size_full()
                        .track_scroll(&self.scroll),
                    )
                    .child(Scrollbar::vertical(&self.scroll)),
            )
            .into_any_element()
    }

    fn render_changes(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let file = self
            .selected_file
            .and_then(|ix| self.detail.as_ref()?.files.get(ix));
        let body = if self.detail_loading && self.detail.is_none() {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(Spinner::new())
                .into_any_element()
        } else if file.is_none() {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_1()
                .child(div().text_sm().font_medium().child("No changes"))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("This revision leaves every file as it was."),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .child(Diff::new(&self.diff).line_number(true).soft_wrap(false))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .h_10()
                    .px_4()
                    .gap_3()
                    .flex_none()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        gpui_kit::component::Icon::new(IconName::FileDiff)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().text_sm().font_medium().child("Changes"))
                    .when_some(file, |this, file| {
                        this.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(file.path.clone()),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        TabBar::new("diff-mode")
                            .segmented()
                            .xsmall()
                            .selected_index(if self.diff_mode == DiffMode::Split {
                                1
                            } else {
                                0
                            })
                            .on_click(cx.listener(|view, ix: &usize, _, cx| {
                                view.set_diff_mode(
                                    if *ix == 1 {
                                        DiffMode::Split
                                    } else {
                                        DiffMode::Unified
                                    },
                                    cx,
                                )
                            }))
                            .child("Unified")
                            .child("Split"),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    fn render_inspector(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(detail) = &self.detail else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if self.detail_loading {
                    Spinner::new().into_any_element()
                } else {
                    "Select a revision".into_any_element()
                })
                .into_any_element();
        };
        let header = &detail.header;
        let mut lines = header.description.lines.iter().map(String::as_str);
        let subject = lines.next().filter(|line| !line.trim().is_empty());
        let body: String = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
        let (additions, deletions) = detail.files.iter().fold((0, 0), |(a, d), file| {
            (a + file.additions, d + file.deletions)
        });
        let refs = ref_chips(&header.refs);

        let fact = |label: &'static str, value: AnyElement| {
            v_flex()
                .gap_0p5()
                .min_w_0()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(div().text_sm().min_w_0().truncate().child(value))
        };

        v_flex()
            .id("inspector")
            .size_full()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .px_4()
                    .pt_4()
                    .pb_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_2xl()
                                    .child(
                                        div()
                                            .font_semibold()
                                            .text_color(theme.primary)
                                            .child(header.id.change.prefix.clone()),
                                    )
                                    .child(div().text_color(theme.muted_foreground).child(
                                        header.id.change.rest.chars().take(6).collect::<String>(),
                                    )),
                            )
                            .child(div().flex_1())
                            .when(header.is_working_copy, |this| {
                                this.child(Tag::primary().small().child("Working copy"))
                            })
                            .when(header.is_immutable, |this| {
                                this.child(Tag::secondary().small().child("Immutable"))
                            })
                            .when(header.has_conflict, |this| {
                                this.child(Tag::danger().small().child("Conflict"))
                            }),
                    )
                    // Immutable history is read-only; anything else is described in place.
                    .when(header.is_immutable, |this| {
                        this.child(match subject {
                            Some(subject) => {
                                div().text_base().font_medium().child(subject.to_owned())
                            }
                            None => div()
                                .text_base()
                                .italic()
                                .text_color(theme.muted_foreground)
                                .child("(no description set)"),
                        })
                        .when(!body.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(body.clone()),
                            )
                        })
                    })
                    .when(!header.is_immutable, |this| {
                        let dirty = self.description_dirty(cx);
                        this.child(Textarea::new(&self.description_input).small())
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(if dirty {
                                                "Unsaved · ⌘↩ to save"
                                            } else {
                                                "Press D to edit"
                                            }),
                                    )
                                    .child(
                                        Button::new("save-description")
                                            .small()
                                            .label("Save")
                                            .when(dirty, |button| button.primary())
                                            .disabled(!dirty || !self.busy.is_empty())
                                            .on_click(cx.listener(|view, _, window, cx| {
                                                view.save_description(window, cx)
                                            })),
                                    ),
                            )
                    })
                    .when(!refs.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .flex_wrap()
                                .children(refs.iter().map(|chip| {
                                    Tag::secondary().xsmall().child(chip.name.clone())
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .grid()
                    .grid_cols(2)
                    .gap_x_4()
                    .gap_y_3()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(fact(
                        "Author",
                        header.author.name.clone().into_any_element(),
                    ))
                    .child(fact(
                        "Date",
                        header
                            .author
                            .timestamp
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                            .into_any_element(),
                    ))
                    .child(fact(
                        "Commit",
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(
                                header.id.commit.prefix.clone()
                                    + &header.id.commit.rest[..6.min(header.id.commit.rest.len())],
                            )
                            .into_any_element(),
                    ))
                    .child(fact(
                        "Parents",
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(if detail.parents.is_empty() {
                                "none".to_owned()
                            } else {
                                detail.parents.join(", ")
                            })
                            .into_any_element(),
                    )),
            )
            .child(
                h_flex()
                    .h_10()
                    .px_4()
                    .gap_2()
                    .child(div().text_sm().font_medium().child("Changed files"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail.files.len().to_string()),
                    )
                    .child(div().flex_1())
                    .when(detail.conflicts > 0, |this| {
                        this.child(
                            Tag::danger()
                                .xsmall()
                                .child(format!("{} conflicts", detail.conflicts)),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(count_color(
                                additions,
                                theme.success,
                                theme.muted_foreground,
                            ))
                            .child(format!("+{additions}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(count_color(
                                deletions,
                                theme.danger,
                                theme.muted_foreground,
                            ))
                            .child(format!("−{deletions}")),
                    ),
            )
            .children(detail.files.iter().enumerate().map(|(ix, file)| {
                let (letter, color) = match file.kind {
                    ChangeKind::Added => ("A", theme.success),
                    ChangeKind::Deleted => ("D", theme.danger),
                    ChangeKind::Modified => ("M", theme.warning),
                    ChangeKind::None => ("·", theme.muted_foreground),
                };
                ListItem::new(("file", ix))
                    .px_4()
                    .py_1()
                    .selected(self.selected_file == Some(ix))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_3()
                            .child(
                                div()
                                    .w_3()
                                    .flex_none()
                                    .text_xs()
                                    .font_semibold()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(if file.has_conflict {
                                        theme.danger
                                    } else {
                                        color
                                    })
                                    .child(letter),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().truncate().child(file.name.clone()))
                                    .when(!file.dir.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .truncate()
                                                .text_color(theme.muted_foreground)
                                                .child(file.dir.clone()),
                                        )
                                    }),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap_2()
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(
                                        div()
                                            .w_8()
                                            .flex()
                                            .justify_end()
                                            .text_color(count_color(
                                                file.additions,
                                                theme.success,
                                                theme.muted_foreground,
                                            ))
                                            .child(format!("+{}", file.additions)),
                                    )
                                    .child(
                                        div()
                                            .w_8()
                                            .flex()
                                            .justify_end()
                                            .text_color(count_color(
                                                file.deletions,
                                                theme.danger,
                                                theme.muted_foreground,
                                            ))
                                            .child(format!("−{}", file.deletions)),
                                    ),
                            ),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| view.select_file(Some(ix), cx)))
            }))
            .into_any_element()
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (state_color, state_label) = match &self.state {
            LoadState::Loading => (theme.warning, "Loading"),
            LoadState::Failed(_) => (theme.danger, "Error"),
            LoadState::Ready => (theme.success, "Ready"),
        };
        let working_copy = self
            .rows
            .iter()
            .find(|row| row.revision.is_working_copy)
            .map(|row| row.revision.id.change.prefix.clone());
        StatusBar::new()
            .left(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .child(div().size_2().rounded_full().bg(state_color))
                    .child(state_label)
                    // A running jj operation replaces the last-operation note.
                    .when_some(self.busy.last().cloned(), |this, busy| {
                        let others = self.busy.len() - 1;
                        this.child(div().text_color(theme.muted_foreground).child("·"))
                            .child(Spinner::new().xsmall())
                            .child(busy)
                            .when(others > 0, |this| {
                                this.child(
                                    div()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("+{others} more")),
                                )
                            })
                            .when_some(self.progress.clone(), |this, progress| {
                                this.child(div().text_color(theme.muted_foreground).child(progress))
                            })
                    })
                    .when_some(
                        self.workspace.as_ref().filter(|_| self.busy.is_empty()),
                        |this, ws| {
                            this.child(div().text_color(theme.muted_foreground).child("·"))
                                .child(
                                    div()
                                        .min_w_0()
                                        .max_w_128()
                                        .truncate()
                                        .text_color(theme.muted_foreground)
                                        .child(ws.operation.clone()),
                                )
                        },
                    ),
            )
            .right(
                h_flex()
                    .gap_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .when_some(working_copy, |this, change| {
                        this.child(
                            h_flex().gap_1().child("@").child(
                                div()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(theme.primary)
                                    .child(change),
                            ),
                        )
                    })
                    .child(format!(
                        "{} revisions{}",
                        self.rows.len(),
                        if self.has_more { "+" } else { "" }
                    ))
                    .when(self.has_more, |this| {
                        this.child(
                            Button::new("load-more")
                                .ghost()
                                .xsmall()
                                .label(if self.loading_more {
                                    "Loading…"
                                } else {
                                    "Load more"
                                })
                                .on_click(cx.listener(|view, _, _, cx| view.load_more(cx))),
                        )
                    }),
            )
    }

    fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match &self.state {
            LoadState::Loading => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(Spinner::new())
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Opening workspace…"),
                )
                .into_any_element(),
            LoadState::Failed(message) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .p_8()
                .child(div().font_medium().child("Couldn't open the workspace"))
                .child(
                    div()
                        .max_w_128()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(message.clone()),
                )
                .into_any_element(),
            LoadState::Ready => {
                let rem = window.rem_size();
                h_resizable("workspace")
                    .when(self.sidebar_visible, |this| {
                        this.child(
                            resizable_panel()
                                .size(rem * 18.)
                                .size_range(rem * 14. ..rem * 28.)
                                .child(self.render_refs_panel(cx)),
                        )
                    })
                    .child(
                        resizable_panel().size_range(rem * 30. ..rem * 200.).child(
                            v_resizable("center")
                                .child(
                                    resizable_panel()
                                        .size(rem * 26.)
                                        .size_range(rem * 10. ..rem * 200.)
                                        .child(self.render_log(cx)),
                                )
                                .child(
                                    resizable_panel()
                                        .size_range(rem * 8. ..rem * 200.)
                                        .child(self.render_changes(cx)),
                                ),
                        ),
                    )
                    .child(
                        resizable_panel()
                            .size(rem * 22.)
                            .size_range(rem * 18. ..rem * 36.)
                            .child(self.render_inspector(cx)),
                    )
                    .into_any_element()
            }
        }
    }
}

impl Render for RepoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_description(window, cx);

        v_flex()
            .id("repo")
            .key_context(REPO_CONTEXT)
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
            .on_action(cx.listener(|view, _: &actions::Reload, _, cx| view.reload(cx)))
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
                    if view.refuse_if_immutable("Describe", window, cx) {
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

    /// True when the editor differs from the selected revision's description.
    fn description_dirty(&self, cx: &App) -> bool {
        self.detail.as_ref().is_some_and(|detail| {
            let saved = detail.header.description.lines.join("\n");
            self.description_input.read(cx).value().trim_end() != saved.trim_end()
        })
    }
}

// ----- drag and drop ---------------------------------------------------------------------------

/// A revision being dragged; dropping it on another revision rebases it there.
#[derive(Clone)]
struct DraggedRevision {
    id: RevId,
    label: SharedString,
}

/// A bookmark or tag being dragged; dropping it on a revision moves it there.
#[derive(Clone)]
struct DraggedRef {
    store: StoreRef,
    label: SharedString,
}

/// The chip that follows the pointer during a drag.
struct DragChip {
    label: SharedString,
    icon: IconName,
}

impl Render for DragChip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .gap_1p5()
            .px_2()
            .py_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.primary)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .text_xs()
            .shadow_md()
            .child(gpui_kit::component::Icon::new(self.icon).xsmall())
            .child(self.label.clone())
    }
}

// ----- context menus ---------------------------------------------------------------------------

/// Commands for the selected revision. Items dispatch actions to the repository
/// tab, so the menu shows the same key bindings as the keyboard.
fn revision_menu(menu: PopupMenu, focus: FocusHandle) -> PopupMenu {
    use actions::*;
    menu.action_context(focus)
        .menu("New change", Box::new(NewChild))
        .menu("Edit", Box::new(EditRevision))
        .menu("Describe", Box::new(DescribeRevision))
        .separator()
        .menu("Create bookmark…", Box::new(CreateBookmark))
        .menu("Push bookmarks", Box::new(Push))
        .separator()
        .menu("Duplicate", Box::new(DuplicateRevision))
        .menu("Back out", Box::new(BackoutRevision))
        .separator()
        .menu("Abandon…", Box::new(AbandonRevision))
}

/// Commands for a bookmark or tag in the bookmarks panel.
fn ref_menu(menu: PopupMenu, store: &StoreRef, focus: FocusHandle) -> PopupMenu {
    use actions::*;
    let menu = menu.action_context(focus);
    match store {
        StoreRef::LocalBookmark { .. } => menu
            .menu("Push", Box::new(PushBookmark))
            .menu("Rename…", Box::new(RenameBookmark))
            .separator()
            .menu("Delete", Box::new(DeleteRef)),
        StoreRef::RemoteBookmark { is_tracked, .. } => menu
            .menu(
                if *is_tracked { "Untrack" } else { "Track" },
                Box::new(ToggleTrackBookmark),
            )
            .separator()
            .menu("Delete", Box::new(DeleteRef)),
        StoreRef::Tag { .. } => menu.menu("Delete", Box::new(DeleteRef)),
    }
}

// ----- rows ------------------------------------------------------------------------------------

fn render_row(row: &RowData, graph: &Graph, ix: usize, selected: bool, cx: &App) -> ListItem {
    let theme = cx.theme();
    ListItem::new(("revision", ix))
        .selected(selected)
        .h(row_height())
        .pl_3()
        .pr_3()
        .py_0()
        .w_full()
        .child(
            h_flex()
                .h_full()
                .gap_2()
                .min_w_0()
                .flex_1()
                .text_sm()
                .child(graph.row_canvas(ix, cx))
                .child(
                    h_flex()
                        .w(gpui_kit::rems(lane::CHANGE))
                        .flex_none()
                        .font_family(theme.mono_font_family.clone())
                        .child(
                            div()
                                .font_semibold()
                                .text_color(theme.primary)
                                .child(row.change_prefix.clone()),
                        )
                        .child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child(row.change_rest.clone()),
                        ),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .gap_1p5()
                        .children(
                            row.refs
                                .iter()
                                .take(MAX_ROW_REFS)
                                .map(|chip| ref_tag(chip, cx)),
                        )
                        .when(row.refs.len() > MAX_ROW_REFS, |this| {
                            this.child(
                                Tag::secondary()
                                    .outline()
                                    .xsmall()
                                    .child(format!("+{}", row.refs.len() - MAX_ROW_REFS)),
                            )
                        })
                        .child(match &row.summary {
                            Some(summary) => div()
                                .min_w_0()
                                .truncate()
                                .when(row.is_working_copy, |this| this.font_medium())
                                .child(summary.clone()),
                            None => div()
                                .min_w_0()
                                .truncate()
                                .italic()
                                .text_color(theme.muted_foreground)
                                .child("(no description set)"),
                        }),
                )
                .child(
                    div()
                        .w(gpui_kit::rems(lane::AUTHOR))
                        .flex_none()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(row.author.clone()),
                )
                .child(
                    div()
                        .w(gpui_kit::rems(lane::TIME))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(row.time.clone()),
                )
                .child(
                    div()
                        .w(gpui_kit::rems(lane::COMMIT))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child(row.commit.clone()),
                ),
        )
}

fn ref_tag(chip: &RefChip, _cx: &App) -> Tag {
    let tag = match chip.kind {
        RefKind::Local => Tag::info(),
        RefKind::Remote => Tag::secondary(),
        RefKind::Tag => Tag::warning(),
    };
    tag.outline().xsmall().child(chip.name.clone())
}

impl RowData {
    fn new(row: &LogRow, now: DateTime<Local>) -> Self {
        let rev = &row.revision;
        Self {
            id: rev.id.clone(),
            change_prefix: rev.id.change.prefix.clone().into(),
            change_rest: rev
                .id
                .change
                .rest
                .chars()
                .take(6)
                .collect::<String>()
                .into(),
            commit: short_id(&rev.id.commit.prefix, &rev.id.commit.rest, 8).into(),
            summary: rev
                .description
                .lines
                .first()
                .filter(|line| !line.trim().is_empty())
                .map(|line| SharedString::from(line.clone())),
            author: rev.author.name.clone().into(),
            time: relative_time(rev.author.timestamp, now).into(),
            refs: ref_chips(&rev.refs),
            is_working_copy: rev.is_working_copy,
        }
    }
}

impl From<&RevChange> for FileItem {
    fn from(change: &RevChange) -> Self {
        let path = change.path.repo_path.clone();
        let (dir, name) = match path.rsplit_once('/') {
            Some((dir, name)) => (dir.to_owned(), name.to_owned()),
            None => (String::new(), path.clone()),
        };
        let (mut additions, mut deletions) = (0, 0);
        for line in change.hunks.iter().flat_map(|hunk| &hunk.lines.lines) {
            if line.starts_with('+') {
                additions += 1;
            } else if line.starts_with('-') {
                deletions += 1;
            }
        }
        Self {
            path: path.into(),
            name: name.into(),
            dir: dir.into(),
            kind: match change.kind {
                ChangeKind::Added => ChangeKind::Added,
                ChangeKind::Deleted => ChangeKind::Deleted,
                ChangeKind::Modified => ChangeKind::Modified,
                ChangeKind::None => ChangeKind::None,
            },
            additions,
            deletions,
            has_conflict: change.has_conflict,
        }
    }
}

/// Zero counts carry no signal, so they stay muted instead of red or green.
fn count_color(count: usize, color: gpui_kit::Hsla, muted: gpui_kit::Hsla) -> gpui_kit::Hsla {
    if count == 0 { muted } else { color }
}

/// Unique prefix plus enough of the rest to make `len` characters.
fn short_id(prefix: &str, rest: &str, len: usize) -> String {
    let extra = len.saturating_sub(prefix.chars().count());
    prefix.chars().chain(rest.chars().take(extra)).collect()
}

/// `~/…` for paths under the home directory, and only the last two components
/// when the path would still crowd the title bar.
fn display_path(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => path.to_owned(),
    };
    if path.chars().count() <= 48 {
        return path;
    }
    let parts: Vec<&str> = path.rsplitn(3, '/').collect();
    match parts.as_slice() {
        [last, parent, _] => format!("…/{parent}/{last}"),
        _ => path,
    }
}

fn ref_chips(refs: &[StoreRef]) -> Vec<RefChip> {
    refs.iter()
        .filter_map(|r| match r {
            StoreRef::LocalBookmark { bookmark_name, .. } => Some(RefChip {
                name: bookmark_name.clone().into(),
                kind: RefKind::Local,
                store: r.clone(),
            }),
            // A remote ref that is in sync with its local bookmark adds no information.
            StoreRef::RemoteBookmark {
                bookmark_name,
                remote_name,
                is_synced,
                is_tracked,
                ..
            } if !(*is_synced && *is_tracked) => Some(RefChip {
                name: format!("{bookmark_name}@{remote_name}").into(),
                kind: RefKind::Remote,
                store: r.clone(),
            }),
            StoreRef::RemoteBookmark { .. } => None,
            StoreRef::Tag { tag_name } => Some(RefChip {
                name: tag_name.clone().into(),
                kind: RefKind::Tag,
                store: r.clone(),
            }),
        })
        .collect()
}

/// Builds a git-style patch for one file so gpui-kit's `Diff` can parse it.
/// GG's hunk ranges are already 1-based (its copy of jj's hunker starts at
/// `1..1`); a unified header only differs for an empty side, which names the
/// line before the hunk (`-0,0` for an added file).
fn unified_patch(change: &RevChange) -> String {
    let path = &change.path.repo_path;
    let old = if matches!(change.kind, ChangeKind::Added) {
        "/dev/null".to_owned()
    } else {
        format!("a/{path}")
    };
    let new = if matches!(change.kind, ChangeKind::Deleted) {
        "/dev/null".to_owned()
    } else {
        format!("b/{path}")
    };
    let mut patch = format!("diff --git a/{path} b/{path}\n--- {old}\n+++ {new}\n");
    for hunk in &change.hunks {
        let from = &hunk.location.from_file;
        let to = &hunk.location.to_file;
        let start = |range: &gg_lib::messages::ChangeRange| unified_start(range.start, range.len);
        patch.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            start(from),
            from.len,
            start(to),
            to.len
        ));
        for line in &hunk.lines.lines {
            patch.push_str(line.trim_end_matches(['\n', '\r']));
            patch.push('\n');
        }
    }
    patch
}

/// Unified-diff start line for a 1-based GG range.
fn unified_start(start: usize, len: usize) -> usize {
    if len == 0 {
        start.saturating_sub(1)
    } else {
        start
    }
}

fn relative_time(timestamp: DateTime<Local>, now: DateTime<Local>) -> String {
    let delta = now.signed_duration_since(timestamp);
    let minutes = delta.num_minutes();
    match minutes {
        m if m < 1 => "just now".to_owned(),
        m if m < 60 => format!("{m}m ago"),
        m if m < 60 * 24 => format!("{}h ago", m / 60),
        m if m < 60 * 24 * 30 => format!("{}d ago", m / (60 * 24)),
        _ => timestamp.format("%Y-%m-%d").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::unified_start;

    /// Values taken from `jj diff --git` on the same commits.
    #[test]
    fn unified_start_matches_jj() {
        // Modified hunk at line 381: `@@ -381,3 +381,5 @@`
        assert_eq!(unified_start(381, 3), 381);
        // Added file: `@@ -0,0 +1,3 @@` (GG reports 1..1 then 1..4)
        assert_eq!(unified_start(1, 0), 0);
        assert_eq!(unified_start(1, 3), 1);
    }
}
