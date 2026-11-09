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
    DeleteRef, DescribeRevision, DuplicateRevisions, ExternalDiff, ExternalResolve, GitFetch,
    GitPush, GitRefspec, MoveChanges, MoveHunk, MoveRef, MoveRevisions, MutationResult,
    RedoOperation, RenameBookmark, TrackBookmark, UndoOperation, UntrackBookmark,
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
                    if std::mem::take(&mut view.reload_after_ops) {
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
                if let Some(workspace) = &mut self.workspace {
                    workspace.operation = new_status.operation_description.into();
                    workspace.can_redo = new_status.can_redo;
                }
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
        let Some(header) = self.selected_header().filter(|header| header.is_immutable) else {
            return false;
        };
        let change = super::short_id(&header.id.change.prefix, &header.id.change.rest, 8);
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
                .description("The file goes back to its content in the parent revision. Undo brings the changes back.")
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
                                to_set: RevSet { from: id.clone(), to: id },
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

    /// The hunk of the selected file that the diff's line selection falls in.
    pub(super) fn selected_hunk(&self) -> Option<(TreePath, ChangeHunk)> {
        let range = self.diff_selection.as_ref()?;
        let detail = self.detail.as_ref()?;
        let file = detail.files.get(self.selected_file?)?;
        if range.path().as_ref() != file.path.as_ref() {
            return None;
        }
        let (start, end) = (
            range.start().min(range.end()),
            range.start().max(range.end()),
        );
        file.hunks
            .iter()
            .find(|hunk| {
                let lines = match range.side() {
                    gpui_kit::component::diff::DiffSide::Original => &hunk.location.from_file,
                    gpui_kit::component::diff::DiffSide::Modified => &hunk.location.to_file,
                };
                // GG ranges are 1-based; an empty side has no lines to hit.
                lines.len > 0 && start < lines.start + lines.len && end >= lines.start
            })
            .map(|hunk| (file.tree_path.clone(), hunk.clone()))
    }

    /// Moves the hunk under the diff selection into the parent revision.
    pub(super) fn squash_hunk_into_parent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refuse_if_immutable("Squash hunk", window, cx) {
            return;
        }
        let (Some((id, parent)), Some((path, hunk))) = (self.detail_ids(), self.selected_hunk())
        else {
            return;
        };
        let label = format!("Squash hunk of {} into parent", path.repo_path);
        self.diff_selection = None;
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

        // Enter submits, like the dialog's primary button.
        self.dialog_subscription = Some(cx.subscribe_in(&input, window, {
            let submit = submit.clone();
            move |view: &mut Self, state, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let value = state.read(cx).value().trim().to_owned();
                    if !value.is_empty() {
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
