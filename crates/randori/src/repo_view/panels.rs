//! Toolbar, bookmarks panel, status bar and the resizable layout around them.

use gpui_kit::component::menu::DropdownMenu as _;

use super::*;

impl RepoView {
    /// The repository toolbar under the tab strip: panel toggle, workspace,
    /// revset query and the repository commands.
    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let switchers = self.render_switchers(cx);
        let theme = cx.theme();
        let ready = matches!(self.state, LoadState::Ready);
        let can_redo = self.workspace.as_ref().is_some_and(|ws| ws.can_redo);
        let has_selection = self.selected.is_some();
        let command = |id: &'static str, icon: IconName, tooltip: &'static str| {
            Button::new(id).ghost().small().icon(icon).tooltip(tooltip)
        };

        h_flex()
            .h_10()
            .flex_none()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("toggle-sidebar")
                    .ghost()
                    .small()
                    .icon(IconName::PanelLeft)
                    .selected(self.sidebar_visible)
                    .tooltip("Show or hide bookmarks")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.sidebar_visible = !view.sidebar_visible;
                        cx.notify();
                    })),
            )
            .child(switchers)
            .child(div().flex_1())
            .when(ready, |bar| {
                // A typed revset is neither scope, so neither tab is selected.
                let scope = self.log_scope().map(usize::from);
                bar.child(
                    TabBar::new("log-scope")
                        .segmented()
                        .xsmall()
                        .when_some(scope, |tabs, ix| tabs.selected_index(ix))
                        .on_click(cx.listener(|view, ix: &usize, window, cx| {
                            view.set_log_scope(*ix == 1, window, cx)
                        }))
                        .child("Mine")
                        .child("All branches"),
                )
                .child(div().w_1())
                .child({
                    let by_date = settings::get(cx).display.log_by_date;
                    Button::new("log-order")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Clock)
                        .label("By date")
                        .selected(by_date)
                        .tooltip(if by_date {
                            "Newest first across branches · click to group by branch"
                        } else {
                            "Grouped by branch · click to list newest first across branches"
                        })
                        .on_click(|_, _, cx| {
                            settings::update_display(cx, |d| d.log_by_date = !d.log_by_date)
                        })
                })
                .child(div().w_2())
            })
            .when_some(
                self.workspace
                    .as_ref()
                    .filter(|ws| ready && !ws.presets.is_empty())
                    .map(|ws| (ws.presets.clone(), ws.query.clone())),
                |bar, (presets, query)| {
                    let view = cx.entity().downgrade();
                    bar.child(
                        Button::new("presets")
                            .ghost()
                            .xsmall()
                            .label("Presets")
                            .icon(IconName::ChevronDown)
                            .tooltip("Revsets from gg.presets")
                            .dropdown_menu_with_anchor(
                                gpui_kit::Anchor::TopRight,
                                move |mut menu, _, _| {
                                    for (title, revset) in &presets {
                                        let view = view.clone();
                                        let revset = revset.clone();
                                        menu =
                                            menu.item(
                                                gpui_kit::component::menu::PopupMenuItem::new(
                                                    title.clone(),
                                                )
                                                .checked(revset == query)
                                                .on_click(move |_, window, cx| {
                                                    let revset = revset.clone();
                                                    let _ = view.update(cx, |view, cx| {
                                                        view.set_query(revset, window, cx)
                                                    });
                                                }),
                                            );
                                    }
                                    menu
                                },
                            ),
                    )
                    .child(div().w_2())
                },
            )
            .child(
                div().w_72().child(
                    Input::new(&self.revset)
                        .small()
                        .prefix(gpui_kit::component::Icon::new(IconName::Search).xsmall())
                        .cleanable(true),
                ),
            )
            .child(div().w_2())
            .child(
                command(
                    "new-change",
                    IconName::Plus,
                    "New change on the selection (N)",
                )
                .disabled(!ready || !has_selection)
                .on_click(cx.listener(|view, _, window, cx| view.new_child(window, cx))),
            )
            .child(
                command("undo", IconName::Undo2, "Undo the last operation")
                    .disabled(!ready)
                    .on_click(cx.listener(|view, _, window, cx| view.undo(window, cx))),
            )
            .child(
                command("redo", IconName::Redo2, "Redo")
                    .disabled(!ready || !can_redo)
                    .on_click(cx.listener(|view, _, window, cx| view.redo(window, cx))),
            )
            .child(
                command("fetch", IconName::CloudDownload, "Fetch from all remotes")
                    .disabled(!ready)
                    .on_click(cx.listener(|view, _, window, cx| view.fetch_all(window, cx))),
            )
            .child(
                command("push", IconName::Upload, "Push the selection's bookmarks")
                    .disabled(!ready || !has_selection)
                    .on_click(cx.listener(|view, _, window, cx| view.push_selected(window, cx))),
            )
            .child(
                command("refresh", IconName::RefreshCw, "Reload revisions")
                    .on_click(cx.listener(|view, _, _, cx| view.reload(cx))),
            )
    }
    pub(super) fn render_refs_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let pulls = self.render_pulls(cx);
        let theme = cx.theme();
        let filter = self.ref_filter;
        let visible: Vec<&RefItem> = self
            .refs
            .iter()
            .filter(|item| match filter {
                1 => item.chip.kind == RefKind::Local,
                2 => item.chip.kind == RefKind::Remote,
                3 => item.chip.kind == RefKind::Tag,
                _ => true,
            })
            .collect();
        let selected_row = self.selected;
        let mark_unpushed = settings::get(cx).mark_unpushed;

        v_flex()
            .size_full()
            .min_h_0()
            .bg(theme.sidebar)
            .children(pulls)
            .child(
                h_flex()
                    .h_10()
                    .px_4()
                    .gap_2()
                    .flex_none()
                    .child(div().text_sm().font_medium().child("Bookmarks"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(self.refs.len().to_string()),
                    ),
            )
            .child(
                div().px_3().pb_2().flex_none().child(
                    TabBar::new("ref-filter")
                        .pill()
                        .xsmall()
                        .selected_index(filter)
                        .on_click(cx.listener(|view, ix: &usize, _, cx| {
                            view.ref_filter = *ix;
                            cx.notify();
                        }))
                        .child("All")
                        .child("Local")
                        .child("Remote")
                        .child("Tags"),
                ),
            )
            .child(div().h_px().flex_none().bg(theme.border))
            .child(
                v_flex()
                    .id("refs")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .when(visible.is_empty(), |this| {
                        this.child(
                            div()
                                .p_4()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("No bookmarks in the loaded revisions."),
                        )
                    })
                    .children(visible.into_iter().enumerate().map(|(ix, item)| {
                        let row = item.row;
                        let store = item.chip.store.clone();
                        let menu_ref = item.chip.store.clone();
                        let icon = match item.chip.kind {
                            RefKind::Local => IconName::GitBranch,
                            RefKind::Remote => IconName::CloudDownload,
                            RefKind::Tag => IconName::Tag,
                        };
                        ListItem::new(("ref", ix))
                            .px_4()
                            .py_1p5()
                            .selected(selected_row == Some(row))
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .gap_3()
                                    .child(
                                        gpui_kit::component::Icon::new(icon)
                                            .small()
                                            .text_color(theme.muted_foreground),
                                    )
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(
                                                h_flex()
                                                    .gap_1p5()
                                                    .min_w_0()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .font_medium()
                                                            .truncate()
                                                            .child(item.chip.name.clone()),
                                                    )
                                                    .when(
                                                        mark_unpushed && item.chip.unpushed,
                                                        |this| this.child(unpushed_mark(cx)),
                                                    ),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_1()
                                                    .min_w_0()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(
                                                        div()
                                                            .flex_none()
                                                            .font_family(
                                                                theme.mono_font_family.clone(),
                                                            )
                                                            .text_color(theme.primary)
                                                            .child(item.change.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .truncate()
                                                            .child(item.summary.clone()),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(item.time.clone()),
                                    ),
                            )
                            .on_click(cx.listener(move |view, _, _, cx| view.select(row, cx)))
                            .on_mouse_down(
                                gpui_kit::MouseButton::Right,
                                cx.listener(move |view, _, window, cx| {
                                    view.ref_target = Some(store.clone());
                                    view.command_focus().focus(window, cx);
                                }),
                            )
                            .on_drag(
                                DraggedRef {
                                    store: item.chip.store.clone(),
                                    label: item.chip.name.clone(),
                                },
                                |dragged, _, _, cx| {
                                    cx.new(|_| DragChip {
                                        label: format!("Move {}", dragged.label).into(),
                                        icon: IconName::Bookmark,
                                    })
                                },
                            )
                            .context_menu({
                                let focus = self.command_focus();
                                move |menu, _, _| ref_menu(menu, &menu_ref, focus.clone())
                            })
                    })),
            )
            .into_any_element()
    }
    pub(super) fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (state_color, state_label) = match &self.state {
            LoadState::Loading => (theme.warning, "Loading"),
            LoadState::Failed(_) => (theme.danger, "Error"),
            LoadState::Ready => (theme.success, "Ready"),
        };
        let working_copy = self
            .rows
            .iter()
            .find(|row| row.revision.is_working_copy)
            .map(|row| row.revision.id.change.prefix.clone());
        StatusBar::new()
            .left(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .child(div().size_2().rounded_full().bg(state_color))
                    .child(state_label)
                    // A running jj operation replaces the last-operation note.
                    .when_some(self.busy.last().cloned(), |this, busy| {
                        let others = self.busy.len() - 1;
                        this.child(div().text_color(theme.muted_foreground).child("·"))
                            .child(Spinner::new().xsmall())
                            .child(busy)
                            .when(others > 0, |this| {
                                this.child(
                                    div()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("+{others} more")),
                                )
                            })
                            .when_some(self.progress.clone(), |this, progress| {
                                this.child(div().text_color(theme.muted_foreground).child(progress))
                            })
                    })
                    .when_some(
                        self.workspace.as_ref().filter(|_| self.busy.is_empty()),
                        |this, ws| {
                            this.child(div().text_color(theme.muted_foreground).child("·"))
                                .child(
                                    div()
                                        .min_w_0()
                                        .max_w_128()
                                        .truncate()
                                        .text_color(theme.muted_foreground)
                                        .child(ws.operation.clone()),
                                )
                        },
                    ),
            )
            .right(
                h_flex()
                    .gap_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .when_some(working_copy, |this, change| {
                        this.child(
                            h_flex().gap_1().child("@").child(
                                div()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(theme.primary)
                                    .child(change),
                            ),
                        )
                    })
                    .child(format!(
                        "{} revisions{}",
                        self.rows.len(),
                        if self.has_more { "+" } else { "" }
                    ))
                    .when(self.has_more, |this| {
                        this.child(
                            Button::new("load-more")
                                .ghost()
                                .xsmall()
                                .label(if self.loading_more {
                                    "Loading…"
                                } else {
                                    "Load more"
                                })
                                .on_click(cx.listener(|view, _, _, cx| view.load_more(cx))),
                        )
                    }),
            )
    }
    pub(super) fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match &self.state {
            LoadState::Loading => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(Spinner::new())
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Opening workspace…"),
                )
                .into_any_element(),
            LoadState::Failed(message) => {
                // jj-lib's wording when the directory has no .jj (see Workspace::load).
                let not_a_repo = message.contains("There is no Jujutsu repo");
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .p_8()
                    .child(div().font_medium().child(if not_a_repo {
                        "This folder isn't a jj repository"
                    } else {
                        "Couldn't open the workspace"
                    }))
                    .child(
                        div()
                            .max_w_128()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(message.clone()),
                    )
                    .child(
                        h_flex()
                            .mt_2()
                            .gap_2()
                            .when(not_a_repo, |this| {
                                this.child(
                                    Button::new("init-repo")
                                        .primary()
                                        .label("Create a jj repository here")
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.init_repository(window, cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new("retry-open")
                                    .outline()
                                    .label("Try again (⌘R)")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.retry_open(window, cx)
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            LoadState::Ready => {
                let rem = window.rem_size();
                h_resizable("workspace")
                    .when(self.sidebar_visible, |this| {
                        this.child(
                            resizable_panel()
                                .size(rem * 18.)
                                .size_range(rem * 14. ..rem * 28.)
                                // the full view's file history takes the side panel's place,
                                // as GitKraken's does; the panel layout stays as it was
                                .child(match &self.focus {
                                    Some(focus) if focus.show_history => {
                                        self.render_history(focus, cx)
                                    }
                                    _ => self.render_refs_panel(cx),
                                }),
                        )
                    })
                    .child(
                        resizable_panel().size_range(rem * 30. ..rem * 200.).child(
                            match &self.focus {
                                // the full view of one file takes the revisions' place
                                Some(focus) => self.render_focus(focus, cx),
                                None => v_resizable("center")
                                    .child(
                                        resizable_panel()
                                            .size(rem * 26.)
                                            .size_range(rem * 10. ..rem * 200.)
                                            .child(self.render_log(cx)),
                                    )
                                    .child(
                                        resizable_panel()
                                            .size_range(rem * 8. ..rem * 200.)
                                            .child(self.render_changes(cx)),
                                    )
                                    .into_any_element(),
                            },
                        ),
                    )
                    .child(
                        resizable_panel()
                            .size(rem * 22.)
                            .size_range(rem * 18. ..rem * 36.)
                            .child(self.render_inspector(cx)),
                    )
                    .into_any_element()
            }
        }
    }
}

/// "Not pushed" next to a local bookmark its remotes do not have.
pub(super) fn unpushed_mark(cx: &App) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .flex_none()
        .gap_0p5()
        .text_xs()
        .text_color(theme.warning)
        .child(gpui_kit::component::Icon::new(IconName::Upload).xsmall())
        .child("not pushed")
        .into_any_element()
}
