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
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            diff: DiffLayout::Unified,
            github: true,
            pull_refresh_secs: 60,
            log_by_date: false,
            columns: ColumnWidths::default(),
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
            },
            Err(err) => {
                log::warn!("reading jj config for settings: {err:#}");
                Self {
                    display,
                    auto_snapshot: None,
                    mark_unpushed: true,
                    recent: Vec::new(),
                    track_recent: false,
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
    }
}
