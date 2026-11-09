//! The revision list: column header, graph rows and the virtual list.

use super::*;

/// Ref chips shown inline in a log row; the rest collapse into a `+N` chip.
pub(super) const MAX_ROW_REFS: usize = 1;

/// Fixed lanes shared by the log header and every row.
pub(super) mod lane {
    pub const CHANGE: f32 = 6.5;
    pub const AUTHOR: f32 = 7.5;
    pub const TIME: f32 = 5.;
    pub const COMMIT: f32 = 5.;
}

// ----- rows ------------------------------------------------------------------------------------

pub(super) fn render_row(
    row: &RowData,
    graph: &Graph,
    ix: usize,
    selected: bool,
    cx: &App,
) -> ListItem {
    let theme = cx.theme();
    ListItem::new(("revision", ix))
        .selected(selected)
        .h(row_height())
        .pl_3()
        .pr_3()
        .py_0()
        .w_full()
        .child(
            h_flex()
                .h_full()
                .gap_2()
                .min_w_0()
                .flex_1()
                .text_sm()
                .child(graph.row_canvas(ix, cx))
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
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .gap_1p5()
                        .children(
                            row.refs
                                .iter()
                                .take(MAX_ROW_REFS)
                                .map(|chip| ref_tag(chip, cx)),
                        )
                        .when(row.refs.len() > MAX_ROW_REFS, |this| {
                            this.child(
                                Tag::secondary()
                                    .outline()
                                    .xsmall()
                                    .child(format!("+{}", row.refs.len() - MAX_ROW_REFS)),
                            )
                        })
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
                )
                .child(
                    div()
                        .w(gpui_kit::rems(lane::COMMIT))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(theme.muted_foreground)
                        .child(row.commit.clone()),
                ),
        )
}

pub(super) fn ref_tag(chip: &RefChip, _cx: &App) -> Tag {
    let tag = match chip.kind {
        RefKind::Local => Tag::info(),
        RefKind::Remote => Tag::secondary(),
        RefKind::Tag => Tag::warning(),
    };
    tag.outline().xsmall().child(chip.name.clone())
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
                    .child(div().w(self.graph.width()).flex_none())
                    .child(
                        div()
                            .w(gpui_kit::rems(lane::CHANGE))
                            .flex_none()
                            .child("Change"),
                    )
                    .child(div().flex_1().min_w_0().child("Description"))
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
                    )
                    .child(
                        div()
                            .w(gpui_kit::rems(lane::COMMIT))
                            .flex_none()
                            .flex()
                            .justify_end()
                            .child("Commit"),
                    ),
            )
    }
    pub(super) fn render_log(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.row_data.clone();
        let graph = self.graph.clone();
        let focus = self.log_focus.clone();
        let selected = self.selected;
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
                        uniform_list("revisions", rows.len(), move |range, _, cx| {
                            range
                                .map(|ix| {
                                    let owner = owner.clone();
                                    let menu_owner = owner.clone();
                                    let drop_owner = owner.clone();
                                    let focus = focus.clone();
                                    render_row(&rows[ix], &graph, ix, selected == Some(ix), cx)
                                        .on_click(move |_, _, cx| {
                                            let _ =
                                                owner.update(cx, |view, cx| view.select(ix, cx));
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
                                                    label: format!("Rebase {}", dragged.label)
                                                        .into(),
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
                                                    view.rebase_onto(
                                                        dragged.id.clone(),
                                                        ix,
                                                        window,
                                                        cx,
                                                    )
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
