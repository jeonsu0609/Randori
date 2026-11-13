//! The tab's commands: each of `actions` bound to what it does, on the tab's
//! root element (see `RepoView::render`).

use super::*;

impl RepoView {
    /// `root` with a handler for each of the tab's actions.
    pub(super) fn bind_actions(
        root: gpui_kit::Stateful<gpui_kit::Div>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        root.on_action(cx.listener(|view, _: &actions::FocusRevset, window, cx| {
            view.revset.update(cx, |state, cx| state.focus(window, cx));
        }))
        .on_action(cx.listener(|view, _: &actions::FocusLog, window, cx| {
            view.log_focus.focus(window, cx);
        }))
        .on_action(cx.listener(|view, _: &actions::Reload, window, cx| {
            if matches!(view.state, LoadState::Failed(_)) {
                view.retry_open(window, cx)
            } else {
                view.refresh_from_disk(true, cx)
            }
        }))
        .on_action(
            cx.listener(|view, _: &actions::InitRepository, window, cx| {
                if matches!(view.state, LoadState::Failed(_)) {
                    view.init_repository(window, cx)
                }
            }),
        )
        .on_action(cx.listener(|_, _: &actions::OrderByDate, _, cx| {
            settings::update_display(cx, |d| d.log_by_date = true)
        }))
        .on_action(cx.listener(|_, _: &actions::GroupByBranch, _, cx| {
            settings::update_display(cx, |d| d.log_by_date = false)
        }))
        .on_action(cx.listener(|view, _: &actions::ShowMyWork, window, cx| {
            view.set_log_scope(false, window, cx)
        }))
        .on_action(
            cx.listener(|view, _: &actions::ShowAllBranches, window, cx| {
                view.set_log_scope(true, window, cx)
            }),
        )
        // a column resize ends wherever the pointer is let go in the tab
        .on_drop(
            cx.listener(|view, _: &log_list::ColumnResize, _, cx| view.finish_column_resize(cx)),
        )
        .on_drop(
            cx.listener(|view, _: &split_diff::SplitResize, _, cx| view.finish_split_resize(cx)),
        )
        .on_action(
            cx.listener(|view, _: &actions::ToggleTerminal, window, cx| {
                view.toggle_terminal(window, cx)
            }),
        )
        .on_action(
            cx.listener(|view, _: &actions::WidenOriginalSide, _, cx| view.widen_original_side(cx)),
        )
        .on_action(
            cx.listener(|view, _: &actions::WidenModifiedSide, _, cx| view.widen_modified_side(cx)),
        )
        .on_action(
            cx.listener(|view, _: &actions::EvenSplitSides, _, cx| view.nudge_split(None, cx)),
        )
        .on_action(
            cx.listener(|view, _: &actions::SwitchWorkspace, window, cx| {
                view.open_workspace_picker(window, cx)
            }),
        )
        .on_action(
            cx.listener(|view, _: &actions::SwitchBookmark, window, cx| {
                view.open_bookmark_picker(window, cx)
            }),
        )
        .on_action(cx.listener(|view, _: &actions::NextConflict, window, cx| {
            view.select_next_conflict(window, cx)
        }))
        .on_action(cx.listener(|view, _: &actions::OpenFileDiff, window, cx| {
            view.open_focus(FocusTab::Diff, false, window, cx)
        }))
        .on_action(cx.listener(|view, _: &actions::OpenFileView, window, cx| {
            view.open_focus(FocusTab::File, false, window, cx)
        }))
        .on_action(cx.listener(|view, _: &actions::OpenFileBlame, window, cx| {
            view.open_focus(FocusTab::Blame, false, window, cx)
        }))
        .on_action(
            cx.listener(|view, _: &actions::OpenFileHistory, window, cx| {
                view.open_focus(FocusTab::Diff, true, window, cx)
            }),
        )
        .on_action(
            cx.listener(|view, _: &actions::RefreshPullRequests, _, cx| {
                view.refresh_pulls(true, cx)
            }),
        )
        .on_action(cx.listener(|view, _: &actions::ToggleSidebar, _, cx| {
            view.sidebar_visible = !view.sidebar_visible;
            cx.notify();
        }))
        .on_action(
            cx.listener(|view, _: &actions::NewChild, window, cx| view.new_child(window, cx)),
        )
        .on_action(
            cx.listener(|view, _: &actions::EditRevision, window, cx| {
                view.edit_selected(window, cx)
            }),
        )
        .on_action(
            cx.listener(|view, _: &actions::DescribeRevision, window, cx| {
                if view.blocked_by_pick(window, cx)
                    || view.refuse_if_immutable("Describe", window, cx)
                {
                    return;
                }
                // the working copy is described by committing it
                let editor = if view.commit_mode() {
                    &view.next_commit.message
                } else {
                    &view.description.editor
                };
                editor.focus(window, cx);
            }),
        )
        .on_action(
            cx.listener(|view, _: &actions::SaveDescription, window, cx| {
                view.save_description(window, cx)
            }),
        )
        .on_action(
            cx.listener(|view, _: &actions::DuplicateRevision, window, cx| {
                view.duplicate_selected(window, cx)
            }),
        )
        .on_action(
            cx.listener(|view, _: &actions::AbandonRevision, window, cx| {
                view.confirm_abandon(window, cx)
            }),
        )
        .on_action(
            cx.listener(|view, _: &actions::BackoutRevision, window, cx| {
                view.backout_selected(window, cx)
            }),
        )
        .on_action(
            cx.listener(|view, _: &actions::CreateBookmark, window, cx| {
                view.prompt_create_bookmark(window, cx)
            }),
        )
        .on_action(cx.listener(|view, _: &actions::PushBookmark, window, cx| {
            view.push_ref_target(window, cx)
        }))
        .on_action(
            cx.listener(|view, _: &actions::RenameBookmark, window, cx| {
                view.prompt_rename_bookmark(window, cx)
            }),
        )
        .on_action(cx.listener(|view, _: &actions::DeleteRef, window, cx| {
            view.delete_ref_target(window, cx)
        }))
        .on_action(
            cx.listener(|view, _: &actions::ToggleTrackBookmark, window, cx| {
                view.toggle_track_ref_target(window, cx)
            }),
        )
        .on_action(
            cx.listener(|view, _: &actions::PickRebase, window, cx| view.pick_rebase(window, cx)),
        )
        .on_action(
            cx.listener(|view, _: &actions::PickMoveBookmark, window, cx| {
                view.pick_move_bookmark(window, cx)
            }),
        )
        .on_action(cx.listener(|view, _: &actions::PickMoveFile, window, cx| {
            view.pick_move_file(window, cx)
        }))
        .on_action(cx.listener(|view, _: &actions::SquashFile, window, cx| {
            view.squash_file_into_parent(window, cx)
        }))
        .on_action(cx.listener(|view, _: &actions::SplitFiles, window, cx| {
            view.prompt_split_files(window, cx)
        }))
        .on_action(cx.listener(|view, _: &actions::RestoreFile, window, cx| {
            view.confirm_restore_file(window, cx)
        }))
        .on_action(cx.listener(|view, _: &actions::ResolveInTool, window, cx| {
            view.resolve_in_tool(window, cx)
        }))
        .on_action(
            cx.listener(|view, _: &actions::DiffInTool, window, cx| view.diff_in_tool(window, cx)),
        )
        .on_action(cx.listener(|view, _: &actions::SquashHunk, window, cx| {
            view.squash_hunk_into_parent(window, cx)
        }))
        .on_action(cx.listener(|view, _: &actions::SplitLines, window, cx| {
            view.prompt_split_lines(window, cx)
        }))
        .on_action(
            cx.listener(|view, _: &actions::IncludeAllChanges, _, cx| view.include_all(true, cx)),
        )
        .on_action(
            cx.listener(|view, _: &actions::ExcludeAllChanges, _, cx| view.include_all(false, cx)),
        )
        .on_action(
            cx.listener(|view, _: &actions::FindCommit, window, cx| view.open_find(window, cx)),
        )
        .on_action(
            cx.listener(|view, _: &actions::FocusPanelFilter, window, cx| {
                view.sidebar_visible = true;
                view.panel
                    .filter
                    .update(cx, |state, cx| state.focus(window, cx));
                cx.notify();
            }),
        )
        .on_action(cx.listener(|view, _: &actions::Undo, window, cx| view.undo(window, cx)))
        .on_action(cx.listener(|view, _: &actions::Redo, window, cx| view.redo(window, cx)))
        .on_action(cx.listener(|view, _: &actions::Fetch, window, cx| view.fetch_all(window, cx)))
        .on_action(
            cx.listener(|view, _: &actions::Push, window, cx| view.push_selected(window, cx)),
        )
    }
}
