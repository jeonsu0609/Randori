//! View model of one workspace: rows, refs, files and the inspector detail, and
//! the pure helpers that build them from GG's messages.

use super::*;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum RefKind {
    Local,
    Remote,
    Tag,
}

#[derive(Clone)]
pub(super) struct RefChip {
    pub(super) name: SharedString,
    pub(super) kind: RefKind,
    /// The ref as GG reports it, for bookmark operations.
    pub(super) store: StoreRef,
}

/// One row of the bookmarks panel.
pub(super) struct RefItem {
    pub(super) chip: RefChip,
    pub(super) row: usize,
    pub(super) change: SharedString,
    pub(super) summary: SharedString,
    pub(super) time: SharedString,
}

/// The parts of a log row the virtual list needs, copied so the list closure owns them.
pub(super) struct RowData {
    pub(super) id: RevId,
    pub(super) change_prefix: SharedString,
    pub(super) change_rest: SharedString,
    pub(super) summary: Option<SharedString>,
    pub(super) author: SharedString,
    /// For the author's avatar; lowercase, as avatars are keyed.
    pub(super) author_email: SharedString,
    pub(super) time: SharedString,
    pub(super) refs: Vec<RefChip>,
    pub(super) is_working_copy: bool,
}

pub(super) struct FileItem {
    /// GG's path, for file and hunk mutations.
    pub(super) tree_path: TreePath,
    /// GG's hunks, for moving one hunk.
    pub(super) hunks: Vec<ChangeHunk>,
    pub(super) path: SharedString,
    pub(super) name: SharedString,
    pub(super) dir: SharedString,
    pub(super) kind: ChangeKind,
    pub(super) additions: usize,
    pub(super) deletions: usize,
    pub(super) has_conflict: bool,
}

pub(super) struct Detail {
    pub(super) header: RevHeader,
    pub(super) parents: Vec<SharedString>,
    pub(super) parent_ids: Vec<RevId>,
    pub(super) files: Vec<FileItem>,
    pub(super) patches: Vec<DiffFile>,
    pub(super) conflicts: usize,
}

impl RowData {
    pub(super) fn new(row: &LogRow, now: DateTime<Local>) -> Self {
        let rev = &row.revision;
        Self {
            id: rev.id.clone(),
            change_prefix: rev.id.change.prefix.clone().into(),
            change_rest: rev
                .id
                .change
                .rest
                .chars()
                .take(6)
                .collect::<String>()
                .into(),
            summary: rev
                .description
                .lines
                .first()
                .filter(|line| !line.trim().is_empty())
                .map(|line| SharedString::from(line.clone())),
            author: rev.author.name.clone().into(),
            author_email: rev.author.email.to_ascii_lowercase().into(),
            time: relative_time(rev.author.timestamp, now).into(),
            refs: ref_chips(&rev.refs),
            is_working_copy: rev.is_working_copy,
        }
    }
}

impl From<&RevChange> for FileItem {
    fn from(change: &RevChange) -> Self {
        let path = change.path.repo_path.clone();
        let (dir, name) = match path.rsplit_once('/') {
            Some((dir, name)) => (dir.to_owned(), name.to_owned()),
            None => (String::new(), path.clone()),
        };
        let (mut additions, mut deletions) = (0, 0);
        for line in change.hunks.iter().flat_map(|hunk| &hunk.lines.lines) {
            if line.starts_with('+') {
                additions += 1;
            } else if line.starts_with('-') {
                deletions += 1;
            }
        }
        Self {
            tree_path: change.path.clone(),
            hunks: change.hunks.clone(),
            path: path.into(),
            name: name.into(),
            dir: dir.into(),
            kind: match change.kind {
                ChangeKind::Added => ChangeKind::Added,
                ChangeKind::Deleted => ChangeKind::Deleted,
                ChangeKind::Modified => ChangeKind::Modified,
                ChangeKind::None => ChangeKind::None,
            },
            additions,
            deletions,
            has_conflict: change.has_conflict,
        }
    }
}

/// Turns a revision query into the inspector's model, parsing every file's
/// patch for the diff view. Runs on a background thread.
pub(super) fn build_detail(result: RevsResult) -> Option<Detail> {
    let RevsResult::Detail {
        headers,
        parents,
        changes,
        conflicts,
        ..
    } = result
    else {
        return None;
    };
    let header = headers.into_iter().last()?;
    let files: Vec<FileItem> = changes.iter().map(FileItem::from).collect();
    let patches: Vec<DiffFile> = changes
        .iter()
        .filter_map(|change| DiffFile::parse(&unified_patch(change)).ok())
        .flatten()
        .collect();
    Some(Detail {
        parent_ids: parents.iter().map(|parent| parent.id.clone()).collect(),
        parents: parents
            .iter()
            .map(|parent| {
                SharedString::from(short_id(
                    &parent.id.change.prefix,
                    &parent.id.change.rest,
                    8,
                ))
            })
            .collect(),
        header,
        files,
        patches,
        conflicts: conflicts.len(),
    })
}

/// Zero counts carry no signal, so they stay muted instead of red or green.
pub(super) fn count_color(
    count: usize,
    color: gpui_kit::Hsla,
    muted: gpui_kit::Hsla,
) -> gpui_kit::Hsla {
    if count == 0 { muted } else { color }
}

/// Unique prefix plus enough of the rest to make `len` characters.
pub(super) fn short_id(prefix: &str, rest: &str, len: usize) -> String {
    let extra = len.saturating_sub(prefix.chars().count());
    prefix.chars().chain(rest.chars().take(extra)).collect()
}

/// `~/…` for paths under the home directory, and only the last two components
/// when the path would still crowd the title bar.
pub(super) fn display_path(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = match path.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => path.to_owned(),
    };
    if path.chars().count() <= 48 {
        return path;
    }
    let parts: Vec<&str> = path.rsplitn(3, '/').collect();
    match parts.as_slice() {
        [last, parent, _] => format!("…/{parent}/{last}"),
        _ => path,
    }
}

pub(super) fn ref_chips(refs: &[StoreRef]) -> Vec<RefChip> {
    refs.iter()
        .filter_map(|r| match r {
            StoreRef::LocalBookmark { bookmark_name, .. } => Some(RefChip {
                name: bookmark_name.clone().into(),
                kind: RefKind::Local,
                store: r.clone(),
            }),
            // A remote ref that is in sync with its local bookmark adds no information.
            StoreRef::RemoteBookmark {
                bookmark_name,
                remote_name,
                is_synced,
                is_tracked,
                ..
            } if !(*is_synced && *is_tracked) => Some(RefChip {
                name: format!("{bookmark_name}@{remote_name}").into(),
                kind: RefKind::Remote,
                store: r.clone(),
            }),
            StoreRef::RemoteBookmark { .. } => None,
            StoreRef::Tag { tag_name } => Some(RefChip {
                name: tag_name.clone().into(),
                kind: RefKind::Tag,
                store: r.clone(),
            }),
        })
        .collect()
}

/// Builds a git-style patch for one file so gpui-kit's `Diff` can parse it.
/// GG's hunk ranges are already 1-based (its copy of jj's hunker starts at
/// `1..1`); a unified header only differs for an empty side, which names the
/// line before the hunk (`-0,0` for an added file).
pub(super) fn unified_patch(change: &RevChange) -> String {
    let path = &change.path.repo_path;
    let old = if matches!(change.kind, ChangeKind::Added) {
        "/dev/null".to_owned()
    } else {
        format!("a/{path}")
    };
    let new = if matches!(change.kind, ChangeKind::Deleted) {
        "/dev/null".to_owned()
    } else {
        format!("b/{path}")
    };
    let mut patch = format!("diff --git a/{path} b/{path}\n--- {old}\n+++ {new}\n");
    for hunk in &change.hunks {
        let from = &hunk.location.from_file;
        let to = &hunk.location.to_file;
        let start = |range: &gg_lib::messages::ChangeRange| unified_start(range.start, range.len);
        patch.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            start(from),
            from.len,
            start(to),
            to.len
        ));
        for line in &hunk.lines.lines {
            patch.push_str(line.trim_end_matches(['\n', '\r']));
            patch.push('\n');
        }
    }
    patch
}

/// Unified-diff start line for a 1-based GG range.
pub(super) fn unified_start(start: usize, len: usize) -> usize {
    if len == 0 {
        start.saturating_sub(1)
    } else {
        start
    }
}

pub(super) fn relative_time(timestamp: DateTime<Local>, now: DateTime<Local>) -> String {
    let delta = now.signed_duration_since(timestamp);
    let minutes = delta.num_minutes();
    match minutes {
        m if m < 1 => "just now".to_owned(),
        m if m < 60 => format!("{m}m ago"),
        m if m < 60 * 24 => format!("{}h ago", m / 60),
        m if m < 60 * 24 * 30 => format!("{}d ago", m / (60 * 24)),
        _ => timestamp.format("%Y-%m-%d").to_string(),
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::unified_start;

    /// Values taken from `jj diff --git` on the same commits.
    #[test]
    fn unified_start_matches_jj() {
        // Modified hunk at line 381: `@@ -381,3 +381,5 @@`
        assert_eq!(unified_start(381, 3), 381);
        // Added file: `@@ -0,0 +1,3 @@` (GG reports 1..1 then 1..4)
        assert_eq!(unified_start(1, 0), 0);
        assert_eq!(unified_start(1, 3), 1);
    }
}
