//! The revision list: column header, graph rows and the virtual list.

use gpui_kit::{Rems, canvas, px};

use super::*;

/// The resizable columns of the revision list. The description takes the
/// width the others leave.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Column {
    /// Branch / tag chips, left of the graph like GitKraken's.
    Refs,
    Graph,
    Change,
    Author,
}

impl Column {
    /// Narrowest width in rems, so a column can be found again.
    fn min(self) -> f32 {
        match self {
            Column::Refs | Column::Graph => 2.,
            Column::Change => 4.,
            Column::Author => 3.,
        }
    }

    /// Columns left of the description grow with a drag to the right; the
    /// ones right of it grow with a drag to the left.
    fn grows_right(self) -> bool {
        matches!(self, Column::Refs | Column::Graph | Column::Change)
    }
}

/// Widths of the header and every row, with the graph's automatic width resolved.
#[derive(Clone, Copy)]
pub(super) struct Columns {
    pub(super) refs: Rems,
    pub(super) graph: Rems,
    pub(super) change: Rems,
    pub(super) author: Rems,
    /// Room right of the author for the age labels, when the list is wide
    /// enough; otherwise the labels sit over the row's right end.
    pub(super) age_gutter: Option<Rems>,
    /// Too narrow for names: the author column shows avatars only.
    pub(super) compact_author: bool,
    /// The optional columns Settings shows; a hidden one is 0 wide.
    pub(super) show_refs: bool,
    pub(super) show_change: bool,
    pub(super) show_author: bool,
}

/// The narrowest description before the author column gives way.
const MIN_DESCRIPTION: f32 = 11.;
/// The age labels' own room on a wide list.
const AGE_GUTTER: f32 = 6.5;
/// The author column with avatars only.
const COMPACT_AUTHOR: f32 = 1.75;
/// Row padding and the gaps between the columns, in rems.
const ROW_CHROME: f32 = 3.5;

/// How the columns fit `width` rems, GitKraken style: a gutter for the age
/// labels when there is room, then labels over the row end, then avatars
/// instead of author names. An unknown width (0) keeps the names, no gutter.
pub(super) fn fit_columns(width: f32, fixed: f32, author: f32) -> (Option<f32>, bool) {
    if width <= 0. {
        return (None, false);
    }
    let room = width - ROW_CHROME - fixed - MIN_DESCRIPTION;
    if room >= author + AGE_GUTTER {
        (Some(AGE_GUTTER), false)
    } else if room >= author {
        (None, false)
    } else {
        (None, true)
    }
}

/// What a column handle drags; which column is in `RepoView::column_drag`.
pub(super) struct ColumnResize;

/// Nothing follows the pointer while a column is resized.
struct ResizeGhost;

impl Render for ResizeGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

// ----- rows ------------------------------------------------------------------------------------

/// One log row. `rem` is the window's rem, which the graph's avatars are
/// sized by; `avatar` is the author's picture when one is known.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_row(
    row: &RowData,
    graph: &Graph,
    ix: usize,
    selected: bool,
    avatar: Option<SharedString>,
    rem: gpui_kit::Pixels,
    cols: Columns,
    cx: &App,
) -> ListItem {
    let theme = cx.theme();
    let color = graph.node_color(ix, cx);
    ListItem::new(("revision", ix))
        .selected(selected)
        .h(row_height())
        .pl_3()
        .pr_3()
        .py_0()
        .w_full()
        .child(
            h_flex()
                .relative()
                .h_full()
                .gap_2()
                .min_w_0()
                .flex_1()
                .text_sm()
                // where an age group starts, a hairline across the row
                .when(row.bucket_start && ix > 0, |this| {
                    this.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h(px(1.))
                            .bg(theme.border),
                    )
                })
                // the row's branch colour, from its node to the end of the row
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        // a node clipped by a narrow graph starts the tint at the edge
                        .left(cols.refs + gpui_kit::rems(graph.node_x(ix).0.min(cols.graph.0)))
                        .bg(color.opacity(0.07)),
                )
                // no gap between the two, so the chip's line runs on to its node
                .child(
                    h_flex()
                        .flex_none()
                        .h_full()
                        .when(cols.show_refs, |this| {
                            this.child(render_ref_column(row, ix, color, cols.refs, cx))
                        })
                        .child(graph.row_element(
                            ix,
                            row.author.clone(),
                            avatar.clone(),
                            !row.refs.is_empty(),
                            cols.graph,
                            rem,
                            cx,
                        )),
                )
                .when(cols.show_change, |this| {
                    this.child(
                        h_flex()
                            .w(cols.change)
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
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
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
                .when(cols.show_author, |this| {
                    this.child(if cols.compact_author {
                        div()
                            .w(cols.author)
                            .flex_none()
                            .flex()
                            .justify_center()
                            .child(crate::graph::avatar(
                                &row.author,
                                avatar,
                                rem * 1.25,
                                theme.border,
                                false,
                                cx,
                            ))
                    } else {
                        div()
                            .w(cols.author)
                            .flex_none()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(row.author.clone())
                    })
                })
                .when_some(cols.age_gutter, |this, gutter| {
                    this.child(div().w(gutter).flex_none())
                })
                // the age of a new group, GitKraken style: a label on the hairline
                // where the group starts, over the row's right end rather than in
                // a column of its own (row 0 has no line above, so it sits inside)
                .when(row.bucket_start, |this| {
                    this.child(
                        div()
                            .absolute()
                            .right_0()
                            .map(|label| {
                                if ix == 0 {
                                    label.top(px(3.))
                                } else {
                                    label.top(px(-9.))
                                }
                            })
                            .h(px(18.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(9.))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.secondary)
                            .text_xs()
                            .whitespace_nowrap()
                            .text_color(theme.muted_foreground)
                            .child(row.bucket_label.clone()),
                    )
                }),
        )
}

/// The branch / tag column: the row's first ref as a chip in its branch
/// colour, `+N` for the rest (all of them in the tooltip), and a line on to
/// the node.
fn render_ref_column(
    row: &RowData,
    ix: usize,
    color: gpui_kit::Hsla,
    width: Rems,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let Some(first) = row.refs.first() else {
        return div().w(width).flex_none().into_any_element();
    };
    let icon = match first.kind {
        RefKind::Local => IconName::GitBranch,
        RefKind::Remote => IconName::CloudDownload,
        RefKind::Tag => IconName::Tag,
    };
    let mark_unpushed = crate::settings::get(cx).mark_unpushed;
    let all: SharedString = row
        .refs
        .iter()
        .map(|chip| {
            if mark_unpushed && chip.unpushed {
                format!("{} (not pushed)", chip.name)
            } else {
                chip.name.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .into();
    let first_unpushed = mark_unpushed && first.unpushed;
    h_flex()
        .id(("refs", ix))
        .w(width)
        .flex_none()
        .h_full()
        .items_center()
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(all.clone()).build(window, cx)
        })
        .child(
            h_flex()
                .min_w_0()
                .flex_shrink(1.)
                .h_5()
                .px_1p5()
                .gap_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(color)
                // a tag's chip is lighter, as a tag is not a branch
                .when(first.kind == RefKind::Tag, |chip| chip.border_dashed())
                .bg(color.opacity(0.16))
                .text_xs()
                .text_color(theme.foreground)
                .child(
                    gpui_kit::component::Icon::new(icon)
                        .xsmall()
                        .text_color(color),
                )
                .child(div().min_w_0().truncate().child(first.name.clone()))
                .when(first_unpushed, |chip| {
                    chip.child(
                        gpui_kit::component::Icon::new(IconName::Upload)
                            .xsmall()
                            .text_color(theme.warning),
                    )
                }),
        )
        .when(row.refs.len() > 1, |this| {
            this.child(
                div()
                    .flex_none()
                    .ml_1()
                    .px_1()
                    .rounded(theme.radius)
                    .bg(color.opacity(0.16))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("+{}", row.refs.len() - 1)),
            )
        })
        .child(div().flex_1().min_w_1().h(gpui_kit::px(1.5)).bg(color))
        .into_any_element()
}

impl RepoView {
    pub(super) fn render_log_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cols = self.columns(cx);
        let header = [
            header_cell("Branch / Tag", cols.refs, Column::Refs, false, cx),
            header_cell("Graph", cols.graph, Column::Graph, false, cx),
            header_cell("Change", cols.change, Column::Change, false, cx),
            if cols.compact_author {
                div()
                    .w(cols.author)
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(gpui_kit::component::Icon::new(IconName::User).xsmall())
                    .into_any_element()
            } else {
                header_cell("Author", cols.author, Column::Author, false, cx)
            },
        ];
        let [refs_cell, graph_cell, change_cell, author_cell] = header;
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
                    .on_drag_move(cx.listener(
                        |view, event: &gpui_kit::DragMoveEvent<ColumnResize>, window, cx| {
                            view.resize_column(event.event.position.x, window.rem_size(), cx)
                        },
                    ))
                    // no gap between the two, as in the rows
                    .child(
                        h_flex()
                            .flex_none()
                            .h_full()
                            .when(cols.show_refs, |this| this.child(refs_cell))
                            .child(graph_cell),
                    )
                    .when(cols.show_change, |this| this.child(change_cell))
                    .child(div().flex_1().min_w_0().truncate().child("Description"))
                    .when(cols.show_author, |this| this.child(author_cell))
                    .when_some(cols.age_gutter, |this, gutter| {
                        this.child(div().w(gutter).flex_none())
                    }),
            )
    }

    /// Resolved widths of the columns.
    pub(super) fn columns(&self, cx: &App) -> Columns {
        let shown = settings::get(cx).display.visible_columns;
        let c = self.columns;
        let rems = gpui_kit::rems;
        let width = |show: bool, width: f32| if show { width } else { 0. };
        let (refs, change, author) = (
            width(shown.refs, c.refs),
            width(shown.change, c.change),
            width(shown.author, c.author),
        );
        let graph = c.graph.map(rems).unwrap_or_else(|| self.graph.width());
        let (age_gutter, compact_author) =
            fit_columns(self.log_width.get(), refs + graph.0 + change, author);
        Columns {
            refs: rems(refs),
            graph,
            change: rems(change),
            author: rems(if compact_author && shown.author {
                COMPACT_AUTHOR
            } else {
                author
            }),
            age_gutter: age_gutter.map(rems),
            compact_author: compact_author && shown.author,
            show_refs: shown.refs,
            show_change: shown.change,
            show_author: shown.author,
        }
    }

    fn column_width(&self, column: Column, cx: &App) -> f32 {
        let cols = self.columns(cx);
        match column {
            Column::Refs => cols.refs.0,
            Column::Graph => cols.graph.0,
            Column::Change => cols.change.0,
            Column::Author => cols.author.0,
        }
    }

    fn start_column_resize(&mut self, column: Column, x: gpui_kit::Pixels, cx: &App) {
        self.column_drag = Some((column, x, self.column_width(column, cx)));
    }

    fn resize_column(
        &mut self,
        x: gpui_kit::Pixels,
        rem: gpui_kit::Pixels,
        cx: &mut Context<Self>,
    ) {
        let Some((column, start_x, start)) = self.column_drag else {
            return;
        };
        let moved = f32::from(x - start_x) / f32::from(rem);
        let width = if column.grows_right() {
            start + moved
        } else {
            start - moved
        }
        .clamp(column.min(), 60.);
        let c = &mut self.columns;
        match column {
            Column::Refs => c.refs = width,
            Column::Graph => c.graph = Some(width),
            Column::Change => c.change = width,
            Column::Author => c.author = width,
        }
        cx.notify();
    }

    /// Saves the dragged widths, which every tab then uses.
    pub(super) fn finish_column_resize(&mut self, cx: &mut Context<Self>) {
        if self.column_drag.take().is_some() {
            let columns = self.columns;
            settings::update_display(cx, |display| display.columns = columns);
        }
    }

    /// A double click on a handle puts the column back to its default width
    /// (the graph back to fitting its lanes).
    fn reset_column(&mut self, column: Column, cx: &mut Context<Self>) {
        let default = settings::ColumnWidths::default();
        let c = &mut self.columns;
        match column {
            Column::Refs => c.refs = default.refs,
            Column::Graph => c.graph = default.graph,
            Column::Change => c.change = default.change,
            Column::Author => c.author = default.author,
        }
        let columns = self.columns;
        settings::update_display(cx, |display| display.columns = columns);
    }

    pub(super) fn render_log(&self, cx: &mut Context<Self>) -> AnyElement {
        let cols = self.columns(cx);
        let rows = self.row_data.clone();
        let graph = self.graph.clone();
        let focus = self.log_focus.clone();
        let selected = self.selected;
        let avatars = self.avatars.clone();
        let github = self.github_active();
        let owner = cx.entity().downgrade();

        v_flex()
            .size_full()
            .min_h_0()
            .child(self.render_log_header(cx))
            .when_some(self.pick.as_ref(), |this, pick| {
                this.child(self.render_pick_bar(pick, cx))
            })
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
                    .on_action(cx.listener(|view, _: &actions::ConfirmPick, window, cx| {
                        view.confirm_pick(window, cx)
                    }))
                    .on_action(
                        cx.listener(|view, _: &actions::CancelPick, _, cx| view.cancel_pick(cx)),
                    )
                    .on_action(cx.listener(|view, _: &actions::SelectFirst, _, cx| {
                        view.move_selection(isize::MIN, cx)
                    }))
                    .on_action(cx.listener(|view, _: &actions::SelectLast, _, cx| {
                        view.move_selection(isize::MAX, cx)
                    }))
                    .child(
                        uniform_list("revisions", rows.len(), move |range, window, cx| {
                            let rem = window.rem_size();
                            range
                                .map(|ix| {
                                    let owner = owner.clone();
                                    let menu_owner = owner.clone();
                                    let drop_owner = owner.clone();
                                    let focus = focus.clone();
                                    let email = rows[ix].author_email.as_ref();
                                    let avatar = avatars.get(email).cloned().or_else(|| {
                                        github
                                            .then(|| crate::github::noreply_avatar(email))
                                            .flatten()
                                            .map(SharedString::from)
                                    });
                                    render_row(
                                        &rows[ix],
                                        &graph,
                                        ix,
                                        selected == Some(ix),
                                        avatar,
                                        rem,
                                        cols,
                                        cx,
                                    )
                                    .on_click(move |_, _, cx| {
                                        let _ = owner.update(cx, |view, cx| view.select(ix, cx));
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
                                                label: format!("Rebase {}", dragged.label).into(),
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
                                                view.rebase_onto(dragged.id.clone(), ix, window, cx)
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
                                    .drag_over::<DraggedFile>(|style, _, _, cx| {
                                        style.bg(cx.theme().primary.opacity(0.18))
                                    })
                                    .on_drop({
                                        let owner = drop_owner.clone();
                                        move |dragged: &DraggedFile, window, cx| {
                                            let _ = owner.update(cx, |view, cx| {
                                                view.move_file_to(
                                                    dragged.path.clone(),
                                                    dragged.from.clone(),
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
                    .child(Scrollbar::vertical(&self.scroll))
                    // measures the list, so the columns can fit its width
                    .child({
                        let width = self.log_width.clone();
                        canvas(
                            move |bounds, window, _| {
                                let rems =
                                    f32::from(bounds.size.width) / f32::from(window.rem_size());
                                if (width.get() - rems).abs() > 0.25 {
                                    width.set(rems);
                                    window.refresh();
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full()
                    }),
            )
            .into_any_element()
    }
}

/// A header cell with a drag handle on the edge next to the description side
/// (right of the left columns, left of the right ones). Drag to resize, double
/// click to restore the default width.
fn header_cell(
    label: &'static str,
    width: Rems,
    column: Column,
    align_end: bool,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let theme = cx.theme();
    let (line, hover) = (theme.border, theme.primary);
    // between the branch / tag and graph columns there is no gap, so the
    // handle straddles the edge; elsewhere it sits in the gap
    let outside = if column == Column::Refs {
        px(4.)
    } else {
        px(8.)
    };
    let handle = div()
        .id(("column-resize", column as usize))
        .absolute()
        .top_0()
        .bottom_0()
        .w(px(8.))
        .map(|handle| {
            if column.grows_right() {
                handle.right(-outside)
            } else {
                handle.left(-outside)
            }
        })
        .flex()
        .justify_center()
        .cursor(gpui_kit::CursorStyle::ResizeColumn)
        .hover(move |style| style.bg(hover.opacity(0.25)))
        .child(div().w(px(1.)).h_full().bg(line))
        .on_mouse_down(
            gpui_kit::MouseButton::Left,
            cx.listener(move |view, event: &gpui_kit::MouseDownEvent, _, cx| {
                if event.click_count >= 2 {
                    view.reset_column(column, cx);
                } else {
                    // where the drag below measures from
                    view.column_press = Some(event.position.x);
                }
            }),
        )
        // the resize starts with the drag itself, so a press that never
        // becomes a drag leaves nothing behind
        .on_drag(ColumnResize, {
            let view = cx.entity().downgrade();
            move |_, _, window, cx| {
                let moved_to = window.mouse_position().x;
                let _ = view.update(cx, |view, cx| {
                    let x = view.column_press.take().unwrap_or(moved_to);
                    view.start_column_resize(column, x, cx)
                });
                cx.new(|_| ResizeGhost)
            }
        });
    div()
        .relative()
        .w(width)
        .flex_none()
        .h_full()
        .flex()
        .items_center()
        .when(align_end, |cell| cell.justify_end())
        .child(div().min_w_0().truncate().child(label))
        .child(handle)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::fit_columns;

    #[test]
    fn columns_give_way_as_the_list_narrows() {
        // fixed columns 20rem, author 7rem: 3.5 + 20 + 11 = 34.5 before the author
        assert_eq!(
            fit_columns(80., 20., 7.),
            (Some(6.5), false),
            "wide: a gutter"
        );
        assert_eq!(
            fit_columns(46., 20., 7.),
            (None, false),
            "labels over the row end"
        );
        assert_eq!(fit_columns(40., 20., 7.), (None, true), "avatars only");
        assert_eq!(fit_columns(0., 20., 7.), (None, false), "not measured yet");
    }
}
