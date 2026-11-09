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
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            diff: DiffLayout::Unified,
            github: true,
            pull_refresh_secs: 60,
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
}

impl Global for Settings {}

impl Settings {
    /// The session's display settings plus the `gg.*` values in jj's user config.
    pub fn load(display: DisplaySettings) -> Self {
        let (auto_snapshot, mark_unpushed) = match gg_lib::read_config(None) {
            Ok((settings, ..)) => (
                settings.query_auto_snapshot(),
                settings.ui_mark_unpushed_bookmarks(),
            ),
            Err(err) => {
                log::warn!("reading jj config for settings: {err:#}");
                (None, true)
            }
        };
        Self {
            display,
            auto_snapshot,
            mark_unpushed,
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
