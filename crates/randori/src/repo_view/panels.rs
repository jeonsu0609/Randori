//! Toolbar, bookmarks panel, status bar and the resizable layout around them.

use super::*;

impl RepoView {
    /// The repository toolbar under the tab strip, laid out as GitKraken's:
    /// panel toggle and the workspace and bookmark switchers at the left, the
    /// repository commands in the middle (icon over name), Refresh and Search
    /// at the right.
    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let switchers = self.render_switchers(cx);
        let theme = cx.theme();
        let ready = matches!(self.state, LoadState::Ready);
        let has_selection = self.selected.is_some();
        let operation = self
            .workspace
            .as_ref()
            .map(|ws| ws.operation.clone())
            .unwrap_or_default();
        let can_redo = self.workspace.as_ref().is_some_and(|ws| ws.can_redo);
        // jj undo's: the latest operation, whoever made it
        let undo_tip: SharedString = format!("Undo: {}", operation_label(&operation)).into();
        let separator = || div().w_px().h_8().mx_1().bg(theme.border);

        h_flex()
            .h_12()
            .flex_none()
            .px_2()
            .gap_1()
            // the selected tab's shade, so the toolbar reads as its own
            .bg(theme.tab_active)
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        Button::new("toggle-sidebar")
                            .ghost()
                            .small()
                            .icon(IconName::PanelLeft)
                            .selected(self.sidebar_visible)
                            .tooltip("Show or hide the side panel (⌘B)")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.sidebar_visible = !view.sidebar_visible;
                                cx.notify();
                            })),
                    )
                    .child(switchers),
            )
            .child(
                h_flex()
                    .flex_none()
                    .h_full()
                    .gap_0p5()
                    .child(
                        tool_button("undo", IconName::Undo2, "Undo", undo_tip, ready, false, cx)
                            .on_click(cx.listener(|view, _, window, cx| view.undo(window, cx))),
                    )
                    .child(
                        tool_button(
                            "redo",
                            IconName::Redo2,
                            "Redo",
                            "Redo".into(),
                            ready && can_redo,
                            false,
                            cx,
                        )
                        .on_click(cx.listener(|view, _, window, cx| view.redo(window, cx))),
                    )
                    .child(separator())
                    .child(
                        tool_button(
                            "fetch",
                            IconName::CloudDownload,
                            "Fetch",
                            "Fetch all remotes (⇧⌘F)".into(),
                            ready,
                            false,
                            cx,
                        )
                        .on_click(cx.listener(|view, _, window, cx| view.fetch_all(window, cx))),
                    )
                    .child(
                        tool_button(
                            "push",
                            IconName::Upload,
                            "Push",
                            "Push the selection's bookmarks (⇧⌘P)".into(),
                            ready && has_selection,
                            false,
                            cx,
                        )
                        .on_click(
                            cx.listener(|view, _, window, cx| view.push_selected(window, cx)),
                        ),
                    )
                    .child(separator())
                    .child(
                        tool_button(
                            "bookmark",
                            IconName::BookmarkPlus,
                            "Bookmark",
                            "Create a bookmark on the selection (B)".into(),
                            ready && has_selection,
                            false,
                            cx,
                        )
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.prompt_create_bookmark(window, cx)
                        })),
                    )
                    .child(
                        tool_button(
                            "new-change",
                            IconName::Plus,
                            "New",
                            "New change on the selection (N)".into(),
                            ready && has_selection,
                            false,
                            cx,
                        )
                        .on_click(cx.listener(|view, _, window, cx| view.new_child(window, cx))),
                    )
                    .child(separator())
                    .child(
                        tool_button(
                            "terminal",
                            IconName::SquareTerminal,
                            "Terminal",
                            "Show or hide the terminal (⌃`)".into(),
                            ready,
                            self.terminal_open,
                            cx,
                        )
                        .on_click(
                            cx.listener(|view, _, window, cx| view.toggle_terminal(window, cx)),
                        ),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .justify_end()
                    .gap_0p5()
                    .child(
                        tool_button(
                            "refresh",
                            IconName::RefreshCw,
                            "Refresh",
                            "Reload the revisions (⌘R)".into(),
                            true,
                            false,
                            cx,
                        )
                        .on_click(cx.listener(|view, _, _, cx| view.refresh_from_disk(true, cx))),
                    )
                    .child(
                        tool_button(
                            "search",
                            IconName::Search,
                            "Search",
                            "Find a commit in the loaded revisions (⌘F)".into(),
                            ready,
                            self.find.is_some(),
                            cx,
                        )
                        .on_click(cx.listener(|view, _, window, cx| {
                            if view.find.is_some() {
                                view.close_find(window, cx)
                            } else {
                                view.open_find(window, cx)
                            }
                        })),
                    ),
            )
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
            .find(|row| is_this_working_copy(&row.revision))
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
                    // the latest operation: what Undo in the toolbar undoes
                    // (jj and other tools record operations too)
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
                                        .child(operation_label(&ws.operation)),
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
                    .when(self.has_more, |this| match self.more_error.clone() {
                        // the next page failed: the log loads again, its pages too
                        Some(error) => this.child(
                            Button::new("load-more")
                                .ghost()
                                .xsmall()
                                .icon(IconName::TriangleAlert)
                                .label("Retry")
                                .tooltip(format!("The next revisions did not load: {error}"))
                                .on_click(cx.listener(|view, _, _, cx| view.reload(cx))),
                        ),
                        None => this.child(
                            Button::new("load-more")
                                .ghost()
                                .xsmall()
                                .label(if self.loading_more {
                                    "Loading…"
                                } else {
                                    "Load more"
                                })
                                .on_click(cx.listener(|view, _, _, cx| view.load_more(cx))),
                        ),
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
                // its own layout with a pull request open, so dropping the
                // inspector does not hand its room to the other panels
                h_resizable(if self.center.pull().is_some() {
                    "workspace-pull"
                } else {
                    "workspace"
                })
                .when(self.sidebar_visible, |this| {
                    this.child(
                        resizable_panel()
                            .size(rem * 18.)
                            .size_range(rem * 14. ..rem * 28.)
                            .child(pane::cached(&self.panes.side)),
                    )
                })
                .child(
                    resizable_panel().size_range(rem * 30. ..rem * 200.).child(
                        match &self.center {
                            // a pull request takes the revisions' place, as in GitKraken
                            Center::Pull(pull) => self.render_pull(pull, cx),
                            // the full view of one file takes the revisions' place
                            Center::File(focus) => self.render_focus(focus, cx),
                            Center::Revisions => v_resizable("center")
                                .child(
                                    resizable_panel()
                                        .size(rem * 26.)
                                        .size_range(rem * 10. ..rem * 200.)
                                        .child(pane::cached(&self.panes.log)),
                                )
                                .child(
                                    resizable_panel()
                                        .size_range(rem * 8. ..rem * 200.)
                                        .child(pane::cached(&self.panes.changes)),
                                )
                                .into_any_element(),
                        },
                    ),
                )
                // a pull request uses the inspector's room too, as in GitKraken
                .when(self.center.pull().is_none(), |this| {
                    this.child(
                        resizable_panel()
                            .size(rem * 22.)
                            .size_range(rem * 18. ..rem * 36.)
                            .child(pane::cached(&self.panes.inspector)),
                    )
                })
                .into_any_element()
            }
        }
    }
}

/// A toolbar command as GitKraken draws one: the icon over its name. Off
/// when not `enabled`, and then a click does nothing; `selected` marks a
/// toggle that is on.
fn tool_button(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    tooltip: SharedString,
    enabled: bool,
    selected: bool,
    cx: &App,
) -> ToolButton {
    let theme = cx.theme();
    let hover = theme.muted;
    let button = v_flex()
        .id(id)
        .h_full()
        .min_w(gpui_kit::rems(3.5))
        .px_2()
        .items_center()
        .justify_center()
        .gap_0p5()
        .rounded(theme.radius)
        .text_color(if enabled {
            theme.foreground
        } else {
            theme.muted_foreground.opacity(0.5)
        })
        .when(selected, |this| this.bg(theme.accent))
        .when(enabled, |this| {
            this.cursor_pointer().hover(move |style| style.bg(hover))
        })
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
        })
        .child(gpui_kit::component::Icon::new(icon).small())
        .child(div().text_xs().child(label));
    ToolButton { button, enabled }
}

/// A [`tool_button`] waiting for what its click does.
struct ToolButton {
    button: gpui_kit::Stateful<gpui_kit::Div>,
    enabled: bool,
}

impl ToolButton {
    fn on_click(
        self,
        handler: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        if self.enabled {
            self.button.on_click(handler)
        } else {
            self.button
        }
    }
}

/// The status bar's name for an operation: jj describes an undo or redo by
/// the id of the operation it restores, which says nothing.
pub(super) fn operation_label(operation: &str) -> SharedString {
    if operation.starts_with("undo: ") {
        "undo".into()
    } else if operation.starts_with("redo: ") {
        "redo".into()
    } else {
        operation.to_owned().into()
    }
}
