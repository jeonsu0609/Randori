//! Find a commit, GitKraken's way: a bar over the revision list (`⌘F`, or
//! Search in the toolbar) that counts the loaded revisions matching its text,
//! steps through them with Enter / Shift-Enter or the arrows, and dims the
//! rest. It searches what the log has loaded; stepping past the last match
//! loads the next page while there is one.
//!
//! Unlike the revset filter above the list, it never changes which revisions
//! are shown.

use gpui_kit::component::input::{Input, InputEvent, InputState};

use super::*;

pub(super) struct FindBar {
    pub(super) input: Entity<InputState>,
    /// Rows matching the text, in list order.
    pub(super) matches: Rc<Vec<usize>>,
    /// Which of `matches` is selected.
    pub(super) current: Option<usize>,
    /// Stepped past the last match while more rows were on their way.
    pending_next: bool,
    /// The field is to take focus in the frame that first draws it: GPUI
    /// drops focus on an element the frame does not have, and the keys typed
    /// would go to the revision list's shortcuts.
    focus_pending: bool,
    _subscription: gpui_kit::Subscription,
}

impl FindBar {
    fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_lowercase()
    }
}

/// Whether `rev` matches `needle` (lowercase): its description, author,
/// change or commit id (from the start) or a bookmark or tag name.
pub(super) fn revision_matches(rev: &RevHeader, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let contains = |text: &str| text.to_lowercase().contains(needle);
    // the id is its prefix then its rest, as jj prints it
    let id_starts = |prefix: &str, rest: &str| match needle.strip_prefix(prefix) {
        Some(more) => rest.starts_with(more),
        None => prefix.starts_with(needle),
    };
    rev.description.lines.iter().any(|line| contains(line))
        || contains(&rev.author.name)
        || contains(&rev.author.email)
        || id_starts(&rev.id.change.prefix, &rev.id.change.rest)
        || id_starts(&rev.id.commit.prefix, &rev.id.commit.rest)
        || rev.refs.iter().any(|r| match r {
            StoreRef::LocalBookmark { bookmark_name, .. } => contains(bookmark_name),
            StoreRef::RemoteBookmark {
                bookmark_name,
                remote_name,
                ..
            } => contains(&format!("{bookmark_name}@{remote_name}")),
            StoreRef::Tag { tag_name } => contains(tag_name),
        })
}

impl RepoView {
    /// Opens the find bar, or focuses its field when it is open.
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = &self.find {
            find.input.update(cx, |state, cx| state.focus(window, cx));
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find commit"));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |view: &mut Self, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => view.update_find(true, 0, cx),
                InputEvent::PressEnter { shift, .. } => {
                    view.step_find(if *shift { -1 } else { 1 }, cx)
                }
                _ => {}
            },
        );
        self.find = Some(FindBar {
            input,
            matches: Rc::default(),
            current: None,
            pending_next: false,
            focus_pending: true,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Called while the tab renders: focuses a field just opened.
    pub(super) fn focus_find_if_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = self.find.as_mut().filter(|find| find.focus_pending) {
            find.focus_pending = false;
            find.input.update(cx, |state, cx| state.focus(window, cx));
        }
    }

    pub(super) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.take().is_some() {
            self.log_focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Finds the matches again: the text changed (`jump`: go to the first
    /// match from the selection down), or the rows did. The rows before
    /// `from` are as they were (a page was added after them): their matches
    /// are kept and only the rows after are searched.
    pub(super) fn update_find(&mut self, jump: bool, from: usize, cx: &mut Context<Self>) {
        let Some(find) = &self.find else {
            return;
        };
        let needle = find.text(cx);
        let mut matches: Vec<usize> = find
            .matches
            .iter()
            .copied()
            .take_while(|&ix| ix < from)
            .collect();
        matches.extend(
            self.rows
                .iter()
                .enumerate()
                .skip(from)
                .filter(|(_, row)| revision_matches(&row.revision, &needle))
                .map(|(ix, _)| ix),
        );
        let selected = self.selected;
        let pending_next = find.pending_next;
        let position = |ix: usize| matches.iter().position(|&m| m == ix);
        let current = if pending_next {
            // past the last match: the first new one, else the one selected stays
            let from = selected.map_or(0, |ix| ix + 1);
            matches
                .iter()
                .position(|&ix| ix >= from)
                .or_else(|| selected.and_then(position))
        } else if jump {
            // new text: the first match from the selection down, else from the top
            let from = selected.unwrap_or(0);
            matches
                .iter()
                .position(|&ix| ix >= from)
                .or((!matches.is_empty()).then_some(0))
        } else {
            // new rows: the match selected, if it still is one
            selected.and_then(position)
        };
        let target = current.map(|at| matches[at]);
        if let Some(find) = &mut self.find {
            find.matches = Rc::new(matches);
            find.current = current;
            find.pending_next = false;
        }
        if (jump || pending_next)
            && let Some(ix) = target
        {
            self.select(ix, cx);
        }
        cx.notify();
    }

    /// Selects the next (`delta` 1) or previous (-1) match, wrapping around,
    /// or past the last one loads more rows when there are some.
    pub(super) fn step_find(&mut self, delta: isize, cx: &mut Context<Self>) {
        let has_more = self.has_more;
        let Some(find) = &mut self.find else {
            return;
        };
        let len = find.matches.len();
        if len == 0 {
            if has_more && delta > 0 {
                find.pending_next = true;
                self.load_more(cx);
            }
            return;
        }
        let at = match find.current {
            None => 0,
            Some(at) if delta > 0 && at + 1 == len && has_more => {
                find.pending_next = true;
                self.load_more(cx);
                return;
            }
            Some(at) => (at as isize + delta).rem_euclid(len as isize) as usize,
        };
        find.current = Some(at);
        let ix = find.matches[at];
        self.select(ix, cx);
    }

    /// The bar, over the top right of the revision list.
    pub(super) fn render_find(&self, find: &FindBar, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let has_text = !find.input.read(cx).value().trim().is_empty();
        let count = match (find.matches.len(), find.current) {
            _ if !has_text => String::new(),
            (0, _) if self.has_more => "None loaded".to_owned(),
            (0, _) => "No results".to_owned(),
            (n, current) => format!(
                "{} of {n}{}",
                current.map_or("–".to_owned(), |at| (at + 1).to_string()),
                if self.has_more { "+" } else { "" }
            ),
        };
        h_flex()
            .id("find-bar")
            .key_context("RandoriFind")
            .occlude()
            .absolute()
            .top_2()
            .right_4()
            .w_96()
            .gap_1()
            .p_1()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.primary)
            .bg(theme.popover)
            .shadow_lg()
            .on_action(
                cx.listener(|view, _: &actions::CloseFind, window, cx| view.close_find(window, cx)),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&find.input)
                        .small()
                        .prefix(gpui_kit::component::Icon::new(IconName::Search).xsmall()),
                ),
            )
            .child(
                div()
                    .flex_none()
                    .min_w_20()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(count),
            )
            .child(
                Button::new("find-previous")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ArrowUp)
                    .tooltip("Previous match (⇧Enter)")
                    .disabled(find.matches.is_empty())
                    .on_click(cx.listener(|view, _, _, cx| view.step_find(-1, cx))),
            )
            .child(
                Button::new("find-next")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ArrowDown)
                    .tooltip("Next match (Enter)")
                    .disabled(find.matches.is_empty() && !self.has_more)
                    .on_click(cx.listener(|view, _, _, cx| view.step_find(1, cx))),
            )
            .child(
                Button::new("find-close")
                    .ghost()
                    .xsmall()
                    .icon(IconName::X)
                    .tooltip("Close (Esc)")
                    .on_click(cx.listener(|view, _, window, cx| view.close_find(window, cx))),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::revision_matches;
    use gg_lib::messages::RevHeader;

    fn header() -> RevHeader {
        serde_json::from_value(serde_json::json!({
            "id": {
                "change": {"type": "ChangeId", "hex": "kwwpyxsk", "prefix": "kw", "rest": "wpyxsk", "offset": null, "is_divergent": false},
                "commit": {"type": "CommitId", "hex": "34c9aa64", "prefix": "34", "rest": "c9aa64"}
            },
            "description": {"lines": ["fix(playlist): 스마트 플레이리스트", "", "What: 담기"]},
            "author": {"name": "Van", "email": "van@example.com", "timestamp": "2026-11-12T01:00:00+09:00"},
            "committed_at": "2026-11-12T01:00:00+09:00",
            "has_conflict": false,
            "is_working_copy": false,
            "working_copy_of": null,
            "is_immutable": false,
            "refs": [{"type": "LocalBookmark", "bookmark_name": "van.dalgom/qa", "has_conflict": false,
                      "is_synced": true, "tracking_remotes": [], "available_remotes": 0, "potential_remotes": 0}],
            "parent_ids": []
        }))
        .expect("a header")
    }

    #[test]
    fn a_revision_matches_its_text_ids_and_refs() {
        let rev = header();
        for needle in ["스마트", "what: 담기", "van", "kwwp", "34c9", "dalgom/qa"] {
            assert!(revision_matches(&rev, needle), "{needle}");
        }
        // ids match from their start only; nothing matches empty text
        for needle in ["pyxsk", "nothing", ""] {
            assert!(!revision_matches(&rev, needle), "{needle}");
        }
    }
}
