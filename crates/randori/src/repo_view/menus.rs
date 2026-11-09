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
pub(super) fn file_menu(
    menu: PopupMenu,
    has_conflict: bool,
    immutable: bool,
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
        menu.separator()
            .menu("Move to revision…", Box::new(PickMoveFile))
            .menu("Squash into parent", Box::new(SquashFile))
            .menu("Discard changes…", Box::new(RestoreFile))
    }
}

/// Commands for a bookmark or tag in the bookmarks panel.
pub(super) fn ref_menu(menu: PopupMenu, store: &StoreRef, focus: FocusHandle) -> PopupMenu {
    use actions::*;
    let menu = menu.action_context(focus);
    match store {
        StoreRef::LocalBookmark { .. } => menu
            .menu("Push", Box::new(PushBookmark))
            .menu("Move to…", Box::new(PickMoveBookmark))
            .menu("Rename…", Box::new(RenameBookmark))
            .separator()
            .menu("Delete", Box::new(DeleteRef)),
        StoreRef::RemoteBookmark { is_tracked, .. } => menu
            .menu(
                if *is_tracked { "Untrack" } else { "Track" },
                Box::new(ToggleTrackBookmark),
            )
            .separator()
            .menu("Delete", Box::new(DeleteRef)),
        StoreRef::Tag { .. } => menu.menu("Delete", Box::new(DeleteRef)),
    }
}
