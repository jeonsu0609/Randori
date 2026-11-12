//! A part of the tab drawn as a view of its own, so GPUI can keep it from
//! one frame to the next. Scrolling the revisions tells only the view that
//! draws them to draw again (and, as GPUI marks the views above it, the tab);
//! without panes the side panel, the inspector and the diff would be drawn
//! again with every frame of a scroll.
//!
//! A pane draws with one of `RepoView`'s render functions and draws again
//! whenever the tab notifies, as it would if it were the tab's own element.

use gpui_kit::{AnyView, WeakEntity};

use super::*;

/// What a pane draws: one of the tab's render functions.
pub(super) type PaneRender = fn(&RepoView, &mut Context<RepoView>) -> AnyElement;

pub(super) struct Pane {
    /// Weak, as the tab holds its panes: a strong handle each way would keep
    /// a closed tab alive.
    owner: WeakEntity<RepoView>,
    render: PaneRender,
    _tab_changes: gpui_kit::Subscription,
}

impl Pane {
    /// A pane of `owner` drawn by `render`.
    pub(super) fn new(
        owner: &Entity<RepoView>,
        render: PaneRender,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            owner: owner.downgrade(),
            render,
            _tab_changes: cx.observe(owner, |_, _, cx| cx.notify()),
        }
    }
}

impl Render for Pane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render = self.render;
        // the tab is not being drawn while its panes are: its render has
        // returned the element they are laid out in. Gone, it is closing,
        // and there is nothing to draw.
        self.owner
            .update(cx, |view, cx| render(view, cx))
            .unwrap_or_else(|_| div().into_any_element())
    }
}

/// `pane` as an element kept between frames while it is not notified.
pub(super) fn cached(pane: &Entity<Pane>) -> AnyElement {
    AnyView::from(pane.clone())
        .cached(gpui_kit::StyleRefinement::default().size_full())
        .into_any_element()
}
