//! Commit graph drawing.
//!
//! GG's worker already lays the graph out: every row has a (column, row)
//! location and every edge is a [`LogLine`] between two locations. GG draws the
//! whole graph as one SVG; Randori's log is a virtual list, so each row paints
//! only the slice of every edge that crosses it, clipped to the row bounds.
//!
//! Each branch keeps one colour from its tip down: a node takes the colour of
//! the child above it in the same lane, and a new lane (a branch tip) takes the
//! next colour. A lane change is drawn in the colour of the side branch, the
//! endpoint further right, so a merge or fork reads as that branch joining.
//!
//! Geometry is written in pixels at a 16px rem and scaled by the window's rem
//! at paint time, so lanes, nodes and the list row height grow together when
//! the interface zooms.

use gg_lib::messages::queries::{LogLine, LogRow};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, App, Bounds, ContentMask, Hsla, IntoElement as _, ParentElement as _, PathBuilder,
    Pixels, Point, Rems, SharedString, Styled as _, StyledImage as _, Window, canvas, div, img,
    point, prelude::FluentBuilder as _, px, rems,
};

/// The rem size the constants below are written for.
const BASE_REM: f32 = 16.;
/// Height of one log row at `BASE_REM`. The list uses [`row_height`].
const ROW_HEIGHT: f32 = 28.;
/// Width of one graph lane; wide enough for an author avatar.
const LANE_WIDTH: f32 = 20.;
/// Space before the first lane and after the last.
const LEADING: f32 = 6.;
/// Lanes the graph column fits by default; deeper lanes are clipped until
/// the column is dragged wider, so a long history of side branches does not
/// push the description off screen.
const MAX_LANES: usize = 6;
/// Author avatar on a node, including its lane-coloured ring.
const AVATAR: f32 = 18.;
const RING: f32 = 2.;
/// Merge commits are a small dot instead of an avatar.
const MERGE_RADIUS: f32 = 4.;
const WORKING_COPY_RADIUS: f32 = 6.;
const STROKE: f32 = 2.;

#[derive(Clone, Copy)]
enum EdgeKind {
    /// Child and parent in one lane, or a lane change after leaving the child.
    Direct { early_break: bool },
    /// The edge joins another lane at an intersection (merge into a lane).
    Merge,
}

#[derive(Clone, Copy)]
struct Edge {
    child: (usize, usize),
    parent: (usize, usize),
    kind: EdgeKind,
    indirect: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum NodeKind {
    WorkingCopy,
    Immutable,
    Conflict,
    Normal,
}

#[derive(Clone, Copy)]
struct Node {
    column: usize,
    kind: NodeKind,
    merge: bool,
}

/// Pre-computed graph for a loaded log, indexed by row.
#[derive(Default)]
pub struct Graph {
    edges: Vec<Edge>,
    /// Palette index of every edge.
    edge_colors: Vec<usize>,
    /// For each row, the edges whose vertical span crosses it.
    edges_by_row: Vec<Vec<usize>>,
    nodes: Vec<Node>,
    /// Palette index of every node: its branch colour.
    node_colors: Vec<usize>,
    max_column: usize,
}

impl Graph {
    pub fn new(rows: &[LogRow]) -> Self {
        let mut graph = Graph {
            nodes: rows
                .iter()
                .map(|row| Node {
                    column: row.location.0,
                    kind: if row.revision.is_working_copy {
                        NodeKind::WorkingCopy
                    } else if row.revision.has_conflict {
                        NodeKind::Conflict
                    } else if row.revision.is_immutable {
                        NodeKind::Immutable
                    } else {
                        NodeKind::Normal
                    },
                    merge: row.revision.parent_ids.len() > 1,
                })
                .collect(),
            edges_by_row: vec![Vec::new(); rows.len()],
            ..Default::default()
        };

        for row in rows {
            graph.max_column = graph.max_column.max(row.location.0);
            for line in &row.lines {
                let (source, target, kind, indirect) = match line {
                    LogLine::FromNode {
                        source,
                        target,
                        indirect,
                    } => (
                        source,
                        target,
                        EdgeKind::Direct { early_break: true },
                        *indirect,
                    ),
                    LogLine::ToNode {
                        source,
                        target,
                        indirect,
                    }
                    | LogLine::ToMissing {
                        source,
                        target,
                        indirect,
                    } => (
                        source,
                        target,
                        EdgeKind::Direct { early_break: false },
                        *indirect,
                    ),
                    LogLine::ToIntersection {
                        source,
                        target,
                        indirect,
                    } => (source, target, EdgeKind::Merge, *indirect),
                };
                graph.max_column = graph.max_column.max(source.0).max(target.0);
                let index = graph.edges.len();
                graph.edges.push(Edge {
                    child: (source.0, source.1),
                    parent: (target.0, target.1),
                    kind,
                    indirect,
                });
                let first = source.1.min(target.1);
                let last = source.1.max(target.1).min(rows.len().saturating_sub(1));
                for row_edges in graph.edges_by_row.iter_mut().take(last + 1).skip(first) {
                    row_edges.push(index);
                }
            }
        }

        let columns: Vec<usize> = graph.nodes.iter().map(|node| node.column).collect();
        let spans: Vec<_> = graph
            .edges
            .iter()
            .map(|edge| (edge.child, edge.parent))
            .collect();
        (graph.node_colors, graph.edge_colors) = branch_colors(&columns, &spans);
        graph
    }

    fn lanes(&self) -> usize {
        (self.max_column + 1).min(MAX_LANES)
    }

    /// Default width of the graph column: every lane up to [`MAX_LANES`].
    pub fn width(&self) -> Rems {
        rems((LEADING * 2. + LANE_WIDTH * self.lanes() as f32) / BASE_REM)
    }

    /// Branch colour of row `row`'s node.
    pub fn node_color(&self, row: usize, cx: &App) -> Hsla {
        let palette = palette(cx);
        palette[self.node_colors.get(row).copied().unwrap_or(0) % palette.len()]
    }

    /// Horizontal centre of row `row`'s node, from the graph's left edge.
    pub fn node_x(&self, row: usize) -> Rems {
        let column = self.nodes.get(row).map_or(0, |node| node.column);
        rems(lane_x(column) / BASE_REM)
    }

    /// Element that paints row `row` of the graph: the edges crossing it,
    /// a connector to the branch / tag column when `has_refs`, and the node
    /// (the author's avatar, `avatar_url` when known, initials otherwise).
    #[allow(clippy::too_many_arguments)]
    pub fn row_element(
        &self,
        row: usize,
        author: SharedString,
        avatar_url: Option<SharedString>,
        has_refs: bool,
        width: Rems,
        rem: Pixels,
        cx: &App,
    ) -> AnyElement {
        let theme = cx.theme();
        let palette = palette(cx);
        let edges: Vec<(Edge, Hsla)> = self
            .edges_by_row
            .get(row)
            .into_iter()
            .flatten()
            .map(|&ix| {
                let color = palette[self.edge_colors[ix] % palette.len()];
                (self.edges[ix], color)
            })
            .collect();
        // a commit with no author (the root) is a plain dot, like a merge
        let node = self.nodes.get(row).copied().map(|mut node| {
            node.merge |= author.is_empty();
            node
        });
        let lane = self.node_color(row, cx);
        let primary = theme.primary;
        let background = theme.background;
        let scale = f32::from(rem) / BASE_REM;

        let lines = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    let origin = point(
                        bounds.origin.x,
                        bounds.origin.y - px(ROW_HEIGHT * scale * row as f32),
                    );
                    for (edge, color) in &edges {
                        paint_edge(window, origin, scale, *edge, *color);
                    }
                    let Some(node) = node else {
                        return;
                    };
                    if has_refs {
                        // joins the chip in the branch / tag column to its node
                        let y = row_y(row);
                        let mut line = PathBuilder::stroke(px(1.5 * scale));
                        line.move_to(at(origin, scale, 0., y));
                        line.line_to(at(origin, scale, lane_x(node.column), y));
                        if let Ok(path) = line.build() {
                            window.paint_path(path, lane);
                        }
                    }
                    paint_dot(window, origin, scale, row, node, lane, primary, background);
                });
            },
        )
        .absolute()
        .size_full();

        let avatar = node
            .filter(|node| !node.merge && node.kind != NodeKind::WorkingCopy)
            .map(|node| {
                let ring = match node.kind {
                    NodeKind::Conflict => theme.danger,
                    _ => lane,
                };
                // Immutable history keeps the round shape with a quieter ring,
                // so every node is one shape whatever the picture is.
                let immutable = node.kind == NodeKind::Immutable;
                avatar(&author, avatar_url, px(AVATAR * scale), ring, immutable, cx)
                    .absolute()
                    .left(px((lane_x(node.column) - AVATAR / 2.) * scale))
                    .top(px((ROW_HEIGHT - AVATAR) / 2. * scale))
            });

        div()
            .relative()
            .flex_none()
            .w(width)
            .h(row_height())
            .overflow_hidden()
            .child(lines)
            .children(avatar)
            .into_any_element()
    }
}

/// An author's avatar, always round: their picture when `url` is known (and
/// loads), otherwise their initials on `ring`'s colour, inside a
/// `ring`-coloured border. `size` is the outer diameter; `quiet` dims the ring.
pub fn avatar(
    name: &str,
    url: Option<SharedString>,
    size: Pixels,
    ring: Hsla,
    quiet: bool,
    cx: &App,
) -> gpui_kit::Div {
    let theme = cx.theme();
    let initials = initials(name);
    let foreground = theme.foreground;
    let face = move || {
        div()
            .size_full()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(ring.opacity(0.28))
            .text_color(foreground)
            .text_size(size * 0.42)
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .child(initials.clone())
    };
    // The picture is clipped to a circle itself: a square image inside a
    // rounded box would show its corners past the ring.
    div()
        .flex_none()
        .size(size)
        .rounded_full()
        .overflow_hidden()
        .border(size * (RING / AVATAR))
        .border_color(if quiet { ring.opacity(0.5) } else { ring })
        .bg(theme.background)
        .map(|this| match url {
            // Pictures with transparency are drawn for GitHub's white page;
            // on a dark background a black line drawing would vanish.
            Some(url) => {
                let paper = if theme.is_dark() {
                    theme.foreground
                } else {
                    theme.background
                };
                let loading = face.clone();
                this.child(
                    div().size_full().rounded_full().bg(paper).child(
                        img(url)
                            .size_full()
                            .rounded_full()
                            .with_loading(move || loading().into_any_element())
                            .with_fallback(move || face().into_any_element()),
                    ),
                )
            }
            None => this.child(face()),
        })
}

/// Branch colours drawn from the theme, so both themes keep their contrast.
fn palette(cx: &App) -> [Hsla; 10] {
    let theme = cx.theme();
    [
        theme.blue,
        theme.magenta,
        theme.green,
        theme.yellow,
        theme.cyan,
        theme.chart_1,
        theme.chart_2,
        theme.chart_3,
        theme.chart_4,
        theme.chart_5,
    ]
}

/// A graph location: `(column, row)`.
type Location = (usize, usize);

/// Palette indices for nodes and edges.
///
/// `columns[row]` is the lane of the node in `row`; `edges` are
/// `(child, parent)` locations as `(column, row)`, the child above. A node
/// continues the colour of a child directly above it in its lane; otherwise
/// it starts a branch and takes the next colour. An edge that changes lane
/// takes the colour of its right-hand endpoint, the side branch.
fn branch_colors(columns: &[usize], edges: &[(Location, Location)]) -> (Vec<usize>, Vec<usize>) {
    // each node's children straight above it in its lane, in edge order:
    // looked up per row, as a scan of every edge per row grows with the
    // square of the rows loaded
    let mut above: Vec<Vec<usize>> = vec![Vec::new(); columns.len()];
    for (child, parent) in edges {
        if columns.get(parent.1) == Some(&parent.0) && child.0 == parent.0 && child.1 < parent.1 {
            above[parent.1].push(child.1);
        }
    }
    let mut node_colors: Vec<Option<usize>> = vec![None; columns.len()];
    let mut next = 0;
    for row in 0..columns.len() {
        let inherited = above[row]
            .iter()
            .filter_map(|&child| node_colors.get(child).copied().flatten())
            .next();
        node_colors[row] = Some(inherited.unwrap_or_else(|| {
            let color = next;
            next += 1;
            color
        }));
    }
    let node_colors: Vec<usize> = node_colors
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect();
    let color_at =
        |(column, row): Location| (columns.get(row) == Some(&column)).then(|| node_colors[row]);
    let edge_colors = edges
        .iter()
        .map(|&(child, parent)| {
            let child_color = color_at(child).unwrap_or_default();
            if parent.0 > child.0 {
                color_at(parent).unwrap_or(child_color)
            } else {
                child_color
            }
        })
        .collect();
    (node_colors, edge_colors)
}

/// Up to two initials, from the first letter or digit of each word:
/// "Thomas Castiglione" → "TC", "jeonsu0609" → "J", "Van (전수영)" → "V전".
fn initials(name: &str) -> SharedString {
    let letters: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if letters.is_empty() {
        "?".into()
    } else {
        letters.into()
    }
}

/// Height of one log row; list items must use it so the graph lines up.
pub fn row_height() -> Rems {
    rems(ROW_HEIGHT / BASE_REM)
}

fn lane_x(column: usize) -> f32 {
    LEADING + LANE_WIDTH * column as f32 + LANE_WIDTH / 2.
}

fn row_y(row: usize) -> f32 {
    ROW_HEIGHT * row as f32 + ROW_HEIGHT / 2.
}

/// A point in base-rem geometry, scaled to the window and offset by `origin`.
fn at(origin: Point<Pixels>, scale: f32, x: f32, y: f32) -> Point<Pixels> {
    point(origin.x + px(x * scale), origin.y + px(y * scale))
}

fn paint_edge(window: &mut Window, origin: Point<Pixels>, scale: f32, edge: Edge, color: Hsla) {
    let (c1, r1) = edge.child;
    let (c2, r2) = edge.parent;
    let x1 = lane_x(c1);
    let x2 = lane_x(c2);
    let y1 = row_y(r1);
    let y2 = row_y(r2);

    let mut builder = PathBuilder::stroke(px(STROKE * scale));
    if edge.indirect {
        builder = builder.dash_array(&[px(2. * scale), px(3. * scale)]);
    }
    builder.move_to(at(origin, scale, x1, y1));

    if c1 == c2 {
        builder.line_to(at(origin, scale, x2, y2));
    } else {
        // Change lanes over one row, at the child's row or just above the
        // parent like GG, as an S-curve between the two row centres.
        let mid = match edge.kind {
            EdgeKind::Merge if c2 > c1 => y1 + ROW_HEIGHT / 2.,
            EdgeKind::Merge => y2 - ROW_HEIGHT / 2.,
            EdgeKind::Direct { early_break } if early_break && c1 > c2 => y2 - ROW_HEIGHT / 2.,
            EdgeKind::Direct { .. } => y1 + ROW_HEIGHT / 2.,
        };
        let top = mid - ROW_HEIGHT / 2.;
        let bottom = mid + ROW_HEIGHT / 2.;
        builder.line_to(at(origin, scale, x1, top));
        builder.cubic_bezier_to(
            at(origin, scale, x2, bottom),
            at(origin, scale, x1, mid),
            at(origin, scale, x2, mid),
        );
        builder.line_to(at(origin, scale, x2, y2));
    }

    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// Nodes that are not an avatar: the working copy and merge commits.
#[allow(clippy::too_many_arguments)]
fn paint_dot(
    window: &mut Window,
    origin: Point<Pixels>,
    scale: f32,
    row: usize,
    node: Node,
    lane: Hsla,
    primary: Hsla,
    background: Hsla,
) {
    let cx = lane_x(node.column);
    let cy = row_y(row);
    let circle = |radius: f32| Bounds {
        origin: at(origin, scale, cx - radius, cy - radius),
        size: gpui_kit::size(px(radius * 2. * scale), px(radius * 2. * scale)),
    };

    if node.kind == NodeKind::WorkingCopy {
        // Hollow ring with a halo: the change you are editing.
        let r = WORKING_COPY_RADIUS;
        window.paint_quad(
            gpui_kit::fill(circle(r + 4.), primary.opacity(0.18))
                .corner_radii(px((r + 4.) * scale)),
        );
        window.paint_quad(
            gpui_kit::fill(circle(r), background)
                .corner_radii(px(r * scale))
                .border_widths(px(2. * scale))
                .border_color(primary),
        );
    } else if node.merge {
        window.paint_quad(
            gpui_kit::fill(circle(MERGE_RADIUS), lane).corner_radii(px(MERGE_RADIUS * scale)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lane_keeps_its_colour_down_the_branch() {
        // c → b → a in lane 0
        let (nodes, edges) = branch_colors(&[0, 0, 0], &[((0, 0), (0, 1)), ((0, 1), (0, 2))]);
        assert_eq!(nodes, vec![0, 0, 0]);
        assert_eq!(edges, vec![0, 0]);
    }

    #[test]
    fn a_side_branch_gets_its_own_colour_and_its_fork_line() {
        // row 0: main tip (lane 0), row 1: feature tip (lane 1), row 2: fork point (lane 0)
        let edges = [((0, 0), (0, 2)), ((1, 1), (0, 2))];
        let (nodes, edge_colors) = branch_colors(&[0, 1, 0], &edges);
        assert_eq!(nodes, vec![0, 1, 0]);
        // the fork line is the feature's colour, not main's
        assert_eq!(edge_colors, vec![0, 1]);
    }

    #[test]
    fn a_merge_line_takes_the_merged_branch_colour() {
        // row 0: merge (lane 0) with parents in lane 0 (row 2) and lane 1 (row 1)
        let edges = [((0, 0), (0, 2)), ((0, 0), (1, 1))];
        let (nodes, edge_colors) = branch_colors(&[0, 1, 0], &edges);
        assert_eq!(nodes, vec![0, 1, 0]);
        assert_eq!(edge_colors, vec![0, 1]);
    }

    #[test]
    fn initials_skip_punctuation_words() {
        assert_eq!(initials("Thomas Castiglione").as_ref(), "TC");
        assert_eq!(initials("jeonsu0609").as_ref(), "J");
        assert_eq!(initials("Van (전수영)").as_ref(), "V전");
        assert_eq!(initials("").as_ref(), "?");
    }
}
