//! Pick mode: the keyboard path for drag and drop. An operation waits for a
//! target revision, chosen with the selection and applied with Enter.

use super::*;

/// An operation in pick mode. The selection moves as usual; Enter applies it
/// to the selected revision, the same as dropping onto that row.
pub(super) enum Pick {
    Rebase {
        id: RevId,
        label: SharedString,
    },
    MoveRef {
        store: StoreRef,
        label: SharedString,
    },
    /// A local bookmark's branch, rebased as `jj rebase -b`.
    RebaseBranch {
        bookmark_name: String,
    },
    MoveFile {
        path: TreePath,
        from: RevId,
        label: SharedString,
    },
}

impl Pick {
    fn prompt(&self) -> String {
        match self {
            Pick::Rebase { label, .. } => format!("Choose the new parent for {label}"),
            Pick::MoveRef { label, .. } => format!("Choose where {label} should point"),
            Pick::RebaseBranch { bookmark_name } => {
                format!("Choose the revision to rebase {bookmark_name}'s branch onto")
            }
            Pick::MoveFile { label, .. } => format!("Choose the revision to move {label} into"),
        }
    }

    fn verb(&self) -> &'static str {
        match self {
            Pick::Rebase { .. } | Pick::RebaseBranch { .. } => "Rebase here",
            Pick::MoveRef { .. } | Pick::MoveFile { .. } => "Move here",
        }
    }
}

impl RepoView {
    pub(super) fn start_pick(&mut self, pick: Pick, window: &mut Window, cx: &mut Context<Self>) {
        self.pick = Some(pick);
        self.log_focus.focus(window, cx);
        cx.notify();
    }
    pub(super) fn pick_rebase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refuse_if_immutable("Rebase", window, cx) {
            return;
        }
        let Some(header) = self.selected_header() else {
            return;
        };
        let pick = Pick::Rebase {
            id: header.id.clone(),
            label: short_id(&header.id.change.prefix, &header.id.change.rest, 8).into(),
        };
        self.start_pick(pick, window, cx);
    }
    pub(super) fn pick_move_bookmark(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = self.ref_target.clone() else {
            return;
        };
        let label = match &store {
            StoreRef::LocalBookmark { bookmark_name, .. }
            | StoreRef::RemoteBookmark { bookmark_name, .. } => bookmark_name.clone(),
            StoreRef::Tag { tag_name } => tag_name.clone(),
        };
        self.start_pick(
            Pick::MoveRef {
                store,
                label: label.into(),
            },
            window,
            cx,
        );
    }
    /// The bookmark menu's "Rebase onto…": its branch, onto the revision
    /// chosen next.
    pub(super) fn pick_rebase_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(StoreRef::LocalBookmark { bookmark_name, .. }) = self.ref_target.clone() else {
            return;
        };
        self.start_pick(Pick::RebaseBranch { bookmark_name }, window, cx);
    }
    /// The palette's "Rebase current branch onto…": the branch of the bookmark
    /// the toolbar shows, onto the revision chosen next.
    pub(super) fn pick_rebase_current_branch(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self.current_bookmark() else {
            gpui_kit::component::WindowExt::push_notification(
                window,
                gpui_kit::component::notification::Notification::info("No bookmark is near the working copy: right-click one in the side panel → Rebase onto…")
                    .title("Rebase current branch"),
                cx,
            );
            return;
        };
        self.start_pick(
            Pick::RebaseBranch {
                bookmark_name: name.to_string(),
            },
            window,
            cx,
        );
    }
    pub(super) fn pick_move_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refuse_if_immutable("Move file", window, cx) {
            return;
        }
        let Some((detail, file)) = self
            .inspected
            .detail
            .as_ref()
            .and_then(|detail| Some((detail, detail.files.get(self.inspected.file?)?)))
        else {
            return;
        };
        let pick = Pick::MoveFile {
            path: file.tree_path.clone(),
            from: detail.header.id.clone(),
            label: file.name.clone(),
        };
        self.start_pick(pick, window, cx);
    }
    /// Enter in pick mode: apply the operation to the selected revision.
    pub(super) fn confirm_pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(pick), Some(target)) = (self.pick.take(), self.selected) else {
            cx.propagate();
            return;
        };
        match pick {
            Pick::Rebase { id, .. } => self.rebase_onto(id, target, window, cx),
            Pick::MoveRef { store, .. } => self.pick_move_ref_to(store, target, window, cx),
            Pick::RebaseBranch { bookmark_name } => {
                if let Some(parent) = self.log.rows.get(target).map(|row| row.revision.id.clone()) {
                    self.rebase_branch(bookmark_name, parent, window, cx)
                }
            }
            Pick::MoveFile { path, from, .. } => self.move_file_to(path, from, target, window, cx),
        }
        cx.notify();
    }
    pub(super) fn cancel_pick(&mut self, cx: &mut Context<Self>) {
        if self.pick.take().is_none() {
            // Not picking: let Escape reach the workspace (back to the list).
            cx.propagate();
        }
        cx.notify();
    }
    /// The pick-mode prompt above the revision list.
    pub(super) fn render_pick_bar(&self, pick: &Pick, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .h_10()
            .flex_none()
            .px_4()
            .gap_3()
            .bg(theme.primary.opacity(0.12))
            .border_b_1()
            .border_color(theme.primary.opacity(0.4))
            .child(
                div().flex_none().child(
                    gpui_kit::component::Icon::new(IconName::MousePointerClick)
                        .small()
                        .text_color(theme.primary),
                ),
            )
            // narrowed, the texts give way (the keys' hint first) and the
            // buttons keep their size
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_medium()
                    .child(pick.prompt()),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .map(|mut hint| {
                        hint.style().flex_shrink = Some(8.);
                        hint
                    })
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("↑↓ to choose · Enter to apply · Esc to cancel"),
            )
            .child(div().flex_1())
            .child(
                h_flex()
                    .flex_none()
                    .gap_3()
                    .child(
                        Button::new("pick-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.pick = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("pick-apply")
                            .primary()
                            .small()
                            .label(pick.verb())
                            .disabled(self.selected.is_none())
                            .on_click(
                                cx.listener(|view, _, window, cx| view.confirm_pick(window, cx)),
                            ),
                    ),
            )
    }
}
