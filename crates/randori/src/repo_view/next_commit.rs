//! The working copy's next commit: what it includes (its files' and lines'
//! boxes, see `included`), and Commit (`jj commit -i`) or Split off
//! (`jj split`) with the message typed.

use gg_lib::messages::mutations::SplitRevision;
use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::{Context, Window};

use super::RepoView;

use super::*;

impl RepoView {
    /// ⌘-click (`range` false) or ⇧-click (`range`: every file from the
    /// selected one to `ix`) in the working copy: the files' boxes ticked, or
    /// cleared when file `ix` was in already, as its box would be.
    pub(super) fn include_clicked(&mut self, ix: usize, range: bool, cx: &mut Context<Self>) {
        let Some(files) = self.inspected.detail.as_ref().map(|detail| &detail.files) else {
            return;
        };
        let Some(clicked) = files.get(ix) else {
            return;
        };
        let included = &mut self.next_commit.included;
        let tick = included.file(&clicked.tree_path.repo_path, &clicked.hunks)
            != super::included::Inclusion::All;
        let anchor = if range {
            self.inspected
                .file
                .filter(|&at| at < files.len())
                .unwrap_or(ix)
        } else {
            ix
        };
        for file in &files[anchor.min(ix)..=anchor.max(ix)] {
            included.set_file(&file.tree_path.repo_path, tick);
        }
        self.next_commit.lines_left_out = false;
        cx.notify();
    }

    /// Includes every change of the working copy in the next commit, or none.
    pub(super) fn include_all(&mut self, included: bool, cx: &mut Context<Self>) {
        if !self.commit_mode() {
            return;
        }
        let Some(detail) = self.inspected.detail.as_ref() else {
            return;
        };
        self.next_commit.included.set_all(&detail.files, included);
        self.next_commit.lines_left_out = false;
        cx.notify();
    }

    /// Includes the working copy's file `ix` whole in the next commit, or
    /// leaves it out.
    pub(super) fn include_file(&mut self, ix: usize, included: bool, cx: &mut Context<Self>) {
        if !self.commit_mode() {
            return;
        }
        let Some(file) = self
            .inspected
            .detail
            .as_ref()
            .and_then(|detail| detail.files.get(ix))
        else {
            return;
        };
        let path = file.tree_path.repo_path.clone();
        self.next_commit.included.set_file(&path, included);
        self.next_commit.lines_left_out = false;
        cx.notify();
    }

    /// Whether the diff on screen is the working copy's, whose lines the next
    /// commit includes. By change id: a snapshot makes the working copy
    /// another commit, and a diff read before it still is the working copy's
    /// (lines whose text changed do not count, see `included`).
    pub(super) fn includes_lines(&self, file: &ShownFile) -> bool {
        self.commit_mode()
            && self
                .inspected
                .detail
                .as_ref()
                .is_some_and(|detail| detail.header.id.change.hex == file.id.change.hex)
    }

    /// Includes the changed lines between `start` and `end` of the diff on
    /// screen (a line's box, or the lines dragged over) in the next commit,
    /// or leaves them out when every one of them is in.
    pub(super) fn toggle_lines(
        &mut self,
        start: (Side, usize),
        end: (Side, usize),
        cx: &mut Context<Self>,
    ) {
        let Some(file) = self
            .shown_diff_file()
            .filter(|file| self.includes_lines(file))
        else {
            return;
        };
        let keys = file.changed_lines_between(start, end);
        if keys.is_empty() {
            return;
        }
        let (path, hunks) = (file.path.repo_path.clone(), file.hunks.to_vec());
        let included = &mut self.next_commit.included;
        let every = keys.iter().all(|&key| included.line(&path, key));
        included.set_lines(&path, &hunks, &keys, !every);
        self.next_commit.lines_left_out = false;
        cx.notify();
    }

    /// The lines dragged over in the diff on screen, included or left out.
    pub(super) fn toggle_selected_lines(&mut self, cx: &mut Context<Self>) {
        if let Some(range) = self.shown_line_range().cloned() {
            self.toggle_lines(
                (side_of(range.side()), range.start()),
                (side_of(range.end_side()), range.end()),
                cx,
            );
            self.clear_line_selection(cx);
        }
    }

    /// Whether every changed line dragged over in the diff on screen is in
    /// the next commit, so their button leaves them out.
    pub(super) fn selected_lines_included(&self) -> bool {
        let (Some(range), Some(file)) = (self.shown_line_range(), self.shown_diff_file()) else {
            return false;
        };
        let keys = file.changed_lines_between(
            (side_of(range.side()), range.start()),
            (side_of(range.end_side()), range.end()),
        );
        !keys.is_empty()
            && keys
                .iter()
                .all(|&key| self.next_commit.included.line(&file.path.repo_path, key))
    }

    /// Splits the changes included off the working copy with the message
    /// typed, as `jj split` does: the working copy keeps the rest, its
    /// description and its bookmarks (see `SplitRevision`).
    pub(super) fn split_included(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.next_commit.message.text(cx);
        if message.is_empty() || !self.commit_mode() || self.blocked_by_pick(window, cx) {
            return;
        }
        let Some(detail) = self.inspected.detail.as_ref() else {
            return;
        };
        let included = &self.next_commit.included;
        let (paths, hunks) = included.changes(&detail.files);
        if paths.is_empty() && hunks.is_empty() {
            // said, as a commit says it, rather than the button doing nothing
            if !included.is_empty() {
                window.push_notification(
                    Notification::error(super::included::NOTHING_LEFT).title("Split off"),
                    cx,
                );
            }
            return;
        }
        let id = detail.header.id.clone();
        let label = format!("Split off {}", included.summary(&detail.files));
        // the message and what is included go once the split is made
        self.next_commit.pending = Some(label.clone().into());
        self.run_mutation(
            label,
            Box::new(SplitRevision {
                id,
                paths,
                hunks,
                description: message,
            }),
            None,
            window,
            cx,
        );
    }

    /// Commits the working copy with the message typed: the changes
    /// included, or every one when none is (see `CommitChanges`).
    pub(super) fn commit_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.next_commit.message.text(cx);
        if message.is_empty() || !self.commit_mode() {
            return;
        }
        let Some(detail) = self.inspected.detail.as_ref() else {
            return;
        };
        if detail.files.is_empty() || self.blocked_by_pick(window, cx) {
            return;
        }
        let id = detail.header.id.clone();
        let included = &self.next_commit.included;
        let (paths, hunks) = included.changes(&detail.files);
        let label = if included.is_empty() {
            "Commit all changes".to_owned()
        } else if paths.is_empty() && hunks.is_empty() {
            // never every change for some, whatever made them nothing
            window.push_notification(
                Notification::error(super::included::NOTHING_LEFT).title("Commit"),
                cx,
            );
            return;
        } else {
            format!("Commit {}", included.summary(&detail.files))
        };
        // the message is cleared once the commit is made, kept if it fails
        self.next_commit.pending = Some(label.clone().into());
        self.run_mutation(
            label,
            Box::new(gg_lib::messages::mutations::CommitChanges {
                id,
                description: message,
                paths,
                hunks,
            }),
            None,
            window,
            cx,
        );
    }
}
