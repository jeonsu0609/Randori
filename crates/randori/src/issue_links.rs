//! Issue keys in text as links, by the rules in Settings (Describe → Issue
//! links): each rule is a regular expression and a URL in which `$0` is the
//! match and `$1`… its groups, e.g. `MELON-\d+` → `…/browse/$0`.
//!
//! The rules compile once per change of the settings, not per frame: the
//! inspector draws on every frame of its tab.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::{App, SharedString};
use regex::Regex;

use crate::settings::{self, IssueLink};

/// One issue key found in a text, with where it links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueRef {
    pub key: String,
    pub url: String,
}

type Compiled = Rc<Vec<(Regex, String)>>;

thread_local! {
    /// The rules last compiled, and what they compiled to.
    static CACHE: RefCell<(Vec<IssueLink>, Compiled)> = RefCell::default();
    /// Pull request texts with their links, for the rules in `CACHE`: review
    /// threads are drawn on every frame of the Files changed tab.
    static LINKED: RefCell<std::collections::HashMap<String, SharedString>> = RefCell::default();
}

/// Past this many texts the linked ones are forgotten and made again.
const LINKED_LIMIT: usize = 2000;

/// The rules of Settings, compiled. A pattern that is not a regular
/// expression is left out (Settings names it).
fn compiled(cx: &App) -> Compiled {
    let links = &settings::get(cx).display.issue_links;
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.0 != *links {
            *cache = (links.clone(), Rc::new(compile(links)));
            LINKED.with(|linked| linked.borrow_mut().clear());
        }
        cache.1.clone()
    })
}

fn compile(links: &[IssueLink]) -> Vec<(Regex, String)> {
    links
        .iter()
        .filter_map(|link| Some((Regex::new(&link.pattern).ok()?, link.url.clone())))
        .collect()
}

/// The issue keys in `text`, each once, in the order they first appear.
pub fn find(text: &str, cx: &App) -> Vec<IssueRef> {
    find_with(text, &compiled(cx))
}

fn find_with(text: &str, rules: &[(Regex, String)]) -> Vec<IssueRef> {
    let mut found: Vec<(usize, IssueRef)> = Vec::new();
    for (pattern, url) in rules {
        for caps in pattern.captures_iter(text) {
            let whole = caps.get(0).expect("group 0 is the match");
            if whole.as_str().is_empty() || found.iter().any(|(_, r)| r.key == whole.as_str()) {
                continue;
            }
            let mut link = String::new();
            caps.expand(url, &mut link);
            found.push((
                whole.start(),
                IssueRef {
                    key: whole.as_str().to_owned(),
                    url: link,
                },
            ));
        }
    }
    found.sort_by_key(|(start, _)| *start);
    found.into_iter().map(|(_, r)| r).collect()
}

/// `markdown` with its issue keys made links, outside code (inline or
/// fenced) and outside links that are there already.
/// A shared string, so a frame that draws the text again copies nothing.
pub fn linkify_markdown(markdown: &str, cx: &App) -> SharedString {
    let rules = compiled(cx);
    if rules.is_empty() {
        return markdown.to_owned().into();
    }
    if let Some(linked) = LINKED.with(|linked| linked.borrow().get(markdown).cloned()) {
        return linked;
    }
    let linked = SharedString::from(linkify_with(markdown, &rules));
    LINKED.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= LINKED_LIMIT {
            cache.clear();
        }
        cache.insert(markdown.to_owned(), linked.clone());
    });
    linked
}

fn linkify_with(markdown: &str, rules: &[(Regex, String)]) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut fenced = false;
    for line in markdown.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            out.push_str(line);
            continue;
        }
        if fenced {
            out.push_str(line);
            continue;
        }
        // odd pieces between backticks are inline code
        for (ix, piece) in line.split('`').enumerate() {
            if ix > 0 {
                out.push('`');
            }
            if ix % 2 == 1 {
                out.push_str(piece);
            } else {
                out.push_str(&linkify_prose(piece, rules));
            }
        }
    }
    out
}

/// Links the keys in a piece of prose, leaving a key that is already a
/// link's text or part of a URL as it is.
fn linkify_prose(text: &str, rules: &[(Regex, String)]) -> String {
    let found = find_with(text, rules);
    if found.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 32);
    let mut rest = text;
    'scan: while !rest.is_empty() {
        // the earliest key from here
        let next = found
            .iter()
            .filter_map(|issue| rest.find(&issue.key).map(|at| (at, issue)))
            .min_by_key(|(at, _)| *at);
        let Some((at, issue)) = next else {
            break 'scan;
        };
        let before = &rest[..at];
        let after = &rest[at + issue.key.len()..];
        out.push_str(before);
        let inside_link = before.ends_with('[')
            || before.ends_with('/')
            || before.ends_with('=')
            || after.starts_with("](")
            || before
                .rfind("](")
                .is_some_and(|open| !before[open..].contains(')'));
        if inside_link {
            out.push_str(&issue.key);
        } else {
            out.push_str(&format!("[{}]({})", issue.key, issue.url));
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Vec<(Regex, String)> {
        compile(&[
            IssueLink {
                pattern: "MELON-\\d+".into(),
                url: "https://jira.example.com/browse/$0".into(),
            },
            IssueLink {
                pattern: "#(\\d+)".into(),
                url: "https://github.com/o/r/issues/$1".into(),
            },
            IssueLink {
                pattern: "(".into(),
                url: "broken".into(),
            },
        ])
    }

    #[test]
    fn keys_are_found_once_in_order() {
        let found = find_with("fix #12 for MELON-5408, see MELON-5408 and #3", &rules());
        let keys: Vec<_> = found.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, vec!["#12", "MELON-5408", "#3"]);
        assert_eq!(found[0].url, "https://github.com/o/r/issues/12");
        assert_eq!(found[1].url, "https://jira.example.com/browse/MELON-5408");
    }

    #[test]
    fn markdown_keys_become_links_outside_code_and_links() {
        let text =
            "Resolves MELON-1 and `MELON-2`\n```\nMELON-3\n```\n[MELON-4](x) https://h/MELON-5";
        assert_eq!(
            linkify_with(text, &rules()),
            "Resolves [MELON-1](https://jira.example.com/browse/MELON-1) and `MELON-2`\n```\nMELON-3\n```\n[MELON-4](x) https://h/MELON-5"
        );
    }
}
