//! The revision list: column header, graph rows and the virtual list.

use super::*;

/// Fixed lanes shared by the log header and every row.
pub(super) mod lane {
    /// Branch / tag column, left of the graph like GitKraken's.
    pub const REFS: f32 = 8.;
    pub const CHANGE: f32 = 6.5;
    pub const AUTHOR: f32 = 7.;
    pub const TIME: f32 = 4.5;
}

// ----- rows ------------------------------------------------------------------------------------

/// One log row. `rem` is the window's rem, which the graph's avatars are
/// sized by; `avatar` is the author's picture when one is known.
pub(super) fn render_row(
    row: &RowData,
    graph: &Graph,
    ix: usize,
    selected: bool,
    avatar: Option<SharedString>,
    rem: gpui_kit::Pixels,
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
                // the row's branch colour, from its node to the end of the row
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .left(gpui_kit::rems(lane::REFS) + graph.node_x(ix))
                        .bg(color.opacity(0.07)),
                )
                // no gap between the two, so the chip's line runs on to its node
                .child(
                    h_flex()
                        .flex_none()
                        .h_full()
                        .child(render_ref_column(row, ix, color, cx))
                        .child(graph.row_element(
                            ix,
                            row.author.clone(),
                            avatar,
                            !row.refs.is_empty(),
                            rem,
                            cx,
                        )),
                )
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
                ),
        )
}

/// The branch / tag column: the row's first ref as a chip in its branch
/// colour, `+N` for the rest (all of them in the tooltip), and a line on to
/// the node.
fn render_ref_column(row: &RowData, ix: usize, color: gpui_kit::Hsla, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let Some(first) = row.refs.first() else {
        return div()
            .w(gpui_kit::rems(lane::REFS))
            .flex_none()
            .into_any_element();
    };
    let icon = match first.kind {
        RefKind::Local => IconName::GitBranch,
        RefKind::Remote => IconName::CloudDownload,
        RefKind::Tag => IconName::Tag,
    };
    let all: SharedString = row
        .refs
        .iter()
        .map(|chip| chip.name.as_ref())
        .collect::<Vec<_>>()
        .join("\n")
        .into();
    h_flex()
        .id(("refs", ix))
        .w(gpui_kit::rems(lane::REFS))
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
                .child(div().min_w_0().truncate().child(first.name.clone())),
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
                    .child(
                        div()
                            .w(gpui_kit::rems(lane::REFS) + self.graph.width())
                            .flex_none()
                            .truncate()
                            .child("Branch / Tag"),
                    )
                    .child(
                        div()
                            .w(gpui_kit::rems(lane::CHANGE))
                            .flex_none()
                            .child("Change"),
                    )
                    .child(div().flex_1().min_w_0().truncate().child("Description"))
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
                    ),
            )
    }
    pub(super) fn render_log(&self, cx: &mut Context<Self>) -> AnyElement {
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
                    .child(Scrollbar::vertical(&self.scroll)),
            )
            .into_any_element()
    }
}
