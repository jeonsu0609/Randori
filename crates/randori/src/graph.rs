//! Commit graph drawing.
//!
//! GG's worker already lays the graph out: every row has a (column, row)
//! location and every edge is a [`LogLine`] between two locations. GG draws the
//! whole graph as one SVG; Randori's log is a virtual list, so each row paints
//! only the slice of every edge that crosses it, clipped to the row bounds.
//!
//! Geometry is written in pixels at a 16px rem and scaled by the window's rem
//! at paint time, so lanes, nodes and the list row height grow together when
//! the interface zooms.

use gg_lib::messages::queries::{LogLine, LogRow};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, Bounds, ContentMask, Hsla, PathBuilder, Pixels, Point, Rems, Styled as _, Window, canvas,
    point, px, rems,
};

/// The rem size the constants below are written for.
const BASE_REM: f32 = 16.;
/// Height of one log row at `BASE_REM`. The list uses [`row_height`].
const ROW_HEIGHT: f32 = 34.;
/// Width of one graph lane.
const LANE_WIDTH: f32 = 16.;
/// Space before the first lane.
const LEADING: f32 = 10.;
const NODE_RADIUS: f32 = 4.5;
const STROKE: f32 = 1.75;
const ELBOW: f32 = 6.;

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

#[derive(Clone, Copy)]
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
}

/// Pre-computed graph for a loaded log, indexed by row.
#[derive(Default)]
pub struct Graph {
    edges: Vec<Edge>,
    /// For each row, the edges whose vertical span crosses it.
    edges_by_row: Vec<Vec<usize>>,
    nodes: Vec<Node>,
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
        graph
    }

    /// Width the graph column needs for the widest lane.
    pub fn width(&self) -> Rems {
        rems((LEADING * 2. + LANE_WIDTH * (self.max_column + 1) as f32) / BASE_REM)
    }

    /// Element that paints row `row` of the graph.
    pub fn row_canvas(&self, row: usize, cx: &App) -> impl gpui_kit::IntoElement + use<> {
        let theme = cx.theme();
        let lanes = [
            theme.chart_1,
            theme.chart_2,
            theme.chart_3,
            theme.chart_4,
            theme.chart_5,
        ];
        let edges: Vec<(Edge, Hsla)> = self
            .edges_by_row
            .get(row)
            .into_iter()
            .flatten()
            .map(|&ix| {
                let edge = self.edges[ix];
                // Colour an edge by the lane it lands in, so a branch keeps one colour.
                (edge, lanes[edge.parent.0 % lanes.len()])
            })
            .collect();
        let node = self.nodes.get(row).copied();
        let node_color = node.map(|node| lanes[node.column % lanes.len()]);
        let primary = theme.primary;
        let danger = theme.danger;
        let background = theme.background;
        let muted = theme.muted_foreground;
        let width = self.width();

        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let scale = f32::from(window.rem_size()) / BASE_REM;
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    let origin = point(
                        bounds.origin.x,
                        bounds.origin.y - px(ROW_HEIGHT * scale * row as f32),
                    );
                    for (edge, color) in &edges {
                        paint_edge(window, origin, scale, *edge, *color);
                    }
                    if let (Some(node), Some(color)) = (node, node_color) {
                        paint_node(
                            window, origin, scale, row, node, color, primary, danger, background,
                            muted,
                        );
                    }
                });
            },
        )
        .w(width)
        .h(row_height())
        .flex_none()
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
        // Turn once, at the child's row or just above the parent, like GG.
        let mid = match edge.kind {
            EdgeKind::Merge if c2 > c1 => y1 + ROW_HEIGHT / 2.,
            EdgeKind::Merge => y2 - ROW_HEIGHT / 2.,
            EdgeKind::Direct { early_break } if early_break && c1 > c2 => y2 - ROW_HEIGHT / 2.,
            EdgeKind::Direct { .. } => y1 + ROW_HEIGHT / 2.,
        };
        let dir = if x2 > x1 { 1. } else { -1. };
        let r = ELBOW.min((x2 - x1).abs() / 2.);
        builder.line_to(at(origin, scale, x1, mid - r));
        builder.curve_to(
            at(origin, scale, x1 + dir * r, mid),
            at(origin, scale, x1, mid),
        );
        builder.line_to(at(origin, scale, x2 - dir * r, mid));
        builder.curve_to(at(origin, scale, x2, mid + r), at(origin, scale, x2, mid));
        builder.line_to(at(origin, scale, x2, y2));
    }

    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_node(
    window: &mut Window,
    origin: Point<Pixels>,
    scale: f32,
    row: usize,
    node: Node,
    lane: Hsla,
    primary: Hsla,
    danger: Hsla,
    background: Hsla,
    muted: Hsla,
) {
    let cx = lane_x(node.column);
    let cy = row_y(row);
    let circle = |radius: f32| Bounds {
        origin: at(origin, scale, cx - radius, cy - radius),
        size: gpui_kit::size(px(radius * 2. * scale), px(radius * 2. * scale)),
    };

    match node.kind {
        NodeKind::WorkingCopy => {
            // Hollow ring with a halo: the change you are editing.
            window.paint_quad(
                gpui_kit::fill(circle(NODE_RADIUS + 4.), primary.opacity(0.18))
                    .corner_radii(px((NODE_RADIUS + 4.) * scale)),
            );
            window.paint_quad(
                gpui_kit::fill(circle(NODE_RADIUS + 1.), background)
                    .corner_radii(px((NODE_RADIUS + 1.) * scale))
                    .border_widths(px(2. * scale))
                    .border_color(primary),
            );
        }
        NodeKind::Immutable => {
            window.paint_quad(
                gpui_kit::fill(circle(NODE_RADIUS), background)
                    .corner_radii(px(1.5 * scale))
                    .border_widths(px(1.5 * scale))
                    .border_color(muted),
            );
        }
        NodeKind::Conflict => {
            window.paint_quad(
                gpui_kit::fill(circle(NODE_RADIUS), danger).corner_radii(px(NODE_RADIUS * scale)),
            );
        }
        NodeKind::Normal => {
            window.paint_quad(
                gpui_kit::fill(circle(NODE_RADIUS), lane).corner_radii(px(NODE_RADIUS * scale)),
            );
        }
    }
}
