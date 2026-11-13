//! Lines of a diff on screen: which lines a selection or a line's button
//! covers, the hunks of only those lines, and the commands on them (Squash
//! into parent, Split off…). The working copy's lines go to the next
//! commit instead (see `next_commit`).

use gg_lib::messages::mutations::{FileHunks, MoveHunk, SplitRevision};
use gg_lib::messages::{ChangeHunk, RevId, TreePath};
use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::{Context, SharedString, Window};

use gg_lib::messages::{ChangeLocation, ChangeRange, MultilineString};

use super::RepoView;

impl RepoView {
    /// The file of the diff on screen: the file's full view when it is open,
    /// else the inspector's selected file.
    pub(super) fn shown_diff_file(&self) -> Option<ShownFile<'_>> {
        if self.center.file().is_some() {
            return self.focus_shown_file();
        }
        let detail = self.inspected.detail.as_ref()?;
        let file = detail.files.get(self.inspected.file?)?;
        Some(ShownFile {
            id: detail.header.id.clone(),
            parent: detail.parent_ids.first().cloned(),
            immutable: detail.header.is_immutable,
            path: file.tree_path.clone(),
            diff_path: file.path.clone(),
            hunks: &file.hunks,
        })
    }

    /// The lines selected in the diff on screen (see [`Self::shown_diff_file`]).
    pub(super) fn selected_lines(&self) -> Option<LineSelection> {
        let range = self.shown_line_range()?;
        let file = self.shown_diff_file()?;
        if range.path().as_ref() != file.diff_path.as_ref() {
            return None;
        }
        file.lines(
            (side_of(range.side()), range.start()),
            (side_of(range.end_side()), range.end()),
        )
    }

    /// The lines dragged over in the diff on screen: the file's full view's
    /// when it is open, else the inspector's.
    pub(super) fn shown_line_range(&self) -> Option<&gpui_kit::component::diff::DiffLineRange> {
        match self.center.file() {
            Some(focus) => focus.selection.as_ref(),
            None => self.line_selection.as_ref(),
        }
    }

    /// Forgets the line selection of both diffs, and its highlight, after a
    /// command took it.
    pub(super) fn clear_line_selection(&mut self, cx: &mut Context<Self>) {
        self.line_selection = None;
        self.diff
            .update(cx, |diff, cx| diff.set_selected_lines(None, cx));
        if let Some(focus) = self.center.file_mut() {
            focus.selection = None;
            focus
                .diff
                .update(cx, |diff, cx| diff.set_selected_lines(None, cx));
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
    /// the parent revision: the line's + in the diff outside the working copy.
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
            changed_lines_label(std::slice::from_ref(&hunk)),
            path.repo_path
        );
        self.clear_line_selection(cx);
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
        let what = format!("{} of {}", changed_lines_label(&hunks), path.repo_path);
        let (paths, hunks) = file_changes(path, hunks, every);
        self.prompt_text(
            format!("Split {what} into a new commit"),
            "Description",
            // as `jj split` offers it: the revision's own description
            self.description_of(&id),
            "Split",
            move |view, description, window, cx| {
                view.clear_line_selection(cx);
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

    /// The changed lines between `start` and `end`, as the diff highlights
    /// them: each by its side and number (see `included::LineKey`).
    pub(super) fn changed_lines_between(
        &self,
        start: (Side, usize),
        end: (Side, usize),
    ) -> Vec<(Side, usize)> {
        let flags = selected_hunk_lines(self.hunks, start, end);
        // the flags of the changed lines, in the order their positions come
        let changed = self.hunks.iter().zip(&flags).flat_map(|(hunk, flags)| {
            hunk.lines
                .lines
                .iter()
                .zip(flags)
                .filter(|(line, _)| matches!(line.as_bytes().first(), Some(b'-' | b'+')))
                .map(|(_, &on)| on)
        });
        changed_line_positions(self.hunks)
            .into_iter()
            .zip(changed)
            .filter_map(|(position, on)| on.then_some(position))
            .collect()
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
fn changed_lines_label(hunks: &[ChangeHunk]) -> String {
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

/// A side of a diff, for one end of a line selection.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub(super) enum Side {
    Original,
    Modified,
}

/// Which lines of a file's `hunks` a diff selection from `start` to `end`
/// (a side and a line number on it) covers, one flag per hunk line, as the
/// diff highlights them: on one side, the lines numbered in the range there;
/// across the two, every patch line between the two ends.
pub(super) fn selected_hunk_lines(
    hunks: &[ChangeHunk],
    start: (Side, usize),
    end: (Side, usize),
) -> Vec<Vec<bool>> {
    // each line's numbers on the two sides, in patch order
    let numbered: Vec<Vec<(Option<usize>, Option<usize>)>> = hunks
        .iter()
        .map(|hunk| {
            let (mut old, mut new) = (hunk.location.from_file.start, hunk.location.to_file.start);
            hunk.lines
                .lines
                .iter()
                .map(|line| match line.as_bytes().first() {
                    Some(b'-') => {
                        old += 1;
                        (Some(old - 1), None)
                    }
                    Some(b'+') => {
                        new += 1;
                        (None, Some(new - 1))
                    }
                    _ => {
                        old += 1;
                        new += 1;
                        (Some(old - 1), Some(new - 1))
                    }
                })
                .collect()
        })
        .collect();
    let number = |(old, new): (Option<usize>, Option<usize>), side| match side {
        Side::Original => old,
        Side::Modified => new,
    };
    if start.0 == end.0 {
        let (side, lo, hi) = (start.0, start.1.min(end.1), start.1.max(end.1));
        return numbered
            .iter()
            .map(|lines| {
                lines
                    .iter()
                    .map(|&line| number(line, side).is_some_and(|n| (lo..=hi).contains(&n)))
                    .collect()
            })
            .collect();
    }
    let flat: Vec<(Option<usize>, Option<usize>)> = numbered.iter().flatten().copied().collect();
    let at = |(side, line): (Side, usize)| flat.iter().position(|&l| number(l, side) == Some(line));
    let (Some(a), Some(b)) = (at(start), at(end)) else {
        return numbered
            .iter()
            .map(|lines| vec![false; lines.len()])
            .collect();
    };
    let (lo, hi) = (a.min(b), a.max(b));
    let mut ix = 0;
    numbered
        .iter()
        .map(|lines| {
            lines
                .iter()
                .map(|_| {
                    ix += 1;
                    (lo..=hi).contains(&(ix - 1))
                })
                .collect()
        })
        .collect()
}

/// Where each changed line of `hunks` is in the diff: an added line by its
/// number on the modified side, a removed one on the original side.
pub(super) fn changed_line_positions(hunks: &[ChangeHunk]) -> Vec<(Side, usize)> {
    changed_lines(hunks).map(|(position, _)| position).collect()
}

/// Each changed line of `hunks` in patch order: where it is (see
/// [`changed_line_positions`]) and its patch line (`+text\n`).
pub(super) fn changed_lines(
    hunks: &[ChangeHunk],
) -> impl Iterator<Item = ((Side, usize), &String)> {
    hunks.iter().flat_map(|hunk| {
        let (mut old, mut new) = (hunk.location.from_file.start, hunk.location.to_file.start);
        hunk.lines
            .lines
            .iter()
            .filter_map(move |line| match line.as_bytes().first() {
                Some(b'-') => {
                    old += 1;
                    Some(((Side::Original, old - 1), line))
                }
                Some(b'+') => {
                    new += 1;
                    Some(((Side::Modified, new - 1), line))
                }
                _ => {
                    old += 1;
                    new += 1;
                    None
                }
            })
    })
}

/// `hunk` with only its `selected` changes, as `git add -p` edits a hunk: a
/// deletion left behind becomes context, an addition left behind goes.
/// `None` when no selected line changes anything.
pub(super) fn partial_hunk(hunk: &ChangeHunk, selected: &[bool]) -> Option<ChangeHunk> {
    let mut lines = Vec::with_capacity(hunk.lines.lines.len());
    let mut changes = false;
    for (line, &picked) in hunk.lines.lines.iter().zip(selected) {
        match line.as_bytes().first() {
            Some(b'-' | b'+') if picked => {
                changes = true;
                lines.push(line.clone());
            }
            Some(b'-') => lines.push(format!(" {}", &line[1..])),
            Some(b'+') => {}
            _ => lines.push(line.clone()),
        }
    }
    if !changes {
        return None;
    }
    let to_len = lines.iter().filter(|line| !line.starts_with('-')).count();
    Some(ChangeHunk {
        location: ChangeLocation {
            from_file: hunk.location.from_file.clone(),
            to_file: ChangeRange {
                start: hunk.location.to_file.start,
                len: to_len,
            },
        },
        lines: MultilineString { lines },
    })
}

/// Whether `selected` (as [`selected_hunk_lines`] gives it) holds every
/// changed line of `hunks`.
pub(super) fn selects_every_change(hunks: &[ChangeHunk], selected: &[Vec<bool>]) -> bool {
    hunks.iter().zip(selected).all(|(hunk, picked)| {
        hunk.lines
            .lines
            .iter()
            .zip(picked)
            .all(|(line, &picked)| picked || !matches!(line.as_bytes().first(), Some(b'-' | b'+')))
    })
}
