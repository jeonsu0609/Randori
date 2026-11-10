//! Randori's settings, kept in a GPUI global that every view reads and
//! observes, so a change applies to open tabs at once.
//!
//! Display preferences are saved with the session. Settings that change how jj
//! behaves are the `gg.*` keys GG reads, written to the user's jj config file so
//! the jj CLI and GG see the same values.

use gg_lib::config::GGSettings as _;
use gpui_kit::{App, BorrowAppContext as _, Global};
use serde::{Deserialize, Serialize};

/// jj config key: snapshot the working copy when the window comes forward.
pub const AUTO_SNAPSHOT_KEY: &str = "gg.queries.auto-snapshot";
/// jj config key: mark local bookmarks that are not on their remote.
pub const MARK_UNPUSHED_KEY: &str = "gg.ui.mark-unpushed-bookmarks";

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffLayout {
    #[default]
    Unified,
    Split,
}

/// Choices for refreshing pull requests, in seconds; 0 only refreshes when asked.
pub const PULL_REFRESH_CHOICES: [(u64, &str); 4] = [
    (60, "Every minute"),
    (300, "Every 5 minutes"),
    (900, "Every 15 minutes"),
    (0, "Only when asked"),
];

/// Widths of the revision list's columns, in rems (they scale with the
/// interface). The description takes what is left.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColumnWidths {
    pub refs: f32,
    /// `None` sizes the graph to its lanes.
    pub graph: Option<f32>,
    pub change: f32,
    pub author: f32,
}

impl Default for ColumnWidths {
    fn default() -> Self {
        Self {
            refs: 8.,
            graph: None,
            change: 6.5,
            author: 7.,
        }
    }
}

/// A split diff starts with its two sides the same width.
pub const DEFAULT_SPLIT_RATIO: f32 = 0.5;

/// How a diff marks changed lines beside the code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeMarks {
    /// `+` and `−`.
    #[default]
    Signs,
    /// A coloured bar.
    Bars,
    None,
}

/// Which of the revision list's optional columns are shown. The graph and
/// the description always are.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColumnVisibility {
    pub refs: bool,
    pub change: bool,
    pub author: bool,
}

impl Default for ColumnVisibility {
    fn default() -> Self {
        Self {
            refs: true,
            change: true,
            author: true,
        }
    }
}

/// Text in descriptions that links to an issue: each match of `pattern` (a
/// regular expression) opens `url` with `$0` replaced by the match, e.g.
/// `MELON-\d+` → `https://jira.example.com/browse/$0`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IssueLink {
    pub pattern: String,
    pub url: String,
}

/// Fetch intervals, in minutes; 0 fetches only when asked.
pub const AUTO_FETCH_CHOICES: [(u32, &str); 5] = [
    (0, "Off"),
    (5, "Every 5 minutes"),
    (15, "Every 15 minutes"),
    (30, "Every 30 minutes"),
    (60, "Every hour"),
];

/// The part saved with the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplaySettings {
    /// Layout new diffs open in; the diff header's switch changes it.
    pub diff: DiffLayout,
    /// Pull requests and GitHub avatars (network requests to GitHub).
    pub github: bool,
    /// Seconds between pull request refreshes; 0 refreshes only when asked.
    pub pull_refresh_secs: u64,
    /// Revisions newest first across branches instead of grouped by branch.
    pub log_by_date: bool,
    /// The revision list's column widths, as last dragged.
    pub columns: ColumnWidths,
    /// The original (left) side's share of a split diff, as last dragged.
    pub split_ratio: f32,
    /// Folders whose jj repositories the workspace picker lists.
    pub source_folders: Vec<String>,
    /// Minutes between automatic fetches of every open tab; 0 is off.
    pub auto_fetch_mins: u32,
    /// A dot on tabs whose working copy changes files.
    pub mark_changed_tabs: bool,
    pub visible_columns: ColumnVisibility,
    /// Code (diffs, file view, ids) in this font instead of the theme's.
    pub code_font: Option<String>,
    pub code_font_size: Option<f32>,
    pub change_marks: ChangeMarks,
    pub syntax_highlight: bool,
    /// A description's first line past these lengths is marked: past the
    /// first as long, past the second as too long.
    pub subject_limits: (u32, u32),
    pub issue_links: Vec<IssueLink>,
    /// The application the terminal button opens (macOS: `open -a`);
    /// `None` is the terminal panel.
    pub external_terminal: Option<String>,
    /// The terminal panel's font, instead of the code font.
    pub terminal_font: Option<String>,
    pub terminal_font_size: Option<f32>,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            diff: DiffLayout::Unified,
            github: true,
            pull_refresh_secs: 60,
            log_by_date: false,
            columns: ColumnWidths::default(),
            split_ratio: DEFAULT_SPLIT_RATIO,
            source_folders: Vec::new(),
            auto_fetch_mins: 0,
            mark_changed_tabs: true,
            visible_columns: ColumnVisibility::default(),
            code_font: None,
            code_font_size: None,
            change_marks: ChangeMarks::Signs,
            syntax_highlight: true,
            subject_limits: (50, 72),
            issue_links: Vec::new(),
            external_terminal: None,
            terminal_font: None,
            terminal_font_size: None,
        }
    }
}

pub struct Settings {
    pub display: DisplaySettings,
    /// `gg.queries.auto-snapshot`: `None` is jj's default, on unless the
    /// repository is large (`gg.queries.large-repo-heuristic`).
    pub auto_snapshot: Option<bool>,
    /// `gg.ui.mark-unpushed-bookmarks`.
    pub mark_unpushed: bool,
    /// `gg.ui.recent-workspaces`, newest first, as GG keeps it.
    pub recent: Vec<String>,
    /// `gg.ui.track-recent-workspaces`.
    pub track_recent: bool,
    /// jj's `user.name` and `user.email`, which new changes are authored by.
    pub user_name: String,
    pub user_email: String,
    /// jj's `ui.merge-editor` and `ui.diff-formatter`, when set to a name.
    pub merge_editor: Option<String>,
    pub diff_formatter: Option<String>,
    /// Bumped by each write of a jj setting that a loaded workspace keeps
    /// (the user, the tools): tabs open their workspace again to read it.
    pub jj_generation: u64,
}

impl Global for Settings {}

/// Recent workspaces GG keeps (and Randori, the same way).
const RECENT_KEY: &str = "gg.ui.recent-workspaces";
const RECENT_LIMIT: usize = 10;

impl Settings {
    /// The session's display settings plus the `gg.*` values in jj's user config.
    pub fn load(display: DisplaySettings) -> Self {
        match gg_lib::read_config(None) {
            Ok((settings, ..)) => Self {
                display,
                auto_snapshot: settings.query_auto_snapshot(),
                mark_unpushed: settings.ui_mark_unpushed_bookmarks(),
                recent: settings.ui_recent_workspaces(),
                track_recent: settings.ui_track_recent_workspaces(),
                user_name: settings.user_name().to_owned(),
                user_email: settings.user_email().to_owned(),
                merge_editor: settings.get_string(MERGE_EDITOR_KEY).ok(),
                diff_formatter: settings.get_string(DIFF_FORMATTER_KEY).ok(),
                jj_generation: 0,
            },
            Err(err) => {
                log::warn!("reading jj config for settings: {err:#}");
                Self {
                    display,
                    auto_snapshot: None,
                    mark_unpushed: true,
                    recent: Vec::new(),
                    track_recent: false,
                    user_name: String::new(),
                    user_email: String::new(),
                    merge_editor: None,
                    diff_formatter: None,
                    jj_generation: 0,
                }
            }
        }
    }
}

pub fn get(cx: &App) -> &Settings {
    cx.global::<Settings>()
}

/// Changes a display setting; observers (the views, the session) follow.
pub fn update_display(cx: &mut App, change: impl FnOnce(&mut DisplaySettings)) {
    cx.update_global::<Settings, _>(|settings, _| change(&mut settings.display));
}

/// Puts `path` first in the recent workspaces and writes the list to the jj
/// config, unless `gg.ui.track-recent-workspaces` is off.
pub fn remember_workspace(cx: &mut App, path: &std::path::Path) {
    if !get(cx).track_recent {
        return;
    }
    let path = path.display().to_string();
    let mut recent = get(cx).recent.clone();
    if recent.first() == Some(&path) {
        return;
    }
    recent.retain(|other| other != &path);
    recent.insert(0, path);
    recent.truncate(RECENT_LIMIT);
    cx.update_global::<Settings, _>(|settings, _| settings.recent = recent.clone());
    cx.background_executor()
        .spawn(async move {
            if let Err(err) = gg_lib::config::write_user_list(RECENT_KEY, &recent) {
                log::warn!("saving recent workspaces: {err:#}");
            }
        })
        .detach();
}

/// Writes `gg.queries.auto-snapshot` and applies it to open tabs.
pub fn set_auto_snapshot(cx: &mut App, value: Option<bool>) -> anyhow::Result<()> {
    gg_lib::config::write_user_setting(AUTO_SNAPSHOT_KEY, value)?;
    cx.update_global::<Settings, _>(|settings, _| settings.auto_snapshot = value);
    Ok(())
}

/// jj config keys the Settings window edits as text.
pub const USER_NAME_KEY: &str = "user.name";
pub const USER_EMAIL_KEY: &str = "user.email";
pub const MERGE_EDITOR_KEY: &str = "ui.merge-editor";
pub const DIFF_FORMATTER_KEY: &str = "ui.diff-formatter";

/// Writes a jj string setting (`None` or empty removes it, leaving jj's
/// default) and has open tabs read it: workspaces keep the config they were
/// loaded with.
pub fn set_jj_string(cx: &mut App, key: &str, value: Option<String>) -> anyhow::Result<()> {
    let value = value
        .map(|value| value.trim().to_owned())
        .filter(|v| !v.is_empty());
    let current = match key {
        USER_NAME_KEY => Some(get(cx).user_name.clone()),
        USER_EMAIL_KEY => Some(get(cx).user_email.clone()),
        MERGE_EDITOR_KEY => get(cx).merge_editor.clone(),
        DIFF_FORMATTER_KEY => get(cx).diff_formatter.clone(),
        _ => None,
    }
    .filter(|current| !current.is_empty());
    if current == value {
        // nothing to write, and no reason for the tabs to load again
        return Ok(());
    }
    gg_lib::config::write_user_string(key, value.as_deref())?;
    cx.update_global::<Settings, _>(|settings, _| {
        match key {
            USER_NAME_KEY => settings.user_name = value.clone().unwrap_or_default(),
            USER_EMAIL_KEY => settings.user_email = value.clone().unwrap_or_default(),
            MERGE_EDITOR_KEY => settings.merge_editor = value.clone(),
            DIFF_FORMATTER_KEY => settings.diff_formatter = value.clone(),
            _ => {}
        }
        settings.jj_generation += 1;
    });
    Ok(())
}

/// Writes `gg.ui.mark-unpushed-bookmarks` and applies it to open tabs.
pub fn set_mark_unpushed(cx: &mut App, value: bool) -> anyhow::Result<()> {
    gg_lib::config::write_user_setting(MARK_UNPUSHED_KEY, Some(value))?;
    cx.update_global::<Settings, _>(|settings, _| settings.mark_unpushed = value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_settings_fill_missing_fields() {
        let old: DisplaySettings = serde_json::from_str(r#"{"diff":"split"}"#).unwrap();
        assert_eq!(old.diff, DiffLayout::Split);
        assert!(old.github);
        assert_eq!(old.pull_refresh_secs, 60);
        assert_eq!(old.split_ratio, DEFAULT_SPLIT_RATIO);
        // what the Settings window added reads as its defaults
        assert_eq!(old.auto_fetch_mins, 0);
        assert!(old.mark_changed_tabs && old.syntax_highlight);
        assert_eq!(old.visible_columns, ColumnVisibility::default());
        assert_eq!(old.change_marks, ChangeMarks::Signs);
        assert_eq!(old.subject_limits, (50, 72));
        assert!(old.code_font.is_none() && old.external_terminal.is_none());
    }
}
