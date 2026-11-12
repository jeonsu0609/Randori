//! Committing the working copy, `jj commit` and `jj commit -i`: the working
//! copy is described and a new one starts on it. With files and lines included,
//! those are what the described revision keeps, and the new working copy has
//! the rest.

use anyhow::Result;
use async_trait::async_trait;
use jj_lib::{commit::conflict_label_for_commits, object_id::ObjectId as _, repo::Repo, rewrite};

use super::change::HunkMismatch;
use super::precondition;
use super::split::{STALE_LINES, tree_with_changes};
use crate::{
    messages::mutations::{CommitChanges, MutationOptions, MutationResult},
    worker::{Mutation, gui_util::WorkspaceSession},
};

#[async_trait(?Send)]
impl Mutation for CommitChanges {
    async fn execute(
        self: Box<Self>,
        ws: &mut WorkspaceSession,
        options: &MutationOptions,
    ) -> Result<MutationResult> {
        if self.description.trim().is_empty() {
            precondition!("A commit needs a description");
        }
        let mut tx = ws.start_transaction().await?;

        let commit = ws.resolve_change_id(&self.id)?;
        if commit.id() != ws.wc_id() {
            precondition!("Only the working copy is committed");
        }
        if ws.check_immutable([commit.id().clone()])? && !options.ignore_immutable {
            precondition!("Revision is immutable");
        }

        // what is included, on the parents' files: the revision committed
        let included =
            !self.paths.is_empty() || self.hunks.iter().any(|file| !file.hunks.is_empty());
        let committed_tree = if included {
            let parents = commit.parents().await?;
            if self.hunks.iter().any(|file| !file.hunks.is_empty()) && parents.len() != 1 {
                precondition!("Cannot commit lines of a merge commit");
            }
            let parent_tree = rewrite::merge_commit_trees(tx.repo(), &parents).await?;
            let selected = tree_with_changes(
                tx.repo().store(),
                &parent_tree,
                &commit.tree(),
                (
                    commit.conflict_label(),
                    conflict_label_for_commits(&parents),
                ),
                &self.paths,
                &self.hunks,
            )
            .await;
            let selected = match selected {
                // lines read from a diff before the file changed
                Err(err) if err.is::<HunkMismatch>() => precondition!("{STALE_LINES}"),
                result => result?,
            };
            if selected.tree_ids() == parent_tree.tree_ids() {
                precondition!("The selected changes are not in the working copy");
            }
            Some(selected)
        } else {
            None
        };

        let mut committed = tx
            .repo_mut()
            .rewrite_commit(&commit)
            .set_description(self.description);
        if let Some(tree) = committed_tree {
            committed = committed.set_tree(tree);
        }
        let committed = committed.write().await?;
        // the working copy's files as they were: on the commit, the rest
        let next = tx
            .repo_mut()
            .new_commit(vec![committed.id().clone()], commit.tree())
            .write()
            .await?;
        tx.repo_mut().edit(ws.name().to_owned(), &next).await?;

        match ws
            .finish_transaction(tx, format!("commit {}", commit.id().hex()))
            .await?
        {
            Some(new_status) => Ok(MutationResult::Updated {
                new_status,
                new_selection: None,
            }),
            None => Ok(MutationResult::Unchanged),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        messages::{
            ChangeHunk, ChangeLocation, ChangeRange, MultilineString, RevId, RevSet,
            mutations::{CreateRevision, DescribeRevision, FileHunks},
        },
        worker::{
            WorkerSession,
            tests::{mkrepo, revs},
        },
    };
    use anyhow::Result;
    use assert_matches::assert_matches;
    use std::fs;

    use super::super::split::tests::{changed_paths, tree_path};
    use crate::worker::mutations::read_file_content;
    use jj_lib::{merged_tree::MergedTree, repo_path::RepoPath};

    fn wc_id(ws: &WorkspaceSession<'_>) -> Result<RevId> {
        Ok(ws.format_id(&ws.get_commit(ws.wc_id())?))
    }

    /// A described base with `base`'s files, and a working copy on it.
    async fn on_a_base(ws: &mut WorkspaceSession<'_>) -> Result<jj_lib::commit::Commit> {
        DescribeRevision {
            id: revs::working_copy(),
            new_description: "base".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(ws)
        .await?;
        let base = ws.get_commit(ws.wc_id())?;
        CreateRevision {
            set: RevSet::singleton(ws.format_id(&base)),
        }
        .execute_unboxed(ws)
        .await?;
        Ok(base)
    }

    #[tokio::test]
    async fn a_commit_takes_every_change_or_those_included() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;
        let base = on_a_base(&mut ws).await?;
        crate::messages::mutations::CreateRef {
            id: wc_id(&ws)?,
            r#ref: crate::messages::StoreRef::LocalBookmark {
                bookmark_name: "topic".to_owned(),
                has_conflict: false,
                is_synced: false,
                tracking_remotes: vec![],
                available_remotes: 0,
                potential_remotes: 0,
            },
        }
        .execute_unboxed(&mut ws)
        .await?;
        fs::write(repo.path().join("x.txt"), "x\n").unwrap();
        fs::write(repo.path().join("y.txt"), "y\n").unwrap();
        let change = ws.get_commit(ws.wc_id())?.change_id().clone();

        // x.txt included: the working copy keeps its change id and x.txt, as
        // `jj commit -i` leaves them; the new working copy has y.txt
        let result = CommitChanges {
            id: wc_id(&ws)?,
            description: "add x".to_owned(),
            paths: vec![tree_path("x.txt")],
            hunks: vec![],
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::Updated { .. });
        let wc = ws.get_commit(ws.wc_id())?;
        let committed = ws.get_commit(&wc.parent_ids()[0])?;
        assert_eq!(committed.description().trim(), "add x");
        assert_eq!(committed.change_id(), &change);
        assert_eq!(committed.parent_ids(), &[base.id().clone()]);
        assert_eq!(
            changed_paths(&ws, ws.format_id(&committed)).await?,
            vec!["x.txt"]
        );
        assert_eq!(changed_paths(&ws, ws.format_id(&wc)).await?, vec!["y.txt"]);
        assert!(wc.description().is_empty());
        // the bookmark stays on the commit, as `jj commit` leaves it
        let matcher = jj_lib::str_util::StringMatcher::Exact("topic".to_owned());
        let (_, target) = ws
            .view()
            .local_bookmarks_matching(&matcher)
            .next()
            .expect("the bookmark is kept");
        assert_eq!(target.as_resolved(), Some(&Some(committed.id().clone())));

        // nothing included: every change, and a new empty working copy
        CommitChanges {
            id: wc_id(&ws)?,
            description: "add y".to_owned(),
            paths: vec![],
            hunks: vec![],
        }
        .execute_unboxed(&mut ws)
        .await?;
        let wc = ws.get_commit(ws.wc_id())?;
        let committed = ws.get_commit(&wc.parent_ids()[0])?;
        assert_eq!(committed.description().trim(), "add y");
        assert_eq!(
            changed_paths(&ws, ws.format_id(&committed)).await?,
            vec!["y.txt"]
        );
        assert!(changed_paths(&ws, ws.format_id(&wc)).await?.is_empty());
        // the files on disk never changed
        assert_eq!(fs::read_to_string(repo.path().join("y.txt"))?, "y\n");
        Ok(())
    }

    #[tokio::test]
    async fn a_commit_of_lines_keeps_the_rest_of_the_file_in_the_working_copy() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;
        fs::write(repo.path().join("f.txt"), "a\nb\n").unwrap();
        on_a_base(&mut ws).await?;
        fs::write(repo.path().join("f.txt"), "a\nb\nc\nd\n").unwrap();
        // `+c` only, `+d` left out
        let added_c = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 3, len: 0 },
                to_file: ChangeRange { start: 3, len: 1 },
            },
            lines: MultilineString {
                lines: vec!["+c\n".to_owned()],
            },
        };
        CommitChanges {
            id: wc_id(&ws)?,
            description: "add c".to_owned(),
            paths: vec![],
            hunks: vec![FileHunks {
                path: tree_path("f.txt"),
                hunks: vec![added_c],
            }],
        }
        .execute_unboxed(&mut ws)
        .await?;
        let store = ws.repo().store().clone();
        let read = |tree: MergedTree| {
            let store = store.clone();
            async move {
                read_file_content(&store, &tree, RepoPath::from_internal_string("f.txt")?).await
            }
        };
        let wc = ws.get_commit(ws.wc_id())?;
        let committed = ws.get_commit(&wc.parent_ids()[0])?;
        assert_eq!(read(committed.tree()).await?, b"a\nb\nc\n");
        assert_eq!(read(wc.tree()).await?, b"a\nb\nc\nd\n");
        Ok(())
    }

    #[tokio::test]
    async fn lines_of_a_changed_file_are_refused() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;
        fs::write(repo.path().join("f.txt"), "a\nb\n").unwrap();
        on_a_base(&mut ws).await?;
        fs::write(repo.path().join("f.txt"), "a\nb\nc\n").unwrap();

        // a hunk read when the file's second line was still `x`
        let stale = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 2, len: 1 },
                to_file: ChangeRange { start: 2, len: 2 },
            },
            lines: MultilineString {
                lines: vec![" x\n".to_owned(), "+c\n".to_owned()],
            },
        };
        let result = CommitChanges {
            id: wc_id(&ws)?,
            description: "add c".to_owned(),
            paths: vec![],
            hunks: vec![FileHunks {
                path: tree_path("f.txt"),
                hunks: vec![stale],
            }],
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(
            result,
            MutationResult::PreconditionError { message } if message.contains("changed since")
        );
        Ok(())
    }

    #[tokio::test]
    async fn only_the_working_copy_is_committed() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;
        fs::write(repo.path().join("x.txt"), "x\n").unwrap();
        let base = on_a_base(&mut ws).await?;
        // an undescribed revision of one's own stays what it is
        let result = CommitChanges {
            id: ws.format_id(&base),
            description: "again".to_owned(),
            paths: vec![],
            hunks: vec![],
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::PreconditionError { .. });
        assert_eq!(ws.get_commit(base.id())?.description().trim(), "base");
        Ok(())
    }
}
