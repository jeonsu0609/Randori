//! The rows of a file list, the inspector's or a comparison's: the changed
//! files as paths or as a tree of folders, with every other file of the
//! revision when all its files are shown. Pure, so it is unit tested; the
//! inspector builds the rows when what they come from changes
//! (`FileRows`), not at every frame, since a revision can have thousands of
//! files.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use gg_lib::messages::ChangeCounts;
use gg_lib::messages::queries::ChangeKind;
use gpui_kit::SharedString;

use super::model::FileItem;

/// One row of a file list.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum FileRow {
    /// A folder of the tree: `path` names it (its own, with the folders it
    /// stands for when they hold nothing else: `a/b`), `name` is what the
    /// row shows. `counts` are the changed files under it.
    Folder {
        path: SharedString,
        name: SharedString,
        depth: usize,
        open: bool,
        counts: ChangeCounts,
    },
    /// The changed file `ix` of the detail.
    Changed { ix: usize, depth: usize },
    /// A file the revision has and does not change (shown with all files).
    Unchanged {
        path: SharedString,
        name: SharedString,
        dir: SharedString,
        depth: usize,
    },
}

#[cfg(test)]
impl FileRow {
    pub(super) fn depth(&self) -> usize {
        match self {
            Self::Folder { depth, .. }
            | Self::Changed { depth, .. }
            | Self::Unchanged { depth, .. } => *depth,
        }
    }
}

/// What a list shows.
pub(super) struct FileListSpec<'a> {
    /// Folders, not paths.
    pub(super) tree: bool,
    /// Every file of the revision, the changed ones among them; `None` shows
    /// the changed files alone.
    pub(super) all: Option<&'a [String]>,
    /// Only paths with this in them, ignoring case; a folder shows when a
    /// path under it does, open.
    pub(super) filter: &'a str,
}

/// The rows of `files` (the changed files) as `spec` says. `open` tells
/// whether the folder at a path is open, given whether it would be by
/// default (open with the changed files alone, closed with all of them).
pub(super) fn file_rows(
    files: &[FileItem],
    spec: &FileListSpec<'_>,
    open: &dyn Fn(&str, bool) -> bool,
) -> Vec<FileRow> {
    let filter = spec.filter.trim().to_lowercase();
    let wanted = |path: &str| filter.is_empty() || path.to_lowercase().contains(&filter);

    // every path to show, the changed ones with their index
    let mut entries: Vec<(SharedString, Option<usize>)> = Vec::new();
    match spec.all {
        None => {
            for (ix, file) in files.iter().enumerate() {
                if wanted(&file.path) {
                    entries.push((file.path.clone(), Some(ix)));
                }
            }
        }
        Some(all) => {
            let changed: BTreeMap<&str, usize> = files
                .iter()
                .enumerate()
                .map(|(ix, file)| (file.path.as_ref(), ix))
                .collect();
            // a deleted file is a change the revision's tree no longer has;
            // looked up in a set, as a revision can change thousands of
            // files of a tree of tens of thousands
            let in_tree: HashSet<&str> = all.iter().map(String::as_str).collect();
            let mut paths: Vec<&str> = in_tree.iter().copied().collect();
            paths.extend(changed.keys().filter(|path| !in_tree.contains(*path)));
            paths.sort_unstable();
            for path in paths {
                if wanted(path) {
                    entries.push((
                        SharedString::from(path.to_owned()),
                        changed.get(path).copied(),
                    ));
                }
            }
        }
    }

    if !spec.tree {
        return entries
            .into_iter()
            .map(|(path, ix)| match ix {
                Some(ix) => FileRow::Changed { ix, depth: 0 },
                None => unchanged(&path, 0),
            })
            .collect();
    }

    let mut root = Folder::default();
    for (path, ix) in &entries {
        let mut folder = &mut root;
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                folder.files.push((part.to_owned(), path.clone(), *ix));
            } else {
                if let Some(ix) = ix {
                    count(&mut folder.counts, &files[*ix]);
                }
                folder = folder.folders.entry(part.to_owned()).or_default();
            }
        }
        if let Some(ix) = ix {
            count(&mut folder.counts, &files[*ix]);
        }
    }

    let default_open = spec.all.is_none() || !filter.is_empty();
    let mut rows = Vec::new();
    push_folder(&root, "", 0, default_open, open, &mut rows);
    rows
}

/// A folder of the tree being built: its folders by name, and its files
/// (name, path, changed index).
#[derive(Default)]
struct Folder {
    folders: BTreeMap<String, Folder>,
    files: Vec<(String, SharedString, Option<usize>)>,
    counts: ChangeCounts,
}

fn count(counts: &mut ChangeCounts, file: &FileItem) {
    match file.kind {
        ChangeKind::Added => counts.added += 1,
        ChangeKind::Deleted => counts.deleted += 1,
        ChangeKind::Modified | ChangeKind::None => counts.modified += 1,
    }
    if file.has_conflict {
        counts.conflicted += 1;
    }
}

/// Names as a file browser orders them: ignoring case, then by the bytes.
fn by_name(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// `folder`'s rows at `depth`, folders first, each folder that holds only
/// one folder shown as one row with it (`a/b`).
fn push_folder(
    folder: &Folder,
    prefix: &str,
    depth: usize,
    default_open: bool,
    open: &dyn Fn(&str, bool) -> bool,
    rows: &mut Vec<FileRow>,
) {
    let mut folders: Vec<(&String, &Folder)> = folder.folders.iter().collect();
    folders.sort_by(|a, b| by_name(a.0, b.0));
    for (name, mut child) in folders {
        let mut name = name.clone();
        while child.files.is_empty() && child.folders.len() == 1 {
            let (next, grandchild) = child.folders.iter().next().expect("one folder");
            name = format!("{name}/{next}");
            child = grandchild;
        }
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let is_open = open(&path, default_open);
        rows.push(FileRow::Folder {
            path: path.clone().into(),
            name: name.into(),
            depth,
            open: is_open,
            counts: child.counts,
        });
        if is_open {
            push_folder(child, &path, depth + 1, default_open, open, rows);
        }
    }
    let mut files: Vec<&(String, SharedString, Option<usize>)> = folder.files.iter().collect();
    files.sort_by(|a, b| by_name(&a.0, &b.0));
    for (_, path, ix) in files {
        rows.push(match ix {
            Some(ix) => FileRow::Changed { ix: *ix, depth },
            None => unchanged(path, depth),
        });
    }
}

fn unchanged(path: &SharedString, depth: usize) -> FileRow {
    let (dir, name) = match path.rsplit_once('/') {
        Some((dir, name)) => (dir.to_owned(), name.to_owned()),
        None => (String::new(), path.to_string()),
    };
    FileRow::Unchanged {
        path: path.clone(),
        name: name.into(),
        dir: dir.into(),
        depth,
    }
}

/// A list's rows, built again only when what they come from changes.
#[derive(Default)]
pub(super) struct FileRows {
    key: Option<RowsKey>,
    rows: Rc<Vec<FileRow>>,
}

/// What the rows were built from: the files (by the detail's serial, which a
/// new detail changes), all of them, the layout, the filter and the folders
/// opened or closed.
#[derive(Clone, PartialEq)]
pub(super) struct RowsKey {
    pub(super) files: usize,
    pub(super) all: Option<usize>,
    pub(super) tree: bool,
    pub(super) filter: String,
    pub(super) folders: u64,
}

impl FileRows {
    /// The rows for `key`, built by `build` when they are not already.
    pub(super) fn get(
        &mut self,
        key: RowsKey,
        build: impl FnOnce() -> Vec<FileRow>,
    ) -> Rc<Vec<FileRow>> {
        if self.key.as_ref() != Some(&key) {
            self.rows = Rc::new(build());
            self.key = Some(key);
        }
        self.rows.clone()
    }
}

#[cfg(test)]
mod tests {
    use gg_lib::messages::{DisplayPath, TreePath};

    use super::*;

    fn file(path: &str, kind: ChangeKind) -> FileItem {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        FileItem {
            tree_path: TreePath {
                repo_path: path.to_owned(),
                relative_path: DisplayPath(path.to_owned()),
            },
            hunks: Vec::new(),
            path: path.to_owned().into(),
            name: name.to_owned().into(),
            dir: dir.to_owned().into(),
            kind,
            additions: 0,
            deletions: 0,
            has_conflict: false,
        }
    }

    fn spec<'a>(tree: bool, all: Option<&'a [String]>, filter: &'a str) -> FileListSpec<'a> {
        FileListSpec { tree, all, filter }
    }

    /// The rows as text: a folder as `name/` and `+` or `-` for open or
    /// closed, a changed file as `*name`, an unchanged one by its name, each
    /// indented two spaces a level.
    fn show(files: &[FileItem], rows: &[FileRow]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                let pad = "  ".repeat(row.depth());
                match row {
                    FileRow::Folder { name, open, .. } => {
                        format!("{pad}{}{name}/", if *open { "+" } else { "-" })
                    }
                    FileRow::Changed { ix, .. } => format!("{pad}*{}", files[*ix].path),
                    FileRow::Unchanged { path, .. } => format!("{pad}{path}"),
                }
            })
            .collect()
    }

    fn open_all(_: &str, default: bool) -> bool {
        default
    }

    fn changed() -> Vec<FileItem> {
        vec![
            file("README.md", ChangeKind::Modified),
            file("crates/app/src/main.rs", ChangeKind::Modified),
            file("crates/app/src/view.rs", ChangeKind::Added),
            file("crates/app/Cargo.toml", ChangeKind::Modified),
            file("src/lib.rs", ChangeKind::Deleted),
        ]
    }

    #[test]
    fn paths_keep_the_detail_s_order() {
        let files = changed();
        let rows = file_rows(&files, &spec(false, None, ""), &open_all);
        assert_eq!(
            rows,
            (0..files.len())
                .map(|ix| FileRow::Changed { ix, depth: 0 })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_tree_puts_folders_first_and_joins_the_ones_with_one_folder() {
        let files = changed();
        let rows = file_rows(&files, &spec(true, None, ""), &open_all);
        // crates holds app alone, so they are one row
        assert_eq!(
            show(&files, &rows),
            [
                "+crates/app/",
                "  +src/",
                "    *crates/app/src/main.rs",
                "    *crates/app/src/view.rs",
                "  *crates/app/Cargo.toml",
                "+src/",
                "  *src/lib.rs",
                "*README.md",
            ]
        );
        let FileRow::Folder { path, counts, .. } = &rows[0] else {
            panic!("a folder");
        };
        assert_eq!(path.as_ref(), "crates/app");
        assert_eq!((counts.modified, counts.added, counts.deleted), (2, 1, 0));
    }

    #[test]
    fn a_closed_folder_hides_what_is_in_it() {
        let files = changed();
        let rows = file_rows(&files, &spec(true, None, ""), &|path, default| {
            path != "crates/app" && default
        });
        assert_eq!(
            show(&files, &rows),
            ["-crates/app/", "+src/", "  *src/lib.rs", "*README.md"]
        );
    }

    #[test]
    fn all_files_show_the_unchanged_ones_and_the_deleted_ones() {
        let files = changed();
        let all: Vec<String> = [
            "Cargo.toml",
            "README.md",
            "crates/app/Cargo.toml",
            "crates/app/src/main.rs",
            "crates/app/src/view.rs",
            "crates/app/src/zeta.rs",
        ]
        .map(str::to_owned)
        .to_vec();
        let rows = file_rows(&files, &spec(false, Some(&all), ""), &open_all);
        assert_eq!(
            show(&files, &rows),
            [
                "Cargo.toml",
                "*README.md",
                "*crates/app/Cargo.toml",
                "*crates/app/src/main.rs",
                "*crates/app/src/view.rs",
                "crates/app/src/zeta.rs",
                "*src/lib.rs",
            ]
        );
        // as a tree, the folders start closed
        let rows = file_rows(&files, &spec(true, Some(&all), ""), &open_all);
        assert_eq!(
            show(&files, &rows),
            ["-crates/app/", "-src/", "Cargo.toml", "*README.md"]
        );
    }

    #[test]
    fn all_files_of_a_large_tree_keep_every_change() {
        // 5,000 changes, half of them deletions, in a tree of 20,000 files
        let all: Vec<String> = (0..20_000)
            .map(|n| format!("d{}/f{n:05}", n % 50))
            .collect();
        let files: Vec<FileItem> = (0..5_000)
            .map(|n| {
                if n % 2 == 0 {
                    file(&all[n * 4], ChangeKind::Modified)
                } else {
                    file(&format!("gone/g{n:05}"), ChangeKind::Deleted)
                }
            })
            .collect();
        let rows = file_rows(&files, &spec(false, Some(&all), ""), &open_all);
        assert_eq!(rows.len(), 20_000 + 2_500);
        let changed = rows
            .iter()
            .filter(|row| matches!(row, FileRow::Changed { .. }))
            .count();
        assert_eq!(changed, 5_000);
    }

    #[test]
    fn names_go_by_letter_whatever_their_case() {
        let files: Vec<FileItem> = ["b.txt", "CLAUDE.md", "Cargo.toml", "a/x", "B/y"]
            .into_iter()
            .map(|path| file(path, ChangeKind::Modified))
            .collect();
        let rows = file_rows(&files, &spec(true, None, ""), &open_all);
        assert_eq!(
            show(&files, &rows),
            [
                "+a/",
                "  *a/x",
                "+B/",
                "  *B/y",
                "*b.txt",
                "*Cargo.toml",
                "*CLAUDE.md"
            ]
        );
    }

    #[test]
    fn a_filter_keeps_the_matching_paths_and_opens_their_folders() {
        let files = changed();
        let all: Vec<String> = [
            "crates/app/src/main.rs",
            "crates/app/src/view.rs",
            "docs/view.md",
        ]
        .map(str::to_owned)
        .to_vec();
        let rows = file_rows(&files, &spec(true, Some(&all), "VIEW"), &open_all);
        assert_eq!(
            show(&files, &rows),
            [
                "+crates/app/src/",
                "  *crates/app/src/view.rs",
                "+docs/",
                "  docs/view.md",
            ]
        );
    }

    #[test]
    fn rows_are_built_again_only_for_another_key() {
        let mut rows = FileRows::default();
        let key = |filter: &str| RowsKey {
            files: 1,
            all: None,
            tree: true,
            filter: filter.to_owned(),
            folders: 0,
        };
        let mut built = 0;
        rows.get(key(""), || {
            built += 1;
            Vec::new()
        });
        rows.get(key(""), || {
            built += 1;
            Vec::new()
        });
        rows.get(key("x"), || {
            built += 1;
            Vec::new()
        });
        assert_eq!(built, 2);
    }
}
