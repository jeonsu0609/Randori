//! The file lists of the inspector and of a comparison: as paths or as a tree
//! of folders (`DisplaySettings::file_tree`), and in the inspector with every
//! file of the revision (`DisplaySettings::all_files`), filtered by a field.
//! The rows come from `file_list` and are drawn by a `uniform_list`, so only
//! the rows in view are drawn: with all files a revision has thousands.
//!
//! A file the revision does not change shows its text in the changes panel
//! (`UnchangedFile`); the commands on a file act only on changed ones, so it
//! is not the inspector's `file`.

use std::ops::Range;

use gg_lib::messages::{ChangeCounts, DisplayPath, queries::FileContent};
use gpui_kit::component::input::Input;

use super::file_list::{FileListSpec, FileRow, RowsKey, file_rows};
use super::included::Inclusion;
use super::inspector::{file_label, file_summary, inclusion_box};
use super::*;
use crate::harness_mark::HarnessMark as _;

/// Every file of the inspected revision, while Settings shows them all.
#[derive(Default)]
pub(super) struct AllFiles {
    /// The commit they are of.
    commit: String,
    pub(super) paths: Option<Rc<Vec<String>>>,
    pub(super) loading: bool,
    task: Option<Task<()>>,
}

/// The folders opened or closed by hand, by path; the others are as `all`
/// says (Expand all, Collapse all), else open with the changed files alone
/// and closed with all of them. `generation` counts the changes, for the
/// rows built from them.
/// Path or Tree and All files, as a tab last followed them, to tell what a
/// change of Settings turned.
#[derive(Clone, Copy)]
pub(super) struct FileListSeen {
    tree: bool,
    all: bool,
}

impl FileListSeen {
    pub(super) fn of(cx: &App) -> Self {
        let display = &settings::get(cx).display;
        Self {
            tree: display.file_tree,
            all: display.all_files,
        }
    }
}

#[derive(Default)]
pub(super) struct Folders {
    open: HashMap<String, bool>,
    all: Option<bool>,
    generation: u64,
}

/// A file of the inspected revision that it does not change, shown.
pub(super) struct UnchangedFile {
    pub(super) path: SharedString,
    /// Why there is no text (binary, not in the revision, the query failed).
    pub(super) note: Option<SharedString>,
    pub(super) loading: bool,
    _task: Task<()>,
}

/// A row a step of ↑↓ can land on: a file, or a folder of a tree.
#[derive(Clone, PartialEq)]
enum Stop {
    Changed(usize),
    Unchanged(SharedString),
    Folder(SharedString),
}

/// The id prefix of the inspector's folder rows; a comparison's are
/// `compared-folder`.
const INSPECTOR_FOLDER: &str = "folder";

/// Left padding of a row at `depth` in a tree.
pub(super) fn indent(depth: usize) -> gpui_kit::Pixels {
    gpui_kit::px(16. + depth as f32 * 14.)
}

impl RepoView {
    fn file_folder_open(&self) -> impl Fn(&str, bool) -> bool + '_ {
        move |path, default| {
            self.folders
                .open
                .get(path)
                .copied()
                .unwrap_or(self.folders.all.unwrap_or(default))
        }
    }

    /// Whether the inspector lists every file of the revision: as Settings
    /// says, except for this tab's working copy, which is the next commit
    /// and lists what it changes.
    pub(super) fn listing_all_files(&self, cx: &App) -> bool {
        settings::get(cx).display.all_files
            && self
                .inspected
                .detail
                .as_ref()
                .is_some_and(|detail| !is_this_working_copy(&detail.header))
    }

    /// The rows of the inspector's file list.
    pub(super) fn inspector_rows(&self, cx: &App) -> Rc<Vec<FileRow>> {
        let Some(detail) = self.inspected.detail.as_ref() else {
            return Rc::default();
        };
        let display = &settings::get(cx).display;
        let tree = display.file_tree;
        let listing = self.listing_all_files(cx);
        let all = listing
            .then(|| self.all_files.paths.clone())
            .flatten()
            .filter(|_| self.all_files.commit == detail.header.id.commit.hex);
        let filter = if listing {
            self.file_filter.read(cx).value().to_string()
        } else {
            String::new()
        };
        let key = RowsKey {
            files: detail.serial as usize,
            all: all.as_ref().map(|paths| Rc::as_ptr(paths) as usize),
            tree,
            filter: filter.clone(),
            folders: self.folders.generation,
        };
        self.inspector_rows.borrow_mut().get(key, || {
            let spec = FileListSpec {
                tree,
                all: all.as_ref().map(|paths| paths.as_slice()),
                filter: &filter,
            };
            file_rows(&detail.files, &spec, &self.file_folder_open())
        })
    }

    /// The rows of a comparison's file list: its changed files, as paths or
    /// a tree.
    pub(super) fn comparison_rows(&self, cx: &App) -> Rc<Vec<FileRow>> {
        let Some(detail) = self.compare.as_ref().and_then(|c| c.detail.as_ref()) else {
            return Rc::default();
        };
        let tree = settings::get(cx).display.file_tree;
        let key = RowsKey {
            files: detail.serial as usize,
            all: None,
            tree,
            filter: String::new(),
            folders: self.folders.generation,
        };
        self.comparison_rows.borrow_mut().get(key, || {
            let spec = FileListSpec {
                tree,
                all: None,
                filter: "",
            };
            file_rows(&detail.files, &spec, &self.file_folder_open())
        })
    }

    pub(super) fn toggle_file_folder(
        &mut self,
        path: SharedString,
        open: bool,
        cx: &mut Context<Self>,
    ) {
        self.folders.open.insert(path.to_string(), !open);
        self.folders.generation += 1;
        cx.notify();
    }

    /// Opens the folders on the way to `path` when all files are shown as a
    /// tree, where folders start closed, so the file shown is in the list.
    pub(super) fn reveal_file(&mut self, path: &str, cx: &App) {
        if !(settings::get(cx).display.file_tree && self.listing_all_files(cx)) {
            return;
        }
        let mut folder = String::new();
        let mut opened = false;
        for part in path
            .split('/')
            .collect::<Vec<_>>()
            .split_last()
            .map_or(&[][..], |(_, dirs)| dirs)
        {
            if !folder.is_empty() {
                folder.push('/');
            }
            folder.push_str(part);
            if self.folders.open.get(&folder) != Some(&true) {
                self.folders.open.insert(folder.clone(), true);
                opened = true;
            }
        }
        if opened {
            self.folders.generation += 1;
        }
    }

    /// Leaves the folder ↑↓ were on when the list, built again, no longer
    /// has it (its files discarded, say), the file shown taking the row.
    /// Not while all files are on their way: the list lacks their folders.
    pub(super) fn drop_lost_folder(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.inspected.folder.clone() else {
            return;
        };
        if self.listing_all_files(cx) && self.all_files.loading {
            return;
        }
        let there = self
            .inspector_rows(cx)
            .iter()
            .any(|row| matches!(row, FileRow::Folder { path, .. } if *path == folder));
        if !there {
            self.inspected.folder = None;
            cx.notify();
        }
    }

    /// Expand all (`true`) or Collapse all.
    pub(super) fn set_all_folders(&mut self, open: bool, cx: &mut Context<Self>) {
        // the folder ↑↓ were on can be in one closed now
        self.inspected.folder = None;
        self.folders.open.clear();
        self.folders.all = Some(open);
        self.folders.generation += 1;
        cx.notify();
    }

    /// Path or Tree, for every tab: each follows it (`follow_file_list`).
    pub(crate) fn set_file_tree(&mut self, tree: bool, cx: &mut Context<Self>) {
        settings::update_display(cx, |display| display.file_tree = tree);
    }

    /// All files, for every tab: each follows it (`follow_file_list`).
    pub(crate) fn set_all_files(&mut self, all: bool, cx: &mut Context<Self>) {
        settings::update_display(cx, |display| display.all_files = all);
    }

    /// Follows Path or Tree and All files turned in this or another tab.
    pub(super) fn follow_file_list(&mut self, cx: &mut Context<Self>) {
        let seen = std::mem::replace(&mut self.file_list_seen, FileListSeen::of(cx));
        let now = self.file_list_seen;
        if now.all != seen.all {
            // closed or open as the new list starts, not as the last one
            // was left
            self.folders.open.clear();
            self.folders.all = None;
            self.folders.generation += 1;
            cx.notify();
        }
        self.load_all_files(cx);
        if !seen.all && now.all && !self.all_files.loading {
            // the files of this revision came before: none to wait for
            self.settle_all_files(cx);
        }
        if seen.all && !now.all {
            // a folder of files it does not change goes with them
            self.drop_lost_folder(cx);
            if self.inspected.unchanged.is_some() {
                let first = self.first_changed(cx);
                self.select_file(first, cx);
            }
        }
        if now.tree != seen.tree {
            // a path list has no folders to be on
            self.inspected.folder = None;
            // the file shown stays in view, its folders opened
            if let Some(path) = self.shown_path() {
                self.reveal_file(&path, cx);
            }
            self.scroll_file_into_view(cx);
            self.scroll_compared_file_into_view(cx);
        }
    }

    /// Asks for every file of the inspected revision when Settings shows them
    /// all and they are not here yet.
    pub(super) fn load_all_files(&mut self, cx: &mut Context<Self>) {
        if !self.listing_all_files(cx) {
            return;
        }
        let Some(id) = self.inspected.detail.as_ref().map(|d| d.header.id.clone()) else {
            return;
        };
        if self.all_files.commit == id.commit.hex
            && (self.all_files.paths.is_some() || self.all_files.loading)
        {
            return;
        }
        let commit = id.commit.hex.clone();
        self.all_files.commit = commit.clone();
        self.all_files.loading = true;
        let worker = self.worker.clone();
        self.all_files.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { worker.query_tree_files(id) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.all_files.commit != commit {
                    return;
                }
                view.all_files.loading = false;
                view.all_files.paths = match result {
                    Ok(paths) => Some(Rc::new(paths)),
                    Err(err) => {
                        log::warn!("listing the revision's files failed: {err:#}");
                        None
                    }
                };
                view.settle_all_files(cx);
                cx.notify();
            });
        }));
    }

    /// The list of every file, here: the file shown in it, its folders
    /// opened, and ↑↓ off a folder it lacks.
    fn settle_all_files(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.shown_file_path() {
            self.reveal_file(&path, cx);
        }
        self.drop_lost_folder(cx);
        // the file shown can be far down a list of thousands
        self.scroll_file_into_view(cx);
    }

    /// The changed file shown, by path.
    fn shown_file_path(&self) -> Option<SharedString> {
        let detail = self.inspected.detail.as_ref()?;
        Some(detail.files.get(self.inspected.file?)?.path.clone())
    }

    /// The file shown, changed or not, by path.
    fn shown_path(&self) -> Option<SharedString> {
        match &self.inspected.unchanged {
            Some(unchanged) => Some(unchanged.path.clone()),
            None => self.shown_file_path(),
        }
    }

    /// The first changed file in the list's order.
    pub(super) fn first_changed(&self, cx: &App) -> Option<usize> {
        self.inspector_rows(cx).iter().find_map(|row| match row {
            FileRow::Changed { ix, .. } => Some(*ix),
            _ => None,
        })
    }

    /// The changed files from `a` to `b` as the list shows them (for ⇧):
    /// in a tree the order is not the detail's. Either not in the list (in
    /// a closed folder), the detail's order.
    pub(super) fn changed_between(&self, a: usize, b: usize, cx: &App) -> Vec<usize> {
        let rows = self.inspector_rows(cx);
        let order: Vec<usize> = rows
            .iter()
            .filter_map(|row| match row {
                FileRow::Changed { ix, .. } => Some(*ix),
                _ => None,
            })
            .collect();
        match (
            order.iter().position(|ix| *ix == a),
            order.iter().position(|ix| *ix == b),
        ) {
            (Some(p), Some(q)) => order[p.min(q)..=p.max(q)].to_vec(),
            _ => (a.min(b)..=a.max(b)).collect(),
        }
    }

    /// The rows ↑↓ go through: every row, folders too; with the full view
    /// open, the changed files alone (it shows a change).
    fn stops(&self, cx: &App) -> Vec<(usize, Stop)> {
        let changed_only = self.center.file().is_some();
        self.inspector_rows(cx)
            .iter()
            .enumerate()
            .filter_map(|(row, entry)| {
                let stop = match entry {
                    FileRow::Changed { ix, .. } => Stop::Changed(*ix),
                    _ if changed_only => return None,
                    FileRow::Unchanged { path, .. } => Stop::Unchanged(path.clone()),
                    FileRow::Folder { path, .. } => Stop::Folder(path.clone()),
                };
                Some((row, stop))
            })
            .collect()
    }

    fn current_stop(&self) -> Option<Stop> {
        // the full view goes through the changed files: from the one shown
        if let Some(folder) = self
            .inspected
            .folder
            .as_ref()
            .filter(|_| self.center.file().is_none())
        {
            return Some(Stop::Folder(folder.clone()));
        }
        if let Some(unchanged) = &self.inspected.unchanged {
            return Some(Stop::Unchanged(unchanged.path.clone()));
        }
        self.inspected.file.map(Stop::Changed)
    }

    /// Goes to `stop` at list row `row`: a file shows, a folder is only
    /// where ↑↓ are, the file shown staying.
    fn land_on(&mut self, row: usize, stop: Stop, cx: &mut Context<Self>) {
        self.inspected.folder = None;
        match stop {
            Stop::Changed(ix) => {
                self.select_file(Some(ix), cx);
                self.retarget_focus(cx);
            }
            Stop::Unchanged(path) => self.select_unchanged(path, cx),
            Stop::Folder(path) => {
                self.inspected.folder = Some(path);
                cx.notify();
            }
        }
        self.file_rows.scroll_to_item(row, ScrollStrategy::Nearest);
    }

    /// The row ↑↓ are on, with its stop, where ← → and Enter act (see
    /// `tree_keys_act`).
    fn tree_cursor(&self, cx: &App) -> Option<(usize, Stop, Rc<Vec<FileRow>>)> {
        if !self.tree_keys_act(cx) {
            return None;
        }
        let current = self.current_stop()?;
        let (row, stop) = self
            .stops(cx)
            .into_iter()
            .find(|(_, stop)| *stop == current)?;
        Some((row, stop, self.inspector_rows(cx)))
    }

    /// Whether ← → and Enter act on folders: in a tree, the full view
    /// closed.
    fn tree_keys_act(&self, cx: &App) -> bool {
        self.file_tree(cx) && self.center.file().is_none()
    }

    /// ← in a tree: closes the open folder ↑↓ are on, else goes to the
    /// folder the row is in (or the file shown is in, out of the list in
    /// it). Whether it did either.
    pub(super) fn collapse_folder_key(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((at, _, rows)) = self.tree_cursor(cx) else {
            return self.go_to_hiding_folder(cx);
        };
        let depth = match &rows[at] {
            FileRow::Folder {
                path, open: true, ..
            } => {
                self.toggle_file_folder(path.clone(), true, cx);
                return true;
            }
            FileRow::Folder { depth, .. }
            | FileRow::Changed { depth, .. }
            | FileRow::Unchanged { depth, .. } => *depth,
        };
        let parent = rows[..at]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(row, entry)| match entry {
                FileRow::Folder { path, depth: d, .. } if *d < depth => Some((row, path.clone())),
                _ => None,
            });
        match parent {
            Some((row, path)) => {
                self.land_on(row, Stop::Folder(path), cx);
                true
            }
            None => false,
        }
    }

    /// ← from the file shown out of the list: to the folder it is in, as
    /// ↑ (see `hiding_folder`). Whether there is one.
    fn go_to_hiding_folder(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.tree_keys_act(cx) {
            return false;
        }
        let Some((row, path)) = self
            .current_stop()
            .and_then(|stop| self.hiding_folder(&stop, cx))
        else {
            return false;
        };
        self.land_on(row, Stop::Folder(path), cx);
        true
    }

    /// → in a tree: opens the closed folder ↑↓ are on, else goes to the
    /// first row in the open one. Whether it did either.
    pub(super) fn expand_folder_key(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((at, _, rows)) = self.tree_cursor(cx) else {
            return false;
        };
        let FileRow::Folder {
            path, open, depth, ..
        } = &rows[at]
        else {
            return false;
        };
        if !*open {
            self.toggle_file_folder(path.clone(), false, cx);
            return true;
        }
        let depth = *depth;
        let inside = rows.get(at + 1).filter(|entry| match entry {
            FileRow::Folder { depth: d, .. }
            | FileRow::Changed { depth: d, .. }
            | FileRow::Unchanged { depth: d, .. } => *d > depth,
        });
        let Some(entry) = inside else {
            return false;
        };
        let stop = match entry {
            FileRow::Folder { path, .. } => Stop::Folder(path.clone()),
            FileRow::Changed { ix, .. } => Stop::Changed(*ix),
            FileRow::Unchanged { path, .. } => Stop::Unchanged(path.clone()),
        };
        self.land_on(at + 1, stop, cx);
        true
    }

    /// Enter on a folder of a tree opens or closes it. Whether ↑↓ were on one.
    fn toggle_folder_key(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((at, _, rows)) = self.tree_cursor(cx) else {
            return false;
        };
        let FileRow::Folder { path, open, .. } = &rows[at] else {
            return false;
        };
        self.toggle_file_folder(path.clone(), *open, cx);
        true
    }

    /// ↑↓ in the files, and the full view's next and previous file: the file
    /// `delta` rows on in the list (`isize::MIN`, `isize::MAX` for the first
    /// and the last), scrolled into view.
    pub(super) fn step_through_files(&mut self, delta: isize, cx: &mut Context<Self>) {
        let stops = self.stops(cx);
        if stops.is_empty() {
            return;
        }
        let last = stops.len() as isize - 1;
        let current = self.current_stop();
        let at = current
            .as_ref()
            .and_then(|stop| stops.iter().position(|(_, s)| s == stop));
        let hidden_in = match at {
            Some(_) => None,
            None => current
                .and_then(|stop| self.hiding_folder(&stop, cx))
                .map(|(row, _)| row),
        };
        let next = match (at, delta, hidden_in) {
            (Some(at), _, _) => (at as isize).saturating_add(delta).clamp(0, last),
            // Home and End go to the ends from anywhere
            (None, isize::MIN, _) => 0,
            (None, isize::MAX, _) => last,
            // the file shown out of the list is just below the row of the
            // folder it is in (see `hiding_folder`): ↑ goes to the folder
            // (the full view, to the file before it), ↓ to the row after it
            (None, delta, Some(folder)) => {
                let above = stops.iter().take_while(|(row, _)| *row <= folder).count() as isize;
                let next = if delta < 0 {
                    above + delta
                } else {
                    above + delta - 1
                };
                if !(0..=last).contains(&next) {
                    // nothing that way
                    return;
                }
                next
            }
            // nothing shown, or not under a folder of the list: ↑ from
            // below the list, ↓ from above it
            (None, delta, None) if delta < 0 => last,
            (None, _, None) => 0,
        } as usize;
        let (row, stop) = stops[next].clone();
        self.land_on(row, stop, cx);
    }

    /// The row and path of the folder `stop`, out of the list, is in: the
    /// deepest folder over it that the list has. Mostly a closed one; an
    /// open one when the filter left `stop` out but kept other files of it.
    fn hiding_folder(&self, stop: &Stop, cx: &App) -> Option<(usize, SharedString)> {
        let path = match stop {
            Stop::Changed(ix) => self.inspected.detail.as_ref()?.files.get(*ix)?.path.clone(),
            Stop::Unchanged(path) | Stop::Folder(path) => path.clone(),
        };
        self.inspector_rows(cx)
            .iter()
            .enumerate()
            .filter_map(|(row, entry)| match entry {
                FileRow::Folder { path: folder, .. }
                    if path
                        .strip_prefix(folder.as_ref())
                        .is_some_and(|rest| rest.starts_with('/')) =>
                {
                    Some((row, folder.clone()))
                }
                _ => None,
            })
            .max_by_key(|(_, folder)| folder.len())
    }

    /// Scrolls the file list to the file shown.
    pub(super) fn scroll_file_into_view(&mut self, cx: &mut Context<Self>) {
        let Some(stop) = self.current_stop() else {
            return;
        };
        if let Some((row, _)) = self.stops(cx).into_iter().find(|(_, s)| *s == stop) {
            self.file_rows.scroll_to_item(row, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    /// Scrolls a comparison's file list to the file it shows.
    fn scroll_compared_file_into_view(&mut self, cx: &mut Context<Self>) {
        let Some(file) = self.compare.as_ref().and_then(|compare| compare.file) else {
            return;
        };
        let row = self
            .comparison_rows(cx)
            .iter()
            .position(|row| matches!(row, FileRow::Changed { ix, .. } if *ix == file));
        if let Some(row) = row {
            self.compared_file_rows
                .scroll_to_item(row, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    /// A click on a folder of the inspector's tree: ↑↓ go on from it.
    fn click_inspector_folder(
        &mut self,
        path: SharedString,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.files_focus.focus(window, cx);
        self.inspected.folder = Some(path.clone());
        self.toggle_file_folder(path, open, cx);
    }

    /// A click on a file the revision does not change: its text in the
    /// changes panel. The full view shows a change, so it closes for it.
    pub(super) fn click_unchanged_row(
        &mut self,
        path: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_focus_to_files(window, cx);
        self.files_focus.focus(window, cx);
        self.inspected.folder = None;
        self.select_unchanged(path, cx);
    }

    /// Shows the text of `path`, a file the inspected revision has and does
    /// not change, in the changes panel.
    pub(super) fn select_unchanged(&mut self, path: SharedString, cx: &mut Context<Self>) {
        let Some(id) = self.inspected.detail.as_ref().map(|d| d.header.id.clone()) else {
            return;
        };
        self.inspected.file = None;
        self.inspected.marked.clear();
        self.line_selection = None;
        if self.compare.is_none() {
            self.diff.update(cx, |state, cx| state.set_files(None, cx));
        }
        let tree_path = TreePath {
            repo_path: path.to_string(),
            relative_path: DisplayPath(path.to_string()),
        };
        let worker = self.worker.clone();
        let asked = path.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { worker.query_file(id, tree_path) })
                .await;
            let _ = this.update(cx, |view, cx| {
                let Some(shown) = view
                    .inspected
                    .unchanged
                    .as_mut()
                    .filter(|shown| shown.path == asked)
                else {
                    return;
                };
                shown.loading = false;
                let file = match result {
                    Ok(FileContent { missing: true, .. }) => {
                        shown.note = Some("The file is not in this revision.".into());
                        None
                    }
                    Ok(FileContent { binary: true, .. }) => {
                        shown.note = Some("Binary file".into());
                        None
                    }
                    Ok(content) => Some(DiffFile::unchanged(asked.to_string(), &content.text)),
                    Err(err) => {
                        shown.note = Some(format!("{err:#}").into());
                        None
                    }
                };
                if view.compare.is_none() {
                    view.diff.update(cx, |state, cx| state.set_files(file, cx));
                }
                cx.notify();
            });
        });
        self.inspected.unchanged = Some(UnchangedFile {
            path,
            note: None,
            loading: true,
            _task: task,
        });
        cx.notify();
    }

    /// Path or Tree, All files and, in a tree, Expand all and Collapse all;
    /// `all` is offered in the inspector of a revision other than this tab's
    /// working copy, not in a comparison.
    pub(super) fn render_file_toolbar(&self, all: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let display = &settings::get(cx).display;
        let tree = display.file_tree;
        let all_files = display.all_files;
        h_flex()
            .h_8()
            .px_4()
            .gap_2()
            .flex_none()
            .child(
                TabBar::new("file-layout")
                    .segmented()
                    .xsmall()
                    .selected_index(usize::from(tree))
                    .on_click(
                        cx.listener(|view, ix: &usize, _, cx| view.set_file_tree(*ix == 1, cx)),
                    )
                    .child("Path")
                    .child("Tree"),
            )
            .when(tree, |this| {
                this.child(
                    Button::new("expand-folders")
                        .ghost()
                        .xsmall()
                        .icon(IconName::ChevronsUpDown)
                        .tooltip("Expand all")
                        .on_click(cx.listener(|view, _, _, cx| view.set_all_folders(true, cx))),
                )
                .child(
                    Button::new("collapse-folders")
                        .ghost()
                        .xsmall()
                        .icon(IconName::ChevronsDownUp)
                        .tooltip("Collapse all")
                        .on_click(cx.listener(|view, _, _, cx| view.set_all_folders(false, cx))),
                )
            })
            .child(div().flex_1())
            .when(all, |this| {
                this.child(
                    gpui_kit::component::checkbox::Checkbox::new("all-files")
                        .checked(all_files)
                        .label("All files")
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .on_click(cx.listener(|view, checked: &bool, _, cx| {
                            view.set_all_files(*checked, cx);
                            view.load_all_files(cx);
                        })),
                )
            })
            .into_any_element()
    }

    /// The inspector's files: the toolbar, the filter with all files, and
    /// the rows. ↑↓ go through the files once one is clicked, the diff
    /// following.
    pub(super) fn render_inspector_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.inspector_rows(cx);
        let all_files = self.listing_all_files(cx);
        let listing = all_files && self.all_files.loading;
        let offered = self
            .inspected
            .detail
            .as_ref()
            .is_some_and(|detail| !is_this_working_copy(&detail.header));
        v_flex()
            .flex_1()
            .min_h(gpui_kit::rems(10.))
            .child(self.render_file_toolbar(offered, cx))
            .when(all_files, |this| {
                this.child(
                    div().px_4().pb_2().flex_none().child(
                        Input::new(&self.file_filter)
                            .small()
                            .prefix(
                                gpui_kit::component::Icon::new(IconName::Search)
                                    .xsmall()
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .cleanable(true),
                    ),
                )
            })
            .when(listing, |this| {
                this.child(
                    h_flex()
                        .px_4()
                        .pb_2()
                        .gap_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(Spinner::new().xsmall())
                        .child("Listing the revision's files"),
                )
            })
            .child(
                div()
                    .id("files")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .track_focus(&self.files_focus)
                    .key_context(FILES_CONTEXT)
                    .on_action(cx.listener(|view, _: &actions::SelectPrevious, _, cx| {
                        view.step_file(-1, cx)
                    }))
                    .on_action(
                        cx.listener(|view, _: &actions::SelectNext, _, cx| view.step_file(1, cx)),
                    )
                    .on_action(cx.listener(|view, _: &actions::SelectFirst, _, cx| {
                        view.step_file(isize::MIN, cx)
                    }))
                    .on_action(cx.listener(|view, _: &actions::SelectLast, _, cx| {
                        view.step_file(isize::MAX, cx)
                    }))
                    .on_action(cx.listener(|view, _: &actions::CollapseFolder, _, cx| {
                        if !view.collapse_folder_key(cx) {
                            cx.propagate();
                        }
                    }))
                    .on_action(cx.listener(|view, _: &actions::ExpandFolder, _, cx| {
                        if !view.expand_folder_key(cx) {
                            cx.propagate();
                        }
                    }))
                    // Enter on a folder opens or closes it; on a file it
                    // opens the full view, as a double click
                    .on_action(
                        cx.listener(|view, _: &actions::ActivateFileRow, window, cx| {
                            if !view.toggle_folder_key(cx)
                                && !view.blocked_by_comparison(window, cx)
                            {
                                view.open_focus(FocusTab::Diff, false, window, cx)
                            }
                        }),
                    )
                    // Esc closes the full view first, a step at a time: then
                    // it goes back to the revisions (`FocusLog`)
                    .on_action(
                        cx.listener(|view, _: &actions::CloseFileFocus, window, cx| {
                            if view.center.file().is_some() {
                                view.close_focus_to_files(window, cx);
                            } else {
                                cx.propagate();
                            }
                        }),
                    )
                    .child(
                        uniform_list(
                            "file-rows",
                            rows.len(),
                            cx.processor(move |view, range: Range<usize>, _, cx| {
                                range
                                    .map(|row| view.render_inspector_row(&rows[row], row, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.file_rows)
                        .size_full(),
                    )
                    .child(Scrollbar::vertical(&self.file_rows)),
            )
            .into_any_element()
    }

    fn render_inspector_row(&self, row: &FileRow, at: usize, cx: &mut Context<Self>) -> AnyElement {
        match row {
            FileRow::Folder {
                path,
                name,
                depth,
                open,
                counts,
            } => self.render_file_folder_row(
                INSPECTOR_FOLDER,
                at,
                path,
                name,
                *depth,
                *open,
                *counts,
                cx,
            ),
            FileRow::Changed { ix, depth } => self.render_changed_row(*ix, *depth, cx),
            FileRow::Unchanged {
                path,
                name,
                dir,
                depth,
            } => {
                let selected = self.inspected.folder.is_none()
                    && self
                        .inspected
                        .unchanged
                        .as_ref()
                        .is_some_and(|shown| &shown.path == path);
                let theme = cx.theme();
                let clicked = path.clone();
                ListItem::new(("unchanged-file", at))
                    .harness_mark({
                        let path = path.clone();
                        move || format!("file:{path}")
                    })
                    .w_full()
                    .h_7()
                    .pl(indent(*depth))
                    .pr_4()
                    .py_0()
                    .map(|item| {
                        super::log_list::select_row(item, selected, self.files_active, theme)
                    })
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_3()
                            // the change letter's column, empty
                            .child(div().w_3().flex_none())
                            .child(file_label(
                                name,
                                dir,
                                !self.file_tree(cx),
                                theme.muted_foreground,
                                theme,
                            )),
                    )
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.click_unchanged_row(clicked.clone(), window, cx)
                    }))
                    .into_any_element()
            }
        }
    }

    /// A folder of a tree: a click opens or closes it. `list` keeps the
    /// inspector's ids apart from a comparison's; the inspector's is where
    /// ↑↓ can be, and in the working copy has a box for the files under it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_file_folder_row(
        &self,
        list: &'static str,
        at: usize,
        path: &SharedString,
        name: &SharedString,
        depth: usize,
        open: bool,
        counts: ChangeCounts,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let inspector = list == INSPECTOR_FOLDER;
        let selected = inspector && self.inspected.folder.as_ref() == Some(path);
        let inclusion = (inspector && self.commit_mode()).then(|| self.folder_inclusion(path));
        let theme = cx.theme();
        let toggled = path.clone();
        ListItem::new((list, at))
            .harness_mark({
                let path = path.clone();
                move || format!("folder:{path}")
            })
            .w_full()
            .h_7()
            .pl(indent(depth))
            .pr_4()
            .py_0()
            .map(|item| super::log_list::select_row(item, selected, self.files_active, theme))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .when_some(inclusion, |row, state| {
                        let folder = path.clone();
                        row.child(
                            div().mr_1().child(
                                inclusion_box(
                                    ("include-folder", at),
                                    state,
                                    match state {
                                        Inclusion::Nothing => "Include the folder in the commit",
                                        Inclusion::Some => {
                                            "Include every change of the folder in the commit"
                                        }
                                        Inclusion::All => "Leave the folder out of the commit",
                                    },
                                    theme,
                                )
                                .harness_mark({
                                    let path = path.clone();
                                    move || format!("include-folder:{path}")
                                })
                                .on_click(cx.listener(
                                    move |view, _, _, cx| {
                                        cx.stop_propagation();
                                        view.include_folder(&folder, state != Inclusion::All, cx)
                                    },
                                )),
                            ),
                        )
                    })
                    .child(
                        gpui_kit::component::Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall()
                        .text_color(theme.muted_foreground),
                    )
                    .child(
                        gpui_kit::component::Icon::new(if open {
                            IconName::FolderOpen
                        } else {
                            IconName::Folder
                        })
                        .xsmall()
                        .text_color(theme.muted_foreground),
                    )
                    .child(div().min_w_0().truncate().text_sm().child(name.clone()))
                    .child(div().flex_1())
                    .child(super::log_list::render_wip_counts(counts, cx)),
            )
            .on_click(cx.listener(move |view, _, window, cx| {
                if inspector {
                    view.click_inspector_folder(toggled.clone(), open, window, cx)
                } else {
                    view.toggle_file_folder(toggled.clone(), open, cx)
                }
            }))
            .into_any_element()
    }

    /// A changed file of the inspected revision: a click shows it (⌘ and ⇧
    /// pick files for Split, or tick boxes in the working copy), a double
    /// click opens its full view, the menu acts on it, and it is dragged
    /// onto a revision to move it there.
    fn render_changed_row(&self, ix: usize, depth: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(detail) = self.inspected.detail.as_ref() else {
            return div().into_any_element();
        };
        let Some(file) = detail.files.get(ix) else {
            return div().into_any_element();
        };
        let header = &detail.header;
        let commit_mode = self.commit_mode();
        let theme = cx.theme();
        let marked = self.inspected.marked.contains(&file.path);
        ListItem::new(("file", ix))
            .harness_mark(|| format!("file:{}", file.tree_path.repo_path))
            .w_full()
            .h_7()
            .pl(indent(depth))
            .pr_4()
            .py_0()
            .map(|item| {
                let selected =
                    (self.inspected.file == Some(ix) && self.inspected.folder.is_none()) || marked;
                super::log_list::select_row(item, selected, self.files_active, theme)
            })
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    // whether the commit includes the file, as GitHub
                    // Desktop's box beside each
                    .when(commit_mode, |row| {
                        let state = self
                            .next_commit
                            .included
                            .file(&file.tree_path.repo_path, &file.hunks);
                        row.child(
                            inclusion_box(
                                ("include-file", ix),
                                state,
                                match state {
                                    Inclusion::Nothing => "Include in the commit",
                                    Inclusion::Some => "Include every change in the commit",
                                    Inclusion::All => "Leave out of the commit",
                                },
                                theme,
                            )
                            .harness_mark(|| format!("include:{}", file.tree_path.repo_path))
                            .on_click(cx.listener(
                                move |view, _, _, cx| {
                                    cx.stop_propagation();
                                    view.include_file(ix, state != Inclusion::All, cx)
                                },
                            )),
                        )
                    })
                    // in a tree the folders say where it is
                    .child(file_summary(file, !self.file_tree(cx), theme)),
            )
            // a double click opens the file's full view, as in GitKraken;
            // ⌘ and ⇧ pick files for Split
            .on_click(
                cx.listener(move |view, event: &gpui_kit::ClickEvent, window, cx| {
                    view.files_focus.focus(window, cx);
                    view.inspected.folder = None;
                    view.click_file(ix, event.modifiers(), cx);
                    if event.modifiers().secondary(cx) || event.modifiers().shift {
                        return;
                    }
                    if view.center.file().is_some() {
                        // the full view follows the file picked here
                        view.retarget_focus(cx);
                    } else if event.click_count() >= 2 {
                        view.open_focus(FocusTab::Diff, false, window, cx);
                    }
                }),
            )
            .on_mouse_down(
                gpui_kit::MouseButton::Right,
                cx.listener(move |view, _, window, cx| {
                    view.inspected.folder = None;
                    // the menu acts on the picked files when this is one of them
                    if marked {
                        view.select_file(Some(ix), cx);
                    } else {
                        view.click_file(ix, gpui_kit::Modifiers::default(), cx);
                    }
                    view.command_focus().focus(window, cx);
                }),
            )
            .on_drag(
                DraggedFile {
                    path: file.tree_path.clone(),
                    from: header.id.clone(),
                    label: file.name.clone(),
                },
                |dragged, _, _, cx| {
                    cx.new(|_| DragChip {
                        label: format!("Move {}", dragged.label).into(),
                        icon: IconName::File,
                    })
                },
            )
            .context_menu({
                let focus = self.command_focus();
                let has_conflict = file.has_conflict;
                let immutable = header.is_immutable;
                // the working copy picks no files for Split (its boxes are
                // for Split off): the file clicked
                let picked = Some(if marked {
                    self.inspected.marked.len()
                } else {
                    1
                });
                let diff_tool = settings::get(cx).display.diff_app.is_some();
                let full = self.workspace.as_ref().map(|workspace| {
                    super::ops::full_path(&workspace.root, &file.tree_path.repo_path)
                });
                move |menu, _, _| {
                    // looked at as the menu opens, not as rows draw
                    let on_disk = full.as_ref().is_some_and(|path| path.exists());
                    file_menu(
                        menu,
                        has_conflict,
                        immutable,
                        picked,
                        diff_tool,
                        on_disk,
                        focus.clone(),
                    )
                }
            })
            .into_any_element()
    }

    pub(super) fn file_tree(&self, cx: &App) -> bool {
        settings::get(cx).display.file_tree
    }
}
