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
    /// A local bookmark its remotes do not have yet (shown when
    /// `gg.ui.mark-unpushed-bookmarks` is on).
    pub(super) unpushed: bool,
    /// A local bookmark where the working copy is, checked as GitKraken
    /// checks the branch HEAD is on (see [`mark_current_ref`]).
    pub(super) current: bool,
}

/// The log row whose bookmarks are spread out. Its chip or the list spread
/// out from it keeps it open: the list reaches over the rows below, off the
/// chip, and covers the chip itself.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(super) struct RefsHover {
    chip: Option<usize>,
    list: Option<usize>,
}

impl RefsHover {
    pub(super) fn open_row(&self) -> Option<usize> {
        self.list.or(self.chip)
    }

    /// The pointer entered (`hovered`) or left row `ix`'s chip, or its list
    /// when `list`. Whether that changes what is spread out.
    pub(super) fn set(&mut self, ix: usize, list: bool, hovered: bool) -> bool {
        let before = self.open_row();
        let slot = if list { &mut self.list } else { &mut self.chip };
        if hovered {
            *slot = Some(ix);
        } else if *slot == Some(ix) {
            *slot = None;
        }
        self.open_row() != before
    }
}

/// One bookmark, remote bookmark or tag of the side panel: every one of
/// the repository, loaded in the log or not. Its names are worked out once,
/// as the panel draws again with every frame the log scrolls.
#[derive(Clone)]
pub(super) struct RefItem {
    pub(super) chip: RefChip,
    /// The revision it is on, and its row when the log has it.
    pub(super) id: RevId,
    pub(super) row: Option<usize>,
    /// Its full name as git spells it: `origin/release/2.0.0` for a remote
    /// bookmark.
    pub(super) full_name: SharedString,
    /// The full name and the chip's, lowercase, for the panel's filter.
    pub(super) search: String,
    /// Where it sits in the panel's tree: its full name split at `/`, as
    /// GitKraken folds `release/2.0.0` under `release` (and a remote
    /// bookmark under its remote).
    pub(super) path: Vec<SharedString>,
}

impl RefItem {
    fn new(chip: RefChip, id: RevId, row: Option<usize>) -> RefItem {
        let full_name: SharedString = match &chip.store {
            StoreRef::RemoteBookmark {
                bookmark_name,
                remote_name,
                ..
            } => format!("{remote_name}/{bookmark_name}").into(),
            _ => chip.name.clone(),
        };
        let search = format!("{full_name}\n{}", chip.name).to_lowercase();
        let path = full_name
            .split('/')
            .map(|part| part.to_owned().into())
            .collect();
        RefItem {
            chip,
            id,
            row,
            full_name,
            search,
            path,
        }
    }
}

/// One workspace of the side panel (GitKraken's worktrees): its name and
/// working-copy revision.
#[derive(Clone)]
pub(super) struct WorkspaceItem {
    pub(super) name: SharedString,
    pub(super) id: RevId,
    pub(super) row: Option<usize>,
    /// The workspace this tab shows.
    pub(super) this: bool,
}

/// Whether `rev` is the working copy of the workspace this tab shows. GG's
/// `is_working_copy` holds for every workspace's working copy, and
/// `working_copy_of` names the workspace for the others.
pub(super) fn is_this_working_copy(rev: &RevHeader) -> bool {
    rev.is_working_copy && rev.working_copy_of.is_none()
}

/// The side panel's refs and workspaces from `headers` (the revisions with
/// any), in the panel's order, not placed in the log yet (see
/// [`place_panel_items`]). Unlike the log's chips, a remote bookmark in sync
/// with its local one is listed too, as GitKraken's REMOTE lists every
/// remote branch. `this_workspace` is the name of the workspace the tab
/// shows: headers name only the other workspaces.
pub(super) fn panel_items(
    headers: &[RevHeader],
    this_workspace: &str,
) -> (Vec<RefItem>, Vec<WorkspaceItem>) {
    let mut refs = Vec::new();
    let mut workspaces = Vec::new();
    for header in headers {
        for r in &header.refs {
            let chip = match r {
                StoreRef::RemoteBookmark {
                    bookmark_name,
                    remote_name,
                    ..
                } => RefChip {
                    name: format!("{bookmark_name}@{remote_name}").into(),
                    kind: RefKind::Remote,
                    store: r.clone(),
                    unpushed: false,
                    current: false,
                },
                // a local bookmark or tag: always a chip
                _ => match ref_chips(std::slice::from_ref(r)).pop() {
                    Some(chip) => chip,
                    None => continue,
                },
            };
            refs.push(RefItem::new(chip, header.id.clone(), None));
        }
        let workspace = |name: &str, this: bool| WorkspaceItem {
            name: name.to_owned().into(),
            id: header.id.clone(),
            row: None,
            this,
        };
        if let Some(other) = &header.working_copy_of {
            workspaces.push(workspace(other, false));
        } else if header.is_working_copy {
            workspaces.push(workspace(this_workspace, true));
        }
    }
    // in tree order, so a folder's refs come together (a conflicted bookmark
    // is on several revisions: listed once)
    refs.sort_by(|a, b| (&a.path, a.chip.kind as u8).cmp(&(&b.path, b.chip.kind as u8)));
    refs.dedup_by(|a, b| a.chip.name == b.chip.name && a.chip.kind == b.chip.kind);
    workspaces.sort_by(|a, b| b.this.cmp(&a.this).then_with(|| a.name.cmp(&b.name)));
    (refs, workspaces)
}

/// Each panel item's row (`row_of_commit`), when the log has its revision, and the
/// check on the `current` bookmark (see [`nearest_bookmark`]).
pub(super) fn place_panel_items(
    refs: &mut [RefItem],
    workspaces: &mut [WorkspaceItem],
    row_of_commit: &HashMap<String, usize>,
    current: Option<&str>,
) {
    let row_of = |id: &RevId| row_of_commit.get(&id.commit.hex).copied();
    for item in refs {
        item.row = row_of(&item.id);
        item.chip.current =
            item.chip.kind == RefKind::Local && current == Some(item.chip.name.as_ref());
    }
    for item in workspaces {
        item.row = row_of(&item.id);
    }
}

/// The parts of a log row the virtual list needs, copied so the list closure owns them.
#[derive(Clone)]
pub(super) struct RowData {
    pub(super) id: RevId,
    pub(super) change_prefix: SharedString,
    pub(super) change_rest: SharedString,
    pub(super) summary: Option<SharedString>,
    pub(super) author: SharedString,
    /// For the author's avatar; lowercase, as avatars are keyed.
    pub(super) author_email: SharedString,
    pub(super) time: SharedString,
    /// The coarse age the time separators group by (see [`time_bucket`]).
    pub(super) bucket: u32,
    pub(super) bucket_label: SharedString,
    /// First row of its age group in the log: draws the separator and label.
    pub(super) bucket_start: bool,
    pub(super) refs: Vec<RefChip>,
    pub(super) is_working_copy: bool,
    /// The description after its first line, on one line, shown dimmed after
    /// the summary as GitKraken does.
    pub(super) body: Option<SharedString>,
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

/// A part of a view that loads: on its way, there (with what it shows),
/// empty for a reason ("no changes"), or failed.
pub(super) enum Load<T = ()> {
    Loading,
    Ready(T),
    Note(SharedString),
    Failed(SharedString),
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
        Self::from_header(&row.revision, now)
    }

    /// A row for a revision outside the log (the file history).
    pub(super) fn from_header(rev: &RevHeader, now: DateTime<Local>) -> Self {
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
            bucket: time_bucket(rev.author.timestamp, now).0,
            bucket_label: time_bucket(rev.author.timestamp, now).1.into(),
            bucket_start: false,
            refs: ref_chips(&rev.refs),
            is_working_copy: rev.is_working_copy,
            body: body_preview(&rev.description.lines),
        }
    }
}

/// The lines after the first, joined on one line; a row shows only so much.
fn body_preview(lines: &[String]) -> Option<SharedString> {
    const MAX: usize = 240;
    let mut body = String::new();
    for line in lines
        .iter()
        .skip(1)
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
    {
        if !body.is_empty() {
            body.push_str(" | ");
        }
        body.push_str(line);
        if body.len() >= MAX {
            break;
        }
    }
    body.truncate(body.floor_char_boundary(MAX));
    (!body.is_empty()).then(|| body.into())
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
    let patches = diff_files(&changes);
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
            StoreRef::LocalBookmark {
                bookmark_name,
                is_synced,
                tracking_remotes,
                potential_remotes,
                ..
            } => Some(RefChip {
                name: bookmark_name.clone().into(),
                kind: RefKind::Local,
                store: r.clone(),
                // moved since it was pushed, or the repository has remotes and
                // none of them tracks it
                unpushed: !*is_synced || (tracking_remotes.is_empty() && *potential_remotes > 0),
                current: false,
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
                unpushed: false,
                current: false,
            }),
            StoreRef::RemoteBookmark { .. } => None,
            StoreRef::Tag { tag_name } => Some(RefChip {
                name: tag_name.clone().into(),
                kind: RefKind::Tag,
                store: r.clone(),
                unpushed: false,
                current: false,
            }),
        })
        .collect()
}

/// The local bookmark nearest to the working copy, with its row: on `@`
/// itself or the closest of its loaded ancestors. jj has no checked-out
/// branch; this stands in for one. Where several bookmarks share that
/// revision, `preferred` (the one last switched to) when it is among them,
/// else the first.
pub(super) fn nearest_bookmark(
    rows: &[LogRow],
    row_of_commit: &HashMap<String, usize>,
    preferred: Option<&str>,
) -> Option<(usize, String)> {
    let start = rows
        .iter()
        .position(|row| is_this_working_copy(&row.revision))?;
    // only the rows the search reaches are looked into
    let parents = |ix: usize| -> Vec<usize> {
        rows[ix]
            .revision
            .parent_ids
            .iter()
            .filter_map(|id| row_of_commit.get(&id.hex).copied())
            .collect()
    };
    let names = |ix: usize| -> Vec<&str> {
        rows[ix]
            .revision
            .refs
            .iter()
            .filter_map(|r| match r {
                StoreRef::LocalBookmark { bookmark_name, .. } => Some(bookmark_name.as_str()),
                _ => None,
            })
            .collect()
    };
    nearest_in(start, parents, names, preferred).map(|(ix, name)| (ix, name.to_owned()))
}

/// [`nearest_bookmark`] over row numbers: each row's parents and bookmark
/// names, breadth first from `start`.
fn nearest_in<'a>(
    start: usize,
    parents: impl Fn(usize) -> Vec<usize>,
    names: impl Fn(usize) -> Vec<&'a str>,
    preferred: Option<&str>,
) -> Option<(usize, &'a str)> {
    let mut queue = std::collections::VecDeque::from([start]);
    let mut seen = std::collections::HashSet::from([start]);
    while let Some(ix) = queue.pop_front() {
        let names = names(ix);
        if let Some(&first) = names.first() {
            let name = preferred
                .and_then(|preferred| names.iter().copied().find(|name| *name == preferred))
                .unwrap_or(first);
            return Some((ix, name));
        }
        for next in parents(ix) {
            if seen.insert(next) {
                queue.push_back(next);
            }
        }
    }
    None
}

/// Checks the current bookmark (see [`nearest_bookmark`]) and lists it first
/// in its row, so a row's one visible chip is the current one. A bookmark
/// checked before is checked no longer.
pub(super) fn mark_current_ref(data: &mut [RowData], current: Option<(usize, &str)>) {
    for chip in data.iter_mut().flat_map(|row| row.refs.iter_mut()) {
        chip.current = false;
    }
    let Some((ix, name)) = current else {
        return;
    };
    let refs = &mut data[ix].refs;
    for chip in refs.iter_mut() {
        chip.current = chip.kind == RefKind::Local && chip.name == name;
    }
    refs.sort_by_key(|chip| !chip.current);
}

/// Builds a git-style patch for one file so gpui-kit's `Diff` can parse it.
/// GG's hunk ranges are already 1-based (its copy of jj's hunker starts at
/// `1..1`); a unified header only differs for an empty side, which names the
/// line before the hunk (`-0,0` for an added file).
/// The changes as the diff view's files: each one written as a unified
/// patch and read back.
pub(super) fn diff_files(changes: &[RevChange]) -> Vec<DiffFile> {
    changes
        .iter()
        .filter_map(|change| DiffFile::parse(&unified_patch(change)).ok())
        .flatten()
        .collect()
}

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

/// The age group of `timestamp`, GitKraken style: each hour up to a day, then
/// days, weeks, months and years. Returns an ordering key (equal keys are one
/// group) and the label ("3 hours ago", "a month ago").
pub(super) fn time_bucket(timestamp: DateTime<Local>, now: DateTime<Local>) -> (u32, String) {
    let delta = now.signed_duration_since(timestamp);
    let hours = delta.num_hours();
    let days = delta.num_days();
    let plural = |n: i64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    // a clock ahead of this one counts as now
    match () {
        _ if hours < 1 => (0, "just now".to_owned()),
        _ if hours < 24 => (100 + hours as u32, plural(hours, "hour")),
        _ if days < 7 => (200 + days as u32, plural(days, "day")),
        _ if days < 30 => (300 + (days / 7) as u32, plural(days / 7, "week")),
        _ if days < 365 => {
            let months = (days / 30).clamp(1, 11);
            let label = if months == 1 {
                "a month ago".to_owned()
            } else {
                format!("{months} months ago")
            };
            (400 + months as u32, label)
        }
        _ => {
            let years = days / 365;
            let label = if years == 1 {
                "a year ago".to_owned()
            } else {
                format!("{years} years ago")
            };
            (500 + years as u32, label)
        }
    }
}

/// Marks the first row of every age group (and the first row), where the
/// log draws a separator.
pub(super) fn mark_bucket_starts(rows: &mut [RowData]) {
    let mut previous = None;
    for row in rows {
        row.bucket_start = previous != Some(row.bucket);
        previous = Some(row.bucket);
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::{
        RefsHover, RevHeader, Side, StoreRef, changed_line_positions, is_this_working_copy,
        nearest_in, panel_items, partial_hunk, ref_chips, selected_hunk_lines,
        selects_every_change, time_bucket, unified_start,
    };
    use gg_lib::messages::ChangeHunk;
    use gg_lib::messages::{ChangeLocation, ChangeRange, MultilineString};

    fn hunk(from: (usize, usize), to: (usize, usize), lines: &[&str]) -> ChangeHunk {
        ChangeHunk {
            location: ChangeLocation {
                from_file: ChangeRange {
                    start: from.0,
                    len: from.1,
                },
                to_file: ChangeRange {
                    start: to.0,
                    len: to.1,
                },
            },
            lines: MultilineString {
                lines: lines.iter().map(|line| format!("{line}\n")).collect(),
            },
        }
    }

    #[test]
    fn a_selection_covers_the_lines_the_diff_highlights() {
        // old 10 `a`, 11 `b` removed; new 10 `A`, 11 `B` added; then old 12 / new 12 `c`
        let hunks = [hunk((10, 3), (10, 3), &["-a", "-b", "+A", "+B", " c"])];
        let flags = |start, end| selected_hunk_lines(&hunks, start, end).remove(0);
        // one side: only lines numbered there
        assert_eq!(
            flags((Side::Modified, 11), (Side::Modified, 11)),
            [false, false, false, true, false]
        );
        assert_eq!(
            flags((Side::Original, 10), (Side::Original, 11)),
            [true, true, false, false, false]
        );
        // across the sides: the patch lines between, in either direction
        assert_eq!(
            flags((Side::Original, 11), (Side::Modified, 10)),
            [false, true, true, false, false]
        );
        assert_eq!(
            flags((Side::Modified, 10), (Side::Original, 11)),
            [false, true, true, false, false]
        );
    }

    #[test]
    fn every_changed_line_has_its_place() {
        // old 10 `a`, 11 `b` removed; new 10 `A`, 11 `B` added; then `c`
        let hunks = [
            hunk((10, 3), (10, 3), &["-a", "-b", "+A", "+B", " c"]),
            hunk((20, 1), (20, 2), &[" d", "+e"]),
        ];
        assert_eq!(
            changed_line_positions(&hunks),
            [
                (Side::Original, 10),
                (Side::Original, 11),
                (Side::Modified, 10),
                (Side::Modified, 11),
                (Side::Modified, 21),
            ]
        );
    }

    #[test]
    fn a_partial_hunk_keeps_the_selected_changes_only() {
        let full = hunk((10, 3), (10, 3), &["-a", "-b", "+A", "+B", " c"]);
        // `-b` and `+A`: `-a` stays as context, `+B` goes
        let part = partial_hunk(&full, &[false, true, true, false, false]).expect("changes");
        assert_eq!(part.lines.lines, [" a\n", "-b\n", "+A\n", " c\n"]);
        assert_eq!(
            (part.location.from_file.start, part.location.from_file.len),
            (10, 3)
        );
        assert_eq!(
            (part.location.to_file.start, part.location.to_file.len),
            (10, 3)
        );
        // context only: nothing to move
        assert!(partial_hunk(&full, &[false, false, false, false, true]).is_none());
        // every change, or not
        assert!(selects_every_change(
            std::slice::from_ref(&full),
            &[vec![true, true, true, true, false]]
        ));
        assert!(!selects_every_change(
            &[full],
            &[vec![true, true, true, false, false]]
        ));
    }

    #[test]
    fn spread_bookmarks_stay_open_from_chip_to_list() {
        let mut hover = RefsHover::default();
        assert!(hover.set(3, false, true));
        assert_eq!(hover.open_row(), Some(3));
        // the list opens over the chip: the chip is left, the list entered
        assert!(!hover.set(3, true, true));
        assert!(!hover.set(3, false, false));
        assert_eq!(hover.open_row(), Some(3));
        // a late leave of another row's chip changes nothing
        assert!(!hover.set(7, false, false));
        // off the list: closed
        assert!(hover.set(3, true, false));
        assert_eq!(hover.open_row(), None);
    }

    #[test]
    fn the_current_bookmark_is_the_nearest_one() {
        // rows newest first: @ (0) on b (1) on a (2); a merge @ also has c (3)
        let parents = [vec![1], vec![2], vec![], vec![]];
        let none: Vec<&str> = Vec::new();
        let nearest = |names: &[Vec<&'static str>], preferred| {
            nearest_in(
                0,
                |ix| parents[ix].clone(),
                |ix| names[ix].clone(),
                preferred,
            )
        };
        let names = [none.clone(), vec!["qa", "van/qa"], vec!["main"], vec![]];
        // the closest revision with bookmarks, its first one
        assert_eq!(nearest(&names, None), Some((1, "qa")));
        // the one switched to, where it shares the revision
        assert_eq!(nearest(&names, Some("van/qa")), Some((1, "van/qa")));
        // one switched to elsewhere does not count
        assert_eq!(nearest(&names, Some("main")), Some((1, "qa")));
        // a bookmark on @ itself comes before its parent's
        let on_wc = [vec!["wip"], vec!["qa"], vec![], vec![]];
        assert_eq!(nearest(&on_wc, None), Some((0, "wip")));
        // further down when the parent has none
        let lower = [none.clone(), none.clone(), vec!["main"], vec![]];
        assert_eq!(nearest(&lower, None), Some((2, "main")));
        // none loaded
        assert_eq!(nearest_in(0, |_| Vec::new(), |_| Vec::new(), None), None);
    }

    #[test]
    fn the_panel_names_every_workspace_once() {
        let header = |hex: &str, is_working_copy: bool, of: Option<&str>| -> RevHeader {
            serde_json::from_value(serde_json::json!({
                "id": {
                    "change": {"type": "ChangeId", "hex": hex, "prefix": hex, "rest": "", "offset": null, "is_divergent": false},
                    "commit": {"type": "CommitId", "hex": hex, "prefix": hex, "rest": ""}
                },
                "description": {"lines": [""]},
                "author": {"name": "a", "email": "a@example.com", "timestamp": "2026-11-12T01:00:00+09:00"},
                "committed_at": "2026-11-12T01:00:00+09:00",
                "has_conflict": false,
                "is_working_copy": is_working_copy,
                "working_copy_of": of,
                "is_immutable": false,
                "refs": [],
                "parent_ids": []
            }))
            .expect("a header")
        };
        // GG marks every workspace's working copy, and names the others
        let headers = [
            header("aa", true, None),
            header("bb", true, Some("default")),
            header("cc", false, None),
        ];
        let (_, workspaces) = panel_items(&headers, "second");
        let listed: Vec<(&str, bool)> = workspaces
            .iter()
            .map(|item| (item.name.as_ref(), item.this))
            .collect();
        assert_eq!(listed, [("second", true), ("default", false)]);
        assert!(is_this_working_copy(&headers[0]));
        assert!(!is_this_working_copy(&headers[1]));
    }

    #[test]
    fn time_buckets_grow_with_age() {
        use chrono::{Duration, Local};
        let now = Local::now();
        let label = |ago: Duration| time_bucket(now - ago, now).1;
        assert_eq!(label(Duration::minutes(5)), "just now");
        assert_eq!(label(Duration::minutes(70)), "1 hour ago");
        assert_eq!(label(Duration::hours(5)), "5 hours ago");
        assert_eq!(label(Duration::days(1)), "1 day ago");
        assert_eq!(label(Duration::days(3)), "3 days ago");
        assert_eq!(label(Duration::days(8)), "1 week ago");
        assert_eq!(label(Duration::days(28)), "4 weeks ago");
        assert_eq!(label(Duration::days(40)), "a month ago");
        assert_eq!(label(Duration::days(200)), "6 months ago");
        assert_eq!(label(Duration::days(400)), "a year ago");
        assert_eq!(label(Duration::days(800)), "2 years ago");
        // a timestamp from a clock ahead of ours
        assert_eq!(label(Duration::hours(-2)), "just now");
        // two times in one group share the key
        assert_eq!(
            time_bucket(now - Duration::days(9), now).0,
            time_bucket(now - Duration::days(12), now).0
        );
    }

    /// `remotes` is how many remotes the repository has.
    fn local(is_synced: bool, tracking: &[&str], remotes: usize) -> StoreRef {
        StoreRef::LocalBookmark {
            bookmark_name: "feature".into(),
            has_conflict: false,
            is_synced,
            tracking_remotes: tracking.iter().map(|r| r.to_string()).collect(),
            available_remotes: tracking.len(),
            potential_remotes: remotes,
        }
    }

    #[test]
    fn unpushed_bookmarks() {
        let unpushed = |r: StoreRef| ref_chips(&[r])[0].unpushed;
        // pushed and in sync
        assert!(!unpushed(local(true, &["origin"], 1)));
        // moved since the last push
        assert!(unpushed(local(false, &["origin"], 1)));
        // never pushed to the remote that exists
        assert!(unpushed(local(true, &[], 1)));
        // no remote at all: nothing to push to
        assert!(!unpushed(local(true, &[], 0)));
    }

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
