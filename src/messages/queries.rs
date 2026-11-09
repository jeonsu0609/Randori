//! Request/response types for read-only operations like log listing and
//! revision details.

use super::*;

/// A file-level change within a revision's diff.
#[derive(Serialize, Deserialize, Debug)]
pub struct RevChange {
    pub kind: ChangeKind,
    pub path: TreePath,
    pub has_conflict: bool,
    pub hunks: Vec<ChangeHunk>,
}

/// A file-level conflict in a revision's parent tree.
#[derive(Serialize, Deserialize, Debug)]
pub struct RevConflict {
    pub path: TreePath,
    pub hunk: ChangeHunk,
}

/// The type of modification made to a file in a diff.
#[derive(Serialize, Deserialize, Debug)]
pub enum ChangeKind {
    None,
    Added,
    Deleted,
    Modified,
}

/// Response to a revision detail query: either not found or full details.
#[derive(Serialize, Debug)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
pub enum RevsResult {
    NotFound {
        set: RevSet,
    },
    Detail {
        /// The revision set that was queried.
        set: RevSet,
        /// All revisions in the set, ordered ancestors-first.
        headers: Vec<RevHeader>,
        /// Parents of the oldest revision in the set.
        parents: Vec<RevHeader>,
        /// Combined changes: diff from oldest parent to newest revision.
        changes: Vec<RevChange>,
        /// Conflicts present in the parent tree of the oldest revision.
        conflicts: Vec<RevConflict>,
    },
}

/// (column, row) position of a node in the log graph.
#[derive(Serialize, Clone, Copy, Debug)]
pub struct LogCoordinates(pub usize, pub usize);

/// An edge segment in the log graph connecting two nodes.
#[derive(Serialize, Debug)]
#[serde(tag = "type")]
pub enum LogLine {
    FromNode {
        source: LogCoordinates,
        target: LogCoordinates,
        indirect: bool,
    },
    ToNode {
        source: LogCoordinates,
        target: LogCoordinates,
        indirect: bool,
    },
    ToIntersection {
        source: LogCoordinates,
        target: LogCoordinates,
        indirect: bool,
    },
    ToMissing {
        source: LogCoordinates,
        target: LogCoordinates,
        indirect: bool,
    },
}

/// A single row in the log graph with its revision and edges.
#[derive(Serialize, Debug)]
pub struct LogRow {
    pub revision: RevHeader,
    pub location: LogCoordinates,
    pub padding: usize,
    pub lines: Vec<LogLine>,
}

/// A paginated slice of the log graph.
#[derive(Serialize)]
pub struct LogPage {
    pub rows: Vec<LogRow>,
    pub has_more: bool,
}

/// One file as it is in one revision, for a full-file view.
#[derive(Serialize, Debug)]
pub struct FileContent {
    /// The text; empty when `binary` or `missing`. Conflicted files come with
    /// git-style conflict markers.
    pub text: String,
    pub binary: bool,
    /// The revision has no file at this path.
    pub missing: bool,
}

/// A file's lines grouped by the revision that last changed them (blame).
#[derive(Serialize, Debug)]
pub struct FileAnnotation {
    pub lines: Vec<String>,
    /// Consecutive lines from one revision, in file order.
    pub blocks: Vec<AnnotationBlock>,
    pub binary: bool,
}

#[derive(Serialize, Debug)]
pub struct AnnotationBlock {
    /// First line of the block, zero-based.
    pub start: usize,
    pub len: usize,
    pub header: RevHeader,
}
