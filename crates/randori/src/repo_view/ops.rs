//! Repository mutations: the jj operations Randori can perform.
//!
//! Every operation is one of GG's mutation types run on the worker. The result
//! drives the UI the same way GG does: `Updated` reloads the log and moves the
//! selection to the revision jj reports, precondition failures (an immutable
//! revision, a conflicting bookmark) become warnings, and `InputRequired`
//! asks for git credentials and retries (see `remote`).

use crate::harness_mark::HarnessMark as _;
use crate::settings;
use std::path::PathBuf;
use std::rc::Rc;

use gg_lib::messages::mutations::{
    AbandonRevisions, BackoutRevisions, CheckoutRevision, CopyChanges, CreateRef, CreateRevision,
    DeleteRef, DescribeRevision, DuplicateRevisions, ExternalDiff, ExternalResolve, MoveChanges,
    MoveRef, MoveRevisions, MutationResult, RebaseBranch, RedoOperation, RenameBookmark,
    SplitRevision, TrackBookmark, UndoOperation, UntrackBookmark,
};
use gg_lib::messages::{CommitId, InputResponse, RevHeader, RevId, RevSet, StoreRef, TreePath};
use gg_lib::worker::Mutation;
use gpui_kit::component::{
    ActiveTheme as _, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    input::{Input, InputEvent, InputState},
    notification::Notification,
    v_flex,
};
use gpui_kit::{
    AppContext as _, Context, ParentElement as _, SharedString, Styled as _, Window, div,
};

use super::RepoView;

pub(super) type BoxedMutation = Box<dyn Mutation + Send + Sync>;
/// Rebuilds a mutation with the credentials the user entered.
pub(super) type Retry = Rc<dyn Fn(InputResponse) -> BoxedMutation>;

impl RepoView {
    pub(super) fn selected_header(&self) -> Option<&RevHeader> {
        self.selected
            .and_then(|ix| self.log.rows.get(ix))
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
                // only the commit's own result: another operation may end first
                let commit = view.next_commit.pending.as_ref() == Some(&label);
                if commit {
                    view.next_commit.pending = None;
                }
                if commit && matches!(result, Ok(MutationResult::Updated { .. })) {
                    view.next_commit.message.set("", window, cx);
                    view.next_commit.included.clear();
                    view.next_commit.lines_left_out = false;
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
    pub(super) fn refuse_rewrite(
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
        // the revision a rebase starts from is the one that moves
        let hint = if action == "Rebase" {
            " To move your work onto it, rebase your work (or drop its bookmark) onto it."
        } else {
            ""
        };
        window.push_notification(
            Notification::warning(format!(
                "{change} is immutable. Change `immutable_heads()` in jj config to rewrite it.{hint}"
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

    /// Moves the selected revision's changes into its parent and abandons it,
    /// as `jj squash -r`: the parent keeps both descriptions, and the
    /// revision's children move onto the parent. Undo brings it back.
    pub(super) fn squash_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked_by_pick(window, cx) {
            return;
        }
        if self.refuse_if_immutable("Squash", window, cx) {
            return;
        }
        let Some(ix) = self.selected else {
            return;
        };
        let (id, to_id) = match self.squash_target(ix) {
            Ok(target) => target,
            Err(reason) => {
                window.push_notification(Notification::warning(reason).title("Squash"), cx);
                return;
            }
        };
        self.run_mutation(
            "Squash into parent",
            Box::new(MoveChanges {
                from: RevSet {
                    from: id.clone(),
                    to: id,
                },
                to_id,
                paths: vec![],
            }),
            None,
            window,
            cx,
        );
    }

    /// Row `ix`'s revision and its one parent, to squash it into, or why
    /// not, as far as the log shows: a merge has no one parent, and jj does
    /// not rewrite an immutable parent (a parent outside the log is left to
    /// jj).
    pub(super) fn squash_target(&self, ix: usize) -> Result<(RevId, CommitId), &'static str> {
        let row = self
            .log
            .rows
            .get(ix)
            .ok_or("The revision is not in the log.")?;
        let parent = match row.revision.parent_ids.as_slice() {
            [parent] => parent,
            [] => return Err("The root revision has no parent."),
            _ => return Err("A merge has more than one parent to squash into."),
        };
        let immutable = self
            .log
            .row_of_commit
            .get(&parent.hex)
            .and_then(|&at| self.log.rows.get(at))
            .is_some_and(|parent| parent.revision.is_immutable);
        if immutable {
            return Err(
                "The parent is immutable. Change `immutable_heads()` in jj config to rewrite it.",
            );
        }
        Ok((row.revision.id.clone(), parent.clone()))
    }

    /// Row `ix`'s commit on GitHub, when the workspace's GitHub remote has
    /// it: the revision is immutable (trunk and what is under it) or under a
    /// bookmark of that remote in the log. Elsewhere the page would be a 404.
    /// Immutable is a guess: a tag never pushed makes a revision immutable
    /// too. It stands for the bookmark the log may not lead to (in Mine, the
    /// rows between are not there); asking jj (`::remote_bookmarks()`) would
    /// be exact, but the menu is built as it opens, with no time to.
    pub(super) fn commit_link(&self, ix: usize) -> Option<String> {
        let workspace = self.workspace.as_ref()?;
        let (remote, repo) = crate::github::GitHubRepo::pick_named(&workspace.remote_urls)?;
        let row = self.log.rows.get(ix)?;
        (row.revision.is_immutable || self.under_remote_bookmark(ix, remote))
            .then(|| repo.commit_url(&row.revision.id.commit.hex))
    }

    /// Whether row `ix` is a bookmark of `remote` or an ancestor of one, as far
    /// as the log's rows go.
    fn under_remote_bookmark(&self, ix: usize, remote: &str) -> bool {
        let rows = &self.log.rows;
        let mut queue: std::collections::VecDeque<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.revision.refs.iter().any(|r| {
                    matches!(r, StoreRef::RemoteBookmark { remote_name, .. } if remote_name == remote)
                })
            })
            .map(|(at, _)| at)
            .collect();
        let mut seen: std::collections::HashSet<usize> = queue.iter().copied().collect();
        while let Some(at) = queue.pop_front() {
            if at == ix {
                return true;
            }
            for parent in &rows[at].revision.parent_ids {
                if let Some(&next) = self.log.row_of_commit.get(&parent.hex)
                    && seen.insert(next)
                {
                    queue.push_back(next);
                }
            }
        }
        false
    }

    /// The selected revision's change id (`change`) or commit id, whole, on
    /// the clipboard.
    pub(super) fn copy_selected_id(&self, change: bool, cx: &mut Context<Self>) {
        let Some(header) = self.selected_header() else {
            return;
        };
        copy_to_clipboard(
            if change {
                header.id.change.hex.clone()
            } else {
                header.id.commit.hex.clone()
            },
            cx,
        );
    }

    /// The selected revision's page on GitHub, on the clipboard. The menu
    /// offers it only where there is one; from the palette, a note says why not.
    pub(super) fn copy_selected_link(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.selected.and_then(|ix| self.commit_link(ix)) {
            Some(link) => copy_to_clipboard(link, cx),
            None => window.push_notification(
                Notification::info(
                    "The revision is not on the workspace's GitHub remote: push it first.",
                )
                .title("No link to copy"),
                cx,
            ),
        }
    }

    /// The inspected revision's commit id, whole, on the clipboard (a click on
    /// the inspector's Commit).
    pub(super) fn copy_inspected_commit_id(&self, cx: &mut Context<Self>) {
        if let Some(detail) = &self.inspected.detail {
            copy_to_clipboard(detail.header.id.commit.hex.clone(), cx);
        }
    }

    /// Selects the inspected revision's parent `n` (a click on one of the
    /// inspector's Parents), widening the log when it does not have it.
    pub(super) fn go_to_parent(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
            .inspected
            .detail
            .as_ref()
            .and_then(|detail| detail.parent_ids.get(n))
            .cloned()
        else {
            return;
        };
        let row = self.log.row_of_commit.get(&id.commit.hex).copied();
        self.select_or_reveal(&id, row, window, cx);
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

    /// Saves the inspector's description editor to the revision it was
    /// loaded from (not a row selected since, whose detail is on its way).
    pub(super) fn save_description(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // what Save is offered for: the text as the fields read it back
        if !self.description_dirty(cx) {
            return;
        }
        let Some(id) = self
            .inspected
            .detail
            .as_ref()
            .map(|detail| detail.header.id.clone())
        else {
            return;
        };
        let new_description = self.description.editor.text(cx);
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
        let Some(parent) = self.log.rows.get(target).map(|row| row.revision.id.clone()) else {
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

    /// Drag and drop: a local bookmark dropped on a revision asks what to do,
    /// as GitKraken asks when a branch is dropped on another: rebase its
    /// branch there (`jj rebase -b`) or only point the bookmark there. A
    /// remote bookmark or a tag only moves (or is refused, as remote ones are).
    pub(super) fn drop_ref(
        &mut self,
        r#ref: StoreRef,
        target: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let StoreRef::LocalBookmark { bookmark_name, .. } = &r#ref else {
            self.move_ref_to(r#ref, target, window, cx);
            return;
        };
        // the revision, not its row: the log may be read again (a fetch, a
        // snapshot) while the dialog is open, and its rows move
        let Some(parent) = self.log.rows.get(target).map(|row| row.revision.id.clone()) else {
            return;
        };
        // forward, as jj moves a bookmark without being asked; else a choice
        let name = bookmark_name.clone();
        self.when_moved_forward(name.clone(), parent.clone(), window, cx, {
            move |view, forward, window, cx| {
                if forward {
                    view.move_ref(r#ref, parent, window, cx);
                } else {
                    view.ask_rebase_or_move(r#ref, name, parent, window, cx);
                }
            }
        });
    }

    /// Asks the worker whether moving local bookmark `name` to `to` moves it
    /// forward (`to` is its revision or a descendant), as `jj bookmark set`
    /// does without `--allow-backwards`, then runs `then` with the answer.
    /// An answer that does not come is "not forward": the move is asked about.
    pub(super) fn when_moved_forward(
        &mut self,
        name: String,
        to: RevId,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, bool, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let worker = self.worker.clone();
        let revset = format!("{} & descendants(bookmarks(exact:{name:?}))", to.commit.hex);
        self.direction_check = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { worker.query_headers(revset, 1) })
                .await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.direction_check = None;
                let forward = match result {
                    Ok(headers) => !headers.is_empty(),
                    Err(err) => {
                        log::warn!("bookmark direction query failed: {err:#}");
                        false
                    }
                };
                then(view, forward, window, cx);
                cx.notify();
            });
        }));
    }

    /// A local bookmark dropped backwards or sideways: rebase its branch onto
    /// the revision (`jj rebase -b`), or point it there anyway (`jj bookmark
    /// set --allow-backwards`), leaving the revisions it was on.
    fn ask_rebase_or_move(
        &mut self,
        r#ref: StoreRef,
        name: String,
        parent: RevId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let onto = super::short_id(&parent.change.prefix, &parent.change.rest, 8);
        let view = cx.entity().downgrade();
        let muted = cx.theme().muted_foreground;
        window.open_dialog(cx, move |dialog, _, _| {
            let (name, onto, r#ref) = (name.clone(), onto.clone(), r#ref.clone());
            let (rebase_view, move_view) = (view.clone(), view.clone());
            let (rebase_name, rebase_parent, move_parent) =
                (name.clone(), parent.clone(), parent.clone());
            dialog
                .title(format!("{name} onto {onto}"))
                .child(
                    v_flex()
                        .gap_2()
                        .text_sm()
                        .child(format!(
                            "Rebase branch moves the revisions of {name} that {onto} does not \
                             have, with what is on them, onto {onto}. The bookmark comes along."
                        ))
                        .child(format!(
                            "Move bookmark only points {name} at {onto}, which is not after it: \
                             the revisions it was on keep no bookmark. No revision changes."
                        ))
                        // apart, so a line never breaks inside the command
                        .child(div().text_xs().whitespace_nowrap().text_color(muted).child(
                            "Rebase branch is jj rebase -b; Move bookmark, jj bookmark set --allow-backwards.",
                        )),
                )
                .footer(
                    DialogFooter::new()
                        .gap_2()
                        .child(
                            DialogClose::new()
                                .child(Button::new("cancel").label("Cancel").outline()),
                        )
                        .child(
                            Button::new("move-bookmark")
                                .harness_mark(|| "move-bookmark")
                                .label("Move bookmark")
                                .outline()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let (r#ref, to_id) = (r#ref.clone(), move_parent.clone());
                                    let _ = move_view.update(cx, |view, cx| {
                                        view.move_ref(r#ref, to_id, window, cx)
                                    });
                                }),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("rebase-branch")
                                    .label("Rebase branch")
                                    .primary(),
                            ),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    let (name, parent) = (rebase_name.clone(), rebase_parent.clone());
                    let _ = rebase_view
                        .update(cx, |view, cx| view.rebase_branch(name, parent, window, cx));
                    true
                })
        });
    }

    /// Rebases the branch of local bookmark `name` onto `parent`, as `jj
    /// rebase -b` (see `RebaseBranch`).
    pub(super) fn rebase_branch(
        &mut self,
        name: String,
        parent: RevId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = format!(
            "Rebase {name} onto {}",
            super::short_id(&parent.change.prefix, &parent.change.rest, 8)
        );
        self.run_mutation(
            label,
            Box::new(RebaseBranch {
                bookmark_name: name,
                parent_id: parent,
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
        if let Some(to_id) = self.log.rows.get(target).map(|row| row.revision.id.clone()) {
            self.move_ref(r#ref, to_id, window, cx);
        }
    }

    /// The pick's "Move here": a local bookmark moved forward moves at once;
    /// backwards or sideways, as `jj bookmark set --allow-backwards`, it is
    /// confirmed first. A tag or remote bookmark moves (or is refused) as is.
    pub(super) fn pick_move_ref_to(
        &mut self,
        r#ref: StoreRef,
        target: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let StoreRef::LocalBookmark { bookmark_name, .. } = &r#ref else {
            self.move_ref_to(r#ref, target, window, cx);
            return;
        };
        let Some(to_id) = self.log.rows.get(target).map(|row| row.revision.id.clone()) else {
            return;
        };
        let name = bookmark_name.clone();
        self.when_moved_forward(name.clone(), to_id.clone(), window, cx, {
            move |view, forward, window, cx| {
                if forward {
                    view.move_ref(r#ref, to_id, window, cx);
                    return;
                }
                let onto = super::short_id(&to_id.change.prefix, &to_id.change.rest, 8);
                let view = cx.entity().downgrade();
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let (view, r#ref, to_id) = (view.clone(), r#ref.clone(), to_id.clone());
                    alert
                        .title(format!("Move {name} to {onto}?"))
                        .description(format!(
                            "{onto} is not after {name}: the revisions {name} was on keep no \
                             bookmark (jj bookmark set --allow-backwards). Undo moves it back."
                        ))
                        .confirm()
                        .ok_text("Move bookmark")
                        .on_ok(move |_, window, cx| {
                            let (r#ref, to_id) = (r#ref.clone(), to_id.clone());
                            let _ =
                                view.update(cx, |view, cx| view.move_ref(r#ref, to_id, window, cx));
                            true
                        })
                });
            }
        });
    }

    /// Points a bookmark or tag at `to_id`.
    pub(super) fn move_ref(
        &mut self,
        r#ref: StoreRef,
        to_id: RevId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        let detail = self.inspected.detail.as_ref()?;
        let parent = detail.parent_ids.first()?.clone();
        Some((detail.header.id.clone(), parent))
    }

    pub(super) fn selected_file_path(&self) -> Option<TreePath> {
        let detail = self.inspected.detail.as_ref()?;
        detail
            .files
            .get(self.inspected.file?)
            .map(|file| file.tree_path.clone())
    }

    /// A click on file `ix` in the inspector: ⌘ adds it to the picked files
    /// or takes it out, ⇧ picks the files from the selected one to it, a
    /// plain click picks none. In the working copy, whose files have boxes
    /// for the next commit (which Split off also takes), ⌘ and ⇧ tick the
    /// boxes instead. The diff shows the file clicked.
    pub(super) fn click_file(
        &mut self,
        ix: usize,
        modifiers: gpui_kit::Modifiers,
        cx: &mut Context<Self>,
    ) {
        if self.commit_mode() && (modifiers.secondary(cx) || modifiers.shift) {
            self.include_clicked(ix, modifiers.shift, cx);
            self.select_file(Some(ix), cx);
            return;
        }
        let Some(files) = self.inspected.detail.as_ref().map(|detail| &detail.files) else {
            return;
        };
        let path = |ix: usize| files.get(ix).map(|file| file.path.clone());
        if modifiers.secondary(cx) {
            // the file shown so far is picked with the first one added
            if self.inspected.marked.is_empty()
                && let Some(shown) = self
                    .inspected
                    .file
                    .filter(|shown| *shown != ix)
                    .and_then(path)
            {
                self.inspected.marked.push(shown);
            }
            if let Some(clicked) = path(ix) {
                match self
                    .inspected
                    .marked
                    .iter()
                    .position(|marked| *marked == clicked)
                {
                    Some(at) => {
                        self.inspected.marked.remove(at);
                    }
                    None => self.inspected.marked.push(clicked),
                }
            }
        } else if modifiers.shift {
            let anchor = self.inspected.file.unwrap_or(ix);
            let (start, end) = (anchor.min(ix), anchor.max(ix));
            self.inspected.marked = (start..=end).filter_map(path).collect();
        } else {
            self.inspected.marked.clear();
        }
        self.select_file(Some(ix), cx);
    }

    /// The files Split takes: the picked ones in the list's order, else the
    /// selected one.
    fn split_paths(&self) -> Vec<TreePath> {
        let Some(detail) = self.inspected.detail.as_ref() else {
            return Vec::new();
        };
        if self.inspected.marked.is_empty() {
            return self.selected_file_path().into_iter().collect();
        }
        detail
            .files
            .iter()
            .filter(|file| self.inspected.marked.contains(&file.path))
            .map(|file| file.tree_path.clone())
            .collect()
    }

    /// The description of revision `id`, the one inspected or the one the
    /// file's full view shows, as its editor would start.
    pub(super) fn description_of(&self, id: &RevId) -> String {
        let inspected = self.inspected.detail.as_ref().map(|detail| &detail.header);
        let shown = self.center.file().map(|focus| &focus.rev);
        [inspected, shown]
            .into_iter()
            .flatten()
            .find(|header| header.id.commit.hex == id.commit.hex)
            .map(|header| header.description.lines.join("\n").trim().to_owned())
            .unwrap_or_default()
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
            // as `jj split` offers it: the revision's own description
            self.description_of(&id),
            "Split",
            move |view, description, window, cx| {
                view.inspected.marked.clear();
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
        let Some(to) = self.log.rows.get(target).map(|row| row.revision.id.clone()) else {
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

    /// Opens the selected file's change in the diff application chosen in
    /// Settings (`diff_apps`), its two versions side by side; with none
    /// chosen, in the built-in full view.
    pub(super) fn diff_in_tool(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some((id, _)), Some(path)) = (self.detail_ids(), self.selected_file_path()) else {
            return;
        };
        // none chosen: the built-in diff, the full view
        let Some(app) = settings::get(cx).display.diff_app.clone() else {
            self.open_focus(super::FocusTab::Diff, false, window, cx);
            return;
        };
        // chosen, then uninstalled or moved
        let Some(tool) = crate::diff_apps::command(&app) else {
            let name = crate::diff_apps::name(&app).map_or(app.clone(), str::to_owned);
            window.push_notification(
                Notification::error(format!(
                    "{name} was not found. Choose another in Settings › Integration › Diff tool."
                ))
                .title("Open in diff tool"),
                cx,
            );
            return;
        };
        let label = format!("Open {} in diff tool", path.repo_path);
        self.run_mutation(
            label,
            Box::new(ExternalDiff {
                id,
                path,
                tool: Some(tool),
            }),
            None,
            window,
            cx,
        );
    }

    /// The selected file's path in the workspace's folder, where the working
    /// copy has it: another revision's file may differ there, or be gone.
    pub(super) fn selected_file_full_path(&self) -> Option<PathBuf> {
        let root = &self.workspace.as_ref()?.root;
        Some(full_path(root, &self.selected_file_path()?.repo_path))
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
    pub(super) fn prompt_text(
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
}

/// A repository path (`/` between its parts) under `root`, in the system's
/// own form.
pub(super) fn full_path(root: &std::path::Path, repo_path: &str) -> PathBuf {
    root.join(repo_path.split('/').collect::<PathBuf>())
}

/// Puts `text` on the clipboard.
pub(super) fn copy_to_clipboard(text: String, cx: &mut gpui_kit::App) {
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
}
