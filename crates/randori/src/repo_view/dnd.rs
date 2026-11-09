//! Drag and drop payloads and the chip that follows the pointer.

use super::*;

// ----- drag and drop ---------------------------------------------------------------------------

/// A revision being dragged; dropping it on another revision rebases it there.
#[derive(Clone)]
pub(super) struct DraggedRevision {
    pub(super) id: RevId,
    pub(super) label: SharedString,
}

/// A bookmark or tag being dragged; dropping it on a revision moves it there.
#[derive(Clone)]
pub(super) struct DraggedRef {
    pub(super) store: StoreRef,
    pub(super) label: SharedString,
}

/// A changed file being dragged from the inspector; dropping it on a revision
/// moves the file's changes there.
#[derive(Clone)]
pub(super) struct DraggedFile {
    pub(super) path: TreePath,
    pub(super) from: RevId,
    pub(super) label: SharedString,
}

/// The chip that follows the pointer during a drag.
pub(super) struct DragChip {
    pub(super) label: SharedString,
    pub(super) icon: IconName,
}

impl Render for DragChip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .gap_1p5()
            .px_2()
            .py_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.primary)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .text_xs()
            .shadow_md()
            .child(gpui_kit::component::Icon::new(self.icon).xsmall())
            .child(self.label.clone())
    }
}
