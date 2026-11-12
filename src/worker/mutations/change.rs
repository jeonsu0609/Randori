use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use itertools::Itertools;
use jj_lib::{
    backend::{CommitId, CopyId, FileId, TreeValue},
    commit::{Commit, conflict_label_for_commits},
    matchers::{EverythingMatcher, FilesMatcher, Matcher},
    merge::Merge,
    merged_tree::MergedTree,
    merged_tree_builder::MergedTreeBuilder,
    object_id::ObjectId as ObjectIdTrait,
    repo::Repo,
    repo_path::RepoPath,
    revset::RevsetExpression,
    rewrite::{self, RebaseOptions, RebasedCommit},
};

use super::{precondition, read_file_content};

pub use crate::{
    messages::{
        ChangeHunk, TreePath,
        mutations::{
            CommitChanges, CopyChanges, CopyHunk, FileHunks, IgnoreNewFiles, MoveChanges, MoveHunk,
            MutationOptions, MutationResult, SplitRevision, StageChanges, UnstageChanges,
        },
    },
    worker::{Mutation, gui_util::WorkspaceSession},
};

#[async_trait(?Send)]
impl Mutation for MoveChanges {
    async fn execute(
        self: Box<Self>,
        ws: &mut WorkspaceSession,
        options: &MutationOptions,
    ) -> Result<MutationResult> {
        let mut tx = ws.start_transaction().await?;

        // resolve & check destination
        let to_id = CommitId::try_from_hex(&self.to_id.hex).expect("frontend-validated id");
        if ws.check_immutable([to_id.clone()])? && !options.ignore_immutable {
            precondition!("Destination revision is immutable");
        }

        let mut to = ws.get_commit(&to_id)?;

        // resolve & check source
        let (from_commits, is_immutable) = ws.resolve_change_set(&self.from, true)?;
        if is_immutable && !options.ignore_immutable {
            precondition!("Some source revisions are immutable");
        }

        let from_newest = from_commits
            .first()
            .ok_or_else(|| anyhow!("empty revset"))?;
        let from_oldest = from_commits.last().ok_or_else(|| anyhow!("empty revset"))?;

        // construct trees: from_tree is newest state, parent_tree is base before oldest
        let matcher = build_matcher(&self.paths)?;
        let from_tree = from_newest.tree();
        let oldest_parents = from_oldest.parents().await?;
        let parent_tree = rewrite::merge_commit_trees(tx.repo(), &oldest_parents).await?;
        let split_tree = rewrite::restore_tree(
            &from_tree,
            &parent_tree,
            from_newest.conflict_label(),
            conflict_label_for_commits(&oldest_parents),
            matcher.as_ref(),
        )
        .await?;

        // all sources will be abandoned if all changes in the range were selected
        let abandon_all = split_tree.tree_ids() == from_tree.tree_ids();

        // process each source commit: abandon, rewrite, or leave unchanged
        for commit in &from_commits {
            let commit_tree = commit.tree();
            let commit_parents = commit.parents().await?;
            let commit_parent_tree =
                rewrite::merge_commit_trees(tx.repo(), &commit_parents).await?;
            let commit_remainder = rewrite::restore_tree(
                &commit_parent_tree,
                &commit_tree,
                conflict_label_for_commits(&commit_parents),
                commit.conflict_label(),
                matcher.as_ref(),
            )
            .await?;

            if commit_remainder.tree_ids() == commit_tree.tree_ids() {
                // commit didn't touch selected paths - leave unchanged
            } else if commit_remainder.tree_ids() == commit_parent_tree.tree_ids() {
                // commit only touched selected paths - abandon it
                tx.repo_mut().record_abandoned_commit(commit);
            } else {
                // commit touched both - rewrite with remaining changes
                tx.repo_mut()
                    .rewrite_commit(commit)
                    .set_tree(commit_remainder)
                    .write()
                    .await?;
            }
        }

        // rebase descendants of source, which may include destination
        if tx
            .repo()
            .index()
            .is_ancestor(from_oldest.id(), to.id())
            .await?
        {
            let mut rebase_map = std::collections::HashMap::new();
            tx.repo_mut()
                .rebase_descendants_with_options(
                    &RevsetExpression::none(),
                    &RebaseOptions::default(),
                    |old_commit, rebased_commit| {
                        rebase_map.insert(
                            old_commit.id().clone(),
                            match rebased_commit {
                                RebasedCommit::Rewritten(new_commit) => new_commit.id().clone(),
                                RebasedCommit::Abandoned { parent_id } => parent_id,
                            },
                        );
                    },
                )
                .await?;
            let rebased_to_id = rebase_map
                .get(to.id())
                .ok_or_else(|| anyhow!("descendant to_commit not found in rebase map"))?
                .clone();
            to = tx.repo().store().get_commit(&rebased_to_id)?;
        }

        // apply changes to destination
        let to_tree = to.tree();
        let new_to_tree = MergedTree::merge(Merge::from_vec(vec![
            (
                to_tree,
                format!("{} (move destination)", to.conflict_label()),
            ),
            (
                parent_tree,
                format!(
                    "{} (parents of moved revision)",
                    from_oldest.conflict_label()
                ),
            ),
            (
                split_tree,
                format!("{} (moved changes)", from_newest.conflict_label()),
            ),
        ]))
        .await?;
        let source_refs: Vec<_> = from_commits.iter().collect();
        let description = combine_messages(&source_refs, &to, abandon_all);
        tx.repo_mut()
            .rewrite_commit(&to)
            .set_tree(new_to_tree)
            .set_description(description)
            .write()
            .await?;

        match ws
            .finish_transaction(
                tx,
                format!(
                    "move changes from {}::{} to {}",
                    from_oldest.id().hex(),
                    from_newest.id().hex(),
                    to.id().hex()
                ),
            )
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
        .await?;
        if selected.tree_ids() == parent_tree.tree_ids() {
            precondition!("The selected files do not change in this revision");
        }

        let first = tx
            .repo_mut()
            .new_commit(
                parents.iter().map(|parent| parent.id().clone()).collect(),
                selected,
            )
            .set_description(self.description)
            .write()
            .await?;
        // its tree as it was: on the new parent, what is left is the rest
        tx.repo_mut()
            .rewrite_commit(&commit)
            .set_parents(vec![first.id().clone()])
            .write()
            .await?;

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

/// The staging revision of a working copy whose parents are `parents` (see
/// [`StageChanges`]): its one parent, when that is undescribed, mutable (so
/// not the root) and no workspace's working copy, which staging would rewrite.
fn staging_revision(ws: &WorkspaceSession, parents: &[Commit]) -> Result<Option<Commit>> {
    let [parent] = parents else {
        return Ok(None);
    };
    let working_copy = ws
        .repo()
        .view()
        .wc_commit_ids()
        .values()
        .any(|id| id == parent.id());
    let staging = parent.description().trim().is_empty()
        && !working_copy
        && !ws.check_immutable([parent.id().clone()])?;
    Ok(staging.then(|| parent.clone()))
}

#[async_trait(?Send)]
impl Mutation for StageChanges {
    async fn execute(
        self: Box<Self>,
        ws: &mut WorkspaceSession,
        options: &MutationOptions,
    ) -> Result<MutationResult> {
        if self.paths.is_empty() && self.hunks.iter().all(|file| file.hunks.is_empty()) {
            precondition!("Nothing to stage");
        }
        let mut tx = ws.start_transaction().await?;

        let commit = ws.resolve_change_id(&self.id)?;
        if ws.check_immutable([commit.id().clone()])? && !options.ignore_immutable {
            precondition!("Revision is immutable");
        }
        let parents = commit.parents().await?;
        let [parent] = parents.as_slice() else {
            precondition!("Cannot stage changes of a merge commit");
        };
        let staged = tree_with_changes(
            tx.repo().store(),
            &parent.tree(),
            &commit.tree(),
            (commit.conflict_label(), parent.conflict_label()),
            &self.paths,
            &self.hunks,
        )
        .await?;
        if staged.tree_ids() == parent.tree().tree_ids() {
            precondition!("The selected changes are not in this revision");
        }

        // into the staging revision, or a new one under the working copy
        let staging = match staging_revision(ws, &parents)? {
            Some(staging) => {
                tx.repo_mut()
                    .rewrite_commit(&staging)
                    .set_tree(staged)
                    .write()
                    .await?
            }
            None => {
                tx.repo_mut()
                    .new_commit(vec![parent.id().clone()], staged)
                    .write()
                    .await?
            }
        };
        // the working copy's files as they were: on the staging revision,
        // what is left is what is not staged
        tx.repo_mut()
            .rewrite_commit(&commit)
            .set_parents(vec![staging.id().clone()])
            .write()
            .await?;

        match ws.finish_transaction(tx, "stage changes").await? {
            Some(new_status) => Ok(MutationResult::Updated {
                new_status,
                new_selection: None,
            }),
            None => Ok(MutationResult::Unchanged),
        }
    }
}

#[async_trait(?Send)]
impl Mutation for UnstageChanges {
    async fn execute(
        self: Box<Self>,
        ws: &mut WorkspaceSession,
        options: &MutationOptions,
    ) -> Result<MutationResult> {
        if self.paths.is_empty() && self.hunks.iter().all(|file| file.hunks.is_empty()) {
            precondition!("Nothing to unstage");
        }
        let mut tx = ws.start_transaction().await?;

        let commit = ws.resolve_change_id(&self.id)?;
        if ws.check_immutable([commit.id().clone()])? && !options.ignore_immutable {
            precondition!("Revision is immutable");
        }
        let parents = commit.parents().await?;
        let Some(staging) = staging_revision(ws, &parents)? else {
            precondition!("Nothing is staged");
        };
        let below = staging.parents().await?;
        let below_tree = rewrite::merge_commit_trees(tx.repo(), &below).await?;
        // the staging revision without the selected changes: the files as
        // its parents have them, the lines read backwards
        let kept = tree_with_changes(
            tx.repo().store(),
            &staging.tree(),
            &below_tree,
            (conflict_label_for_commits(&below), staging.conflict_label()),
            &self.paths,
            &self.hunks,
        )
        .await?;
        if kept.tree_ids() == staging.tree().tree_ids() {
            precondition!("The selected changes are not staged");
        }

        let new_parents = if kept.tree_ids() == below_tree.tree_ids() {
            // nothing left staged: no staging revision
            tx.repo_mut().record_abandoned_commit(&staging);
            staging.parent_ids().to_vec()
        } else {
            let staging = tx
                .repo_mut()
                .rewrite_commit(&staging)
                .set_tree(kept)
                .write()
                .await?;
            vec![staging.id().clone()]
        };
        // the working copy's files as they were, so the changes are its again
        tx.repo_mut()
            .rewrite_commit(&commit)
            .set_parents(new_parents)
            .write()
            .await?;

        match ws.finish_transaction(tx, "unstage changes").await? {
            Some(new_status) => Ok(MutationResult::Updated {
                new_status,
                new_selection: None,
            }),
            None => Ok(MutationResult::Unchanged),
        }
    }
}

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
        if ws.check_immutable([commit.id().clone()])? && !options.ignore_immutable {
            precondition!("Revision is immutable");
        }
        let parents = commit.parents().await?;
        // a staging revision with nothing in it has nothing to commit: it
        // goes, and the working copy is committed on what is under it
        let mut staging = staging_revision(ws, &parents)?;
        let mut empty_staging = None;
        if let Some(revision) = &staging {
            let below = rewrite::merge_commit_trees(tx.repo(), &revision.parents().await?).await?;
            if revision.tree().tree_ids() == below.tree_ids() {
                empty_staging = staging.take();
            }
        }
        let message = match staging {
            Some(staging) => {
                tx.repo_mut()
                    .rewrite_commit(&staging)
                    .set_description(self.description)
                    .write()
                    .await?;
                format!("commit staged changes {}", staging.id().hex())
            }
            None => {
                let mut builder = tx
                    .repo_mut()
                    .rewrite_commit(&commit)
                    .set_description(self.description);
                if let Some(empty) = &empty_staging {
                    builder = builder.set_parents(empty.parent_ids().to_vec());
                }
                let committed = builder.write().await?;
                if let Some(empty) = &empty_staging {
                    tx.repo_mut().record_abandoned_commit(empty);
                }
                let next = tx
                    .repo_mut()
                    .new_commit(vec![committed.id().clone()], committed.tree())
                    .write()
                    .await?;
                tx.repo_mut().edit(ws.name().to_owned(), &next).await?;
                format!("commit {}", commit.id().hex())
            }
        };

        match ws.finish_transaction(tx, message).await? {
            Some(new_status) => Ok(MutationResult::Updated {
                new_status,
                new_selection: None,
            }),
            None => Ok(MutationResult::Unchanged),
        }
    }
}

#[async_trait(?Send)]
impl Mutation for CopyChanges {
    async fn execute(
        self: Box<Self>,
        ws: &mut WorkspaceSession,
        options: &MutationOptions,
    ) -> Result<MutationResult> {
        let mut tx = ws.start_transaction().await?;

        let from = ws.resolve_commit_id(&self.from_id)?;
        let from_tree = from.tree();
        let matcher = build_matcher(&self.paths)?;

        let (commits, is_immutable) = ws.resolve_change_set(&self.to_set, true)?;
        if is_immutable && !options.ignore_immutable {
            if commits.len() == 1 {
                precondition!("Destination revision is immutable");
            } else {
                precondition!("Some destination revisions are immutable");
            }
        }

        if commits.is_empty() {
            return Ok(MutationResult::Unchanged);
        }

        // walk up the range, replacing the specified changes to eliminate each revision's contribution to the combined diff
        let mut any_changed = false;
        for commit in commits.iter().rev() {
            let to_tree = commit.tree();
            let new_tree = rewrite::restore_tree(
                &from_tree,
                &to_tree,
                from.conflict_label(),
                commit.conflict_label(),
                matcher.as_ref(),
            )
            .await?;

            if new_tree.tree_ids() != to_tree.tree_ids() {
                any_changed = true;
                tx.repo_mut()
                    .rewrite_commit(commit)
                    .set_tree(new_tree)
                    .write()
                    .await?;
            }
        }

        if !any_changed {
            return Ok(MutationResult::Unchanged);
        }

        tx.repo_mut().rebase_descendants().await?;

        let description = if commits.len() == 1 {
            format!("restore into commit {}", commits[0].id().hex())
        } else {
            format!("restore into {} commits", commits.len())
        };

        match ws.finish_transaction(tx, description).await? {
            Some(new_status) => Ok(MutationResult::Updated {
                new_status,
                new_selection: None,
            }),
            None => Ok(MutationResult::Unchanged),
        }
    }
}

#[async_trait(?Send)]
impl Mutation for MoveHunk {
    async fn execute(
        self: Box<Self>,
        ws: &mut WorkspaceSession,
        options: &MutationOptions,
    ) -> Result<MutationResult> {
        let from = ws.resolve_change_id(&self.from_id)?;
        let mut to = ws.resolve_commit_id(&self.to_id)?;

        if ws.check_immutable(vec![from.id().clone(), to.id().clone()])?
            && !options.ignore_immutable
        {
            precondition!("Some revisions are immutable");
        }

        // Split-rebase-squash algorithm:
        // - sibling_tree represents a virtual commit with just the hunk (like jj split)
        // - from_tree is modified by extracting the hunk, and its descendants updated (like jj rebase)
        // - to_tree is given the added hunk by doing a 3-way merge (like jj squash)
        let mut tx: jj_lib::transaction::Transaction = ws.start_transaction().await?;
        let repo_path = RepoPath::from_internal_string(&self.path.repo_path)?;

        // Get the base tree (from's parent) - this is the tree the hunk was computed against
        let from_tree = from.tree();
        let from_parents = from.parents().await?;
        if from_parents.len() != 1 {
            precondition!("Cannot move hunk from a merge commit");
        }
        let base_tree = from_parents[0].tree();

        // Construct the "sibling tree": base_tree with just this hunk applied.
        // This represents a virtual sibling commit containing only the hunk.
        let store = tx.repo().store();
        let base_content = read_file_content(store, &base_tree, repo_path).await?;
        let sibling_content = apply_hunk_to_base(&base_content, &self.hunk)?;
        let sibling_blob_id = store
            .write_file(repo_path, &mut sibling_content.as_slice())
            .await?;
        let sibling_executable = match from_tree.path_value(repo_path).await?.into_resolved() {
            Ok(Some(TreeValue::File { executable, .. })) => executable,
            Ok(_) => false,
            Err(_) => false,
        };
        let sibling_tree = update_tree_entry(
            store,
            &base_tree,
            repo_path,
            sibling_blob_id,
            sibling_executable,
        )
        .await?;

        // Remove hunk from source: backout the base→sibling diff from from_tree
        let remainder_tree = MergedTree::merge(Merge::from_vec(vec![
            (
                from_tree.clone(),
                format!("{} (hunk source)", from.conflict_label()),
            ),
            (
                sibling_tree.clone(),
                format!("{} (moved hunk)", from.conflict_label()),
            ),
            (
                base_tree.clone(),
                format!(
                    "{} (parent of hunk source)",
                    from_parents[0].conflict_label()
                ),
            ),
        ]))
        .await?;

        // Apply hunk to destination: merge the base→sibling diff into to_tree
        // (may be recomputed after rebase in the from_is_ancestor case)
        let to_tree = to.tree();
        let mut new_to_tree = MergedTree::merge(Merge::from_vec(vec![
            (
                to_tree,
                format!("{} (hunk destination)", to.conflict_label()),
            ),
            (
                base_tree.clone(),
                format!(
                    "{} (parent of hunk source)",
                    from_parents[0].conflict_label()
                ),
            ),
            (
                sibling_tree.clone(),
                format!("{} (moved hunk)", from.conflict_label()),
            ),
        ]))
        .await?;

        let abandon_source = remainder_tree.tree_ids() == base_tree.tree_ids();
        let description = combine_messages(&[&from], &to, abandon_source);

        // Check ancestry to determine rebase strategy. The hunk must be applied to the destination's
        // tree AFTER any ancestry-related rebasing, so we do it early if moving from an ancestor.
        let from_is_ancestor = tx.repo().index().is_ancestor(from.id(), to.id()).await?;
        let to_is_ancestor = tx.repo().index().is_ancestor(to.id(), from.id()).await?;

        if to_is_ancestor {
            // Child→Parent: apply hunk to ancestor, then handle source
            tx.repo_mut()
                .rewrite_commit(&to)
                .set_tree(new_to_tree)
                .set_description(description)
                .write()
                .await?;

            if abandon_source {
                tx.repo_mut().record_abandoned_commit(&from);
            } else {
                tx.repo_mut()
                    .rewrite_commit(&from)
                    .set_tree(remainder_tree)
                    .write()
                    .await?;
            }

            // Rebase all descendants, which includes rebasing source's descendants onto modified ancestor
            tx.repo_mut().rebase_descendants().await?;
        } else {
            // Parent→Child or Unrelated: modify source first
            if abandon_source {
                tx.repo_mut().record_abandoned_commit(&from);
            } else {
                tx.repo_mut()
                    .rewrite_commit(&from)
                    .set_tree(remainder_tree)
                    .write()
                    .await?;
            }

            if from_is_ancestor {
                // Parent→Child: rebase descendants first, then apply hunk to the rebased destination
                let mut rebase_map = std::collections::HashMap::new();
                tx.repo_mut()
                    .rebase_descendants_with_options(
                        &RevsetExpression::none(),
                        &RebaseOptions::default(),
                        |old_commit, rebased_commit| {
                            rebase_map.insert(
                                old_commit.id().clone(),
                                match rebased_commit {
                                    RebasedCommit::Rewritten(new_commit) => new_commit.id().clone(),
                                    RebasedCommit::Abandoned { parent_id } => parent_id,
                                },
                            );
                        },
                    )
                    .await?;

                // The destination was rebased onto the modified source, so its tree changed.
                // Recompute the hunk application against the rebased tree.
                let rebased_to_id = rebase_map
                    .get(to.id())
                    .ok_or_else(|| anyhow!("descendant to_commit not found in rebase map"))?
                    .clone();
                to = tx.repo().store().get_commit(&rebased_to_id)?;
                new_to_tree = MergedTree::merge(Merge::from_vec(vec![
                    (
                        to.tree(),
                        format!("{} (rebased hunk destination)", to.conflict_label()),
                    ),
                    (
                        base_tree.clone(),
                        format!(
                            "{} (parent of hunk source)",
                            from_parents[0].conflict_label()
                        ),
                    ),
                    (
                        sibling_tree.clone(),
                        format!("{} (moved hunk)", from.conflict_label()),
                    ),
                ]))
                .await?;
            }

            // Apply hunk to destination
            tx.repo_mut()
                .rewrite_commit(&to)
                .set_tree(new_to_tree)
                .set_description(description)
                .write()
                .await?;

            // Rebase all descendants as usual
            tx.repo_mut().rebase_descendants().await?;
        }

        match ws
            .finish_transaction(
                tx,
                format!(
                    "move hunk in {} from {} to {}",
                    self.path.repo_path,
                    from.id().hex(),
                    to.id().hex()
                ),
            )
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

#[async_trait(?Send)]
impl Mutation for CopyHunk {
    async fn execute(
        self: Box<Self>,
        ws: &mut WorkspaceSession,
        options: &MutationOptions,
    ) -> Result<MutationResult> {
        let mut tx = ws.start_transaction().await?;

        let from = ws.resolve_commit_id(&self.from_id)?;
        let to = ws.resolve_change_id(&self.to_id)?;
        let repo_path = RepoPath::from_internal_string(&self.path.repo_path)?;

        if ws.check_immutable(vec![to.id().clone()])? && !options.ignore_immutable {
            precondition!("Revision is immutable");
        }

        let store = tx.repo().store();
        let to_tree = to.tree();

        // vheck for conflicts in destination
        let to_path_value = to_tree.path_value(repo_path).await?;
        if to_path_value.into_resolved().is_err() {
            precondition!("Cannot restore hunk: destination file has conflicts");
        }

        // read destination content
        let to_content = read_file_content(store, &to_tree, repo_path).await?;
        let to_text = String::from_utf8_lossy(&to_content);
        let to_lines: Vec<&str> = to_text.lines().collect();

        // validate destination bounds
        let to_start_0based = self.hunk.location.to_file.start.saturating_sub(1);
        let to_end_0based = to_start_0based + self.hunk.location.to_file.len;
        if to_end_0based > to_lines.len() {
            precondition!(
                "Hunk location out of bounds: file has {} lines, hunk requires lines {}-{}",
                to_lines.len(),
                self.hunk.location.to_file.start,
                to_end_0based
            );
        }

        // validate destination content
        let expected_to_lines: Vec<&str> = self
            .hunk
            .lines
            .lines
            .iter()
            .filter(|line| line.starts_with(' ') || line.starts_with('+'))
            .map(|line| line[1..].trim_end())
            .collect();
        let actual_to_lines: Vec<&str> = to_lines[to_start_0based..to_end_0based]
            .iter()
            .map(|line| line.trim_end())
            .collect();

        if expected_to_lines.len() != actual_to_lines.len() {
            return Err(anyhow!(
                "Hunk validation failed: expected {} lines, found {} lines at destination",
                expected_to_lines.len(),
                actual_to_lines.len()
            ));
        }

        for (i, (expected, actual)) in expected_to_lines
            .iter()
            .zip(actual_to_lines.iter())
            .enumerate()
        {
            if expected != actual {
                return Err(anyhow!(
                    "Hunk validation failed at line {}: expected '{}', found '{}'",
                    to_start_0based + i + 1,
                    expected,
                    actual
                ));
            }
        }

        // read source content
        let from_tree = from.tree();
        let from_content = read_file_content(store, &from_tree, repo_path).await?;
        let from_text = String::from_utf8_lossy(&from_content);
        let from_lines: Vec<&str> = from_text.lines().collect();

        // validate source bounds
        let from_start_0based = self.hunk.location.from_file.start.saturating_sub(1);
        let from_end_0based = from_start_0based + self.hunk.location.from_file.len;
        if from_end_0based > from_lines.len() {
            precondition!(
                "Source hunk location out of bounds: file has {} lines, hunk requires lines {}-{}",
                from_lines.len(),
                self.hunk.location.from_file.start,
                from_end_0based
            );
        }

        // extract source region
        let source_region_lines = &from_lines[from_start_0based..from_end_0based];

        // construct destination content and check whether anything changed
        let mut new_to_lines = Vec::new();
        new_to_lines.extend(to_lines[..to_start_0based].iter().map(|s| s.to_string()));
        new_to_lines.extend(source_region_lines.iter().map(|s| s.to_string()));
        new_to_lines.extend(to_lines[to_end_0based..].iter().map(|s| s.to_string()));

        let ends_with_newline = to_content.ends_with(b"\n");
        let mut new_to_content = Vec::new();
        let num_lines = new_to_lines.len();
        for (i, line) in new_to_lines.iter().enumerate() {
            new_to_content.extend_from_slice(line.as_bytes());
            if i < num_lines - 1 {
                new_to_content.push(b'\n');
            }
        }
        if ends_with_newline && !new_to_content.is_empty() && !new_to_content.ends_with(b"\n") {
            new_to_content.push(b'\n');
        }

        if new_to_content == to_content {
            return Ok(MutationResult::Unchanged);
        }

        // create new destination tree with preserved executable bit
        let new_to_blob_id = store
            .write_file(repo_path, &mut new_to_content.as_slice())
            .await?;

        let to_executable = match to_tree.path_value(repo_path).await?.into_resolved() {
            Ok(Some(TreeValue::File { executable, .. })) => executable,
            _ => false,
        };

        let new_to_tree =
            update_tree_entry(store, &to_tree, repo_path, new_to_blob_id, to_executable).await?;

        // rewrite destination
        tx.repo_mut()
            .rewrite_commit(&to)
            .set_tree(new_to_tree)
            .write()
            .await?;

        tx.repo_mut().rebase_descendants().await?;

        match ws
            .finish_transaction(
                tx,
                format!(
                    "restore hunk in {} from {} into {}",
                    self.path.repo_path, self.from_id.hex, self.to_id.commit.hex
                ),
            )
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

/// Construct a sibling tree's file content by applying a hunk to its base.
///
/// The hunk was computed as a diff between `base` (the source commit's parent) and the
/// source commit. This function applies that diff to reconstruct the file content that
/// would exist in a virtual "sibling" commit containing only this hunk.
///
/// Line numbers must match exactly since the hunk was computed against this base.
#[allow(clippy::manual_strip)]
fn apply_hunk_to_base(base_content: &[u8], hunk: &ChangeHunk) -> Result<Vec<u8>> {
    let base_text = String::from_utf8_lossy(base_content);
    let base_lines: Vec<&str> = base_text.lines().collect();
    let ends_with_newline = base_content.ends_with(b"\n");

    let mut result_lines: Vec<String> = Vec::new();
    let hunk_lines = hunk.lines.lines.iter().peekable();

    // Convert 1-indexed line number to 0-indexed
    let hunk_start = hunk.location.from_file.start.saturating_sub(1);

    // Copy lines before the hunk unchanged
    result_lines.extend(base_lines[..hunk_start].iter().map(|s| s.to_string()));
    let mut base_idx = hunk_start;

    for diff_line in hunk_lines {
        if diff_line.starts_with(' ') || diff_line.starts_with('-') {
            // Context or deletion: verify the base content matches
            let expected = &diff_line[1..];
            if base_idx < base_lines.len() && base_lines[base_idx].trim_end() == expected.trim_end()
            {
                if diff_line.starts_with(' ') {
                    result_lines.push(base_lines[base_idx].to_string());
                }
                // Deletions are consumed but not added to result
                base_idx += 1;
            } else {
                anyhow::bail!(
                    "Hunk mismatch at line {}: expected '{}', found '{}'",
                    base_idx + 1,
                    expected.trim_end(),
                    base_lines.get(base_idx).map_or("<EOF>", |l| l.trim_end())
                );
            }
        } else if diff_line.starts_with('+') {
            // Addition: include in result
            let added = diff_line[1..].trim_end_matches('\n');
            result_lines.push(added.to_string());
        } else {
            anyhow::bail!("Malformed diff line: {}", diff_line);
        }
    }

    // Copy remaining lines after the hunk unchanged
    result_lines.extend(base_lines[base_idx..].iter().map(|s| s.to_string()));

    let mut result_bytes = Vec::new();
    let num_lines = result_lines.len();
    for (i, line) in result_lines.iter().enumerate() {
        result_bytes.extend_from_slice(line.as_bytes());
        if i < num_lines - 1 {
            result_bytes.push(b'\n');
        }
    }

    if ends_with_newline && !result_bytes.is_empty() && !result_bytes.ends_with(b"\n") {
        result_bytes.push(b'\n');
    }

    Ok(result_bytes)
}

fn build_matcher(paths: &[TreePath]) -> Result<Box<dyn Matcher>> {
    if paths.is_empty() {
        Ok(Box::new(EverythingMatcher))
    } else {
        let repo_paths: Vec<_> = paths
            .iter()
            .map(|p| RepoPath::from_internal_string(&p.repo_path))
            .try_collect()?;
        Ok(Box::new(FilesMatcher::new(&repo_paths)))
    }
}

fn combine_messages(sources: &[&Commit], destination: &Commit, abandon_source: bool) -> String {
    if abandon_source {
        // collect non-empty descriptions: destination first, then sources (newest to oldest)
        let descriptions: Vec<_> = std::iter::once(destination.description())
            .chain(sources.iter().map(|c| c.description()))
            .filter(|d| !d.is_empty())
            .collect();
        descriptions.join("\n")
    } else {
        destination.description().to_owned()
    }
}

/// `base` with the changes `paths` and `hunks` pick from `source`: each of
/// `paths` as `source` has it, and `hunks` applied to `base`'s lines. Lines
/// move without the file's mode: a file keeps `base`'s, and one `base` lacks
/// takes `source`'s and ends its last line as `source` does. `labels` name
/// `source` and `base` in conflicts.
async fn tree_with_changes(
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

async fn update_tree_entry(
    _store: &Arc<jj_lib::store::Store>,
    original_tree: &MergedTree,
    path: &RepoPath,
    new_blob: FileId,
    executable: bool,
) -> Result<MergedTree, anyhow::Error> {
    let mut builder = MergedTreeBuilder::new(original_tree.clone());
    builder.set_or_remove(
        path.to_owned(),
        Merge::normal(TreeValue::File {
            id: new_blob,
            executable,
            copy_id: CopyId::placeholder(),
        }),
    );
    let new_tree = builder.write_tree().await?;
    Ok(new_tree)
}

#[async_trait(?Send)]
impl Mutation for IgnoreNewFiles {
    async fn execute(
        self: Box<Self>,
        ws: &mut WorkspaceSession,
        _options: &MutationOptions,
    ) -> Result<MutationResult> {
        if self.paths.is_empty() {
            return Ok(MutationResult::Unchanged);
        }
        // the files are taken in by a snapshot; make sure it has happened
        ws.import_and_snapshot(true, false).await?;
        let wc_commit = ws.get_commit(ws.wc_id())?;
        let parents = wc_commit.parents().await?;
        let parent_tree = rewrite::merge_commit_trees(ws.repo(), &parents).await?;

        let mut paths = Vec::new();
        let mut patterns: Vec<String> = Vec::new();
        for path in &self.paths {
            let path = RepoPath::from_internal_string(&path.repo_path)?.to_owned();
            if !parent_tree.path_value(&path).await?.is_absent() {
                precondition!(
                    "{} is in the parent revision too; only files the working copy picked up can be ignored",
                    path.as_internal_file_string()
                );
            }
            let pattern = exclude_pattern(&path, &parent_tree).await?;
            if !patterns.contains(&pattern) {
                patterns.push(pattern);
            }
            paths.push(path);
        }
        ws.ignore_and_untrack(&paths, &patterns).await?;
        Ok(MutationResult::Updated {
            new_status: ws.format_status(),
            new_selection: None,
        })
    }
}

/// The `info/exclude` line for a picked-up file: its top-most directory that
/// the parent does not have (a whole build output folder is one line), or
/// the file itself, anchored at the root and with gitignore's special
/// characters escaped.
async fn exclude_pattern(path: &RepoPath, parent_tree: &MergedTree) -> Result<String> {
    let mut ancestors: Vec<&RepoPath> = path.ancestors().collect();
    ancestors.reverse(); // root first
    for dir in ancestors.iter().skip(1) {
        if *dir == path {
            break;
        }
        if parent_tree.path_value(dir).await?.is_absent() {
            return Ok(format!(
                "/{}/",
                escape_gitignore(dir.as_internal_file_string())
            ));
        }
    }
    Ok(format!(
        "/{}",
        escape_gitignore(path.as_internal_file_string())
    ))
}

fn escape_gitignore(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for (ix, c) in path.chars().enumerate() {
        if matches!(c, '\\' | '*' | '?' | '[' | ']') || (ix == 0 && matches!(c, '#' | '!')) {
            out.push('\\');
        }
        out.push(c);
    }
    // trailing spaces are dropped by git unless escaped
    if out.ends_with(' ') {
        out.pop();
        out.push_str("\\ ");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        messages::{
            ChangeLocation, ChangeRange, MultilineString, RevId, RevSet,
            mutations::{CreateRevision, DescribeRevision, FileHunks},
            queries::RevsResult,
        },
        worker::{
            WorkerSession, queries,
            tests::{get_by_chid, mkrepo, query_by_chid, query_by_id, revs},
        },
    };
    use anyhow::Result;
    use assert_matches::assert_matches;
    use futures_util::AsyncReadExt;
    use std::fs;

    fn tree_path(path: &str) -> TreePath {
        TreePath {
            repo_path: path.to_owned(),
            relative_path: "".into(),
        }
    }

    async fn changed_paths(ws: &WorkspaceSession<'_>, id: RevId) -> Result<Vec<String>> {
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
        let wc = ws.get_commit(ws.wc_id())?;
        assert_eq!(ws.format_id(&wc).change.hex, wc_id.change.hex);
        assert!(wc.description().starts_with("work"));
        assert_eq!(changed_paths(&ws, ws.format_id(&wc)).await?, vec!["y.txt"]);

        // under it, the new revision with the selected file, where it was
        let first = ws.get_commit(&wc.parent_ids()[0])?;
        assert_eq!(wc.parent_ids().len(), 1);
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
    async fn staged_changes_gather_under_the_working_copy() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // a described base with f.txt, and a working copy that changes it and adds g.txt
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
        let wc_id = |ws: &WorkspaceSession<'_>| -> Result<RevId> {
            Ok(ws.format_id(&ws.get_commit(ws.wc_id())?))
        };
        let store = ws.repo().store().clone();
        let read = |tree: MergedTree, path: &'static str| {
            let store = store.clone();
            async move { read_file_content(&store, &tree, RepoPath::from_internal_string(path)?).await }
        };
        let added_c = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 3, len: 0 },
                to_file: ChangeRange { start: 3, len: 1 },
            },
            lines: MultilineString {
                lines: vec!["+c\n".to_owned()],
            },
        };

        // g.txt: a staging revision is made under the working copy
        let result = StageChanges {
            id: wc_id(&ws)?,
            paths: vec![tree_path("g.txt")],
            hunks: vec![],
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::Updated { .. });
        let wc = ws.get_commit(ws.wc_id())?;
        let staging = ws.get_commit(&wc.parent_ids()[0])?;
        assert!(staging.description().is_empty());
        assert_eq!(staging.parent_ids(), &[base.id().clone()]);
        assert_eq!(
            changed_paths(&ws, ws.format_id(&staging)).await?,
            vec!["g.txt"]
        );
        assert_eq!(changed_paths(&ws, ws.format_id(&wc)).await?, vec!["f.txt"]);

        // `+c` of f.txt: into the same staging revision, no other is made
        StageChanges {
            id: wc_id(&ws)?,
            paths: vec![],
            hunks: vec![FileHunks {
                path: tree_path("f.txt"),
                hunks: vec![added_c.clone()],
            }],
        }
        .execute_unboxed(&mut ws)
        .await?;
        let wc = ws.get_commit(ws.wc_id())?;
        let staging = ws.get_commit(&wc.parent_ids()[0])?;
        assert_eq!(staging.parent_ids(), &[base.id().clone()]);
        assert_eq!(read(staging.tree(), "f.txt").await?, b"a\nb\nc\n");
        assert_eq!(read(wc.tree(), "f.txt").await?, b"a\nb\nc\nd\n");

        // `+c` unstaged, read backwards: the staging revision's line goes
        let removed_c = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 3, len: 1 },
                to_file: ChangeRange { start: 3, len: 0 },
            },
            lines: MultilineString {
                lines: vec!["-c\n".to_owned()],
            },
        };
        UnstageChanges {
            id: wc_id(&ws)?,
            paths: vec![],
            hunks: vec![FileHunks {
                path: tree_path("f.txt"),
                hunks: vec![removed_c],
            }],
        }
        .execute_unboxed(&mut ws)
        .await?;
        let wc = ws.get_commit(ws.wc_id())?;
        let staging = ws.get_commit(&wc.parent_ids()[0])?;
        assert_eq!(read(staging.tree(), "f.txt").await?, b"a\nb\n");
        assert_eq!(
            changed_paths(&ws, ws.format_id(&staging)).await?,
            vec!["g.txt"]
        );

        // g.txt too: nothing is staged, and the staging revision goes
        UnstageChanges {
            id: wc_id(&ws)?,
            paths: vec![tree_path("g.txt")],
            hunks: vec![],
        }
        .execute_unboxed(&mut ws)
        .await?;
        let wc = ws.get_commit(ws.wc_id())?;
        assert_eq!(wc.parent_ids(), &[base.id().clone()]);
        let mut rest = changed_paths(&ws, ws.format_id(&wc)).await?;
        rest.sort();
        assert_eq!(rest, vec!["f.txt", "g.txt"]);
        // the files on disk never changed
        assert_eq!(
            fs::read_to_string(repo.path().join("f.txt"))?,
            "a\nb\nc\nd\n"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_commit_takes_the_staged_changes_or_else_all() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;
        let wc_id = |ws: &WorkspaceSession<'_>| -> Result<RevId> {
            Ok(ws.format_id(&ws.get_commit(ws.wc_id())?))
        };
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
        fs::write(repo.path().join("x.txt"), "x\n").unwrap();
        fs::write(repo.path().join("y.txt"), "y\n").unwrap();

        // x.txt staged and committed: the commit has it, the working copy y.txt
        StageChanges {
            id: wc_id(&ws)?,
            paths: vec![tree_path("x.txt")],
            hunks: vec![],
        }
        .execute_unboxed(&mut ws)
        .await?;
        CommitChanges {
            id: wc_id(&ws)?,
            description: "add x".to_owned(),
        }
        .execute_unboxed(&mut ws)
        .await?;
        let wc = ws.get_commit(ws.wc_id())?;
        let committed = ws.get_commit(&wc.parent_ids()[0])?;
        assert_eq!(committed.description().trim(), "add x");
        assert_eq!(committed.parent_ids(), &[base.id().clone()]);
        assert_eq!(
            changed_paths(&ws, ws.format_id(&committed)).await?,
            vec!["x.txt"]
        );
        assert_eq!(changed_paths(&ws, ws.format_id(&wc)).await?, vec!["y.txt"]);

        // nothing staged: the working copy is the commit, and a new one starts
        CommitChanges {
            id: wc_id(&ws)?,
            description: "add y".to_owned(),
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
        assert!(wc.description().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn committing_everything_drops_an_empty_staging_revision() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;
        DescribeRevision {
            id: revs::working_copy(),
            new_description: "base".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;
        let base = ws.get_commit(ws.wc_id())?;
        // an empty undescribed revision on the base, and the working copy on it
        for _ in 0..2 {
            let on = ws.get_commit(ws.wc_id())?;
            CreateRevision {
                set: RevSet::singleton(ws.format_id(&on)),
            }
            .execute_unboxed(&mut ws)
            .await?;
        }
        fs::write(repo.path().join("x.txt"), "x\n").unwrap();

        CommitChanges {
            id: ws.format_id(&ws.get_commit(ws.wc_id())?),
            description: "add x".to_owned(),
        }
        .execute_unboxed(&mut ws)
        .await?;
        let wc = ws.get_commit(ws.wc_id())?;
        let committed = ws.get_commit(&wc.parent_ids()[0])?;
        assert_eq!(committed.description().trim(), "add x");
        // straight on the base: the empty revision is gone
        assert_eq!(committed.parent_ids(), &[base.id().clone()]);
        assert_eq!(
            changed_paths(&ws, ws.format_id(&committed)).await?,
            vec!["x.txt"]
        );
        Ok(())
    }

    #[tokio::test]
    async fn nothing_is_unstaged_without_a_staging_revision() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;
        fs::write(repo.path().join("x.txt"), "x").unwrap();
        DescribeRevision {
            id: revs::working_copy(),
            new_description: "described".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;
        let described = ws.get_commit(ws.wc_id())?;
        CreateRevision {
            set: RevSet::singleton(ws.format_id(&described)),
        }
        .execute_unboxed(&mut ws)
        .await?;
        // the parent is described: it is a commit, not a staging revision
        let result = UnstageChanges {
            id: ws.format_id(&ws.get_commit(ws.wc_id())?),
            paths: vec![tree_path("x.txt")],
            hunks: vec![],
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::PreconditionError { .. });
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

    #[tokio::test]
    async fn move_changes_all_paths() -> Result<()> {
        let repo = mkrepo();

        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        let parent_header =
            queries::query_revision(&ws, &revs::conflict_bookmark())?.expect("exists");
        assert!(parent_header.has_conflict);

        let result = MoveChanges {
            from: RevSet::singleton(revs::resolve_conflict()),
            to_id: revs::conflict_bookmark().commit,
            paths: vec![],
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::Updated { .. });

        let parent_header =
            queries::query_revision(&ws, &revs::conflict_bookmark())?.expect("exists");
        assert!(!parent_header.has_conflict);

        Ok(())
    }

    #[tokio::test]
    async fn move_changes_single_path() -> Result<()> {
        let repo = mkrepo();

        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        let from_rev = query_by_id(&ws, revs::main_bookmark()).await?;
        let to_rev = query_by_id(&ws, revs::working_copy()).await?;
        assert_matches!(from_rev, RevsResult::Detail { changes, .. } if changes.len() == 2);
        assert_matches!(to_rev, RevsResult::Detail { changes, .. } if changes.is_empty());

        let result = MoveChanges {
            from: RevSet::singleton(revs::main_bookmark()),
            to_id: revs::working_copy().commit,
            paths: vec![TreePath {
                repo_path: "c.txt".to_owned(),
                relative_path: "".into(),
            }],
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::Updated { .. });

        let from_rev = query_by_id(&ws, revs::main_bookmark()).await?;
        let to_rev = query_by_id(&ws, revs::working_copy()).await?;
        assert_matches!(from_rev, RevsResult::Detail { changes, .. } if changes.len() == 1);
        assert_matches!(to_rev, RevsResult::Detail { changes, .. } if changes.len() == 1);

        Ok(())
    }

    #[tokio::test]
    async fn move_changes_range_partial() -> Result<()> {
        let repo = mkrepo();

        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // commit A: adds x.txt and y.txt
        fs::write(repo.path().join("x.txt"), "x content").unwrap();
        fs::write(repo.path().join("y.txt"), "y content").unwrap();
        DescribeRevision {
            id: revs::working_copy(),
            new_description: "commit A".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;

        // new WC on top of A
        let a = ws.get_commit(ws.wc_id())?;
        let a_id = ws.format_id(&a);

        let result = CreateRevision {
            set: RevSet::singleton(a_id.clone()),
        }
        .execute_unboxed(&mut ws)
        .await?;
        let b_id = match result {
            MutationResult::Updated {
                new_selection: Some(sel),
                ..
            } => sel.id,
            _ => panic!("expected new revision"),
        };

        // becomes commit B: modifies y.txt and adds z.txt
        fs::write(repo.path().join("y.txt"), "y modified").unwrap();
        fs::write(repo.path().join("z.txt"), "z content").unwrap();
        DescribeRevision {
            id: b_id.clone(),
            new_description: "commit B".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;

        let b = get_by_chid(&ws, &b_id)?;
        let b_id = ws.format_id(&b);

        // commit C: sibling of A, move destination
        let c_base = revs::main_bookmark();
        let result = CreateRevision {
            set: RevSet::singleton(c_base.clone()),
        }
        .execute_unboxed(&mut ws)
        .await?;
        let c_id = match result {
            MutationResult::Updated {
                new_selection: Some(sel),
                ..
            } => sel.id,
            _ => panic!("expected new revision"),
        };

        // move z.txt from A::B to C
        let result = MoveChanges {
            from: RevSet::sequence(a_id.clone(), b_id.clone()),
            to_id: c_id.commit.clone(),
            paths: vec![TreePath {
                repo_path: "z.txt".to_owned(),
                relative_path: "".into(),
            }],
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // A should be unchanged (2 changes: x.txt, y.txt)
        let a_rev = query_by_chid(&ws, &a_id.change.hex).await?;
        assert_matches!(a_rev, RevsResult::Detail { changes, .. } if changes.len() == 2);

        // B should have 1 change (y.txt only, z.txt was moved)
        let b_rev = query_by_chid(&ws, &b_id.change.hex).await?;
        assert_matches!(b_rev, RevsResult::Detail { changes, .. } if changes.len() == 1);

        // C should have 1 change (z.txt)
        let c_rev = query_by_chid(&ws, &c_id.change.hex).await?;
        assert_matches!(c_rev, RevsResult::Detail { changes, .. } if changes.len() == 1);

        Ok(())
    }

    #[tokio::test]
    async fn move_changes_range_partial_multi_touch() -> Result<()> {
        let repo = mkrepo();

        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // commit A: add z.txt
        fs::write(repo.path().join("z.txt"), "version 1").unwrap();
        DescribeRevision {
            id: revs::working_copy(),
            new_description: "commit A: create z.txt".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;

        let a = ws.get_commit(ws.wc_id())?;
        let a_id = ws.format_id(&a);

        // new WC on top of A
        let result = CreateRevision {
            set: RevSet::singleton(a_id.clone()),
        }
        .execute_unboxed(&mut ws)
        .await?;
        let b_id = match result {
            MutationResult::Updated {
                new_selection: Some(sel),
                ..
            } => sel.id,
            _ => panic!("expected new revision"),
        };

        // becomes commit B: modify z.txt and add y.txt
        fs::write(repo.path().join("z.txt"), "version 2").unwrap();
        fs::write(repo.path().join("y.txt"), "y content").unwrap();
        DescribeRevision {
            id: b_id.clone(),
            new_description: "commit B: modify z.txt, add y.txt".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;

        let b = get_by_chid(&ws, &b_id)?;
        let b_id = ws.format_id(&b);

        // A should have 1 change (z.txt)
        let a_rev = query_by_id(&ws, a_id.clone()).await?;
        assert_matches!(a_rev, RevsResult::Detail { changes, .. } if changes.len() == 1);

        // B should have 2 changes (z.txt modified, y.txt added)
        let b_rev = query_by_id(&ws, b_id.clone()).await?;
        assert_matches!(b_rev, RevsResult::Detail { changes, .. } if changes.len() == 2);

        // commit C: sibling of A, destination of move
        let c_base = revs::main_bookmark();
        let result = CreateRevision {
            set: RevSet::singleton(c_base.clone()),
        }
        .execute_unboxed(&mut ws)
        .await?;
        let c_id = match result {
            MutationResult::Updated {
                new_selection: Some(sel),
                ..
            } => sel.id,
            _ => panic!("expected new revision"),
        };

        // move z.txt from A::B to C
        let result = MoveChanges {
            from: RevSet::sequence(a_id.clone(), b_id.clone()),
            to_id: c_id.commit.clone(),
            paths: vec![TreePath {
                repo_path: "z.txt".to_owned(),
                relative_path: "".into(),
            }],
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // A should be abandoned (only touched z.txt)
        let a_exists = ws.evaluate_revset_str(&a_id.change.hex);
        assert!(
            a_exists.is_err() || a_exists.unwrap().is_empty()?,
            "commit A should be abandoned"
        );

        // B should have 1 change (y.txt only, z.txt was moved)
        let b_rev = query_by_chid(&ws, &b_id.change.hex).await?;
        assert_matches!(b_rev, RevsResult::Detail { changes, .. } if changes.len() == 1);

        // C should have 1 change (z.txt with accumulated changes)
        let c_rev = query_by_chid(&ws, &c_id.change.hex).await?;
        assert_matches!(c_rev, RevsResult::Detail { changes, .. } if changes.len() == 1);

        let c = get_by_chid(&ws, &c_id)?;
        let tree = c.tree();
        let path = jj_lib::repo_path::RepoPath::from_internal_string("z.txt")?;
        let value = tree.path_value(path).await?;
        assert!(value.is_resolved());

        Ok(())
    }

    #[tokio::test]
    async fn move_changes_descendant_target() -> Result<()> {
        let repo = mkrepo();

        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        let base = ws.get_commit(ws.wc_id())?;
        let base_id = ws.format_id(&base);

        let result = CreateRevision {
            set: RevSet::singleton(base_id.clone()),
        }
        .execute_unboxed(&mut ws)
        .await?;
        let source_id = match result {
            MutationResult::Updated {
                new_selection: Some(sel),
                ..
            } => sel.id,
            _ => panic!("expected new revision"),
        };
        fs::write(repo.path().join("move_changes_source.txt"), "source").unwrap();
        DescribeRevision {
            id: source_id.clone(),
            new_description: "source".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;
        let source = get_by_chid(&ws, &source_id)?;
        let source_id = ws.format_id(&source);

        let result = CreateRevision {
            set: RevSet::singleton(source_id.clone()),
        }
        .execute_unboxed(&mut ws)
        .await?;
        let middle_id = match result {
            MutationResult::Updated {
                new_selection: Some(sel),
                ..
            } => sel.id,
            _ => panic!("expected new revision"),
        };
        fs::write(repo.path().join("move_changes_middle.txt"), "middle").unwrap();
        DescribeRevision {
            id: middle_id.clone(),
            new_description: "middle".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;
        let middle = get_by_chid(&ws, &middle_id)?;
        let middle_id = ws.format_id(&middle);

        let result = CreateRevision {
            set: RevSet::singleton(middle_id.clone()),
        }
        .execute_unboxed(&mut ws)
        .await?;
        let target_id = match result {
            MutationResult::Updated {
                new_selection: Some(sel),
                ..
            } => sel.id,
            _ => panic!("expected new revision"),
        };
        fs::write(repo.path().join("move_changes_target.txt"), "target").unwrap();
        DescribeRevision {
            id: target_id.clone(),
            new_description: "target".to_owned(),
            reset_author: false,
        }
        .execute_unboxed(&mut ws)
        .await?;
        let target = get_by_chid(&ws, &target_id)?;
        let target_id = ws.format_id(&target);

        let result = MoveChanges {
            from: RevSet::singleton(source_id.clone()),
            to_id: target_id.commit.clone(),
            paths: vec![TreePath {
                repo_path: "move_changes_source.txt".to_owned(),
                relative_path: "".into(),
            }],
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::Updated { .. });

        let source_header = queries::query_revision(&ws, &source_id)?;
        assert!(source_header.is_none(), "source should be abandoned");

        let middle_after = get_by_chid(&ws, &middle_id)?;
        let target_after = get_by_chid(&ws, &target_id)?;

        assert_eq!(middle_after.parent_ids()[0], base.id().clone());
        assert_eq!(target_after.parent_ids()[0], middle_after.id().clone());

        for (label, commit) in [("middle", &middle_after), ("target", &target_after)] {
            let targets = ws
                .repo()
                .resolve_change_id(commit.change_id())
                .await?
                .expect("commit should resolve");
            assert!(
                !targets.is_divergent(),
                "{label} should not be divergent after move"
            );
        }

        let middle_rev = query_by_chid(&ws, &middle_id.change.hex).await?;
        assert_matches!(&middle_rev, RevsResult::Detail { changes, .. } if changes.iter().any(|c| c.path.repo_path == "move_changes_middle.txt")
            && !changes.iter().any(|c| c.path.repo_path == "move_changes_source.txt"));

        let target_rev = query_by_chid(&ws, &target_id.change.hex).await?;
        assert_matches!(&target_rev, RevsResult::Detail { changes, .. } if changes.iter().any(|c| c.path.repo_path == "move_changes_target.txt")
            && changes.iter().any(|c| c.path.repo_path == "move_changes_source.txt"));

        Ok(())
    }

    #[tokio::test]
    async fn copy_changes() -> Result<()> {
        let repo = mkrepo();

        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        let from_rev = query_by_id(&ws, revs::resolve_conflict()).await?;
        let to_rev = query_by_id(&ws, revs::working_copy()).await?;
        assert_matches!(from_rev, RevsResult::Detail { changes, .. } if changes.len() == 1);
        assert_matches!(to_rev, RevsResult::Detail { changes, .. } if changes.is_empty());

        let result = CopyChanges {
            from_id: revs::resolve_conflict().commit,
            to_set: RevSet::singleton(revs::working_copy()),
            paths: vec![TreePath {
                repo_path: "b.txt".to_owned(),
                relative_path: "".into(),
            }],
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::Updated { .. });

        let from_rev = query_by_id(&ws, revs::resolve_conflict()).await?;
        let to_rev = query_by_id(&ws, revs::working_copy()).await?;
        assert_matches!(from_rev, RevsResult::Detail { changes, .. } if changes.len() == 1);
        assert_matches!(to_rev, RevsResult::Detail { changes, .. } if changes.len() == 1);

        Ok(())
    }

    /// Test restoring changes into a range of revisions.
    /// This verifies that all commits in the range have the specified paths restored.
    #[tokio::test]
    async fn copy_changes_range() -> Result<()> {
        let repo = mkrepo();

        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // hunk_child_single modifies line 2 of hunk_test.txt
        // hunk_grandchild (child of hunk_child_single) modifies line 3 of hunk_test.txt
        let child_rev = query_by_id(&ws, revs::hunk_child_single()).await?;
        let grandchild_rev = query_by_id(&ws, revs::hunk_grandchild()).await?;
        assert_matches!(&child_rev, RevsResult::Detail { changes, .. } if changes.iter().any(|c| c.path.repo_path == "hunk_test.txt"));
        assert_matches!(&grandchild_rev, RevsResult::Detail { changes, .. } if changes.iter().any(|c| c.path.repo_path == "hunk_test.txt"));

        // restore hunk_test.txt from hunk_base (parent of hunk_child_single) into the range
        let result = CopyChanges {
            from_id: revs::hunk_base().commit,
            to_set: RevSet::sequence(revs::hunk_child_single(), revs::hunk_grandchild()),
            paths: vec![TreePath {
                repo_path: "hunk_test.txt".to_owned(),
                relative_path: "".into(),
            }],
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // after restoring, neither commit should have changes to hunk_test.txt
        // query by change id to get the rewritten commits
        let new_child_rev = query_by_id(&ws, revs::hunk_child_single()).await?;
        let new_grandchild_rev = query_by_id(&ws, revs::hunk_grandchild()).await?;
        assert_matches!(&new_child_rev, RevsResult::Detail { changes, .. } if !changes.iter().any(|c| c.path.repo_path == "hunk_test.txt"));
        assert_matches!(&new_grandchild_rev, RevsResult::Detail { changes, .. } if !changes.iter().any(|c| c.path.repo_path == "hunk_test.txt"));

        Ok(())
    }

    /// Test that restoring into a range containing immutable commits fails.
    #[tokio::test]
    async fn copy_changes_range_immutable() -> Result<()> {
        let repo = mkrepo();

        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // try to restore into a range of immutable commits
        let result = CopyChanges {
            from_id: revs::immutable_grandparent().commit,
            to_set: RevSet::sequence(revs::immutable_parent(), revs::immutable_bookmark()),
            paths: vec![],
        }
        .execute_unboxed(&mut ws)
        .await?;

        assert_matches!(result, MutationResult::PreconditionError { .. });

        Ok(())
    }

    #[tokio::test]
    async fn move_hunk_descendant_partial() -> anyhow::Result<()> {
        use jj_lib::repo::Repo;

        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // Move one of two hunks from descendant (hunk_child_multi) to ancestor (hunk_base)
        // hunk_child_multi modifies lines 2 and 4: line2 -> changed2, line4 -> changed4
        // Move only the line 2 change to hunk_base, keeping line 4 change in source
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 1, len: 3 },
                to_file: ChangeRange { start: 1, len: 3 },
            },
            lines: MultilineString {
                lines: vec![
                    " line1".to_owned(),
                    "-line2".to_owned(),
                    "+changed2".to_owned(),
                    " line3".to_owned(),
                ],
            },
        };

        let mutation = MoveHunk {
            from_id: revs::hunk_child_multi(),
            to_id: revs::hunk_base().commit,
            path: TreePath {
                repo_path: "hunk_test.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // Verify source still has the line 4 change but not line 2
        let source_commit = get_by_chid(&ws, &revs::hunk_child_multi())?;
        let source_tree = source_commit.tree();
        let repo_path = jj_lib::repo_path::RepoPath::from_internal_string("hunk_test.txt")?;

        match source_tree.path_value(repo_path).await?.into_resolved() {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(repo_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                let content_str = String::from_utf8_lossy(&content);
                // Source should have changed2 (inherited from rebased parent) and changed4 (its own change)
                assert_eq!(
                    content_str, "line1\nchanged2\nline3\nchanged4\nline5\n",
                    "Source should have both changes after rebase (parent now has changed2)"
                );
            }
            _ => panic!("Expected hunk_test.txt to be a file in source commit"),
        }

        // Verify target (hunk_base) has the line 2 change applied
        let target_commit = get_by_chid(&ws, &revs::hunk_base())?;
        let target_tree = target_commit.tree();

        match target_tree.path_value(repo_path).await?.into_resolved() {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(repo_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                let content_str = String::from_utf8_lossy(&content);
                assert_eq!(
                    content_str, "line1\nchanged2\nline3\nline4\nline5\n",
                    "Target should have line 2 changed but not line 4"
                );
            }
            _ => panic!("Expected hunk_test.txt to be a file in target commit"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn move_hunk_message() -> anyhow::Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // Move only hunk from source, abandoning it
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 2, len: 1 },
                to_file: ChangeRange { start: 2, len: 1 },
            },
            lines: MultilineString {
                lines: vec!["-line2".to_owned(), "+modified2".to_owned()],
            },
        };

        let mutation = MoveHunk {
            from_id: revs::hunk_child_single(),
            to_id: revs::hunk_sibling().commit,
            path: TreePath {
                repo_path: "hunk_test.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // Source should be abandoned (not found in repo)
        let source_header = queries::query_revision(&ws, &revs::hunk_child_single())?;
        assert!(source_header.is_none(), "Source should be abandoned");

        // Target should have combined description
        let target_header =
            queries::query_revision(&ws, &revs::hunk_sibling())?.expect("target exists");
        let desc = target_header.description.lines.join("\n");
        assert!(
            desc.contains("hunk sibling") && desc.contains("hunk child single"),
            "Target description should combine both: got '{}'",
            desc
        );

        Ok(())
    }

    #[tokio::test]
    async fn move_hunk_invalid() -> anyhow::Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // Invalid hunk - doesn't match the actual content of b.txt in hunk_source
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 1, len: 1 },
                to_file: ChangeRange { start: 1, len: 1 },
            },
            lines: MultilineString {
                lines: vec!["-nonexistent".to_owned(), "+something".to_owned()],
            },
        };

        let mutation = MoveHunk {
            from_id: revs::hunk_source(),
            to_id: revs::working_copy().commit,
            path: TreePath {
                repo_path: "b.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await;
        assert!(result.is_err(), "Should fail with invalid hunk");

        Ok(())
    }

    #[tokio::test]
    async fn move_hunk_descendant_abandons_source() -> anyhow::Result<()> {
        use jj_lib::repo::Repo;

        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // hunk_child_single's only change is line 2: "line2" -> "modified2"
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 1, len: 3 },
                to_file: ChangeRange { start: 1, len: 3 },
            },
            lines: MultilineString {
                lines: vec![
                    " line1".to_owned(),
                    "-line2".to_owned(),
                    "+modified2".to_owned(),
                    " line3".to_owned(),
                ],
            },
        };

        // Move the only hunk from child to parent - child should be abandoned
        let mutation = MoveHunk {
            from_id: revs::hunk_child_single(),
            to_id: revs::hunk_base().commit,
            path: TreePath {
                repo_path: "hunk_test.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // Source should be abandoned (not found in repo)
        let source_header = queries::query_revision(&ws, &revs::hunk_child_single())?;
        assert!(source_header.is_none(), "Source should be abandoned");

        // Target (hunk_base) should have the change
        let target_commit = get_by_chid(&ws, &revs::hunk_base())?;
        let target_tree = target_commit.tree();
        let repo_path = jj_lib::repo_path::RepoPath::from_internal_string("hunk_test.txt")?;

        match target_tree.path_value(repo_path).await?.into_resolved() {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(repo_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                let content_str = String::from_utf8_lossy(&content);
                assert_eq!(
                    content_str, "line1\nmodified2\nline3\nline4\nline5\n",
                    "Target should have the hunk applied"
                );
            }
            _ => panic!("Expected hunk_test.txt to be a file in target commit"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn move_hunk_unrelated() -> anyhow::Result<()> {
        use jj_lib::repo::Repo;

        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // hunk_child_single modifies line 2: "line2" -> "modified2"
        // hunk_sibling extends the file with new6, new7, new8
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 1, len: 3 },
                to_file: ChangeRange { start: 1, len: 3 },
            },
            lines: MultilineString {
                lines: vec![
                    " line1".to_owned(),
                    "-line2".to_owned(),
                    "+modified2".to_owned(),
                    " line3".to_owned(),
                ],
            },
        };

        // Move a hunk from hunk_child_single to hunk_sibling (unrelated commits, both children of hunk_base)
        let mutation = MoveHunk {
            from_id: revs::hunk_child_single(),
            to_id: revs::hunk_sibling().commit,
            path: TreePath {
                repo_path: "hunk_test.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // Verify source has the hunk removed (becomes empty and should be abandoned or have no changes)
        let from_header = queries::query_revision(&ws, &revs::hunk_child_single())?;
        if from_header.is_some() {
            let from_rev = query_by_id(&ws, revs::hunk_child_single()).await?;
            assert_matches!(from_rev, RevsResult::Detail { changes, .. } if changes.is_empty(),
            "Expected source commit to have no changes after hunk move");
        }

        // Verify target has the hunk applied (with the new lines still there)
        let sibling_commit = get_by_chid(&ws, &revs::hunk_sibling())?;
        let sibling_tree = sibling_commit.tree();
        let repo_path = jj_lib::repo_path::RepoPath::from_internal_string("hunk_test.txt")?;

        match sibling_tree.path_value(repo_path).await?.into_resolved() {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(repo_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                let content_str = String::from_utf8_lossy(&content);
                assert_eq!(
                    content_str, "line1\nmodified2\nline3\nline4\nline5\nnew6\nnew7\nnew8\n",
                    "Sibling should have modified2 plus the new lines"
                );
            }
            _ => panic!("Expected hunk_test.txt to be a file in sibling commit"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn move_hunk_unrelated_different_structure_creates_conflict() -> anyhow::Result<()> {
        // This test documents that moving a hunk between unrelated commits with different
        // file structures will create a conflict. This is the correct semantic behavior
        // of 3-way merge: the hunk was computed against a different file than the target.
        //
        // Example: hunk_source's parent has b.txt="1\n", but working_copy has b.txt="1\n2\n"
        // Moving the "1->11" hunk creates a conflict because the target file has content
        // (line 2) that wasn't present when the hunk was computed.

        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // Hunk from hunk_source that changes line 1: "1" -> "11"
        // hunk_source's parent has b.txt = "1\n" (single line)
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 1, len: 1 },
                to_file: ChangeRange { start: 1, len: 1 },
            },
            lines: MultilineString {
                lines: vec!["-1".to_owned(), "+11".to_owned()],
            },
        };

        // Move to working_copy which has b.txt = "1\n2\n" (different structure)
        let mutation = MoveHunk {
            from_id: revs::hunk_source(),
            to_id: revs::working_copy().commit,
            path: TreePath {
                repo_path: "b.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // The target should have a conflict because the file structures differ
        let to_header =
            queries::query_revision(&ws, &revs::working_copy())?.expect("working copy exists");
        assert!(
            to_header.has_conflict,
            "Expected conflict when moving hunk to file with different structure"
        );

        Ok(())
    }

    #[tokio::test]
    async fn move_hunk_ancestor_to_descendant() -> anyhow::Result<()> {
        // Test moving a hunk FROM an ancestor TO a descendant (the from_is_ancestor code path).
        //
        // Hierarchy:
        //   hunk_base: line1, line2, line3, line4, line5
        //   └─ hunk_child_single: line1, modified2, line3, line4, line5  (changes line2)
        //      └─ hunk_grandchild: line1, modified2, grandchild3, line4, line5  (changes line3)
        //
        // We move the "line3 -> grandchild3" hunk FROM hunk_grandchild TO hunk_child_single.
        // Wait, that's descendant-to-ancestor which is already tested.
        //
        // For a true ancestor-to-descendant test without causing source abandonment,
        // we use hunk_child_multi (which has 2 hunks) and move one hunk to hunk_sibling.
        // But they're siblings, not ancestor-descendant.
        //
        // Actually, let's test moving hunk_child_single's change to hunk_grandchild.
        // When the source's only change is moved, the source is abandoned and the
        // grandchild is rebased. This verifies the rebase-and-apply logic.
        use jj_lib::repo::Repo;

        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        let repo_path = jj_lib::repo_path::RepoPath::from_internal_string("hunk_test.txt")?;

        // Verify initial state
        let child_before = get_by_chid(&ws, &revs::hunk_child_single())?;
        let child_tree_before = child_before.tree();
        match child_tree_before
            .path_value(repo_path)
            .await?
            .into_resolved()
        {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(repo_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                assert_eq!(
                    String::from_utf8_lossy(&content),
                    "line1\nmodified2\nline3\nline4\nline5\n",
                    "hunk_child_single initial state"
                );
            }
            _ => panic!("Expected hunk_test.txt in hunk_child_single"),
        }

        let grandchild_before = get_by_chid(&ws, &revs::hunk_grandchild())?;
        let grandchild_tree_before = grandchild_before.tree();
        match grandchild_tree_before
            .path_value(repo_path)
            .await?
            .into_resolved()
        {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(repo_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                assert_eq!(
                    String::from_utf8_lossy(&content),
                    "line1\nmodified2\ngrandchild3\nline4\nline5\n",
                    "hunk_grandchild initial state"
                );
            }
            _ => panic!("Expected hunk_test.txt in hunk_grandchild"),
        }

        // The hunk in hunk_child_single's context: parent (hunk_base) has line2, child has modified2
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 1, len: 3 },
                to_file: ChangeRange { start: 1, len: 3 },
            },
            lines: MultilineString {
                lines: vec![
                    " line1".to_owned(),
                    "-line2".to_owned(),
                    "+modified2".to_owned(),
                    " line3".to_owned(),
                ],
            },
        };

        // Move FROM ancestor (hunk_child_single) TO descendant (hunk_grandchild)
        let mutation = MoveHunk {
            from_id: revs::hunk_child_single(),
            to_id: revs::hunk_grandchild().commit,
            path: TreePath {
                repo_path: "hunk_test.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // Verify source (hunk_child_single) was ABANDONED because its only change was moved
        let source_header = queries::query_revision(&ws, &revs::hunk_child_single())?;
        assert!(
            source_header.is_none(),
            "Source should be abandoned (its only change was moved)"
        );

        // Verify destination (hunk_grandchild) - should have the hunk applied correctly
        let dest_after = get_by_chid(&ws, &revs::hunk_grandchild())?;
        let dest_tree = dest_after.tree();

        let dest_targets = ws
            .repo()
            .resolve_change_id(dest_after.change_id())
            .await?
            .expect("destination should resolve");
        assert!(
            !dest_targets.is_divergent(),
            "destination should not be divergent after move"
        );

        let path_value = dest_tree.path_value(repo_path).await?;
        match path_value.into_resolved() {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(repo_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                let content_str = String::from_utf8_lossy(&content);
                assert_eq!(
                    content_str, "line1\nmodified2\ngrandchild3\nline4\nline5\n",
                    "Destination should have modified2 (moved) and grandchild3 (own)"
                );
            }
            Ok(None) => panic!("hunk_test.txt does not exist in destination"),
            Ok(other) => panic!("hunk_test.txt has unexpected type: {:?}", other),
            Err(_conflict) => {
                panic!("Destination should not have a conflict after move");
            }
        }

        // Verify grandchild is now a direct child of hunk_base (parent was abandoned)
        let grandchild_parents = dest_after.parents().await?;
        assert_eq!(
            grandchild_parents.len(),
            1,
            "Grandchild should have one parent"
        );
        let parent = &grandchild_parents[0];
        let base = get_by_chid(&ws, &revs::hunk_base())?;
        assert_eq!(
            parent.id(),
            base.id(),
            "Grandchild's parent should now be hunk_base (skipping abandoned hunk_child_single)"
        );

        Ok(())
    }

    #[tokio::test]
    async fn move_hunk_between_siblings() -> anyhow::Result<()> {
        // Test moving hunks between sibling commits (both children of same parent).
        // This exercises the general code path where neither commit is ancestor of the other.
        use jj_lib::repo::Repo;

        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // hunk_child_multi has: line1, changed2, line3, changed4, line5
        // hunk_sibling has: line1, line2, line3, line4, line5, new6, new7, new8
        // Both are children of hunk_base (line1, line2, line3, line4, line5)
        //
        // Move the line2->changed2 hunk from hunk_child_multi to hunk_sibling
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 1, len: 3 },
                to_file: ChangeRange { start: 1, len: 3 },
            },
            lines: MultilineString {
                lines: vec![
                    " line1".to_owned(),
                    "-line2".to_owned(),
                    "+changed2".to_owned(),
                    " line3".to_owned(),
                ],
            },
        };

        let mutation = MoveHunk {
            from_id: revs::hunk_child_multi(),
            to_id: revs::hunk_sibling().commit,
            path: TreePath {
                repo_path: "hunk_test.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // Verify source has the hunk removed (only changed4 remains)
        let source_commit = get_by_chid(&ws, &revs::hunk_child_multi())?;
        let source_tree = source_commit.tree();
        let repo_path = jj_lib::repo_path::RepoPath::from_internal_string("hunk_test.txt")?;

        match source_tree.path_value(repo_path).await?.into_resolved() {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(repo_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                let content_str = String::from_utf8_lossy(&content);
                assert_eq!(
                    content_str, "line1\nline2\nline3\nchanged4\nline5\n",
                    "Source should have only changed4 (changed2 was moved)"
                );
            }
            _ => panic!("Expected hunk_test.txt in source"),
        }

        // Verify target has both the new lines AND the moved hunk
        let target_commit = get_by_chid(&ws, &revs::hunk_sibling())?;
        let target_tree = target_commit.tree();

        match target_tree.path_value(repo_path).await?.into_resolved() {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(repo_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                let content_str = String::from_utf8_lossy(&content);
                assert_eq!(
                    content_str, "line1\nchanged2\nline3\nline4\nline5\nnew6\nnew7\nnew8\n",
                    "Target should have changed2 (moved) plus its own new lines"
                );
            }
            _ => panic!("Expected hunk_test.txt in target"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn move_hunk_does_not_affect_other_files() -> anyhow::Result<()> {
        // Verify that moving a hunk in one file doesn't affect other files in the same commits.
        use jj_lib::repo::Repo;

        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // Get the original content of a.txt in hunk_child_multi before the move
        let child_before = get_by_chid(&ws, &revs::hunk_child_multi())?;
        let child_tree_before = child_before.tree();
        let a_txt_path = jj_lib::repo_path::RepoPath::from_internal_string("a.txt")?;

        let a_txt_content_before = match child_tree_before
            .path_value(a_txt_path)
            .await?
            .into_resolved()
        {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(a_txt_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                String::from_utf8_lossy(&content).to_string()
            }
            Ok(None) => String::new(), // File doesn't exist
            _ => panic!("Unexpected state for a.txt"),
        };

        // Also check hunk_base's a.txt content
        let parent_before = get_by_chid(&ws, &revs::hunk_base())?;
        let parent_tree_before = parent_before.tree();

        let parent_a_txt_before = match parent_tree_before
            .path_value(a_txt_path)
            .await?
            .into_resolved()
        {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(a_txt_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                String::from_utf8_lossy(&content).to_string()
            }
            Ok(None) => String::new(),
            _ => panic!("Unexpected state for a.txt in parent"),
        };

        // Move a hunk in hunk_test.txt from child to parent
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 1, len: 3 },
                to_file: ChangeRange { start: 1, len: 3 },
            },
            lines: MultilineString {
                lines: vec![
                    " line1".to_owned(),
                    "-line2".to_owned(),
                    "+changed2".to_owned(),
                    " line3".to_owned(),
                ],
            },
        };

        let mutation = MoveHunk {
            from_id: revs::hunk_child_multi(),
            to_id: revs::hunk_base().commit,
            path: TreePath {
                repo_path: "hunk_test.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // Verify a.txt in child is unchanged
        let child_after = get_by_chid(&ws, &revs::hunk_child_multi())?;
        let child_tree_after = child_after.tree();

        let a_txt_content_after = match child_tree_after
            .path_value(a_txt_path)
            .await?
            .into_resolved()
        {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(a_txt_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                String::from_utf8_lossy(&content).to_string()
            }
            Ok(None) => String::new(),
            _ => panic!("Unexpected state for a.txt after move"),
        };

        assert_eq!(
            a_txt_content_before, a_txt_content_after,
            "a.txt in child should be unchanged after moving hunk in hunk_test.txt"
        );

        // Verify a.txt in parent is unchanged
        let parent_after = get_by_chid(&ws, &revs::hunk_base())?;
        let parent_tree_after = parent_after.tree();

        let parent_a_txt_after = match parent_tree_after
            .path_value(a_txt_path)
            .await?
            .into_resolved()
        {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(a_txt_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                String::from_utf8_lossy(&content).to_string()
            }
            Ok(None) => String::new(),
            _ => panic!("Unexpected state for a.txt in parent after move"),
        };

        assert_eq!(
            parent_a_txt_before, parent_a_txt_after,
            "a.txt in parent should be unchanged after moving hunk in hunk_test.txt"
        );

        Ok(())
    }

    #[tokio::test]
    async fn move_hunk_second_of_two_hunks() -> anyhow::Result<()> {
        use jj_lib::repo::Repo;

        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // hunk_child_multi has two hunks: line2->changed2 and line4->changed4
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 3, len: 3 },
                to_file: ChangeRange { start: 3, len: 3 },
            },
            lines: MultilineString {
                lines: vec![
                    " line3".to_owned(),
                    "-line4".to_owned(),
                    "+changed4".to_owned(),
                    " line5".to_owned(),
                ],
            },
        };

        // Move only the second hunk (line4->changed4) to hunk_sibling
        let mutation = MoveHunk {
            from_id: revs::hunk_child_multi(),
            to_id: revs::hunk_sibling().commit,
            path: TreePath {
                repo_path: "hunk_test.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // Verify source still has the first hunk (changed2), but not the second
        let source_commit = get_by_chid(&ws, &revs::hunk_child_multi())?;
        let source_tree = source_commit.tree();
        let repo_path = jj_lib::repo_path::RepoPath::from_internal_string("hunk_test.txt")?;

        match source_tree.path_value(repo_path).await?.into_resolved() {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(repo_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                let content_str = String::from_utf8_lossy(&content);
                assert_eq!(
                    content_str, "line1\nchanged2\nline3\nline4\nline5\n",
                    "Source should have first hunk (changed2) but not second"
                );
            }
            _ => panic!("Expected hunk_test.txt to be a file in source commit"),
        }

        // Verify target has the second hunk added
        let target_commit = get_by_chid(&ws, &revs::hunk_sibling())?;
        let target_tree = target_commit.tree();

        match target_tree.path_value(repo_path).await?.into_resolved() {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(repo_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                let content_str = String::from_utf8_lossy(&content);
                assert_eq!(
                    content_str, "line1\nline2\nline3\nchanged4\nline5\nnew6\nnew7\nnew8\n",
                    "Target should have the second hunk (changed4) added plus new lines"
                );
            }
            _ => panic!("Expected hunk_test.txt to be a file in target commit"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn copy_hunk_from_parent() -> anyhow::Result<()> {
        use jj_lib::repo::Repo;

        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // Copy/restore hunk from hunk_base (parent) to hunk_child_single (child)
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 1, len: 3 },
                to_file: ChangeRange { start: 1, len: 3 },
            },
            lines: MultilineString {
                lines: vec![
                    " line1".to_owned(),
                    "-line2".to_owned(),
                    "+modified2".to_owned(),
                    " line3".to_owned(),
                ],
            },
        };

        // This should restore "line2" back (undo the "modified2" change)
        let mutation = CopyHunk {
            from_id: revs::hunk_base().commit,
            to_id: revs::hunk_child_single(),
            path: TreePath {
                repo_path: "hunk_test.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // Verify: child should now have parent's content (restoration)
        let child_commit = get_by_chid(&ws, &revs::hunk_child_single())?;
        let child_tree = child_commit.tree();
        let repo_path = jj_lib::repo_path::RepoPath::from_internal_string("hunk_test.txt")?;

        match child_tree.path_value(repo_path).await?.into_resolved() {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(repo_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                let content_str = String::from_utf8_lossy(&content);
                assert_eq!(
                    content_str, "line1\nline2\nline3\nline4\nline5\n",
                    "Child should have parent's content after restore"
                );
            }
            _ => panic!("Expected hunk_test.txt to be a file"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn copy_hunk_to_conflict() -> anyhow::Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // Use the existing conflict_bookmark from test repo
        let conflict_commit = revs::conflict_bookmark();
        let parent_commit = revs::main_bookmark();

        // Try to copy a hunk to the conflicted commit
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 1, len: 1 },
                to_file: ChangeRange { start: 1, len: 1 },
            },
            lines: MultilineString {
                lines: vec!["-original".to_owned(), "+changed".to_owned()],
            },
        };

        let mutation = CopyHunk {
            from_id: parent_commit.commit.clone(),
            to_id: conflict_commit.clone(),
            path: TreePath {
                repo_path: "b.txt".to_owned(), // This file has conflicts
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await;

        // Should fail with precondition error about conflicts
        match result {
            Ok(MutationResult::PreconditionError { message }) => {
                assert!(
                    message.contains("conflict"),
                    "Expected error message about conflicts, got: {}",
                    message
                );
            }
            Ok(_) => panic!("Expected precondition error for conflicted file"),
            Err(e) => panic!("Expected precondition error, got hard error: {}", e),
        }

        Ok(())
    }

    #[tokio::test]
    async fn copy_hunk_out_of_bounds() -> anyhow::Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // small_parent has small.txt with "line1\nline2\n"
        // small_child has small.txt with "line1\nchanged\n"
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 1, len: 1 },
                to_file: ChangeRange { start: 10, len: 5 }, // Way out of bounds
            },
            lines: MultilineString {
                lines: vec!["-something".to_owned(), "+else".to_owned()],
            },
        };

        // Try to copy a hunk with out-of-bounds location using the small file commits
        let mutation = CopyHunk {
            from_id: revs::small_parent().commit,
            to_id: revs::small_child(),
            path: TreePath {
                repo_path: "small.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await;

        match result {
            Ok(MutationResult::PreconditionError { message }) => {
                assert!(
                    message.contains("out of bounds"),
                    "Expected error about bounds, got: {}",
                    message
                );
            }
            Ok(_) => panic!("Expected precondition error for out of bounds"),
            Err(e) => panic!("Expected precondition error, got hard error: {}", e),
        }

        Ok(())
    }

    #[tokio::test]
    async fn copy_hunk_unchanged() -> anyhow::Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // hunk_base has line1-line5, hunk_sibling has line1-line5 plus new6-new8
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 1, len: 3 },
                to_file: ChangeRange { start: 1, len: 3 },
            },
            lines: MultilineString {
                lines: vec![
                    " line1".to_owned(),
                    " line2".to_owned(),
                    " line3".to_owned(),
                ],
            },
        };

        // Copy a hunk between hunk_base and hunk_sibling where that part is identical
        let mutation = CopyHunk {
            from_id: revs::hunk_base().commit,
            to_id: revs::hunk_sibling(),
            path: TreePath {
                repo_path: "hunk_test.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        // Trying to "restore" lines 1-3 from base to sibling should be unchanged (they're already identical)
        let result = mutation.execute_unboxed(&mut ws).await?;
        assert_matches!(result, MutationResult::Unchanged);

        Ok(())
    }

    #[tokio::test]
    async fn copy_hunk_multiple_hunks() -> anyhow::Result<()> {
        use jj_lib::repo::Repo;

        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // hunk_child_multi modifies two lines: line2->changed2 and line4->changed4
        let hunk = ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange { start: 3, len: 3 },
                to_file: ChangeRange { start: 3, len: 3 },
            },
            lines: MultilineString {
                lines: vec![
                    " line3".to_owned(),
                    "-line4".to_owned(),
                    "+changed4".to_owned(),
                    " line5".to_owned(),
                ],
            },
        };

        // Restore only the second hunk (line 4) from hunk_base
        let mutation = CopyHunk {
            from_id: revs::hunk_base().commit,
            to_id: revs::hunk_child_multi(),
            path: TreePath {
                repo_path: "hunk_test.txt".to_owned(),
                relative_path: "".into(),
            },
            hunk,
        };

        let result = mutation.execute_unboxed(&mut ws).await?;
        assert_matches!(result, MutationResult::Updated { .. });

        // Verify: line 2 still modified (changed2), line 4 restored (line4)
        let child_commit = get_by_chid(&ws, &revs::hunk_child_multi())?;
        let child_tree = child_commit.tree();
        let repo_path = jj_lib::repo_path::RepoPath::from_internal_string("hunk_test.txt")?;

        match child_tree.path_value(repo_path).await?.into_resolved() {
            Ok(Some(jj_lib::backend::TreeValue::File { id, .. })) => {
                let mut reader = ws.repo().store().read_file(repo_path, &id).await?;
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                let content_str = String::from_utf8_lossy(&content);
                assert_eq!(
                    content_str, "line1\nchanged2\nline3\nline4\nline5\n",
                    "Line 2 should remain modified (changed2), line 4 should be restored"
                );
            }
            _ => panic!("Expected hunk_test.txt to be a file"),
        }

        Ok(())
    }

    fn tracks(ws: &WorkspaceSession, path: &str) -> bool {
        use pollster::FutureExt as _;
        let tree = ws.get_commit(ws.wc_id()).unwrap().tree();
        !tree
            .path_value(RepoPath::from_internal_string(path).unwrap())
            .block_on()
            .unwrap()
            .is_absent()
    }

    #[tokio::test]
    async fn ignore_new_files_untracks_and_keeps_them() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;

        // a build output the working copy picked up
        fs::create_dir_all(repo.path().join("out/sub"))?;
        fs::write(repo.path().join("out/app.bin"), "build")?;
        fs::write(repo.path().join("out/sub/x.o"), "obj")?;
        ws.import_and_snapshot(true, false).await?;
        assert!(tracks(&ws, "out/app.bin"));

        let result = IgnoreNewFiles {
            paths: ["out/app.bin", "out/sub/x.o"]
                .iter()
                .map(|p| TreePath {
                    repo_path: p.to_string(),
                    relative_path: "".into(),
                })
                .collect(),
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::Updated { .. });

        assert!(!tracks(&ws, "out/app.bin") && !tracks(&ws, "out/sub/x.o"));
        assert!(
            repo.path().join("out/app.bin").exists(),
            "the file stays on disk"
        );
        // a later snapshot does not take it in again
        ws.import_and_snapshot(true, false).await?;
        assert!(!tracks(&ws, "out/app.bin"));

        // one line for the whole new folder
        let exclude = fs::read_dir(repo.path().join(".jj/repo/store"))?
            .filter_map(|e| e.ok())
            .map(|e| e.path().join("info/exclude"))
            .find(|p| p.exists())
            .expect("an exclude file in the git store");
        let text = fs::read_to_string(exclude)?;
        assert_eq!(text.lines().filter(|l| *l == "/out/").count(), 1, "{text}");
        Ok(())
    }

    #[tokio::test]
    async fn ignore_new_files_refuses_files_the_parent_has() -> Result<()> {
        let repo = mkrepo();
        let mut session = WorkerSession::default();
        let mut ws = session.load_workspace(repo.path()).await?;
        let parent_file = ws
            .get_commit(ws.wc_id())?
            .tree()
            .entries()
            .next()
            .map(|(path, _)| path.as_internal_file_string().to_owned())
            .expect("a file in the working copy");
        let result = IgnoreNewFiles {
            paths: vec![TreePath {
                repo_path: parent_file,
                relative_path: "".into(),
            }],
        }
        .execute_unboxed(&mut ws)
        .await?;
        assert_matches!(result, MutationResult::PreconditionError { .. });
        Ok(())
    }

    #[test]
    fn gitignore_escapes() {
        assert_eq!(escape_gitignore("a/b.txt"), "a/b.txt");
        assert_eq!(escape_gitignore("x[1]*?.log"), "x\\[1\\]\\*\\?.log");
        assert_eq!(escape_gitignore("#notes"), "\\#notes");
        assert_eq!(escape_gitignore("!keep"), "\\!keep");
        assert_eq!(escape_gitignore("trail "), "trail\\ ");
    }
}
