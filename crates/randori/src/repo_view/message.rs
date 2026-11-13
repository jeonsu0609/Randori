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
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
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
    use super::{join_message, normal_message, split_message};

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
