//! Splitting a revision, as `jj split` does: the revision keeps its change
//! id and the files and lines selected, with a new description; a new
//! revision on it has the rest, with the old description, and its bookmarks,
//! children and working copy move there. [`tree_with_changes`] puts picked changes on a
//! tree, for committing some of them too (see `commit`).

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use jj_lib::{
    backend::TreeValue, commit::conflict_label_for_commits, merge::Merge, merged_tree::MergedTree,
    object_id::ObjectId as _, repo::Repo, repo_path::RepoPath, rewrite,
};

use super::change::{HunkMismatch, apply_hunk_to_base, build_matcher, update_tree_entry};
use super::{precondition, read_file_content};
use crate::{
    messages::{
        TreePath,
        mutations::{FileHunks, MutationOptions, MutationResult, SplitRevision},
    },
    worker::{Mutation, gui_util::WorkspaceSession},
};

#[async_trait(?Send)]
impl Mutation for SplitRevision {
    async fn execute(
        self: Box<Self>,
        ws: &mut WorkspaceSession,
        options: &MutationOptions,
    ) -> Result<MutationResult> {
        if self.paths.is_empty() && self.hunks.iter().all(|file| file.hunks.is_empty()) {
            precondition!("Nothing to split off");
        }
        let mut tx = ws.start_transaction().await?;

        let commit = ws.resolve_change_id(&self.id)?;
        if ws.check_immutable([commit.id().clone()])? && !options.ignore_immutable {
            precondition!("Revision is immutable");
        }

        let parents = commit.parents().await?;
        if self.hunks.iter().any(|file| !file.hunks.is_empty()) && parents.len() != 1 {
            precondition!("Cannot split lines off a merge commit");
        }
        // the parents' tree with the selected files and lines as the revision has them
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
            // lines picked from a diff read before the file changed
            Err(err) if err.is::<HunkMismatch>() => precondition!("{STALE_LINES}"),
            result => result?,
        };
        if selected.tree_ids() == parent_tree.tree_ids() {
            precondition!("The selected files do not change in this revision");
        }

        // as jj-cli 0.46 splits: the selected changes keep the change id
        let first = tx
            .repo_mut()
            .rewrite_commit(&commit)
            .set_tree(selected)
            .set_description(super::jj_description(&self.description))
            .write()
            .await?;
        // the revision's tree as it was: on the first, what is left is the
        // rest, a new change with the old description
        let second = tx
            .repo_mut()
            .rewrite_commit(&commit)
            .clear_rewrite_source()
            .generate_new_change_id()
            .set_parents(vec![first.id().clone()])
            .set_tree(commit.tree())
            .write()
            .await?;
        // bookmarks, children and working copies go on to the rest, as
        // `split.legacy-bookmark-behavior` (jj's default) has them
        tx.repo_mut()
            .set_rewritten_commit(commit.id().clone(), second.id().clone());

        match ws
            .finish_transaction(tx, format!("split commit {}", commit.id().hex()))
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

/// What a command on lines says when their file changed since its diff was
/// shown, which they were read from.
pub(super) const STALE_LINES: &str =
    "The file has changed since its diff was shown. Select the lines again.";

/// `base` with the changes `paths` and `hunks` pick from `source`: each of
/// `paths` as `source` has it, and `hunks` applied to `base`'s lines. Lines
/// move without the file's mode: a file keeps `base`'s, and one `base` lacks
/// takes `source`'s and ends its last line as `source` does. `labels` name
/// `source` and `base` in conflicts.
pub(super) async fn tree_with_changes(
    store: &Arc<jj_lib::store::Store>,
    base: &MergedTree,
    source: &MergedTree,
    labels: (String, String),
    paths: &[TreePath],
    hunks: &[FileHunks],
) -> Result<MergedTree> {
    let mut tree = if paths.is_empty() {
        base.clone()
    } else {
        let matcher = build_matcher(paths)?;
        rewrite::restore_tree(source, base, labels.0, labels.1, matcher.as_ref()).await?
    };
    for file in hunks.iter().filter(|file| !file.hunks.is_empty()) {
        let repo_path = RepoPath::from_internal_string(&file.path.repo_path)?;
        let mut content = read_file_content(store, base, repo_path).await?;
        let new_file = content.is_empty();
        // from the bottom up, so the lines a hunk starts at are still the base's
        let mut hunks = file.hunks.clone();
        hunks.sort_by_key(|hunk| std::cmp::Reverse(hunk.location.from_file.start));
        for hunk in &hunks {
            content = apply_hunk_to_base(&content, hunk)?;
        }
        // applying keeps the base's last newline; a new file has no base,
        // so its last line ends as the source's does
        if new_file
            && !content.is_empty()
            && read_file_content(store, source, repo_path)
                .await?
                .ends_with(b"\n")
        {
            content.push(b'\n');
        }
        let blob = store.write_file(repo_path, &mut content.as_slice()).await?;
        let mode_of = |value: Merge<Option<TreeValue>>| match value.into_resolved() {
            Ok(Some(TreeValue::File { executable, .. })) => Some(executable),
            _ => None,
        };
        let executable = match mode_of(base.path_value(repo_path).await?) {
            Some(executable) => executable,
            None => mode_of(source.path_value(repo_path).await?).unwrap_or(false),
        };
        tree = update_tree_entry(store, &tree, repo_path, blob, executable).await?;
    }
    Ok(tree)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::{
        messages::{
            ChangeHunk, ChangeLocation, ChangeRange, MultilineString, RevId, RevSet,
            mutations::{CreateRevision, DescribeRevision},
            queries::RevsResult,
        },
        worker::{
            WorkerSession,
            tests::{mkrepo, query_by_id, revs},
        },
    };
    use anyhow::Result;
    use assert_matches::assert_matches;
    use std::fs;

    pub(in crate::worker::mutations) fn tree_path(path: &str) -> TreePath {
        TreePath {
            repo_path: path.to_owned(),
            relative_path: "".into(),
        }
    }

    pub(in crate::worker::mutations) async fn changed_paths(
        ws: &WorkspaceSession<'_>,
        id: RevId,
    ) -> Result<Vec<String>> {
        match query_by_id(ws, id).await? {
            RevsResult::Detail { changes, .. } => {
                Ok(changes.into_iter().map(|c| c.path.repo_path).collect())
            }
            RevsResult::NotFound { .. } => panic!("revision not found"),
        }
    }

    #[tokio::test]
    async fn split_revision_puts_the_selected_files_before_it() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        fs::write(repo.path().join("x.txt"), "x content").unwrap();
        fs::write(repo.path().join("y.txt"), "y content").unwrap();
        DescribeRevision {
            id: revs::working_copy(),
            new_description: "work".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;
        crate::messages::mutations::CreateRef {
            id: revs::working_copy(),
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
        let wc = ws.get_commit(ws.wc_id())?;
        let wc_id = ws.format_id(&wc);
        let old_parents = wc.parent_ids().to_vec();

        let result = SplitRevision {
            id: wc_id.clone(),
            paths: vec![tree_path("x.txt")],
            hunks: vec![],
            description: "add x".to_owned(),
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // the same change is still the working copy, with the rest of it
        // as `jj split x.txt`: the rest is a new change, still the working
        // copy, with the old description and the bookmark
        let wc = ws.get_commit(ws.wc_id())?;
        assert_ne!(ws.format_id(&wc).change.hex, wc_id.change.hex);
        assert!(wc.description().starts_with("work"));
        assert_eq!(changed_paths(&ws, ws.format_id(&wc)).await?, vec!["y.txt"]);
        let matcher = jj_lib::str_util::StringMatcher::Exact("topic".to_owned());
        let (_, target) = ws
            .view()
            .local_bookmarks_matching(&matcher)
            .next()
            .expect("the bookmark is kept");
        assert_eq!(target.as_resolved(), Some(&Some(wc.id().clone())));

        // under it, the new revision with the selected file, where it was
        // the selected changes keep the change id
        let first = ws.get_commit(&wc.parent_ids()[0])?;
        assert_eq!(wc.parent_ids().len(), 1);
        assert_eq!(ws.format_id(&first).change.hex, wc_id.change.hex);
        assert_eq!(first.description().trim(), "add x");
        assert_eq!(first.parent_ids(), old_parents.as_slice());
        assert_eq!(
            changed_paths(&ws, ws.format_id(&first)).await?,
            vec!["x.txt"]
        );

        // the files on disk are as they were
        assert_eq!(fs::read_to_string(repo.path().join("x.txt"))?, "x content");
        assert_eq!(fs::read_to_string(repo.path().join("y.txt"))?, "y content");
        Ok(())
    }

    #[tokio::test]
    async fn children_of_a_split_revision_move_onto_the_rest() -> Result<()> {
        // as `jj split -r target x.txt` leaves them: first (the change id,
        // x.txt), then the rest (a new change, the old description, y.txt),
        // then the child
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;
        fs::write(repo.path().join("x.txt"), "x\n").unwrap();
        fs::write(repo.path().join("y.txt"), "y\n").unwrap();
        DescribeRevision {
            id: revs::working_copy(),
            new_description: "target".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;
        let target = ws.get_commit(ws.wc_id())?;
        CreateRevision {
            set: RevSet::singleton(ws.format_id(&target)),
        }
        .execute_unboxed(&mut ws)
        .await?;
        fs::write(repo.path().join("z.txt"), "z\n").unwrap();
        let child_change = ws.get_commit(ws.wc_id())?.change_id().clone();

        SplitRevision {
            id: ws.format_id(&target),
            paths: vec![tree_path("x.txt")],
            hunks: vec![],
            description: "first".to_owned(),
        }
        .execute_unboxed(&mut ws)
        .await?;
        let child = ws.get_commit(ws.wc_id())?;
        assert_eq!(child.change_id(), &child_change);
        let rest = ws.get_commit(&child.parent_ids()[0])?;
        assert_ne!(rest.change_id(), target.change_id());
        assert_eq!(rest.description(), "target\n");
        assert_eq!(
            changed_paths(&ws, ws.format_id(&rest)).await?,
            vec!["y.txt"]
        );
        let first = ws.get_commit(&rest.parent_ids()[0])?;
        assert_eq!(first.change_id(), target.change_id());
        assert_eq!(first.description(), "first\n");
        assert_eq!(
            changed_paths(&ws, ws.format_id(&first)).await?,
            vec!["x.txt"]
        );
        Ok(())
    }

    #[tokio::test]
    async fn split_revision_takes_only_the_selected_lines() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // a parent with the file, and a working copy that adds two lines to it
        fs::write(repo.path().join("f.txt"), "a\nb\n").unwrap();
        DescribeRevision {
            id: revs::working_copy(),
            new_description: "base".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;
        let base = ws.get_commit(ws.wc_id())?;
        CreateRevision {
            set: RevSet::singleton(ws.format_id(&base)),
        }
        .execute_unboxed(&mut ws)
        .await?;
        fs::write(repo.path().join("f.txt"), "a\nb\nc\nd\n").unwrap();
        fs::write(repo.path().join("g.txt"), "g\n").unwrap();
        DescribeRevision {
            id: ws.format_id(&ws.get_commit(ws.wc_id())?),
            new_description: "work".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;
        let wc_id = ws.format_id(&ws.get_commit(ws.wc_id())?);

        // the hunk `+c +d`, with `+d` left behind
        let partial = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 3, len: 0 },
                to_file: ChangeRange { start: 3, len: 1 },
            },
            lines: MultilineString {
                lines: vec!["+c\n".to_owned()],
            },
        };
        let result = SplitRevision {
            id: wc_id,
            paths: vec![],
            hunks: vec![FileHunks {
                path: tree_path("f.txt"),
                hunks: vec![partial],
            }],
            description: "add c".to_owned(),
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::Updated { .. });

        let wc = ws.get_commit(ws.wc_id())?;
        let first = ws.get_commit(&wc.parent_ids()[0])?;
        assert_eq!(first.description().trim(), "add c");
        assert_eq!(first.parent_ids(), &[base.id().clone()]);
        let store = ws.repo().store().clone();
        let f = RepoPath::from_internal_string("f.txt")?;
        let read = |tree| {
            let store = store.clone();
            async move { read_file_content(&store, &tree, f).await }
        };
        assert_eq!(read(first.tree()).await?, b"a\nb\nc\n");
        // the rest stays in the working copy: `+d`, and all of g.txt
        assert_eq!(read(wc.tree()).await?, b"a\nb\nc\nd\n");
        assert_eq!(
            changed_paths(&ws, ws.format_id(&first)).await?,
            vec!["f.txt"]
        );
        let mut rest = changed_paths(&ws, ws.format_id(&wc)).await?;
        rest.sort();
        assert_eq!(rest, vec!["f.txt", "g.txt"]);
        Ok(())
    }

    #[tokio::test]
    async fn split_revision_ends_lines_as_the_files_do() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // a parent whose file has no last newline, and a working copy that
        // adds a line to it (and the newline), and a new file of two lines
        fs::write(repo.path().join("f.txt"), "a\nb").unwrap();
        DescribeRevision {
            id: revs::working_copy(),
            new_description: "base".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;
        let base = ws.get_commit(ws.wc_id())?;
        CreateRevision {
            set: RevSet::singleton(ws.format_id(&base)),
        }
        .execute_unboxed(&mut ws)
        .await?;
        fs::write(repo.path().join("f.txt"), "a\nX\nb\n").unwrap();
        fs::write(repo.path().join("n.txt"), "1\n2\n").unwrap();
        DescribeRevision {
            id: ws.format_id(&ws.get_commit(ws.wc_id())?),
            new_description: "work".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;
        let wc_id = ws.format_id(&ws.get_commit(ws.wc_id())?);

        let added = |at: usize, line: &str| ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: at, len: 0 },
                to_file: ChangeRange { start: at, len: 1 },
            },
            lines: MultilineString {
                lines: vec![format!("+{line}\n")],
            },
        };
        // `+X` alone, not the newline; the new file's first line alone
        let result = SplitRevision {
            id: wc_id,
            paths: vec![],
            hunks: vec![
                FileHunks {
                    path: tree_path("f.txt"),
                    hunks: vec![added(2, "X")],
                },
                FileHunks {
                    path: tree_path("n.txt"),
                    hunks: vec![added(1, "1")],
                },
            ],
            description: "some lines".to_owned(),
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::Updated { .. });

        let wc = ws.get_commit(ws.wc_id())?;
        let first = ws.get_commit(&wc.parent_ids()[0])?;
        let store = ws.repo().store().clone();
        let read = |tree, path: &'static str| {
            let store = store.clone();
            async move { read_file_content(&store, &tree, RepoPath::from_internal_string(path)?).await }
        };
        // the parent's file still ends without a newline; the new file's
        // line ends as its lines in the revision do
        assert_eq!(read(first.tree(), "f.txt").await?, b"a\nX\nb");
        assert_eq!(read(first.tree(), "n.txt").await?, b"1\n");
        // the revision's files are as they were
        assert_eq!(read(wc.tree(), "f.txt").await?, b"a\nX\nb\n");
        assert_eq!(read(wc.tree(), "n.txt").await?, b"1\n2\n");
        Ok(())
    }

    #[tokio::test]
    async fn split_revision_refuses_files_it_does_not_change() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        fs::write(repo.path().join("x.txt"), "x content").unwrap();
        DescribeRevision {
            id: revs::working_copy(),
            new_description: "work".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;
        let wc_id = ws.format_id(&ws.get_commit(ws.wc_id())?);

        for paths in [vec![], vec![tree_path("not-changed.txt")]] {
            let result = SplitRevision {
                id: wc_id.clone(),
                paths,
                hunks: vec![],
                description: "nothing".to_owned(),
            }
            .execute_unboxed(&mut ws)
            .await?;
            assert_matches!(result, MutationResult::PreconditionError { .. });
        }
        Ok(())
    }
}
