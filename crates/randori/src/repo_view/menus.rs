//! Context menus. Items dispatch actions to the tab, so they show the same key
//! bindings as the keyboard.

use super::*;

// ----- context menus ---------------------------------------------------------------------------

/// Commands for the selected revision. Items dispatch actions to the repository
/// tab, so the menu shows the same key bindings as the keyboard.
pub(super) fn revision_menu(menu: PopupMenu, focus: FocusHandle) -> PopupMenu {
    use actions::*;
    menu.action_context(focus)
        .menu("New change", Box::new(NewChild))
        .menu("Edit", Box::new(EditRevision))
        .menu("Describe", Box::new(DescribeRevision))
        .separator()
        .menu("Rebase onto…", Box::new(PickRebase))
        .menu("Create bookmark…", Box::new(CreateBookmark))
        .menu("Push bookmarks", Box::new(Push))
        .separator()
        .menu("Duplicate", Box::new(DuplicateRevision))
        .menu("Back out", Box::new(BackoutRevision))
        .separator()
        .menu("Abandon…", Box::new(AbandonRevision))
}

/// Commands for a changed file in the inspector.
/// `picked` is how many files the menu acts on, for Split; `None` in the
/// working copy, which is split by committing what is ticked.
pub(super) fn file_menu(
    menu: PopupMenu,
    has_conflict: bool,
    immutable: bool,
    picked: Option<usize>,
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
        .separator()
        .menu("Open in diff tool", Box::new(DiffInTool));
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
