//! The divider of a split diff: dragging it moves the boundary between the
//! original and modified sides, double-clicking it puts it back in the middle.
//! From the keyboard, the palette and the View menu widen either side or even
//! them. The ratio is a display setting, shared by every tab and diff.
//!
//! Split diffs wrap long lines, so each side fits its column and the divider
//! stays where it is drawn; with horizontal scrolling the two sides would
//! scroll together and the modified side would start off screen.

use gpui_kit::component::diff::DiffFileStatus;

use super::*;
use crate::settings::DEFAULT_SPLIT_RATIO;

/// The narrowest a side may get, as a share of the diff's width.
const MIN_SIDE: f32 = 0.15;
/// How far one keyboard command moves the divider.
const NUDGE: f32 = 0.05;

/// `ratio` within the sides' limits; anything unusable (a hand-edited
/// session) is the default.
fn clamp_ratio(ratio: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(MIN_SIDE, 1. - MIN_SIDE)
    } else {
        DEFAULT_SPLIT_RATIO
    }
}

/// A diff of `state` drawn as Settings says: how changed lines are marked
/// and whether code is highlighted.
pub(super) fn styled_diff(state: &Entity<DiffState>, cx: &App) -> Diff {
    use gpui_kit::component::diff::DiffChangeIndicator;
    let display = &settings::get(cx).display;
    let marks = match display.change_marks {
        settings::ChangeMarks::Signs => DiffChangeIndicator::Signs,
        settings::ChangeMarks::Bars => DiffChangeIndicator::Bars,
        settings::ChangeMarks::None => DiffChangeIndicator::None,
    };
    Diff::new(state)
        .change_indicator(marks)
        .syntax_highlight(display.syntax_highlight)
}

/// What the divider drags; `0` names the diff it belongs to, since the
/// inspector and the full file view can both be on screen.
pub(super) struct SplitResize(pub(super) &'static str);

/// Nothing follows the pointer while the divider moves.
struct DividerGhost;

impl Render for DividerGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl RepoView {
    /// The original side's share of a split diff, following a drag in progress.
    pub(super) fn split_ratio(&self, cx: &App) -> f32 {
        // clamped here so the divider is drawn where the diff splits
        clamp_ratio(
            self.split_drag
                .unwrap_or(settings::get(cx).display.split_ratio),
        )
    }

    /// The keyboard's divider: moves it by `delta` (towards the modified side
    /// when positive), or back to the middle with `None`. Shows the split
    /// layout first if the diff is unified, so the change can be seen.
    pub(super) fn nudge_split(&mut self, delta: Option<f32>, cx: &mut Context<Self>) {
        let ratio = match delta {
            Some(delta) => clamp_ratio(self.split_ratio(cx) + delta),
            None => DEFAULT_SPLIT_RATIO,
        };
        if self.diff_mode != DiffMode::Split {
            self.set_diff_mode(DiffMode::Split, cx);
        }
        settings::update_display(cx, |display| display.split_ratio = ratio);
    }

    pub(super) fn widen_original_side(&mut self, cx: &mut Context<Self>) {
        self.nudge_split(Some(NUDGE), cx);
    }

    pub(super) fn widen_modified_side(&mut self, cx: &mut Context<Self>) {
        self.nudge_split(Some(-NUDGE), cx);
    }

    /// Wraps `diff` with the divider when it shows two sides. `id` tells this
    /// diff's drag from another's; `state` is the diff's state.
    pub(super) fn split_diff(
        &self,
        id: &'static str,
        diff: Diff,
        state: &Entity<DiffState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ratio = self.split_ratio(cx);
        // unchanged and conflicted files have one column even in split mode
        let two_sides = self.diff_mode == DiffMode::Split
            && state.read(cx).files().iter().any(|file| {
                !matches!(
                    file.status(),
                    DiffFileStatus::Unchanged | DiffFileStatus::Conflicted
                )
            });
        let split = self.diff_mode == DiffMode::Split;
        let diff = diff.soft_wrap(split).split_ratio(ratio).size_full();
        if !two_sides {
            return div().size_full().child(diff).into_any_element();
        }
        // no line of its own: the diff draws the boundary on code rows, and a
        // line here would also cross the file and hunk header rows
        let hover = cx.theme().primary;
        let dragging = self.split_drag.is_some();
        div()
            .id(id)
            .relative()
            .size_full()
            .child(diff)
            .on_drag_move(cx.listener(
                move |view, event: &gpui_kit::DragMoveEvent<SplitResize>, _, cx| {
                    if event.drag(cx).0 != id {
                        return;
                    }
                    let width = f32::from(event.bounds.size.width);
                    if width <= 0. {
                        return;
                    }
                    let x = f32::from(event.event.position.x - event.bounds.left());
                    view.split_drag = Some(clamp_ratio(x / width));
                    cx.notify();
                },
            ))
            .child(
                div()
                    .id((id, 1usize))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(gpui_kit::relative(ratio))
                    .ml(gpui_kit::px(-4.))
                    .w(gpui_kit::px(8.))
                    .role(gpui_kit::Role::Splitter)
                    .aria_label("Resize split diff")
                    .cursor(gpui_kit::CursorStyle::ResizeColumn)
                    .hover(move |style| style.bg(hover.opacity(0.25)))
                    .when(dragging, |this| this.bg(hover.opacity(0.25)))
                    .on_mouse_down(
                        gpui_kit::MouseButton::Left,
                        cx.listener(|view, event: &gpui_kit::MouseDownEvent, _, cx| {
                            if event.click_count >= 2 {
                                view.split_drag = None;
                                settings::update_display(cx, |display| {
                                    display.split_ratio = DEFAULT_SPLIT_RATIO
                                });
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .on_drag(SplitResize(id), {
                        let view = cx.entity().downgrade();
                        move |_, _, _, cx| {
                            let _ = view.update(cx, |view, cx| {
                                view.split_drag = Some(view.split_ratio(cx))
                            });
                            cx.new(|_| DividerGhost)
                        }
                    }),
            )
            .into_any_element()
    }

    /// Saves the dragged ratio, which every diff then uses.
    pub(super) fn finish_split_resize(&mut self, cx: &mut Context<Self>) {
        if let Some(ratio) = self.split_drag.take() {
            settings::update_display(cx, |display| display.split_ratio = ratio);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratios_stay_where_the_diff_can_draw_them() {
        assert_eq!(clamp_ratio(0.6), 0.6);
        assert_eq!(clamp_ratio(0.95), 1. - MIN_SIDE, "a hand-edited session");
        assert_eq!(clamp_ratio(-1.), MIN_SIDE);
        assert_eq!(clamp_ratio(f32::NAN), DEFAULT_SPLIT_RATIO);
    }
}
