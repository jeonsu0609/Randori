//! Two revisions compared, as GitKraken's ⌘-click on a second commit: the
//! inspector lists what changes from one tree to the other (`jj diff --from
//! --to`) and the changes panel shows a file's diff. The revision lower in the
//! log, older as the log is ordered, is the side compared from.
//!
//! A comparison is something to look at. The revision commands stay with the
//! selected revision, which the log still marks. The files' menu and the line
//! buttons are not drawn, and the commands on a file from the palette or a
//! key, and Describe, wait until it ends (`blocked_by_comparison`): they would
//! act on the selected revision's file, which the comparison hides. A plain
//! click on a row, Esc, or ⌘-click on either revision again ends it. While a
//! target revision is being chosen (pick mode) ⌘-click chooses, as a click
//! does.

use gpui_kit::component::{WindowExt as _, notification::Notification};

use super::*;
use crate::harness_mark::HarnessMark as _;

/// The revision ⌘-clicked besides the selected one, and what changes between
/// the two.
pub(super) struct Comparison {
    /// Its row in the log.
    pub(super) row: usize,
    /// Its change, to find the row again when the log reloads.
    change: String,
    /// The selected revision's change when the comparison started: a reload
    /// that leaves another selected (the revision abandoned, a new change
    /// made) ends the comparison rather than compare what was not chosen.
    selected_change: String,
    pub(super) detail: Option<Detail>,
    pub(super) loading: bool,
    /// The file whose diff shows.
    pub(super) file: Option<usize>,
    /// The query of the detail; a newer comparison drops the last one.
    task: Option<Task<()>>,
    /// The commits (from, to) of the detail shown: a reload that leaves both
    /// as they were asks nothing again. Unset when a query failed, so the
    /// next reload tries again.
    shown: Option<(String, String)>,
    /// How many queries were sent (the tests').
    #[cfg(test)]
    pub(super) queries: usize,
}

impl RepoView {
    /// The row compared with the selected one, if any.
    pub(super) fn compared_row(&self) -> Option<usize> {
        self.compare.as_ref().map(|compare| compare.row)
    }

    /// A ⌘-click on row `ix`: compares it with the selected revision. On the
    /// selected row, or on the one compared, it ends the comparison. While a
    /// target revision is being chosen, it chooses, as a click does.
    pub(super) fn click_to_compare(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(selected) = self.selected.filter(|_| self.pick.is_none()) else {
            self.select(ix, cx);
            return;
        };
        if ix == selected || self.compared_row() == Some(ix) {
            self.end_comparison(cx);
            return;
        }
        let (Some(row), Some(selected_row)) = (self.log.rows.get(ix), self.log.rows.get(selected))
        else {
            return;
        };
        self.compare = Some(Comparison {
            row: ix,
            change: row.revision.id.change.hex.clone(),
            selected_change: selected_row.revision.id.change.hex.clone(),
            detail: None,
            // until `load_comparison` asks
            loading: false,
            file: None,
            task: None,
            shown: None,
            #[cfg(test)]
            queries: 0,
        });
        self.load_comparison(cx);
        cx.notify();
    }

    /// While comparing, the commands on a file (from the palette or a key)
    /// and Describe wait: they act on the selected revision's file and
    /// description, which the comparison hides, not on what it shows.
    pub(super) fn blocked_by_comparison(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.compare.is_none() {
            return false;
        }
        window.push_notification(
            Notification::info("Press Esc to go back to the selected revision.")
                .title("Stop comparing first"),
            cx,
        );
        true
    }

    /// Back to the selected revision, its file's diff in the changes panel.
    pub(super) fn end_comparison(&mut self, cx: &mut Context<Self>) {
        if self.compare.take().is_some() {
            self.select_file(self.inspected.file, cx);
            cx.notify();
        }
    }

    /// After the log reloads: the compared revision's row, found again by its
    /// change, and the comparison asked for again when either side was
    /// rewritten. Gone, now the selected row, or with another revision
    /// selected than the one it started from, it ends.
    pub(super) fn follow_comparison(&mut self, cx: &mut Context<Self>) {
        let Some((change, selected_change)) = self
            .compare
            .as_ref()
            .map(|compare| (compare.change.clone(), compare.selected_change.clone()))
        else {
            return;
        };
        let still_selected = self
            .selected
            .and_then(|ix| self.log.rows.get(ix))
            .is_some_and(|row| row.revision.id.change.hex == selected_change);
        let row = self
            .log
            .rows
            .iter()
            .position(|row| row.revision.id.change.hex == change)
            .filter(|row| still_selected && Some(*row) != self.selected);
        match row {
            Some(row) => {
                if let Some(compare) = self.compare.as_mut() {
                    compare.row = row;
                }
                let same = self.compared_ids().is_some_and(|(from, to)| {
                    self.compare.as_ref().and_then(|c| c.shown.as_ref())
                        == Some(&(from.commit.hex, to.commit.hex))
                });
                if !same {
                    self.load_comparison(cx);
                }
            }
            None => self.end_comparison(cx),
        }
    }

    /// The two revisions compared, the one lower in the log first.
    fn compared_ids(&self) -> Option<(RevId, RevId)> {
        let (a, b) = (self.selected?, self.compared_row()?);
        let id = |ix: usize| Some(self.log.rows.get(ix)?.revision.id.clone());
        Some(if a > b {
            (id(a)?, id(b)?)
        } else {
            (id(b)?, id(a)?)
        })
    }

    fn load_comparison(&mut self, cx: &mut Context<Self>) {
        let Some((from, to)) = self.compared_ids() else {
            return;
        };
        let Some(compare) = self.compare.as_mut() else {
            return;
        };
        compare.loading = true;
        #[cfg(test)]
        {
            compare.queries += 1;
        }
        let asked = (from.commit.hex.clone(), to.commit.hex.clone());
        let change = compare.change.clone();
        let worker = self.worker.clone();
        compare.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { worker.query_comparison(from, to).map(build_detail) },
                )
                .await;
            let _ = this.update(cx, |view, cx| {
                let Some(compare) = view.compare.as_mut().filter(|c| c.change == change) else {
                    return;
                };
                compare.loading = false;
                let detail = result.unwrap_or_else(|err| {
                    log::warn!("comparison query failed: {err:#}");
                    None
                });
                compare.shown = detail.is_some().then_some(asked);
                // after a reload, stay on the file that was shown
                let previous = compare
                    .file
                    .and_then(|ix| compare.detail.as_ref()?.files.get(ix))
                    .map(|file| file.path.clone());
                let file = detail.as_ref().and_then(|detail| {
                    previous
                        .and_then(|path| detail.files.iter().position(|file| file.path == path))
                        .or((!detail.files.is_empty()).then_some(0))
                });
                compare.detail = detail;
                view.select_compared_file(file, cx);
            });
        }));
    }

    /// Shows file `ix` of the comparison in the changes panel.
    pub(super) fn select_compared_file(&mut self, ix: Option<usize>, cx: &mut Context<Self>) {
        let Some(compare) = self.compare.as_mut() else {
            return;
        };
        compare.file = ix;
        let patch = ix.and_then(|ix| file_patch(compare.detail.as_ref()?, ix));
        self.line_selection = None;
        self.diff.update(cx, |state, cx| state.set_files(patch, cx));
        cx.notify();
    }

    /// The comparison's file shown, while there is a comparison.
    pub(super) fn compared_file(&self) -> Option<&FileItem> {
        let compare = self.compare.as_ref()?;
        compare.detail.as_ref()?.files.get(compare.file?)
    }

    /// The inspector while comparing: the two revisions, and the files that
    /// differ between them.
    pub(super) fn render_comparison(
        &self,
        compare: &Comparison,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let rows = &self.log.row_data;
        let lower = self.selected.max(Some(compare.row));
        let upper = self.selected.min(Some(compare.row));
        let card = |label: &'static str, ix: Option<usize>| {
            let row = ix.and_then(|ix| rows.get(ix));
            v_flex()
                .gap_0p5()
                .min_w_0()
                // the label, the change and the time keep their width; the
                // author gives way in a narrow inspector
                .child(
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(div().flex_none().child(label))
                        .when_some(row, |this, row| {
                            this.child(
                                h_flex()
                                    .flex_none()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(
                                        div()
                                            .font_semibold()
                                            .text_color(theme.primary)
                                            .child(row.change_prefix.clone()),
                                    )
                                    .child(row.change_rest.clone()),
                            )
                            .child(
                                div()
                                    .ml_auto()
                                    .min_w_0()
                                    .truncate()
                                    .child(row.author.clone()),
                            )
                            .child(div().flex_none().child(format!("· {}", row.time)))
                        }),
                )
                .child(div().text_sm().truncate().map(|this| {
                    match row.and_then(|row| row.summary.clone()) {
                        Some(summary) => this.child(summary),
                        None => this
                            .italic()
                            .text_color(theme.muted_foreground)
                            .child("(no description set)"),
                    }
                }))
        };

        let head = v_flex()
            .px_4()
            .pt_4()
            .pb_3()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_base()
                            .font_medium()
                            .child("Comparing two revisions"),
                    )
                    .child(
                        Button::new("end-comparison")
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip("Stop comparing (Esc)")
                            .on_click(cx.listener(|view, _, _, cx| view.end_comparison(cx))),
                    ),
            )
            .child(card("From", lower))
            .child(card("To", upper))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Esc, a click on a revision or ⌘-click on either ends it"),
            );

        let body = match &compare.detail {
            None if compare.loading => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .py_6()
                .child(Spinner::new())
                .into_any_element(),
            None => div()
                .px_4()
                .py_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("The two revisions could not be compared")
                .into_any_element(),
            Some(detail) => {
                let (additions, deletions) = detail.files.iter().fold((0, 0), |(a, d), file| {
                    (a + file.additions, d + file.deletions)
                });
                v_flex()
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
                    .when(detail.files.is_empty(), |this| {
                        this.child(
                            div()
                                .px_4()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("The two trees are the same"),
                        )
                    })
                    .children(detail.files.iter().enumerate().map(|(ix, file)| {
                        ListItem::new(("compared-file", ix))
                            .harness_mark(|| format!("compared:{}", file.tree_path.repo_path))
                            .px_4()
                            .py_1()
                            .map(|item| {
                                super::log_list::select_row(
                                    item,
                                    compare.file == Some(ix),
                                    true,
                                    theme,
                                )
                            })
                            .child(super::inspector::file_summary(file, theme))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.select_compared_file(Some(ix), cx)
                            }))
                    }))
                    .into_any_element()
            }
        };

        let inspector = v_flex()
            .id("comparison")
            .size_full()
            .min_h_0()
            .bg(theme.sidebar)
            .overflow_y_scroll()
            .track_scroll(&self.inspector_scroll)
            .child(head)
            .child(body);
        div()
            .relative()
            .size_full()
            .min_h_0()
            .child(inspector)
            .child(Scrollbar::vertical(&self.inspector_scroll))
            .into_any_element()
    }
}
