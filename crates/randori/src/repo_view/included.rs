//! What the next commit takes of the working copy, included in it in the
//! inspector as GitHub Desktop includes changes: files whole, or some of
//! their changed lines. jj has no index, so nothing is written until the
//! commit: what is included is the tab's, and the commit hands it to
//! `CommitChanges` (`jj commit -i`).

use std::collections::BTreeMap;

use gg_lib::messages::mutations::FileHunks;

use super::lines::changed_lines;

use super::*;

/// What a commit or Split off says when the lines included in it are no
/// longer there.
pub(super) const NOTHING_LEFT: &str =
    "The changes included are no longer in the working copy. Include them again.";

/// A changed line: its side and number in the diff.
pub(super) type LineKey = (Side, usize);

/// The files and lines included, of one working copy.
#[derive(Default)]
pub(super) struct Included {
    /// The working copy (change id) they are of: another one starts empty.
    change: Option<String>,
    /// By repository path.
    files: BTreeMap<String, FileInclusion>,
}

enum FileInclusion {
    /// Every change of the file, as it is when committed.
    Whole,
    /// Some changed lines, each with its patch line (`+text\n`): a line
    /// whose text is another when the file is read again is not included.
    Lines(BTreeMap<LineKey, String>),
}

/// How much of a file is included.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Inclusion {
    Nothing,
    Some,
    All,
}

impl Included {
    /// Keeps what is included while the working copy is `change`; another
    /// one (the commit made a new working copy) starts with nothing.
    pub(super) fn follow(&mut self, change: &str) {
        if self.change.as_deref() != Some(change) {
            self.files.clear();
            self.change = Some(change.to_owned());
        }
    }

    pub(super) fn clear(&mut self) {
        self.files.clear();
    }

    pub(super) fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// How much of the file at `path`, changed by `hunks`, is included.
    pub(super) fn file(&self, path: &str, hunks: &[ChangeHunk]) -> Inclusion {
        match self.files.get(path) {
            None => Inclusion::Nothing,
            Some(FileInclusion::Whole) => Inclusion::All,
            // each changed line in, as it is now: a line ticked in a diff
            // read before the file changed does not count
            Some(FileInclusion::Lines(lines)) => {
                if changed_lines(hunks).all(|(key, line)| lines.get(&key) == Some(line)) {
                    Inclusion::All
                } else {
                    Inclusion::Some
                }
            }
        }
    }

    /// How much of every file of `files` is included.
    pub(super) fn all_files(&self, files: &[FileItem]) -> Inclusion {
        let mut states = files
            .iter()
            .map(|file| self.file(&file.tree_path.repo_path, &file.hunks));
        match states.next() {
            None => Inclusion::Nothing,
            Some(first) if states.all(|state| state == first) => first,
            Some(_) => Inclusion::Some,
        }
    }

    /// Whether changed line `key` of the file at `path` is included.
    pub(super) fn line(&self, path: &str, key: LineKey) -> bool {
        match self.files.get(path) {
            None => false,
            Some(FileInclusion::Whole) => true,
            Some(FileInclusion::Lines(lines)) => lines.contains_key(&key),
        }
    }

    /// Includes the file at `path` whole, or nothing of it.
    pub(super) fn set_file(&mut self, path: &str, included: bool) {
        if included {
            self.files.insert(path.to_owned(), FileInclusion::Whole);
        } else {
            self.files.remove(path);
        }
    }

    /// Includes every file of `files`, or nothing.
    pub(super) fn set_all(&mut self, files: &[FileItem], included: bool) {
        self.files.clear();
        if included {
            for file in files {
                self.set_file(&file.tree_path.repo_path, true);
            }
        }
    }

    /// Includes the changed lines `keys` of the file at `path`, changed by
    /// `hunks`, or leaves them out.
    pub(super) fn set_lines(
        &mut self,
        path: &str,
        hunks: &[ChangeHunk],
        keys: &[LineKey],
        included: bool,
    ) {
        let changed: BTreeMap<LineKey, String> = changed_lines(hunks)
            .map(|(key, line)| (key, line.clone()))
            .collect();
        // a file included whole is each of its lines, to leave some out
        let mut lines = match self.files.remove(path) {
            Some(FileInclusion::Whole) => changed.clone(),
            Some(FileInclusion::Lines(lines)) => lines,
            None => BTreeMap::new(),
        };
        for key in keys {
            match changed.get(key) {
                Some(line) if included => {
                    lines.insert(*key, line.clone());
                }
                _ => {
                    lines.remove(key);
                }
            }
        }
        if !lines.is_empty() {
            self.files
                .insert(path.to_owned(), FileInclusion::Lines(lines));
        }
    }

    /// After the working copy is read again: forgets files no longer
    /// changed, and included lines no longer there as they were. Whether
    /// included lines were forgotten, which the user is told.
    pub(super) fn keep_current(&mut self, files: &[FileItem]) -> bool {
        let mut lines_lost = false;
        self.files.retain(|path, inclusion| {
            let Some(file) = files.iter().find(|file| &file.tree_path.repo_path == path) else {
                lines_lost |= matches!(inclusion, FileInclusion::Lines(_));
                return false;
            };
            let FileInclusion::Lines(lines) = inclusion else {
                return true;
            };
            let current: BTreeMap<LineKey, &String> = changed_lines(&file.hunks).collect();
            let before = lines.len();
            lines.retain(|key, line| current.get(key) == Some(&&*line));
            lines_lost |= lines.len() != before;
            !lines.is_empty()
        });
        lines_lost
    }

    /// The commit's changes (see `CommitChanges`): files included whole, or
    /// with every line included, as paths; others as hunks of their lines.
    pub(super) fn changes(&self, files: &[FileItem]) -> (Vec<TreePath>, Vec<FileHunks>) {
        let (mut paths, mut hunks) = (Vec::new(), Vec::new());
        for file in files {
            let lines = match self.files.get(&file.tree_path.repo_path) {
                None => continue,
                Some(FileInclusion::Whole) => {
                    paths.push(file.tree_path.clone());
                    continue;
                }
                Some(FileInclusion::Lines(lines)) => lines,
            };
            if self.file(&file.tree_path.repo_path, &file.hunks) == Inclusion::All {
                paths.push(file.tree_path.clone());
                continue;
            }
            let included: Vec<ChangeHunk> = file
                .hunks
                .iter()
                .filter_map(|hunk| {
                    let selected: Vec<bool> = {
                        let mut keys = changed_lines(std::slice::from_ref(hunk));
                        hunk.lines
                            .lines
                            .iter()
                            .map(|line| match line.as_bytes().first() {
                                Some(b'-' | b'+') => keys
                                    .next()
                                    .is_some_and(|(key, _)| lines.get(&key) == Some(line)),
                                _ => false,
                            })
                            .collect()
                    };
                    partial_hunk(hunk, &selected)
                })
                .collect();
            if !included.is_empty() {
                hunks.push(FileHunks {
                    path: file.tree_path.clone(),
                    hunks: included,
                });
            }
        }
        (paths, hunks)
    }

    /// What is included, for the commit button: "a.txt", "3 files", "2
    /// lines of a.txt".
    pub(super) fn summary(&self, files: &[FileItem]) -> String {
        let included: Vec<&FileItem> = files
            .iter()
            .filter(|file| self.file(&file.tree_path.repo_path, &file.hunks) != Inclusion::Nothing)
            .collect();
        match included.as_slice() {
            [] => String::new(),
            [file] => match self.files.get(&file.tree_path.repo_path) {
                Some(FileInclusion::Lines(lines))
                    if self.file(&file.tree_path.repo_path, &file.hunks) == Inclusion::Some =>
                {
                    let n = lines.len();
                    format!("{n} line{} of {}", if n == 1 { "" } else { "s" }, file.name)
                }
                _ => file.name.to_string(),
            },
            files => format!("{} files", files.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_lib::messages::{ChangeLocation, ChangeRange, MultilineString};

    /// f.txt changed from `a b c` to `a B c d`.
    fn file() -> FileItem {
        FileItem {
            tree_path: TreePath {
                repo_path: "f.txt".to_owned(),
                relative_path: "f.txt".into(),
            },
            hunks: vec![ChangeHunk {
                location: ChangeLocation {
                    from_file: ChangeRange { start: 1, len: 3 },
                    to_file: ChangeRange { start: 1, len: 4 },
                },
                lines: MultilineString {
                    lines: ["a", "-b", "+B", "c", "+d"]
                        .map(|line| {
                            if line.starts_with(['-', '+']) {
                                format!("{line}\n")
                            } else {
                                format!(" {line}\n")
                            }
                        })
                        .to_vec(),
                },
            }],
            path: "f.txt".into(),
            name: "f.txt".into(),
            dir: "".into(),
            kind: ChangeKind::Modified,
            additions: 2,
            deletions: 1,
            has_conflict: false,
        }
    }

    #[test]
    fn a_file_is_included_whole_or_line_by_line() {
        let file = file();
        let (path, hunks) = ("f.txt", &file.hunks[..]);
        let mut inclusion = Included::default();
        assert_eq!(inclusion.file(path, hunks), Inclusion::Nothing);

        // `+d` only: the hunk keeps `-b` as context and drops `+B`
        inclusion.set_lines(path, hunks, &[(Side::Modified, 4)], true);
        assert_eq!(inclusion.file(path, hunks), Inclusion::Some);
        assert!(inclusion.line(path, (Side::Modified, 4)));
        assert!(!inclusion.line(path, (Side::Modified, 2)));
        let (paths, picked) = inclusion.changes(std::slice::from_ref(&file));
        assert!(paths.is_empty());
        assert_eq!(
            picked[0].hunks[0].lines.lines,
            vec![" a\n", " b\n", " c\n", "+d\n"]
        );
        assert_eq!(
            inclusion.summary(std::slice::from_ref(&file)),
            "1 line of f.txt"
        );

        // every line: the file goes whole
        inclusion.set_lines(
            path,
            hunks,
            &[(Side::Original, 2), (Side::Modified, 2)],
            true,
        );
        assert_eq!(inclusion.file(path, hunks), Inclusion::All);
        let (paths, picked) = inclusion.changes(std::slice::from_ref(&file));
        assert_eq!(paths.len(), 1);
        assert!(picked.is_empty());

        // a line left out of a file included whole
        inclusion.set_file(path, true);
        inclusion.set_lines(path, hunks, &[(Side::Modified, 4)], false);
        assert_eq!(inclusion.file(path, hunks), Inclusion::Some);
        assert!(inclusion.line(path, (Side::Original, 2)));
    }

    #[test]
    fn lines_no_longer_there_are_forgotten() {
        let mut file = file();
        let mut inclusion = Included::default();
        inclusion.follow("change");
        inclusion.set_lines("f.txt", &file.hunks, &[(Side::Modified, 4)], true);
        inclusion.set_lines("f.txt", &file.hunks, &[(Side::Modified, 2)], true);

        // the working copy read again with `+d` now `+e`: that line goes
        file.hunks[0].lines.lines[4] = "+e\n".to_owned();
        assert!(inclusion.keep_current(std::slice::from_ref(&file)));
        assert!(!inclusion.line("f.txt", (Side::Modified, 4)));
        assert!(inclusion.line("f.txt", (Side::Modified, 2)));
        assert!(!inclusion.keep_current(std::slice::from_ref(&file)));

        // the file no longer changed: nothing of it is included
        assert!(inclusion.keep_current(&[]));
        assert!(inclusion.is_empty());

        // another working copy starts with nothing included
        inclusion.set_file("f.txt", true);
        inclusion.follow("change");
        assert!(!inclusion.is_empty());
        inclusion.follow("next");
        assert!(inclusion.is_empty());
    }

    #[test]
    fn lines_ticked_in_an_older_diff_never_take_the_whole_file() {
        let file = file();
        // the full view's diff, read when `+d` was still `+x`
        let mut older = file.hunks.clone();
        older[0].lines.lines[4] = "+x\n".to_owned();
        let mut inclusion = Included::default();
        let every = changed_line_positions(&older);
        inclusion.set_lines("f.txt", &older, &every, true);

        // every place ticked, but not every line as it is now
        assert_eq!(inclusion.file("f.txt", &file.hunks), Inclusion::Some);
        let (paths, picked) = inclusion.changes(std::slice::from_ref(&file));
        assert!(paths.is_empty());
        assert_eq!(
            picked[0].hunks[0].lines.lines,
            vec![" a\n", "-b\n", "+B\n", " c\n"]
        );
    }
}
