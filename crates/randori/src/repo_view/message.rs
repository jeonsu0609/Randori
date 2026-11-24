//! A commit message edited as Git spells one and GitKraken asks for one: a
//! summary on one line, counted against Settings' subject limits, then a
//! description; kept as the summary, a blank line and the description. Both
//! fields sit in one box, as GitKraken's do, so the message reads as one.

use std::{cell::Cell, rc::Rc};

use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};

use crate::harness_mark::HarnessMark as _;

use super::*;

/// The summary and description fields of a message.
pub(super) struct MessageEditor {
    summary: Entity<InputState>,
    body: Entity<TextareaState>,
    /// Which field has focus (bit 0 the summary, bit 1 the description), for
    /// the box's focus ring: the fields draw none of their own.
    focused: Rc<Cell<u8>>,
}

impl MessageEditor {
    /// Fields hinting `summary_hint` and `body_hint`. `submit` runs on ⌘↩ in
    /// either; Enter in the summary goes on to the description.
    pub(super) fn new(
        summary_hint: &'static str,
        body_hint: &'static str,
        submit: fn(&mut RepoView, &mut Window, &mut Context<RepoView>),
        subscriptions: &mut Vec<Subscription>,
        window: &mut Window,
        cx: &mut Context<RepoView>,
    ) -> Self {
        let summary = cx.new(|cx| InputState::new(window, cx).placeholder(summary_hint));
        let lines = clamp_lines(settings::get(cx).display.message_lines);
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(MIN_LINES as usize, lines as usize)
                .placeholder(body_hint)
        });
        let focused = Rc::new(Cell::new(0));
        let next = body.clone();
        let summary_focus = focused.clone();
        subscriptions.push(cx.subscribe_in(
            &summary,
            window,
            move |view: &mut RepoView, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter {
                    secondary: true, ..
                } => submit(view, window, cx),
                InputEvent::PressEnter { .. } => {
                    next.update(cx, |state, cx| state.focus(window, cx))
                }
                InputEvent::Change => view.notify_pane(PaneId::Inspector, cx),
                event => note_focus(view, &summary_focus, 1, event, cx),
            },
        ));
        let body_focus = focused.clone();
        subscriptions.push(cx.subscribe_in(
            &body,
            window,
            move |view: &mut RepoView, _, event: &InputEvent, window, cx| match event {
                // a plain Enter is a newline
                InputEvent::PressEnter {
                    secondary: true, ..
                } => submit(view, window, cx),
                InputEvent::Change => view.notify_pane(PaneId::Inspector, cx),
                event => note_focus(view, &body_focus, 2, event, cx),
            },
        ));
        Self {
            summary,
            body,
            focused,
        }
    }

    /// The message: the summary, and after a blank line the description.
    pub(super) fn text(&self, cx: &App) -> String {
        join_message(&self.summary.read(cx).value(), &self.body.read(cx).value())
    }

    /// The summary's length in characters, for the subject limits.
    pub(super) fn summary_len(&self, cx: &App) -> usize {
        self.summary.read(cx).value().trim().chars().count()
    }

    /// Puts `text` in the fields: its first line in the summary, the rest
    /// in the description.
    pub(super) fn set(&self, text: &str, window: &mut Window, cx: &mut App) {
        let (summary, body) = split_message(text);
        self.summary
            .update(cx, |state, cx| state.set_value(summary, window, cx));
        self.body
            .update(cx, |state, cx| state.set_value(body, window, cx));
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.summary.update(cx, |state, cx| state.focus(window, cx));
    }

    /// The summary as typed.
    pub(super) fn summary(&self, cx: &App) -> SharedString {
        self.summary.read(cx).value()
    }

    /// The summary's field, for what follows what is typed in it.
    pub(super) fn summary_field(&self) -> &Entity<InputState> {
        &self.summary
    }

    /// Puts `text` in the summary alone, the description left as it is.
    pub(super) fn set_summary(&self, text: SharedString, window: &mut Window, cx: &mut App) {
        self.summary
            .update(cx, |state, cx| state.set_value(text, window, cx));
    }

    pub(super) fn focus_body(&self, window: &mut Window, cx: &mut App) {
        self.body.update(cx, |state, cx| state.focus(window, cx));
    }

    /// The description grows to `lines` lines, then scrolls.
    pub(super) fn set_lines(&self, lines: u32, cx: &mut App) {
        self.body.update(cx, |state, cx| {
            state.set_auto_grow(MIN_LINES as usize, lines as usize, cx)
        });
    }

    /// The box of the two fields, the summary with its length against
    /// `limits` (long past the first, too long past the second).
    pub(super) fn render(&self, limits: (u32, u32), cx: &App) -> AnyElement {
        let theme = cx.theme();
        let len = self.summary_len(cx);
        let (long, too_long) = (limits.0 as usize, limits.1 as usize);
        let color = if len > too_long {
            theme.danger
        } else if len > long {
            theme.warning
        } else {
            theme.muted_foreground
        };
        let tip: SharedString =
            format!("Summary: {len} characters · aim for {long}, at most {too_long}").into();
        v_flex()
            .py_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(if self.focused.get() == 0 {
                theme.input
            } else {
                theme.ring
            })
            .bg(theme.background)
            .child(
                // a mark takes children, which an Input does not
                div().harness_mark(|| "summary").child(
                    Input::new(&self.summary)
                        .appearance(false)
                        .text_base()
                        .font_medium()
                        .suffix(
                            div()
                                .id("summary-length")
                                .text_xs()
                                .font_normal()
                                .text_color(color)
                                .child(format!("{len} / {too_long}"))
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(tip.clone())
                                        .build(window, cx)
                                }),
                        ),
                ),
            )
            .child(Textarea::new(&self.body).appearance(false).small())
            .into_any_element()
    }
}

/// The fewest and the most lines a description's box shows.
const MIN_LINES: u32 = 2;
const MAX_LINES: u32 = 40;
/// A line of a read-only description, the unit of its box's height.
pub(super) const LINE_REMS: f32 = 1.25;

/// `lines` within the box's limits; a hand-edited session's too.
pub(super) fn clamp_lines(lines: u32) -> u32 {
    lines.clamp(MIN_LINES, MAX_LINES)
}

/// The lines after the handle is dragged `dy` pixels from where it showed
/// `from` lines, a line being `line` pixels high.
fn dragged_lines(from: u32, dy: f32, line: f32) -> u32 {
    let lines = from as f32 + (dy / line).round();
    clamp_lines(lines.max(0.) as u32)
}

/// What the message box's handle drags.
pub(super) struct MessageResize;

/// The message box's handle dragged: from where, from how many lines, and
/// the lines now.
#[derive(Clone, Copy)]
pub(super) struct MessageDrag {
    from_y: gpui_kit::Pixels,
    from: u32,
    lines: u32,
}

impl RepoView {
    /// Lines a description's box shows, following a drag in progress.
    pub(super) fn message_lines(&self, cx: &App) -> u32 {
        clamp_lines(
            self.message_drag
                .map_or(settings::get(cx).display.message_lines, |drag| drag.lines),
        )
    }

    /// Sizes the description fields' boxes as `message_lines` says.
    pub(super) fn show_message_lines(&mut self, cx: &mut Context<Self>) {
        let lines = self.message_lines(cx);
        self.description.editor.set_lines(lines, cx);
        self.next_commit.message.set_lines(lines, cx);
    }

    /// The handle under a message box, as GitKraken's: dragged, it makes the
    /// box taller or shorter (by lines), and a double click puts it back.
    pub(super) fn render_message_handle(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (bar, hover) = (theme.border, theme.primary);
        let dragging = self.message_drag.is_some();
        h_flex()
            .id("message-handle")
            .harness_mark(|| "message-handle")
            .group("message-handle")
            .h(gpui_kit::px(10.))
            .w_full()
            .justify_center()
            .items_center()
            .role(gpui_kit::Role::Splitter)
            .aria_label("Resize the message")
            .cursor(gpui_kit::CursorStyle::ResizeRow)
            .child(
                div()
                    .w(gpui_kit::px(40.))
                    .h(gpui_kit::px(4.))
                    .rounded_full()
                    .bg(if dragging { hover } else { bar })
                    .group_hover("message-handle", move |style| style.bg(hover)),
            )
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|view, event: &gpui_kit::MouseDownEvent, _, cx| {
                    if event.click_count >= 2 {
                        view.message_press = None;
                        view.message_drag = None;
                        settings::update_display(cx, |display| {
                            display.message_lines = settings::DEFAULT_MESSAGE_LINES
                        });
                    } else {
                        view.message_press = Some((event.position.y, view.message_lines(cx)));
                    }
                    cx.stop_propagation();
                }),
            )
            .on_drag(MessageResize, {
                let view = cx.entity().downgrade();
                move |_, _, _, cx| {
                    let _ = view.update(cx, |view, _| {
                        view.message_drag = view.message_press.map(|(from_y, from)| MessageDrag {
                            from_y,
                            from,
                            lines: from,
                        });
                    });
                    cx.new(|_| super::split_diff::DividerGhost)
                }
            })
            .into_any_element()
    }

    /// The pointer moved with the handle: the box follows, by lines.
    pub(super) fn drag_message(
        &mut self,
        event: &gpui_kit::DragMoveEvent<MessageResize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.message_drag.as_mut() else {
            return;
        };
        let line = LINE_REMS * f32::from(window.rem_size());
        let lines = dragged_lines(
            drag.from,
            f32::from(event.event.position.y - drag.from_y),
            line,
        );
        if lines != drag.lines {
            drag.lines = lines;
            self.show_message_lines(cx);
            self.notify_pane(PaneId::Inspector, cx);
        }
    }

    /// Saves the dragged lines, which every tab's box then shows.
    pub(super) fn finish_message_resize(&mut self, cx: &mut Context<Self>) {
        self.message_press = None;
        if let Some(drag) = self.message_drag.take() {
            settings::update_display(cx, |display| display.message_lines = drag.lines);
        }
    }
}

/// Records a focus change of `field` (a bit of `focused`), drawing the box
/// again when it changes whether the box has focus.
fn note_focus(
    view: &mut RepoView,
    focused: &Cell<u8>,
    field: u8,
    event: &InputEvent,
    cx: &mut Context<RepoView>,
) {
    let before = focused.get();
    match event {
        InputEvent::Focus => focused.set(before | field),
        InputEvent::Blur => focused.set(before & !field),
        _ => return,
    }
    if (before == 0) != (focused.get() == 0) {
        view.notify_pane(PaneId::Inspector, cx);
    }
}

/// `text` as a summary (its first line) and a description (the lines after,
/// without the blank lines that part them).
pub(super) fn split_message(text: &str) -> (String, String) {
    let text = text.trim();
    let (summary, rest) = text.split_once('\n').unwrap_or((text, ""));
    (summary.trim().to_owned(), rest.trim().to_owned())
}

/// A message of `summary` and `body`: a blank line between, none without a
/// body.
pub(super) fn join_message(summary: &str, body: &str) -> String {
    let (summary, body) = (summary.trim(), body.trim());
    if body.is_empty() {
        summary.to_owned()
    } else {
        format!("{summary}\n\n{body}")
    }
}

/// `text` as it would read back from the fields: a message saved without the
/// blank line under its summary compares equal to the same message edited.
pub(super) fn normal_message(text: &str) -> String {
    let (summary, body) = split_message(text);
    join_message(&summary, &body)
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_LINES, MIN_LINES, clamp_lines, dragged_lines, join_message, normal_message,
        split_message,
    };

    #[test]
    fn a_drag_moves_the_box_by_whole_lines_within_its_limits() {
        assert_eq!(dragged_lines(8, 0., 20.), 8);
        assert_eq!(dragged_lines(8, 29., 20.), 9, "past half a line");
        assert_eq!(dragged_lines(8, 9., 20.), 8, "short of half a line");
        assert_eq!(dragged_lines(8, -60., 20.), 5);
        assert_eq!(dragged_lines(8, -1000., 20.), MIN_LINES);
        assert_eq!(dragged_lines(8, 10_000., 20.), MAX_LINES);
        assert_eq!(clamp_lines(0), MIN_LINES, "a hand-edited session");
    }

    #[test]
    fn a_message_is_a_summary_then_a_description() {
        let text = "fix(log): keep the page\n\nWhat:\n- one\n";
        assert_eq!(
            split_message(text),
            (
                "fix(log): keep the page".to_owned(),
                "What:\n- one".to_owned()
            )
        );
        assert_eq!(
            join_message("fix(log): keep the page", "What:\n- one"),
            text.trim()
        );
        // no description, no blank line
        assert_eq!(join_message("summary", "  "), "summary");
        // a body right under the summary reads back with the blank line
        assert_eq!(normal_message("a\nb"), "a\n\nb");
        assert_eq!(normal_message("a\n\n\nb\n"), "a\n\nb");
    }
}
