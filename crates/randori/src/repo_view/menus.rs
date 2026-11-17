//! Context menus. Items dispatch actions to the tab, so they show the same key
//! bindings as the keyboard.

use super::*;

// ----- context menus ---------------------------------------------------------------------------

/// Commands for the selected revision. Items dispatch actions to the repository
/// tab, so the menu shows the same key bindings as the keyboard. An immutable
/// revision's rewrites are offered disabled: jj would refuse them, and
/// "Rebase onto…" on, say, trunk moves trunk, not one's work onto it.
pub(super) fn revision_menu(menu: PopupMenu, immutable: bool, focus: FocusHandle) -> PopupMenu {
    use actions::*;
    menu.action_context(focus)
        .menu("New change", Box::new(NewChild))
        .menu("Edit", Box::new(EditRevision))
        .menu("Describe", Box::new(DescribeRevision))
        .separator()
        .menu_with_disabled("Rebase onto…", Box::new(PickRebase), immutable)
        .menu("Create bookmark…", Box::new(CreateBookmark))
        .menu("Push bookmarks", Box::new(Push))
        .separator()
        .menu("Duplicate", Box::new(DuplicateRevision))
        .menu("Back out", Box::new(BackoutRevision))
        .separator()
        .menu_with_disabled("Abandon…", Box::new(AbandonRevision), immutable)
}

/// Commands for a changed file in the inspector.
/// `picked` is how many files the menu acts on, for Split; `None` in the
/// working copy, which is split by committing what is ticked. `diff_tool`:
/// a diff application is chosen in Settings (without one the full view is
/// the diff, and the item is left out); `on_disk`: the file is in the
/// workspace's folder, to show. The names are the same on every
/// system.
#[allow(clippy::too_many_arguments)]
pub(super) fn file_menu(
    menu: PopupMenu,
    has_conflict: bool,
    immutable: bool,
    picked: Option<usize>,
    diff_tool: bool,
    on_disk: bool,
    focus: FocusHandle,
) -> PopupMenu {
    use actions::*;
    let menu = menu.action_context(focus);
    let menu = if has_conflict {
        menu.menu("Resolve in merge tool", Box::new(ResolveInTool))
            .separator()
    } else {
        menu
    };
    let menu = menu
        .menu("Open full view", Box::new(OpenFileDiff))
        .menu("Blame", Box::new(OpenFileBlame))
        .menu("File history", Box::new(OpenFileHistory))
        .separator();
    let menu = if diff_tool {
        menu.menu("Open in diff tool", Box::new(DiffInTool))
            .separator()
    } else {
        menu
    };
    let menu = menu
        .menu_with_disabled("Show in folder", Box::new(RevealFile), !on_disk)
        .menu("Copy full path", Box::new(CopyFullFilePath));
    if immutable {
        menu
    } else {
        let menu = menu.separator();
        let menu = match picked {
            Some(picked) if picked > 1 => menu.menu(
                format!("Split {picked} files into a new commit…"),
                Box::new(SplitFiles),
            ),
            Some(_) => menu.menu("Split into a new commit…", Box::new(SplitFiles)),
            None => menu,
        };
        menu.menu("Move to revision…", Box::new(PickMoveFile))
            .menu("Squash into parent", Box::new(SquashFile))
            .menu("Discard changes…", Box::new(RestoreFile))
    }
}
