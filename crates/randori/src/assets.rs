//! Embedded assets: the icons Randori uses plus gpui-kit's own component set.
//!
//! The full Lucide catalog (`gpui_kit::assets::AllAssets`, ~1,800 SVGs) is not
//! embedded; adding an icon to the UI means adding its name below.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(pub AppIcons, [
    ArrowDownToLine,
    Bookmark,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    CircleCheck,
    CircleDot,
    CircleX,
    CloudDownload,
    Command,
    Copy,
    ExternalLink,
    File,
    FileDiff,
    FileInput,
    FileText,
    FileX,
    FolderGit2,
    FolderOpen,
    Maximize2,
    GitBranch,
    GitCommitHorizontal,
    GitGraph,
    Monitor,
    Moon,
    MousePointerClick,
    PanelLeft,
    Pencil,
    Plus,
    Redo2,
    RefreshCw,
    Search,
    Settings,
    Sun,
    Tag,
    TextCursorInput,
    Trash,
    TriangleAlert,
    Undo,
    Undo2,
    Upload,
    User,
    X,
]);

/// Randori's icons first, then the icons gpui-kit components draw themselves
/// (close buttons, checks, chevrons, notification symbols).
pub struct RandoriAssets;

impl AssetSource for RandoriAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = AppIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = AppIcons.list(path)?;
        paths.extend(gpui_kit::assets::Assets.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every icon the UI names must be embedded, or it renders as nothing.
    #[test]
    fn every_used_icon_is_embedded() {
        let source = concat!(
            include_str!("app_view.rs"),
            include_str!("repo_view.rs"),
            include_str!("repo_view/dnd.rs"),
            include_str!("repo_view/focus.rs"),
            include_str!("repo_view/inspector.rs"),
            include_str!("repo_view/log_list.rs"),
            include_str!("repo_view/menus.rs"),
            include_str!("repo_view/model.rs"),
            include_str!("repo_view/ops.rs"),
            include_str!("repo_view/panels.rs"),
            include_str!("repo_view/pick.rs"),
            include_str!("repo_view/pulls.rs"),
            include_str!("repo_view/switcher.rs"),
        );
        let mut missing = Vec::new();
        for (ix, _) in source.match_indices("IconName::") {
            let name: String = source[ix + "IconName::".len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            let path = format!("icons/{}.svg", kebab(&name));
            if RandoriAssets.load(&path).ok().flatten().is_none() && !missing.contains(&name) {
                missing.push(name);
            }
        }
        assert!(missing.is_empty(), "add to icon_assets!: {missing:?}");
    }

    /// The test above guesses asset paths from names; check the guess against
    /// gpui-kit's own mapping for the awkward cases.
    #[test]
    fn kebab_matches_icon_paths() {
        use gpui_kit::assets::IconName;
        for (name, icon) in [
            ("GitCommitHorizontal", IconName::GitCommitHorizontal),
            ("Redo2", IconName::Redo2),
            ("FolderGit2", IconName::FolderGit2),
            ("X", IconName::X),
        ] {
            assert_eq!(format!("icons/{}.svg", kebab(name)), icon.path().as_ref());
        }
    }

    /// `GitCommitHorizontal` → `git-commit-horizontal`, `Redo2` → `redo-2`.
    fn kebab(name: &str) -> String {
        let mut out = String::new();
        let mut prev_lower_or_digit = false;
        let mut prev_alpha = false;
        for c in name.chars() {
            if c.is_ascii_uppercase() {
                if prev_lower_or_digit {
                    out.push('-');
                }
                out.push(c.to_ascii_lowercase());
                prev_lower_or_digit = false;
                prev_alpha = true;
            } else if c.is_ascii_digit() {
                if prev_alpha {
                    out.push('-');
                }
                out.push(c);
                prev_lower_or_digit = true;
                prev_alpha = false;
            } else {
                out.push(c);
                prev_lower_or_digit = true;
                prev_alpha = true;
            }
        }
        out
    }
}
