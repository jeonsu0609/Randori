//! Repository mutations: the jj operations Randori can perform.
//!
//! Every operation is one of GG's mutation types run on the worker. The result
//! drives the UI the same way GG does: `Updated` reloads the log and moves the
//! selection to the revision jj reports, precondition failures (an immutable
//! revision, a conflicting bookmark) become warnings, and `InputRequired`
//! asks for git credentials and retries.

use std::collections::HashMap;
use std::rc::Rc;

use gg_lib::messages::mutations::{
    AbandonRevisions, BackoutRevisions, CheckoutRevision, CopyChanges, CreateRef, CreateRevision,
    DeleteRef, DescribeRevision, DuplicateRevisions, ExternalDiff, ExternalResolve, FileHunks,
    GitFetch, GitPush, GitRefspec, MoveChanges, MoveHunk, MoveRef, MoveRevisions, MutationResult,
    RedoOperation, RenameBookmark, SplitRevision, StageChanges, TrackBookmark, UndoOperation,
    UnstageChanges, UntrackBookmark,
};
use gg_lib::messages::{
    ChangeHunk, InputRequest, InputResponse, RevHeader, RevId, RevSet, StoreRef, TreePath,
};
use gg_lib::worker::Mutation;
use gpui_kit::component::{
    WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    input::{Input, InputEvent, InputState},
    notification::Notification,
    v_flex,
};
use gpui_kit::{
    AppContext as _, Context, Entity, ParentElement as _, SharedString, Styled as _, Window, div,
};

use super::RepoView;
use super::model::{
    Side, partial_hunk, reverse_partial_hunk, selected_hunk_lines, selects_every_change,
};

type BoxedMutation = Box<dyn Mutation + Send + Sync>;
/// Rebuilds a mutation with the credentials the user entered.
type Retry = Rc<dyn Fn(InputResponse) -> BoxedMutation>;

impl RepoView {
    pub(super) fn selected_header(&self) -> Option<&RevHeader> {
        self.selected
            .and_then(|ix| self.rows.get(ix))
            .map(|row| &row.revision)
    }

    fn selected_set(&self) -> Option<RevSet> {
        self.selected_header().map(|header| RevSet {
            from: header.id.clone(),
            to: header.id.clone(),
        })
    }

    /// Runs a mutation on the worker and applies its result.
    pub(super) fn run_mutation(
        &mut self,
        label: impl Into<SharedString>,
        mutation: BoxedMutation,
        retry: Option<Retry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.blocked_by_pick(window, cx) {
            return;
        }
        let label = label.into();
        self.busy.push(label.clone());
        cx.notify();

        let worker = self.worker.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { worker.mutate(mutation) })
                .await;
            let _ = this.update_in(cx, |view, window, cx| {
                if let Some(ix) = view.busy.iter().position(|busy| *busy == label) {
                    view.busy.remove(ix);
                }
                if std::mem::take(&mut view.commit_pending)
                    && matches!(result, Ok(MutationResult::Updated { .. }))
                {
                    view.commit_input
                        .update(cx, |state, cx| state.set_value("", window, cx));
                }
                match result {
                    Ok(result) => view.apply_result(&label, result, retry, window, cx),
                    Err(err) => {
                        window.push_notification(
                            Notification::error(format!("{err:#}")).title(label.clone()),
                            cx,
                        );
                    }
                }
                // Several operations can run at once (one push per bookmark, one fetch
                // per remote); the log is read again once, after the last of them.
                if view.busy.is_empty() {
                    view.progress = None;
                    let reload = std::mem::take(&mut view.reload_after_ops);
                    if let Some(snapshot) = view.refresh_after_ops.take() {
                        // for the terminal's change: a snapshot only when
                        // Git alone moved, since jj snapshotted
                        view.refresh_from_disk(snapshot, cx);
                    } else if reload {
                        view.reload(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_result(
        &mut self,
        label: &SharedString,
        result: MutationResult,
        retry: Option<Retry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            MutationResult::Unchanged => {}
            MutationResult::Updated {
                new_status,
                new_selection,
            } => {
                self.apply_status(&new_status, cx);
                if let Some(header) = new_selection {
                    self.pending_select = Some(header.id.change.hex);
                }
                self.reload_after_ops = true;
            }
            MutationResult::Reconfigured { .. } => self.reload_after_ops = true,
            MutationResult::InputRequired { request } => match retry {
                Some(retry) => self.prompt_credentials(label.clone(), request, retry, window, cx),
                None => window.push_notification(
                    Notification::warning(request.detail).title(request.title),
                    cx,
                ),
            },
            MutationResult::PreconditionError { message } => {
                window.push_notification(Notification::warning(message).title(label.clone()), cx);
            }
            MutationResult::InternalError { message } => {
                window.push_notification(
                    Notification::error(message.lines.join("\n")).title(label.clone()),
                    cx,
                );
            }
        }
    }

    // ----- revisions ------------------------------------------------------------------------

    pub(super) fn new_child(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(set) = self.selected_set() {
            self.check_picked_up_after();
            self.run_mutation(
                "New change",
                Box::new(CreateRevision { set }),
                None,
                window,
                cx,
            );
        }
    }

    /// While a target revision is being chosen, other commands wait: the keys
    /// that move the selection would otherwise also be able to run them.
    pub(super) fn blocked_by_pick(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.pick.is_none() {
            return false;
        }
        window.push_notification(
            Notification::info("Press Enter to apply it or Esc to cancel.")
                .title("Finish choosing a revision first"),
            cx,
        );
        true
    }

    /// jj refuses to rewrite immutable history; say so before asking anything.
    /// Returns true when the selection is immutable.
    pub(super) fn refuse_if_immutable(
        &self,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match self.selected_header() {
            Some(header) => {
                self.refuse_rewrite(action, header.is_immutable, &header.id, window, cx)
            }
            None => false,
        }
    }

    /// Whether `action` is refused as revision `id` is `immutable`, with a
    /// note on how to allow it.
    fn refuse_rewrite(
        &self,
        action: &str,
        immutable: bool,
        id: &RevId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !immutable {
            return false;
        }
        let change = super::short_id(&id.change.prefix, &id.change.rest, 8);
        window.push_notification(
            Notification::warning(format!(
                "{change} is immutable. Change `immutable_heads()` in jj config to rewrite it."
            ))
            .title(action.to_owned()),
            cx,
        );
        true
    }

    pub(super) fn edit_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refuse_if_immutable("Edit", window, cx) {
            return;
        }
        if let Some(header) = self.selected_header() {
            let id = header.id.clone();
            self.check_picked_up_after();
            self.run_mutation("Edit", Box::new(CheckoutRevision { id }), None, window, cx);
        }
    }

    pub(super) fn duplicate_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(set) = self.selected_set() {
            self.run_mutation(
                "Duplicate",
                Box::new(DuplicateRevisions { set }),
                None,
                window,
                cx,
            );
        }
    }

    pub(super) fn backout_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(set) = self.selected_set() {
            self.run_mutation(
                "Back out",
                Box::new(BackoutRevisions { set }),
                None,
                window,
                cx,
            );
        }
    }

    /// Abandon is destructive, so it asks first.
    pub(super) fn confirm_abandon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked_by_pick(window, cx) {
            return;
        }
        if self.refuse_if_immutable("Abandon", window, cx) {
            return;
        }
        let Some(header) = self.selected_header() else {
            return;
        };
        let change = super::short_id(&header.id.change.prefix, &header.id.change.rest, 8);
        let summary = header
            .description
            .lines
            .first()
            .filter(|line| !line.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| "(no description set)".to_owned());
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            alert
                .title(format!("Abandon {change}?"))
                .description(format!(
                    "“{summary}” is removed and its children move onto its parents. Undo brings it back."
                ))
                .confirm()
                .ok_text("Abandon")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let _ = view.update(cx, |view, cx| view.abandon_selected(window, cx));
                    true
                })
        });
    }

    fn abandon_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(set) = self.selected_set() {
            self.run_mutation(
                "Abandon",
                Box::new(AbandonRevisions { set }),
                None,
                window,
                cx,
            );
        }
    }

    /// Saves the inspector's description editor to the selected revision.
    pub(super) fn save_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(header) = self.selected_header() else {
            return;
        };
        let id = header.id.clone();
        let new_description = self.description_input.read(cx).value().to_string();
        if new_description == header.description.lines.join("\n") {
            return;
        }
        self.run_mutation(
            "Describe",
            Box::new(DescribeRevision {
                id,
                new_description,
                reset_author: false,
            }),
            None,
            window,
            cx,
        );
    }

    /// Drag and drop: rebases `source` (alone, like `jj rebase -r`) onto the
    /// revision in row `target`.
    pub(super) fn rebase_onto(
        &mut self,
        source: RevId,
        target: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(parent) = self.rows.get(target).map(|row| row.revision.id.clone()) else {
            return;
        };
        if parent.commit == source.commit {
            window.push_notification(
                Notification::info("Choose a different revision as the new parent.")
                    .title("Can't rebase onto itself"),
                cx,
            );
            return;
        }
        let label = format!(
            "Rebase {} onto {}",
            super::short_id(&source.change.prefix, &source.change.rest, 8),
            super::short_id(&parent.change.prefix, &parent.change.rest, 8)
        );
        self.run_mutation(
            label,
            Box::new(MoveRevisions {
                set: RevSet {
                    from: source.clone(),
                    to: source,
                },
                parent_ids: vec![parent],
            }),
            None,
            window,
            cx,
        );
    }

    /// Drag and drop: points a bookmark or tag at the revision in row `target`.
    pub(super) fn move_ref_to(
        &mut self,
        r#ref: StoreRef,
        target: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(to_id) = self.rows.get(target).map(|row| row.revision.id.clone()) else {
            return;
        };
        let name = match &r#ref {
            StoreRef::LocalBookmark { bookmark_name, .. }
            | StoreRef::RemoteBookmark { bookmark_name, .. } => bookmark_name.clone(),
            StoreRef::Tag { tag_name } => tag_name.clone(),
        };
        self.run_mutation(
            format!("Move {name}"),
            Box::new(MoveRef { r#ref, to_id }),
            None,
            window,
            cx,
        );
    }

    // ----- files and hunks ------------------------------------------------------------------

    /// The revision shown in the inspector and its first parent.
    fn detail_ids(&self) -> Option<(RevId, RevId)> {
        let detail = self.detail.as_ref()?;
        let parent = detail.parent_ids.first()?.clone();
        Some((detail.header.id.clone(), parent))
    }

    fn selected_file_path(&self) -> Option<TreePath> {
        let detail = self.detail.as_ref()?;
        detail
            .files
            .get(self.selected_file?)
            .map(|file| file.tree_path.clone())
    }

    /// A click on file `ix` in the inspector: ⌘ adds it to the picked files
    /// or takes it out, ⇧ picks the files from the selected one to it, a
    /// plain click picks none. The diff shows the file clicked.
    pub(super) fn click_file(
        &mut self,
        ix: usize,
        modifiers: gpui_kit::Modifiers,
        cx: &mut Context<Self>,
    ) {
        let Some(files) = self.detail.as_ref().map(|detail| &detail.files) else {
            return;
        };
        let path = |ix: usize| files.get(ix).map(|file| file.path.clone());
        if modifiers.secondary(cx) {
            // the file shown so far is picked with the first one added
            if self.marked_files.is_empty()
                && let Some(shown) = self
                    .selected_file
                    .filter(|shown| *shown != ix)
                    .and_then(path)
            {
                self.marked_files.push(shown);
            }
            if let Some(clicked) = path(ix) {
                match self
                    .marked_files
                    .iter()
                    .position(|marked| *marked == clicked)
                {
                    Some(at) => {
                        self.marked_files.remove(at);
                    }
                    None => self.marked_files.push(clicked),
                }
            }
        } else if modifiers.shift {
            let anchor = self.selected_file.unwrap_or(ix);
            let (start, end) = (anchor.min(ix), anchor.max(ix));
            self.marked_files = (start..=end).filter_map(path).collect();
        } else {
            self.marked_files.clear();
        }
        self.select_file(Some(ix), cx);
    }

    /// The files Split takes: the picked ones in the list's order, else the
    /// selected one.
    fn split_paths(&self) -> Vec<TreePath> {
        let Some(detail) = self.detail.as_ref() else {
            return Vec::new();
        };
        if self.marked_files.is_empty() {
            return self.selected_file_path().into_iter().collect();
        }
        detail
            .files
            .iter()
            .filter(|file| self.marked_files.contains(&file.path))
            .map(|file| file.tree_path.clone())
            .collect()
    }

    /// Asks for a description, then moves the picked files' changes into a
    /// new revision before the inspected one (`jj split <paths>`).
    pub(super) fn prompt_split_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refuse_if_immutable("Split", window, cx) {
            return;
        }
        let Some((id, _)) = self.detail_ids() else {
            return;
        };
        let paths = self.split_paths();
        let what = match paths.as_slice() {
            [] => return,
            [one] => one.repo_path.clone(),
            many => format!("{} files", many.len()),
        };
        self.prompt_text(
            format!("Split {what} into a new commit"),
            "Description",
            String::new(),
            "Split",
            move |view, description, window, cx| {
                view.marked_files.clear();
                view.run_mutation(
                    format!("Split {what}"),
                    Box::new(SplitRevision {
                        id: id.clone(),
                        paths: paths.clone(),
                        hunks: Vec::new(),
                        description,
                    }),
                    None,
                    window,
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Moves the selected file's changes into the parent (`jj squash <path>`).
    pub(super) fn squash_file_into_parent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refuse_if_immutable("Squash", window, cx) {
            return;
        }
        let (Some((id, parent)), Some(path)) = (self.detail_ids(), self.selected_file_path())
        else {
            return;
        };
        let name = path.repo_path.clone();
        self.run_mutation(
            format!("Squash {name} into parent"),
            Box::new(MoveChanges {
                from: RevSet {
                    from: id.clone(),
                    to: id,
                },
                to_id: parent.commit,
                paths: vec![path],
            }),
            None,
            window,
            cx,
        );
    }

    /// Drag and drop: moves a file's changes from the inspected revision into
    /// the revision in row `target` (`jj squash --into <target> <path>`).
    pub(super) fn move_file_to(
        &mut self,
        path: TreePath,
        from: RevId,
        target: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(to) = self.rows.get(target).map(|row| row.revision.id.clone()) else {
            return;
        };
        if to.commit == from.commit {
            window.push_notification(
                Notification::info("The file is already in this revision.")
                    .title("Nothing to move"),
                cx,
            );
            return;
        }
        let label = format!(
            "Move {} into {}",
            path.repo_path,
            super::short_id(&to.change.prefix, &to.change.rest, 8)
        );
        self.run_mutation(
            label,
            Box::new(MoveChanges {
                from: RevSet {
                    from: from.clone(),
                    to: from,
                },
                to_id: to.commit,
                paths: vec![path],
            }),
            None,
            window,
            cx,
        );
    }

    /// Discards the selected file's changes by copying it back from the parent
    /// (`jj restore <path>`). Asks first: the edits are only reachable by undo.
    pub(super) fn confirm_restore_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked_by_pick(window, cx) {
            return;
        }
        if self.refuse_if_immutable("Discard", window, cx) {
            return;
        }
        let (Some((id, parent)), Some(path)) = (self.detail_ids(), self.selected_file_path())
        else {
            return;
        };
        let view = cx.entity().downgrade();
        let name = path.repo_path.clone();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            let (id, parent, path) = (id.clone(), parent.clone(), path.clone());
            alert
                .title(format!("Discard changes to {name}?"))
                .description("The file returns to its parent's version. Undo restores it.")
                .confirm()
                .ok_text("Discard")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let (id, parent, path) = (id.clone(), parent.clone(), path.clone());
                    let _ = view.update(cx, |view, cx| {
                        view.run_mutation(
                            format!("Discard {}", path.repo_path),
                            Box::new(CopyChanges {
                                from_id: parent.commit,
                                to_set: RevSet {
                                    from: id.clone(),
                                    to: id,
                                },
                                paths: vec![path],
                            }),
                            None,
                            window,
                            cx,
                        )
                    });
                    true
                })
        });
    }

    /// The file of the diff on screen: the file's full view when it is open,
    /// else the inspector's selected file.
    pub(super) fn shown_diff_file(&self) -> Option<ShownFile<'_>> {
        if self.center.file().is_some() {
            return self.focus_shown_file();
        }
        if let Some(staged) = self
            .staged_file
            .and_then(|ix| self.staged.as_ref()?.files.get(ix))
        {
            let staging = self.staged.as_ref()?;
            return Some(ShownFile {
                id: staging.header.id.clone(),
                parent: staging.parent_ids.first().cloned(),
                immutable: staging.header.is_immutable,
                path: staged.tree_path.clone(),
                diff_path: staged.path.clone(),
                hunks: &staged.hunks,
                staged: true,
            });
        }
        let detail = self.detail.as_ref()?;
        let file = detail.files.get(self.selected_file?)?;
        Some(ShownFile {
            id: detail.header.id.clone(),
            parent: detail.parent_ids.first().cloned(),
            immutable: detail.header.is_immutable,
            path: file.tree_path.clone(),
            diff_path: file.path.clone(),
            hunks: &file.hunks,
            staged: false,
        })
    }

    /// The lines selected in the diff on screen (see [`Self::shown_diff_file`]).
    pub(super) fn selected_lines(&self) -> Option<LineSelection> {
        let range = match self.center.file() {
            Some(focus) => focus.selection.as_ref()?,
            None => self.diff_selection.as_ref()?,
        };
        let file = self.shown_diff_file()?;
        if range.path().as_ref() != file.diff_path.as_ref() {
            return None;
        }
        file.lines(
            (side_of(range.side()), range.start()),
            (side_of(range.end_side()), range.end()),
        )
    }

    /// Forgets the line selection of both diffs, after a mutation took it.
    pub(super) fn clear_line_selection(&mut self) {
        self.diff_selection = None;
        self.keep_line_selection = false;
        if let Some(focus) = self.center.file_mut() {
            focus.selection = None;
        }
    }

    /// Moves the selected lines into the parent revision. They are to be in
    /// one hunk, which is what GG moves at a time.
    pub(super) fn squash_hunk_into_parent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(selection) = self.selected_lines() {
            self.squash_lines(selection, window, cx);
        }
    }

    /// Moves one changed line, `line` on `side` of the diff on screen, into
    /// the parent revision: the line's + in the diff, jj's way of staging it.
    pub(super) fn squash_line(
        &mut self,
        side: Side,
        line: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selection = self
            .shown_diff_file()
            .and_then(|file| file.lines((side, line), (side, line)));
        if let Some(selection) = selection {
            self.squash_lines(selection, window, cx);
        }
    }

    // ----- staging ----------------------------------------------------------------------------

    /// Stages changes of the working copy inspected: `paths` whole, and the
    /// lines of `hunks`.
    pub(super) fn stage(
        &mut self,
        what: String,
        paths: Vec<TreePath>,
        hunks: Vec<FileHunks>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.detail.as_ref().map(|detail| detail.header.id.clone()) else {
            return;
        };
        self.clear_line_selection();
        self.run_mutation(
            format!("Stage {what}"),
            Box::new(StageChanges { id, paths, hunks }),
            None,
            window,
            cx,
        );
    }

    /// Unstages changes back into the working copy inspected: `paths` whole,
    /// and `hunks` of the staging revision read backwards.
    fn unstage(
        &mut self,
        what: String,
        paths: Vec<TreePath>,
        hunks: Vec<FileHunks>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.detail.as_ref().map(|detail| detail.header.id.clone()) else {
            return;
        };
        self.clear_line_selection();
        self.run_mutation(
            format!("Unstage {what}"),
            Box::new(UnstageChanges { id, paths, hunks }),
            None,
            window,
            cx,
        );
    }

    pub(super) fn stage_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self.detail.as_ref().and_then(|detail| detail.files.get(ix)) else {
            return;
        };
        let (what, path) = (file.tree_path.repo_path.clone(), file.tree_path.clone());
        self.stage(what, vec![path], Vec::new(), window, cx);
    }

    pub(super) fn stage_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let paths: Vec<TreePath> = detail
            .files
            .iter()
            .map(|file| file.tree_path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        self.stage(
            format!("{} files", paths.len()),
            paths,
            Vec::new(),
            window,
            cx,
        );
    }

    pub(super) fn unstage_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self.staged.as_ref().and_then(|staged| staged.files.get(ix)) else {
            return;
        };
        let (what, path) = (file.tree_path.repo_path.clone(), file.tree_path.clone());
        self.unstage(what, vec![path], Vec::new(), window, cx);
    }

    pub(super) fn unstage_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(staged) = self.staged.as_ref() else {
            return;
        };
        let paths: Vec<TreePath> = staged
            .files
            .iter()
            .map(|file| file.tree_path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        self.unstage(
            format!("{} files", paths.len()),
            paths,
            Vec::new(),
            window,
            cx,
        );
    }

    /// Stages (or, in a staged file, unstages) the changes between `start`
    /// and `end` of the diff on screen: a line's + or −, or the lines
    /// dragged over.
    pub(super) fn stage_lines(
        &mut self,
        start: (Side, usize),
        end: (Side, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(file) = self.shown_diff_file() else {
            return;
        };
        let path = file.path.clone();
        if file.staged {
            let Some((hunks, every)) = file.reversed_lines(start, end) else {
                return;
            };
            let what = format!("{} of {}", changed_lines(&hunks), path.repo_path);
            let (paths, hunks) = file_changes(path, hunks, every);
            self.unstage(what, paths, hunks, window, cx);
        } else {
            let Some(selection) = file.lines(start, end) else {
                return;
            };
            let what = format!("{} of {}", changed_lines(&selection.hunks), path.repo_path);
            let (paths, hunks) = file_changes(path, selection.hunks, selection.every);
            self.stage(what, paths, hunks, window, cx);
        }
    }

    /// The lines dragged over in the diff on screen, staged or unstaged.
    pub(super) fn stage_selected_lines(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let range = match self.center.file() {
            Some(focus) => focus.selection.clone(),
            None => self.diff_selection.clone(),
        };
        if let Some(range) = range {
            self.stage_lines(
                (side_of(range.side()), range.start()),
                (side_of(range.end_side()), range.end()),
                window,
                cx,
            );
        }
    }

    /// Commits with the message typed: what is staged, or, with nothing
    /// staged, the working copy (see `CommitChanges`).
    pub(super) fn commit_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.commit_input.read(cx).value().trim().to_owned();
        if message.is_empty() || !self.staging_mode() {
            return;
        }
        let Some(id) = self.detail.as_ref().map(|detail| detail.header.id.clone()) else {
            return;
        };
        let staged = self
            .staged
            .as_ref()
            .is_some_and(|staged| !staged.files.is_empty());
        let changed = self
            .detail
            .as_ref()
            .is_some_and(|detail| !detail.files.is_empty());
        if (!staged && !changed) || self.blocked_by_pick(window, cx) {
            return;
        }
        // the message is cleared once the commit is made, kept if it fails
        self.commit_pending = true;
        self.run_mutation(
            if staged {
                "Commit staged changes"
            } else {
                "Commit all changes"
            },
            Box::new(gg_lib::messages::mutations::CommitChanges {
                id,
                description: message,
            }),
            None,
            window,
            cx,
        );
    }

    fn squash_lines(
        &mut self,
        selection: LineSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let LineSelection {
            id,
            parent,
            immutable,
            path,
            mut hunks,
            ..
        } = selection;
        if self.refuse_rewrite("Squash lines", immutable, &id, window, cx) {
            return;
        }
        let Some(parent) = parent else {
            return;
        };
        if hunks.len() != 1 {
            window.push_notification(
                Notification::info("Select lines in one hunk, or use Split off.")
                    .title("Lines in several hunks"),
                cx,
            );
            return;
        }
        let hunk = hunks.remove(0);
        let label = format!(
            "Squash {} of {} into parent",
            changed_lines(std::slice::from_ref(&hunk)),
            path.repo_path
        );
        self.clear_line_selection();
        self.run_mutation(
            label,
            Box::new(MoveHunk {
                from_id: id,
                to_id: parent.commit,
                path,
                hunk,
            }),
            None,
            window,
            cx,
        );
    }

    /// Asks for a description, then moves the selected lines into a new
    /// revision before the inspected one (`jj split -i`, those lines picked).
    pub(super) fn prompt_split_lines(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(LineSelection {
            id,
            immutable,
            path,
            hunks,
            every,
            ..
        }) = self.selected_lines()
        else {
            return;
        };
        if self.refuse_rewrite("Split", immutable, &id, window, cx) {
            return;
        }
        let what = format!("{} of {}", changed_lines(&hunks), path.repo_path);
        let (paths, hunks) = file_changes(path, hunks, every);
        self.prompt_text(
            format!("Split {what} into a new commit"),
            "Description",
            String::new(),
            "Split",
            move |view, description, window, cx| {
                view.clear_line_selection();
                view.run_mutation(
                    format!("Split {what}"),
                    Box::new(SplitRevision {
                        id: id.clone(),
                        paths: paths.clone(),
                        hunks: hunks.clone(),
                        description,
                    }),
                    None,
                    window,
                    cx,
                );
            },
            window,
            cx,
        );
    }

    /// Opens the selected file in the configured external merge tool.
    pub(super) fn resolve_in_tool(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some((id, _)), Some(path)) = (self.detail_ids(), self.selected_file_path()) else {
            return;
        };
        if !self.workspace.as_ref().is_some_and(|ws| ws.has_merge_tool) {
            window.push_notification(
                Notification::info(
                    "Set `ui.merge-editor` in jj config to resolve conflicts in a tool.",
                )
                .title("No merge tool"),
                cx,
            );
            return;
        }
        let label = format!("Resolve {}", path.repo_path);
        self.run_mutation(
            label,
            Box::new(ExternalResolve { id, path }),
            None,
            window,
            cx,
        );
    }

    /// Opens the selected file's change in the configured external diff tool.
    pub(super) fn diff_in_tool(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some((id, _)), Some(path)) = (self.detail_ids(), self.selected_file_path()) else {
            return;
        };
        if !self.workspace.as_ref().is_some_and(|ws| ws.has_diff_tool) {
            window.push_notification(
                Notification::info(
                    "Set `ui.diff-formatter` in jj config to open changes in a tool.",
                )
                .title("No diff tool"),
                cx,
            );
            return;
        }
        let label = format!("Open {} in diff tool", path.repo_path);
        self.run_mutation(label, Box::new(ExternalDiff { id, path }), None, window, cx);
    }

    // ----- bookmarks ------------------------------------------------------------------------

    pub(super) fn prompt_create_bookmark(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(header) = self.selected_header() else {
            return;
        };
        let id = header.id.clone();
        let change = super::short_id(&header.id.change.prefix, &header.id.change.rest, 8);
        self.prompt_text(
            format!("New bookmark on {change}"),
            "Bookmark name",
            String::new(),
            "Create",
            move |view, name, window, cx| {
                let r#ref = StoreRef::LocalBookmark {
                    bookmark_name: name,
                    has_conflict: false,
                    is_synced: false,
                    tracking_remotes: vec![],
                    available_remotes: 0,
                    potential_remotes: 0,
                };
                view.run_mutation(
                    "Create bookmark",
                    Box::new(CreateRef {
                        id: id.clone(),
                        r#ref,
                    }),
                    None,
                    window,
                    cx,
                );
            },
            window,
            cx,
        );
    }

    pub(super) fn prompt_rename_bookmark(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r#ref) = self.ref_target.clone() else {
            return;
        };
        let StoreRef::LocalBookmark { bookmark_name, .. } = &r#ref else {
            return;
        };
        let bookmark_name = bookmark_name.clone();
        self.prompt_text(
            format!("Rename {bookmark_name}"),
            "New name",
            bookmark_name.clone(),
            "Rename",
            move |view, new_name, window, cx| {
                view.run_mutation(
                    "Rename bookmark",
                    Box::new(RenameBookmark {
                        r#ref: r#ref.clone(),
                        new_name,
                    }),
                    None,
                    window,
                    cx,
                );
            },
            window,
            cx,
        );
    }

    pub(super) fn delete_ref_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(r#ref) = self.ref_target.clone() {
            self.run_mutation("Delete", Box::new(DeleteRef { r#ref }), None, window, cx);
        }
    }

    pub(super) fn toggle_track_ref_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.ref_target.clone() {
            Some(
                r#ref @ StoreRef::RemoteBookmark {
                    is_tracked: true, ..
                },
            ) => self.run_mutation(
                "Untrack",
                Box::new(UntrackBookmark { r#ref }),
                None,
                window,
                cx,
            ),
            Some(r#ref @ StoreRef::RemoteBookmark { .. }) => {
                self.run_mutation("Track", Box::new(TrackBookmark { r#ref }), None, window, cx)
            }
            _ => {}
        }
    }

    /// Pushes one local bookmark to every remote it tracks (or the default remote).
    pub(super) fn push_ref_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(r#ref @ StoreRef::LocalBookmark { .. }) = self.ref_target.clone() {
            self.push_bookmarks(vec![r#ref], window, cx);
        }
    }

    /// Pushes the local bookmarks that point at the selected revision.
    pub(super) fn push_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(header) = self.selected_header() else {
            return;
        };
        let bookmarks: Vec<StoreRef> = header
            .refs
            .iter()
            .filter(|r| matches!(r, StoreRef::LocalBookmark { .. }))
            .cloned()
            .collect();
        if bookmarks.is_empty() {
            window.push_notification(
                Notification::info("Create a bookmark on this revision to push it.")
                    .title("Nothing to push"),
                cx,
            );
            return;
        }
        self.push_bookmarks(bookmarks, window, cx);
    }

    fn push_bookmarks(
        &mut self,
        bookmarks: Vec<StoreRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remotes = self
            .workspace
            .as_ref()
            .map(|ws| ws.remotes.clone())
            .unwrap_or_default();
        for bookmark_ref in bookmarks {
            let name = bookmark_ref.as_bookmark().unwrap_or_default().to_owned();
            // A tracked bookmark goes to the remotes it tracks. An untracked one goes
            // to `origin` (or the only remote), which also starts tracking it.
            let tracked = matches!(
                &bookmark_ref,
                StoreRef::LocalBookmark { tracking_remotes, .. } if !tracking_remotes.is_empty()
            );
            let target = if tracked {
                None
            } else {
                default_remote(&remotes)
            };
            if !tracked && target.is_none() {
                window.push_notification(
                    Notification::info("Add a git remote to push this bookmark.")
                        .title(format!("Push {name}")),
                    cx,
                );
                continue;
            }
            let refspec = move |bookmark_ref: StoreRef| match &target {
                Some(remote_name) => GitRefspec::RemoteBookmark {
                    remote_name: remote_name.clone(),
                    bookmark_ref,
                },
                None => GitRefspec::AllRemotes { bookmark_ref },
            };
            let label = match refspec(bookmark_ref.clone()) {
                GitRefspec::RemoteBookmark { remote_name, .. } => {
                    format!("Push {name} to {remote_name}")
                }
                _ => format!("Push {name}"),
            };
            let refspec = Rc::new(refspec);
            let retry_ref = bookmark_ref.clone();
            let retry_spec = refspec.clone();
            let retry: Retry = Rc::new(move |input| {
                Box::new(GitPush {
                    refspec: retry_spec(retry_ref.clone()),
                    input: Some(input),
                })
            });
            self.run_mutation(
                label,
                Box::new(GitPush {
                    refspec: refspec(bookmark_ref),
                    input: None,
                }),
                Some(retry),
                window,
                cx,
            );
        }
    }

    /// Settings' "Fetch automatically": while it is on, a look every minute
    /// whether the interval has passed since the last automatic fetch; while
    /// it is off, nothing runs.
    pub(super) fn follow_auto_fetch(&mut self, cx: &mut Context<Self>) {
        let on = crate::settings::get(cx).display.auto_fetch_mins > 0;
        if on == self.auto_fetch.is_some() {
            return;
        }
        if !on {
            self.auto_fetch = None;
            return;
        }
        self.auto_fetch = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(60))
                    .await;
                if this
                    .update(cx, |view, cx| view.auto_fetch_if_due(cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// Fetches every remote quietly when due: no progress, no password
    /// prompt, and a failure only logged, since nobody asked for it. Skipped
    /// while the tab's own commands run.
    fn auto_fetch_if_due(&mut self, cx: &mut Context<Self>) {
        let mins = crate::settings::get(cx).display.auto_fetch_mins;
        let due = std::time::Duration::from_secs(u64::from(mins) * 60);
        if mins == 0 || !self.busy.is_empty() || self.last_auto_fetch.elapsed() < due {
            return;
        }
        let remotes = self
            .workspace
            .as_ref()
            .map(|ws| ws.remotes.clone())
            .unwrap_or_default();
        if remotes.is_empty() {
            return;
        }
        self.last_auto_fetch = std::time::Instant::now();
        let worker = self.worker.clone();
        cx.spawn(async move |this, cx| {
            let status = cx
                .background_spawn(async move {
                    let mut status = None;
                    for remote_name in remotes {
                        let fetch = GitFetch {
                            refspec: GitRefspec::AllBookmarks {
                                remote_name: remote_name.clone(),
                            },
                            input: None,
                        };
                        match worker.mutate(Box::new(fetch)) {
                            Ok(MutationResult::Updated { new_status, .. }) => {
                                status = Some(new_status)
                            }
                            Ok(MutationResult::Unchanged) => {}
                            Ok(_) => log::info!("automatic fetch of {remote_name}: skipped"),
                            Err(err) => log::info!("automatic fetch of {remote_name}: {err:#}"),
                        }
                    }
                    status
                })
                .await;
            if let Some(status) = status {
                let _ = this.update(cx, |view, cx| {
                    view.apply_status(&status, cx);
                    view.reload(cx);
                });
            }
        })
        .detach();
    }

    /// Fetches every bookmark from every git remote of the workspace.
    pub(super) fn fetch_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let remotes = self
            .workspace
            .as_ref()
            .map(|ws| ws.remotes.clone())
            .unwrap_or_default();
        if remotes.is_empty() {
            window.push_notification(
                Notification::info("This workspace has no git remotes.").title("Nothing to fetch"),
                cx,
            );
            return;
        }
        for remote_name in remotes {
            let retry_remote = remote_name.clone();
            let retry: Retry = Rc::new(move |input| {
                Box::new(GitFetch {
                    refspec: GitRefspec::AllBookmarks {
                        remote_name: retry_remote.clone(),
                    },
                    input: Some(input),
                })
            });
            self.run_mutation(
                format!("Fetch {remote_name}"),
                Box::new(GitFetch {
                    refspec: GitRefspec::AllBookmarks { remote_name },
                    input: None,
                }),
                Some(retry),
                window,
                cx,
            );
        }
    }

    // ----- operation log --------------------------------------------------------------------

    pub(super) fn undo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_mutation("Undo", Box::new(UndoOperation), None, window, cx);
    }

    pub(super) fn redo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_mutation("Redo", Box::new(RedoOperation), None, window, cx);
    }

    // ----- dialogs --------------------------------------------------------------------------

    /// A one-field dialog. `submit` runs with the trimmed, non-empty value.
    #[allow(clippy::too_many_arguments)]
    fn prompt_text(
        &mut self,
        title: String,
        label: &'static str,
        initial: String,
        ok: &'static str,
        submit: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.blocked_by_pick(window, cx) {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
        let view = cx.entity().downgrade();
        let submit = Rc::new(submit);
        let input_handle = input.clone();
        // Enter reaches both the field and the dialog's OK; one submit only
        let submitted = Rc::new(std::cell::Cell::new(false));

        // Enter submits, like the dialog's primary button.
        self.dialog_subscription = Some(cx.subscribe_in(&input, window, {
            let submit = submit.clone();
            let submitted = submitted.clone();
            move |view: &mut Self, state, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let value = state.read(cx).value().trim().to_owned();
                    if !value.is_empty() && !submitted.replace(true) {
                        window.close_dialog(cx);
                        submit(view, value, window, cx);
                    }
                }
            }
        }));
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            let input_for_ok = input.clone();
            let submit = submit.clone();
            let submitted = submitted.clone();
            dialog
                .title(title.clone())
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(label))
                        .child(Input::new(&input)),
                )
                .footer(
                    DialogFooter::new()
                        .gap_2()
                        .child(
                            DialogClose::new()
                                .child(Button::new("cancel").label("Cancel").outline()),
                        )
                        .child(DialogAction::new().child(Button::new("ok").label(ok).primary())),
                )
                .on_ok(move |_, window, cx| {
                    let value = input_for_ok.read(cx).value().trim().to_owned();
                    if value.is_empty() {
                        return false;
                    }
                    if submitted.replace(true) {
                        return true;
                    }
                    let submit = submit.clone();
                    let _ = view.update(cx, |view, cx| submit(view, value, window, cx));
                    true
                })
        });
        // The dialog takes focus when it opens; hand it to the field afterwards.
        let field = input_handle;
        window.defer(cx, move |window, cx| {
            field.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    /// Asks for the fields GG reports for a git operation, then retries it.
    fn prompt_credentials(
        &mut self,
        label: SharedString,
        request: InputRequest,
        retry: Retry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fields: Vec<(String, Entity<InputState>)> = request
            .fields
            .iter()
            .map(|field| {
                let secret = field.label.to_lowercase().contains("pass")
                    || field.label.to_lowercase().contains("token");
                let state = cx.new(|cx| InputState::new(window, cx).masked(secret));
                (field.label.clone(), state)
            })
            .collect();
        if let Some((_, first)) = fields.first() {
            let first = first.clone();
            window.defer(cx, move |window, cx| {
                first.update(cx, |state, cx| state.focus(window, cx));
            });
        }
        let view = cx.entity().downgrade();
        let title = request.title.clone();
        let detail = request.detail.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            let retry = retry.clone();
            let label = label.clone();
            let fields_for_ok = fields.clone();
            dialog
                .title(title.clone())
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_sm().child(detail.clone()))
                        .children(fields.iter().map(|(name, state)| {
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child(name.clone()))
                                .child(Input::new(state))
                        })),
                )
                .footer(
                    DialogFooter::new()
                        .gap_2()
                        .child(
                            DialogClose::new()
                                .child(Button::new("cancel").label("Cancel").outline()),
                        )
                        .child(
                            DialogAction::new()
                                .child(Button::new("ok").label("Continue").primary()),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    let values: HashMap<String, String> = fields_for_ok
                        .iter()
                        .map(|(name, state)| (name.clone(), state.read(cx).value().to_string()))
                        .collect();
                    let mutation = retry(InputResponse { fields: values });
                    let label = label.clone();
                    let _ = view.update(cx, |view, cx| {
                        view.run_mutation(label, mutation, None, window, cx)
                    });
                    true
                })
        });
    }
}

/// `origin` if the workspace has it, otherwise the first remote.
fn default_remote(remotes: &[String]) -> Option<String> {
    remotes
        .iter()
        .find(|remote| remote.as_str() == "origin")
        .or_else(|| remotes.first())
        .cloned()
}

/// Lines selected in a diff, as Squash into parent and Split off take them.
pub(super) struct LineSelection {
    pub(super) id: RevId,
    /// The revision lines are squashed into, when there is one.
    pub(super) parent: Option<RevId>,
    pub(super) immutable: bool,
    pub(super) path: TreePath,
    /// The hunks the selection touches, each with only its selected changes.
    pub(super) hunks: Vec<ChangeHunk>,
    /// The selection holds every change of the file.
    pub(super) every: bool,
}

/// A file of a diff on screen, with what its line commands need.
pub(super) struct ShownFile<'a> {
    pub(super) id: RevId,
    /// The revision lines are squashed into, when there is one.
    pub(super) parent: Option<RevId>,
    pub(super) immutable: bool,
    pub(super) path: TreePath,
    /// The file's path in the diff, which its line positions carry.
    pub(super) diff_path: SharedString,
    pub(super) hunks: &'a [ChangeHunk],
    /// A file of the staging revision, shown as staged.
    pub(super) staged: bool,
}

impl ShownFile<'_> {
    /// The changes between `start` and `end` (a side and a line number), as
    /// the diff highlights them; `None` when they hold no change.
    pub(super) fn lines(&self, start: (Side, usize), end: (Side, usize)) -> Option<LineSelection> {
        let picked = selected_hunk_lines(self.hunks, start, end);
        let every = selects_every_change(self.hunks, &picked);
        let hunks: Vec<ChangeHunk> = self
            .hunks
            .iter()
            .zip(&picked)
            .filter_map(|(hunk, picked)| partial_hunk(hunk, picked))
            .collect();
        (!hunks.is_empty()).then(|| LineSelection {
            id: self.id.clone(),
            parent: self.parent.clone(),
            immutable: self.immutable,
            path: self.path.clone(),
            hunks,
            every,
        })
    }

    /// The changes between `start` and `end` read backwards (see
    /// [`reverse_partial_hunk`]), to unstage them, and whether they are all
    /// of the file's.
    pub(super) fn reversed_lines(
        &self,
        start: (Side, usize),
        end: (Side, usize),
    ) -> Option<(Vec<ChangeHunk>, bool)> {
        let picked = selected_hunk_lines(self.hunks, start, end);
        let every = selects_every_change(self.hunks, &picked);
        let hunks: Vec<ChangeHunk> = self
            .hunks
            .iter()
            .zip(&picked)
            .filter_map(|(hunk, picked)| reverse_partial_hunk(hunk, picked))
            .collect();
        (!hunks.is_empty()).then_some((hunks, every))
    }
}

/// gpui-kit's side of a diff as the line helpers name it.
pub(super) fn side_of(side: gpui_kit::component::diff::DiffSide) -> Side {
    match side {
        gpui_kit::component::diff::DiffSide::Original => Side::Original,
        gpui_kit::component::diff::DiffSide::Modified => Side::Modified,
    }
}

/// The changes of `path` a command takes: the file whole when they are
/// `every` change of it, so an added or deleted one goes as it is, else its
/// `hunks`.
fn file_changes(
    path: TreePath,
    hunks: Vec<ChangeHunk>,
    every: bool,
) -> (Vec<TreePath>, Vec<FileHunks>) {
    if every {
        (vec![path], Vec::new())
    } else {
        (Vec::new(), vec![FileHunks { path, hunks }])
    }
}

/// "3 lines", counting the changed lines of `hunks`.
fn changed_lines(hunks: &[ChangeHunk]) -> String {
    let n = hunks
        .iter()
        .flat_map(|hunk| &hunk.lines.lines)
        .filter(|line| line.starts_with('-') || line.starts_with('+'))
        .count();
    if n == 1 {
        "1 line".to_owned()
    } else {
        format!("{n} lines")
    }
}
