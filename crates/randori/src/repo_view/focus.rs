//! The full view of one file, GitKraken style: it takes the place of the
//! revision list and the diff. Diff (with discard and squash on every hunk),
//! the whole file, blame, and the file's history beside them.

use gg_lib::messages::queries::{FileAnnotation, FileContent};
use gpui_kit::component::diff::{DiffAnnotation, DiffLinePosition, DiffSide};

use super::ops::ShownFile;
use super::*;

/// Revisions the history lists at most.
const HISTORY_LIMIT: usize = 500;
/// Key context of the full view; Esc closes it.
pub(super) const FOCUS_CONTEXT: &str = "RandoriFocus";

#[derive(Clone, Copy, PartialEq)]
pub(super) enum FocusTab {
    Diff,
    File,
    Blame,
}

/// Loading state of one part of the view.
struct BlameBlock {
    rev: RevHeader,
    change: SharedString,
    summary: SharedString,
    author: SharedString,
    email: SharedString,
    time: SharedString,
}

struct BlameLine {
    text: SharedString,
    block: usize,
    /// First line of its block, where the block's revision is shown.
    first: bool,
}

pub(super) struct FileFocus {
    pub(super) tab: FocusTab,
    pub(super) path: TreePath,
    name: SharedString,
    dir: SharedString,
    /// The revision shown; picking one in the history changes it.
    pub(super) rev: RevHeader,
    /// First parent of `rev`, the other side of a hunk action.
    parent: Option<RevId>,
    /// The revision the view opened on; the history is its ancestors'.
    base: RevId,
    pub(super) show_history: bool,
    pub(super) diff: Entity<DiffState>,
    /// `path`'s hunks in `rev`, for discard and squash.
    hunks: Vec<ChangeHunk>,
    /// Lines selected in the diff, for Squash into parent and Split off.
    pub(super) selection: Option<gpui_kit::component::diff::DiffLineRange>,
    diff_load: Load,
    file: Entity<DiffState>,
    file_load: Option<Load>,
    blame: Rc<(Vec<BlameBlock>, Vec<BlameLine>)>,
    blame_load: Option<Load>,
    blame_scroll: UniformListScrollHandle,
    history: Rc<Vec<(RevHeader, RowData)>>,
    history_load: Option<Load>,
    pub(super) handle: FocusHandle,
    _tasks: Vec<Task<()>>,
    _diff_events: gpui_kit::Subscription,
}

impl RepoView {
    /// Opens the full view of the selected file in the selected revision.
    pub(super) fn open_focus(
        &mut self,
        tab: FocusTab,
        history: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let Some(file) = self.selected_file.and_then(|ix| detail.files.get(ix)) else {
            return;
        };
        let rev = detail.header.clone();
        let mode = self.diff_mode;
        let diff = cx.new(|cx| DiffState::new([], cx).with_mode(mode));
        // its line selection, as the inspector's diff keeps its own
        let diff_events = cx.subscribe(&diff, |view: &mut Self, _, event: &DiffEvent, cx| {
            let Some(focus) = view.center.file_mut() else {
                return;
            };
            match event {
                DiffEvent::SelectionEnded(range) => focus.selection = Some(range.clone()),
                // kept for the line buttons, as the inspector's diff does
                DiffEvent::SelectionChanged(None) => {
                    if !std::mem::take(&mut view.keep_line_selection) {
                        focus.selection = None;
                    }
                }
                _ => return,
            }
            cx.notify();
        });
        let file_state = cx.new(|cx| DiffState::new([], cx).with_context_lines(None));
        let handle = cx.focus_handle();
        let mut focus = FileFocus {
            tab,
            path: file.tree_path.clone(),
            name: file.name.clone(),
            dir: file.dir.clone(),
            parent: detail
                .parent_ids
                .first()
                .cloned()
                .filter(|_| detail.parent_ids.len() == 1),
            base: rev.id.clone(),
            rev,
            show_history: history,
            diff,
            hunks: Vec::new(),
            selection: None,
            diff_load: Load::Loading,
            file: file_state,
            file_load: None,
            blame: Rc::default(),
            blame_load: None,
            blame_scroll: UniformListScrollHandle::new(),
            history: Rc::default(),
            history_load: None,
            handle: handle.clone(),
            _tasks: Vec::new(),
            _diff_events: diff_events,
        };
        // the selected revision's detail is loaded already
        show_diff_of(&mut focus, detail, cx);
        self.center = Center::File(Box::new(focus));
        self.load_focus_tab(cx);
        if history {
            self.load_focus_history(cx);
        }
        handle.focus(window, cx);
        cx.notify();
    }

    /// Shows the file selected in the inspector, in the selected revision,
    /// keeping the tab and whether the history is open.
    pub(super) fn retarget_focus(&mut self, cx: &mut Context<Self>) {
        let (Some(focus), Some(detail)) = (self.center.file_mut(), self.detail.as_ref()) else {
            return;
        };
        let Some(file) = self.selected_file.and_then(|ix| detail.files.get(ix)) else {
            return;
        };
        if focus.path.repo_path == file.tree_path.repo_path
            && focus.rev.id.commit.hex == detail.header.id.commit.hex
        {
            return;
        }
        focus.path = file.tree_path.clone();
        focus.name = file.name.clone();
        focus.dir = file.dir.clone();
        focus.base = detail.header.id.clone();
        focus.file_load = None;
        focus.blame_load = None;
        focus.history_load = None;
        show_diff_of(focus, detail, cx);
        let history = focus.show_history;
        self.load_focus_tab(cx);
        if history {
            self.load_focus_history(cx);
        }
        cx.notify();
    }

    pub(super) fn close_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.center.file().is_some() {
            self.center = Center::Revisions;
            self.log_focus.focus(window, cx);
            cx.notify();
        }
    }

    fn set_focus_tab(&mut self, tab: FocusTab, cx: &mut Context<Self>) {
        if let Some(focus) = self.center.file_mut() {
            focus.tab = tab;
        }
        self.load_focus_tab(cx);
        cx.notify();
    }

    fn toggle_focus_history(&mut self, cx: &mut Context<Self>) {
        let Some(focus) = self.center.file_mut() else {
            return;
        };
        focus.show_history = !focus.show_history;
        if focus.show_history && focus.history_load.is_none() {
            self.load_focus_history(cx);
        }
        cx.notify();
    }

    /// Shows the file as it is in `rev` (a history or blame pick).
    fn show_focus_rev(&mut self, rev: RevHeader, cx: &mut Context<Self>) {
        let Some(focus) = self.center.file_mut() else {
            return;
        };
        focus.rev = rev;
        focus.parent = None;
        focus.hunks.clear();
        focus.diff_load = Load::Loading;
        focus.file_load = None;
        focus.blame_load = None;
        self.load_focus_diff(cx);
        self.load_focus_tab(cx);
        cx.notify();
    }

    /// After a reload: follow the shown revision to its rewritten commit (a
    /// hunk was discarded or squashed), or keep it when it did not change.
    pub(super) fn refresh_focus(&mut self, cx: &mut Context<Self>) {
        let Some(focus) = self.center.file() else {
            return;
        };
        let change = focus.rev.id.change.hex.clone();
        let commit = focus.rev.id.commit.hex.clone();
        let Some(row) = self
            .rows
            .iter()
            .find(|row| row.revision.id.change.hex == change)
        else {
            return;
        };
        if row.revision.id.commit.hex != commit {
            let rev = row.revision.clone();
            if let Some(focus) = self.center.file_mut() {
                focus.history_load = None;
                if focus.base.change.hex == rev.id.change.hex {
                    focus.base = rev.id.clone();
                }
            }
            self.show_focus_rev(rev, cx);
            if self.center.file().is_some_and(|focus| focus.show_history) {
                self.load_focus_history(cx);
            }
        }
    }

    fn load_focus_diff(&mut self, cx: &mut Context<Self>) {
        let Some(focus) = self.center.file_mut() else {
            return;
        };
        let set = RevSet {
            from: focus.rev.id.clone(),
            to: focus.rev.id.clone(),
        };
        let asked = focus.key();
        let worker = self.worker.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { worker.query_revisions(set).map(build_detail) })
                .await;
            let _ = this.update(cx, |view, cx| {
                // a later pick (another revision or file) owns the view now
                let Some(focus) = view.center.file_mut().filter(|focus| focus.key() == asked)
                else {
                    return;
                };
                match result {
                    Ok(Some(detail)) => show_diff_of(focus, &detail, cx),
                    Ok(None) => focus.diff_load = Load::Note("This revision is gone.".into()),
                    Err(err) => focus.diff_load = Load::Failed(format!("{err:#}").into()),
                }
                cx.notify();
            });
        });
        focus._tasks.push(task);
    }

    /// Loads what the current tab needs, once per revision.
    fn load_focus_tab(&mut self, cx: &mut Context<Self>) {
        let Some(focus) = self.center.file_mut() else {
            return;
        };
        let (id, path) = (focus.rev.id.clone(), focus.path.clone());
        let asked = focus.key();
        let worker = self.worker.clone();
        let workspace = self.wd.clone();
        match focus.tab {
            FocusTab::Diff => {}
            FocusTab::File if focus.file_load.is_none() => {
                focus.file_load = Some(Load::Loading);
                let display = path.repo_path.clone();
                focus._tasks.push(cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_spawn(async move { worker.query_file(id, path) })
                        .await;
                    let _ = this.update(cx, |view, cx| {
                        let Some(focus) =
                            view.center.file_mut().filter(|focus| focus.key() == asked)
                        else {
                            return;
                        };
                        focus.file_load = Some(match result {
                            Ok(FileContent { missing: true, .. }) => {
                                Load::Note("The file is not in this revision.".into())
                            }
                            Ok(FileContent { binary: true, .. }) => {
                                Load::Note("Binary file".into())
                            }
                            Ok(content) => {
                                let file = DiffFile::unchanged(display, &content.text);
                                focus
                                    .file
                                    .update(cx, |state, cx| state.set_files([file], cx));
                                Load::Ready(())
                            }
                            Err(err) => Load::Failed(format!("{err:#}").into()),
                        });
                        cx.notify();
                    });
                }));
            }
            FocusTab::Blame if focus.blame_load.is_none() => {
                focus.blame_load = Some(Load::Loading);
                focus._tasks.push(cx.spawn(async move |this, cx| {
                    // a long history takes a while; a load of its own keeps the
                    // tab's worker (log, diff, mutations) free meanwhile
                    let result = match workspace {
                        Some(wd) => {
                            crate::worker::on_own_thread("randori-blame", move || {
                                gg_lib::worker::annotate_apart(&wd, &id, &path)
                            })
                            .await
                        }
                        None => {
                            cx.background_spawn(async move { worker.query_annotation(id, path) })
                                .await
                        }
                    };
                    let _ = this.update(cx, |view, cx| {
                        let Some(focus) =
                            view.center.file_mut().filter(|focus| focus.key() == asked)
                        else {
                            return;
                        };
                        focus.blame_load = Some(match result {
                            Ok(FileAnnotation { binary: true, .. }) => {
                                Load::Note("Binary file".into())
                            }
                            Ok(annotation) if annotation.lines.is_empty() => {
                                Load::Note("The file is empty or not in this revision.".into())
                            }
                            Ok(annotation) => {
                                focus.blame = Rc::new(blame_rows(annotation, Local::now()));
                                Load::Ready(())
                            }
                            Err(err) => Load::Failed(format!("{err:#}").into()),
                        });
                        cx.notify();
                    });
                }));
            }
            _ => {}
        }
    }

    fn load_focus_history(&mut self, cx: &mut Context<Self>) {
        let Some(focus) = self.center.file_mut() else {
            return;
        };
        focus.history_load = Some(Load::Loading);
        let (id, path) = (focus.base.clone(), focus.path.clone());
        let asked = (id.commit.hex.clone(), path.repo_path.clone());
        let worker = self.worker.clone();
        focus._tasks.push(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { worker.query_file_history(id, path, HISTORY_LIMIT) })
                .await;
            let _ = this.update(cx, |view, cx| {
                let Some(focus) = view.center.file_mut().filter(|focus| {
                    (focus.base.commit.hex.clone(), focus.path.repo_path.clone()) == asked
                }) else {
                    return;
                };
                focus.history_load = Some(match result {
                    Ok(headers) if headers.is_empty() => {
                        Load::Note("No revision changed this file.".into())
                    }
                    Ok(headers) => {
                        let now = Local::now();
                        focus.history = Rc::new(
                            headers
                                .into_iter()
                                .map(|header| {
                                    let row = RowData::from_header(&header, now);
                                    (header, row)
                                })
                                .collect(),
                        );
                        Load::Ready(())
                    }
                    Err(err) => Load::Failed(format!("{err:#}").into()),
                });
                cx.notify();
            });
        }));
    }

    /// Discards hunk `ix` from the shown revision: copies the parent's side back.
    fn discard_focus_hunk(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((rev, parent, path, hunk)) = self.focus_hunk(ix) else {
            return;
        };
        self.run_mutation(
            format!("Discard a hunk of {}", path.repo_path),
            Box::new(gg_lib::messages::mutations::CopyHunk {
                from_id: parent.commit,
                to_id: rev,
                path,
                hunk,
            }),
            None,
            window,
            cx,
        );
    }

    /// Stages hunk `ix` of the working copy's file shown.
    fn stage_focus_hunk(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(focus) = self.center.file() else {
            return;
        };
        let Some(hunk) = focus.hunks.get(ix).cloned() else {
            return;
        };
        let path = focus.path.clone();
        self.stage(
            format!("a hunk of {}", path.repo_path),
            Vec::new(),
            vec![gg_lib::messages::mutations::FileHunks {
                path,
                hunks: vec![hunk],
            }],
            window,
            cx,
        );
    }

    /// Moves hunk `ix` of the shown revision into its parent.
    fn squash_focus_hunk(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((rev, parent, path, hunk)) = self.focus_hunk(ix) else {
            return;
        };
        self.run_mutation(
            format!("Squash a hunk of {} into parent", path.repo_path),
            Box::new(gg_lib::messages::mutations::MoveHunk {
                from_id: rev,
                to_id: parent.commit,
                path,
                hunk,
            }),
            None,
            window,
            cx,
        );
    }

    /// The full view's file (see [`RepoView::shown_diff_file`]).
    pub(super) fn focus_shown_file(&self) -> Option<ShownFile<'_>> {
        let focus = self.center.file()?;
        Some(ShownFile {
            id: focus.rev.id.clone(),
            parent: focus.parent.clone(),
            immutable: focus.rev.is_immutable,
            path: focus.path.clone(),
            diff_path: focus.path.repo_path.clone().into(),
            hunks: &focus.hunks,
            staged: false,
        })
    }

    fn focus_hunk(&self, ix: usize) -> Option<(RevId, RevId, TreePath, ChangeHunk)> {
        let focus = self.center.file()?;
        if focus.rev.is_immutable {
            return None;
        }
        Some((
            focus.rev.id.clone(),
            focus.parent.clone()?,
            focus.path.clone(),
            focus.hunks.get(ix)?.clone(),
        ))
    }

    // ----- drawing ------------------------------------------------------------------------------

    pub(super) fn render_focus(&self, focus: &FileFocus, cx: &mut Context<Self>) -> AnyElement {
        // line by line, as in the inspector's diff
        let line_actions = (focus.tab == FocusTab::Diff
            && matches!(focus.diff_load, Load::Ready(()))
            && !focus.rev.is_immutable)
            .then(|| self.render_line_actions(cx));
        let theme = cx.theme();
        let tab_ix = match focus.tab {
            FocusTab::Diff => 0,
            FocusTab::File => 1,
            FocusTab::Blame => 2,
        };
        // two rows, as GitKraken's: the file and the way back, then the
        // view's controls; the close button never gives way
        let title = h_flex()
            .h_10()
            .flex_none()
            .px_3()
            .gap_2()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap_2()
                    .text_sm()
                    // the name stays whole; the summary gives way first (it has
                    // only the room left), then the directory
                    .child(
                        h_flex()
                            .max_w(gpui_kit::rems(28.))
                            .min_w_0()
                            .overflow_hidden()
                            .when(!focus.dir.is_empty(), |this| {
                                this.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{}/", focus.dir)),
                                )
                            })
                            .child(div().flex_none().font_medium().child(focus.name.clone())),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .gap_1p5()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("in")
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(theme.primary)
                                    .child(focus.rev.id.change.prefix.clone()),
                            )
                            .child(
                                div().min_w_0().truncate().child(
                                    focus
                                        .rev
                                        .description
                                        .lines
                                        .first()
                                        .cloned()
                                        .unwrap_or_default(),
                                ),
                            ),
                    ),
            )
            .child(
                Button::new("focus-close")
                    .ghost()
                    .small()
                    .icon(IconName::X)
                    .tooltip("Back to the revisions (Esc)")
                    .on_click(cx.listener(|view, _, window, cx| view.close_focus(window, cx))),
            );
        // wraps where the window is narrow: the line actions go under the
        // controls rather than over them
        let tools = h_flex()
            .min_h_10()
            .flex_none()
            .flex_wrap()
            .px_3()
            .py_1()
            .gap_2()
            .border_t_1()
            .border_color(theme.border)
            .child(
                Button::new("focus-history")
                    .ghost()
                    .xsmall()
                    .icon(IconName::GitGraph)
                    .label("History")
                    .selected(focus.show_history)
                    .tooltip("Revisions that changed this file")
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_focus_history(cx))),
            )
            .child(
                TabBar::new("focus-tab")
                    .segmented()
                    .xsmall()
                    .selected_index(tab_ix)
                    .on_click(cx.listener(|view, ix: &usize, _, cx| {
                        let tab = match ix {
                            1 => FocusTab::File,
                            2 => FocusTab::Blame,
                            _ => FocusTab::Diff,
                        };
                        view.set_focus_tab(tab, cx)
                    }))
                    .child("Diff")
                    .child("File")
                    .child("Blame"),
            )
            .when(focus.tab == FocusTab::Diff, |this| {
                this.child(
                    Button::new("focus-previous")
                        .ghost()
                        .xsmall()
                        .icon(IconName::ChevronUp)
                        .tooltip("Previous change")
                        .on_click(cx.listener(|view, _, _, cx| view.focus_change(false, cx))),
                )
                .child(
                    Button::new("focus-next")
                        .ghost()
                        .xsmall()
                        .icon(IconName::ChevronDown)
                        .tooltip("Next change")
                        .on_click(cx.listener(|view, _, _, cx| view.focus_change(true, cx))),
                )
                .child(
                    TabBar::new("focus-diff-mode")
                        .segmented()
                        .xsmall()
                        .selected_index(usize::from(self.diff_mode == DiffMode::Split))
                        .on_click(cx.listener(|view, ix: &usize, _, cx| {
                            let mode = if *ix == 1 {
                                DiffMode::Split
                            } else {
                                DiffMode::Unified
                            };
                            view.set_diff_mode(mode, cx)
                        }))
                        .child("Unified")
                        .child("Split"),
                )
            })
            .child(h_flex().flex_grow(1.).justify_end().children(line_actions));
        let header = v_flex()
            .flex_none()
            .bg(theme.list_head)
            .border_b_1()
            .border_color(theme.border)
            .child(title)
            .child(tools);

        let content = match focus.tab {
            FocusTab::Diff => self.render_focus_diff(focus, cx),
            FocusTab::File => match &focus.file_load {
                Some(Load::Ready(())) => split::styled_diff(&focus.file, cx)
                    .line_number(true)
                    .header_visible(false)
                    .size_full()
                    .into_any_element(),
                other => load_note(other.as_ref(), cx),
            },
            FocusTab::Blame => match &focus.blame_load {
                Some(Load::Ready(())) => self.render_blame(focus, cx),
                other => load_note(other.as_ref(), cx),
            },
        };

        v_flex()
            .id("file-focus")
            .key_context(FOCUS_CONTEXT)
            .track_focus(&focus.handle)
            .size_full()
            .min_h_0()
            .on_action(
                cx.listener(|view, _: &actions::CloseFileFocus, window, cx| {
                    view.close_focus(window, cx)
                }),
            )
            .on_action(
                cx.listener(|view, _: &actions::NextChange, _, cx| view.focus_change(true, cx)),
            )
            .on_action(
                cx.listener(|view, _: &actions::PreviousChange, _, cx| {
                    view.focus_change(false, cx)
                }),
            )
            .on_action(cx.listener(|view, _: &actions::NextFile, _, cx| view.focus_file(1, cx)))
            .on_action(
                cx.listener(|view, _: &actions::PreviousFile, _, cx| view.focus_file(-1, cx)),
            )
            .child(header)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    // with the side panel hidden, the history sits in the view itself
                    .when(focus.show_history && !self.sidebar_visible, |this| {
                        this.child(
                            div()
                                .w(gpui_kit::rems(24.))
                                .flex_none()
                                .h_full()
                                .border_r_1()
                                .border_color(cx.theme().border)
                                .child(self.render_history(focus, cx)),
                        )
                    })
                    .child(div().flex_1().min_w_0().h_full().child(content)),
            )
            .into_any_element()
    }

    /// The next or previous changed file of the selected revision.
    fn focus_file(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.detail.as_ref().map_or(0, |detail| detail.files.len());
        if count == 0 {
            return;
        }
        let current = self.selected_file.unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, count as isize - 1) as usize;
        self.select_file(Some(next), cx);
        self.retarget_focus(cx);
    }

    fn focus_change(&mut self, next: bool, cx: &mut Context<Self>) {
        if let Some(focus) = self.center.file() {
            focus.diff.update(cx, |state, cx| {
                if next {
                    state.next_change(cx)
                } else {
                    state.previous_change(cx)
                }
            });
        }
    }

    fn render_focus_diff(&self, focus: &FileFocus, cx: &mut Context<Self>) -> AnyElement {
        if !matches!(focus.diff_load, Load::Ready(())) {
            return load_note(Some(&focus.diff_load), cx);
        }
        // Discard and squash below every hunk, for a mutable revision with one parent.
        let editable = !focus.rev.is_immutable && focus.parent.is_some();
        let path: SharedString = focus.path.repo_path.clone().into();
        let annotations: Vec<DiffAnnotation> = if editable {
            focus
                .hunks
                .iter()
                .enumerate()
                .map(|(ix, hunk)| {
                    let (side, range) = if hunk.location.to_file.len > 0 {
                        (DiffSide::Modified, &hunk.location.to_file)
                    } else {
                        (DiffSide::Original, &hunk.location.from_file)
                    };
                    let last = range.start + range.len.saturating_sub(1);
                    DiffAnnotation::line(
                        ("hunk", ix),
                        DiffLinePosition::new(path.clone(), side, last),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let view = cx.entity().downgrade();
        let mut styled = split::styled_diff(&focus.diff, cx)
            .line_number(true)
            .header_visible(false);
        // in the working copy a hunk is staged, as its lines are
        let mut stage = false;
        if let Some(file) = self.focus_shown_file() {
            stage = self.line_command(&file) == Some(super::inspector::LineCommand::Stage);
            styled = self.with_line_buttons(styled, &file, cx);
        }
        let diff = styled
            .annotations(annotations)
            .render_annotation(move |annotation, _, cx| {
                let gpui_kit::ElementId::NamedInteger(_, ix) = annotation.id() else {
                    return div().into_any_element();
                };
                let ix = *ix as usize;
                let (discard, squash) = (view.clone(), view.clone());
                // at the start: with long lines the row is wider than the
                // view, and its end is off screen until scrolled to
                h_flex()
                    .px_3()
                    .py_1()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(if stage {
                        Button::new(("hunk-stage", ix))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .label("Stage hunk")
                            .on_click(move |_, window, cx| {
                                let _ = squash
                                    .update(cx, |view, cx| view.stage_focus_hunk(ix, window, cx));
                            })
                    } else {
                        Button::new(("hunk-squash", ix))
                            .ghost()
                            .xsmall()
                            .icon(IconName::ArrowDownToLine)
                            .label("Squash into parent")
                            .on_click(move |_, window, cx| {
                                let _ = squash
                                    .update(cx, |view, cx| view.squash_focus_hunk(ix, window, cx));
                            })
                    })
                    .child(
                        Button::new(("hunk-discard", ix))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Undo2)
                            .label("Discard hunk")
                            .tooltip("Restore the parent's lines")
                            .on_click(move |_, window, cx| {
                                let _ = discard
                                    .update(cx, |view, cx| view.discard_focus_hunk(ix, window, cx));
                            }),
                    )
                    .into_any_element()
            });
        self.split_diff("focus-split", diff, &focus.diff, cx)
    }

    pub(super) fn render_history(&self, focus: &FileFocus, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let body = match &focus.history_load {
            Some(Load::Ready(())) => {
                let rows = focus.history.clone();
                let shown = focus.rev.id.commit.hex.clone();
                let avatars = self.avatars.clone();
                let owner = cx.entity().downgrade();
                uniform_list("file-history", rows.len(), move |range, window, cx| {
                    let rem = window.rem_size();
                    range
                        .map(|ix| {
                            let (header, row) = &rows[ix];
                            let selected = header.id.commit.hex == shown;
                            let owner = owner.clone();
                            let header = header.clone();
                            let theme = cx.theme();
                            let avatar = avatars.get(row.author_email.as_ref()).cloned();
                            ListItem::new(("history", ix))
                                .w_full()
                                .px_3()
                                .py_1()
                                .selected(selected)
                                .child(
                                    h_flex()
                                        .w_full()
                                        .min_w_0()
                                        .gap_2()
                                        .child(crate::graph::avatar(
                                            &row.author,
                                            avatar,
                                            rem * 1.4,
                                            theme.border,
                                            false,
                                            cx,
                                        ))
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w_0()
                                                .child(div().text_sm().truncate().child(
                                                    row.summary.clone().unwrap_or_else(|| {
                                                        "(no description set)".into()
                                                    }),
                                                ))
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .truncate()
                                                        .text_color(theme.muted_foreground)
                                                        .child(format!(
                                                            "{} by {}",
                                                            row.time, row.author
                                                        )),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .text_xs()
                                                .font_family(theme.mono_font_family.clone())
                                                .text_color(theme.primary)
                                                .child(row.change_prefix.clone()),
                                        ),
                                )
                                .on_click(move |_, _, cx| {
                                    let header = header.clone();
                                    let _ = owner
                                        .update(cx, |view, cx| view.show_focus_rev(header, cx));
                                })
                                .into_any_element()
                        })
                        .collect()
                })
                .size_full()
                .into_any_element()
            }
            other => load_note(other.as_ref(), cx),
        };
        v_flex()
            .size_full()
            .overflow_hidden()
            .bg(theme.sidebar)
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if focus.history.len() >= HISTORY_LIMIT {
                        format!(
                            "The latest {HISTORY_LIMIT} revisions that changed {} from {}",
                            focus.name, focus.base.change.prefix
                        )
                    } else {
                        format!(
                            "History of {} from {}",
                            focus.name, focus.base.change.prefix
                        )
                    }),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    fn render_blame(&self, focus: &FileFocus, cx: &mut Context<Self>) -> AnyElement {
        let data = focus.blame.clone();
        let avatars = self.avatars.clone();
        let owner = cx.entity().downgrade();
        let shown = focus.rev.id.commit.hex.clone();
        div()
            .size_full()
            .relative()
            .child(
                uniform_list("blame", data.1.len(), move |range, window, cx| {
                    let rem = window.rem_size();
                    let theme = cx.theme();
                    range
                        .map(|ix| {
                            let line = &data.1[ix];
                            let block = &data.0[line.block];
                            // alternate blocks, so where one ends is visible
                            let band = if line.block.is_multiple_of(2) {
                                theme.background
                            } else {
                                theme.muted.opacity(0.35)
                            };
                            let gutter = if line.first {
                                let owner = owner.clone();
                                let rev = block.rev.clone();
                                let avatar = avatars.get(block.email.as_ref()).cloned();
                                h_flex()
                                    .id(("blame-rev", ix))
                                    .w_full()
                                    .min_w_0()
                                    .gap_1p5()
                                    .cursor_pointer()
                                    .child(crate::graph::avatar(
                                        &block.author,
                                        avatar,
                                        rem,
                                        theme.border,
                                        false,
                                        cx,
                                    ))
                                    .child(
                                        div()
                                            .flex_none()
                                            .font_family(theme.mono_font_family.clone())
                                            .text_color(if block.rev.id.commit.hex == shown {
                                                theme.foreground
                                            } else {
                                                theme.primary
                                            })
                                            .child(block.change.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(block.summary.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_color(theme.muted_foreground)
                                            .child(block.time.clone()),
                                    )
                                    .on_click(move |_, _, cx| {
                                        let rev = rev.clone();
                                        let _ = owner.update(cx, |view, cx| {
                                            if let Some(focus) = view.center.file_mut() {
                                                focus.tab = FocusTab::Diff;
                                            }
                                            view.show_focus_rev(rev, cx)
                                        });
                                    })
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            };
                            h_flex()
                                .h(rem * 1.5)
                                .w_full()
                                .bg(band)
                                .text_xs()
                                .child(
                                    div()
                                        .w(gpui_kit::rems(20.))
                                        .flex_none()
                                        .px_2()
                                        .overflow_hidden()
                                        .border_r_1()
                                        .border_color(theme.border)
                                        .child(gutter),
                                )
                                .child(
                                    div()
                                        .w(gpui_kit::rems(3.5))
                                        .flex_none()
                                        .pr_2()
                                        .flex()
                                        .justify_end()
                                        .font_family(theme.mono_font_family.clone())
                                        .text_color(theme.muted_foreground)
                                        .child((ix + 1).to_string()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .font_family(theme.mono_font_family.clone())
                                        .child(line.text.clone()),
                                )
                                .into_any_element()
                        })
                        .collect()
                })
                .track_scroll(&focus.blame_scroll)
                .size_full(),
            )
            .child(Scrollbar::vertical(&focus.blame_scroll))
            .into_any_element()
    }
}

impl FileFocus {
    /// What a load was asked for: a result for another revision or file is dropped.
    fn key(&self) -> (String, String) {
        (self.rev.id.commit.hex.clone(), self.path.repo_path.clone())
    }
}

/// Puts `focus.path`'s diff from `detail` into the view.
fn show_diff_of(focus: &mut FileFocus, detail: &Detail, cx: &mut Context<RepoView>) {
    focus.parent = detail
        .parent_ids
        .first()
        .cloned()
        .filter(|_| detail.parent_ids.len() == 1);
    focus.rev = detail.header.clone();
    let path = focus.path.repo_path.as_str();
    let file = detail
        .files
        .iter()
        .find(|file| file.tree_path.repo_path == path);
    // matched by path: a patch that does not parse leaves `patches` shorter than `files`
    let patch = detail
        .patches
        .iter()
        .find(|patch| patch.path().as_ref() == path)
        .cloned();
    // the lines selected were another revision's or file's
    focus.selection = None;
    match (file, patch) {
        (Some(file), Some(patch)) => {
            focus.hunks = file.hunks.clone();
            focus
                .diff
                .update(cx, |state, cx| state.set_files([patch], cx));
            focus.diff_load = Load::Ready(());
        }
        _ => {
            focus.hunks.clear();
            focus.diff_load = Load::Note("This revision does not change the file.".into());
        }
    }
}

/// Blame blocks and lines ready to draw.
fn blame_rows(
    annotation: FileAnnotation,
    now: DateTime<Local>,
) -> (Vec<BlameBlock>, Vec<BlameLine>) {
    let mut lines: Vec<BlameLine> = annotation
        .lines
        .into_iter()
        .map(|text| BlameLine {
            // tabs would otherwise collapse to nothing in a single-line text run
            text: text.replace('\t', "    ").into(),
            block: 0,
            first: false,
        })
        .collect();
    let blocks = annotation
        .blocks
        .into_iter()
        .enumerate()
        .map(|(ix, block)| {
            for line in lines.iter_mut().skip(block.start).take(block.len) {
                line.block = ix;
            }
            if let Some(line) = lines.get_mut(block.start) {
                line.first = true;
            }
            let rev = block.header;
            BlameBlock {
                change: rev.id.change.prefix.clone().into(),
                summary: rev
                    .description
                    .lines
                    .first()
                    .cloned()
                    .unwrap_or_default()
                    .into(),
                author: rev.author.name.clone().into(),
                email: rev.author.email.to_ascii_lowercase().into(),
                time: relative_time(rev.author.timestamp, now).into(),
                rev,
            }
        })
        .collect();
    (blocks, lines)
}

fn load_note(load: Option<&Load>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (text, color) = match load {
        None | Some(Load::Loading) | Some(Load::Ready(())) => {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(Spinner::new())
                .into_any_element();
        }
        Some(Load::Note(text)) => (text.clone(), theme.muted_foreground),
        Some(Load::Failed(text)) => (text.clone(), theme.danger),
    };
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .px_6()
        .text_sm()
        .text_color(color)
        .child(text)
        .into_any_element()
}
