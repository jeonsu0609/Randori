//! The Settings window's own fields: text that is written when the user is
//! done with it, not on every keystroke.

use super::*;

/// A text field written when the user is done (Enter, or leaving it), not on
/// every keystroke: for values that go to the jj config file.
pub(super) fn committed_input(
    id: &'static str,
    value: impl Fn(&App) -> String + 'static,
    commit: impl Fn(String, &mut App) + 'static,
) -> SettingField<SharedString> {
    let value = std::rc::Rc::new(value);
    let commit = std::rc::Rc::new(commit);
    SettingField::render(move |_, window, cx| {
        let current = value(cx);
        let state = window.use_keyed_state(SharedString::from(format!("committed-{id}")), cx, {
            let current = current.clone();
            let commit = commit.clone();
            let value = value.clone();
            move |window, cx| {
                let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
                let subscription =
                    cx.subscribe(
                        &input,
                        move |_, input, event: &InputEvent, cx| match event {
                            InputEvent::Change => {
                                let (input, commit, value) =
                                    (input.clone(), commit.clone(), value.clone());
                                let write: WriteEdit = std::rc::Rc::new(move |cx| {
                                    let typed = input.read(cx).value().to_string();
                                    if typed.trim() != value(cx).trim() {
                                        commit(typed, cx);
                                    }
                                });
                                pend(id, write, cx);
                            }
                            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                                write_pending(id, cx)
                            }
                            _ => {}
                        },
                    );
                CommittedInput {
                    input,
                    _subscription: subscription,
                }
            }
        });
        let input = state.read(cx).input.clone();
        // a change made elsewhere shows, unless the field is being edited
        if !input.read(cx).focus_handle(cx).is_focused(window)
            && input.read(cx).value().as_ref() != current
        {
            input.update(cx, |input, cx| input.set_value(current.clone(), window, cx));
        }
        Input::new(&input).w_64()
    })
}

struct CommittedInput {
    input: Entity<InputState>,
    _subscription: Subscription,
}

struct IssueLinksEditor {
    input: Entity<TextareaState>,
    _subscription: Subscription,
}

/// The issue link rules as lines of text, read back when the field loses
/// focus. A rule whose pattern does not compile is kept, and named below.
pub(super) fn issue_links_editor(window: &mut Window, cx: &mut App) -> gpui_kit::AnyElement {
    let text = issue_links_text(&display(cx).issue_links);
    let state = window.use_keyed_state("issue-links", cx, {
        let text = text.clone();
        move |window, cx| {
            let input = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(3, 8)
                    .placeholder("MELON-\\d+ https://example.atlassian.net/browse/$0")
                    .default_value(text)
            });
            let subscription =
                cx.subscribe(&input, |_, input, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        let input = input.clone();
                        let write: WriteEdit = std::rc::Rc::new(move |cx| {
                            let links = parse_issue_links(&input.read(cx).value());
                            if links != display(cx).issue_links {
                                settings::update_display(cx, |d| d.issue_links = links);
                            }
                        });
                        pend("issue-links", write, cx);
                    }
                    InputEvent::Blur => write_pending("issue-links", cx),
                    _ => {}
                });
            IssueLinksEditor {
                input,
                _subscription: subscription,
            }
        }
    });
    let input = state.read(cx).input.clone();
    let invalid: Vec<String> = display(cx)
        .issue_links
        .iter()
        .filter(|link| regex::Regex::new(&link.pattern).is_err())
        .map(|link| link.pattern.clone())
        .collect();
    let danger = cx.theme().danger;
    v_flex()
        .w_full()
        .gap_1()
        .child(Textarea::new(&input).small())
        .when(!invalid.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(danger)
                    .child(format!("Not a regular expression: {}", invalid.join(", "))),
            )
        })
        .into_any_element()
}

fn issue_links_text(links: &[IssueLink]) -> String {
    links
        .iter()
        .map(|link| format!("{} {}", link.pattern, link.url))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Lines of `pattern url`; a line without both is left out.
fn parse_issue_links(text: &str) -> Vec<IssueLink> {
    text.lines()
        .filter_map(|line| {
            let (pattern, url) = line.trim().split_once(char::is_whitespace)?;
            let url = url.trim();
            (!pattern.is_empty() && !url.is_empty()).then(|| IssueLink {
                pattern: pattern.to_owned(),
                url: url.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_link_rules_read_one_a_line() {
        let links = parse_issue_links(
            "MELON-\\d+ https://jira.example.com/browse/$0\n\n  #(\\d+)   https://github.com/o/r/issues/$1 \nlonely",
        );
        assert_eq!(
            links,
            vec![
                IssueLink {
                    pattern: "MELON-\\d+".into(),
                    url: "https://jira.example.com/browse/$0".into()
                },
                IssueLink {
                    pattern: "#(\\d+)".into(),
                    url: "https://github.com/o/r/issues/$1".into()
                },
            ]
        );
        assert_eq!(parse_issue_links(&issue_links_text(&links)), links);
    }
}
