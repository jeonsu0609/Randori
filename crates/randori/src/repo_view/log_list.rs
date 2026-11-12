//! The revision list: column header, graph rows and the virtual list.

use gpui_kit::component::Icon;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::{Rems, WeakEntity, canvas, deferred, px};

use super::*;

/// The resizable columns of the revision list. The description takes the
/// width the others leave.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Column {
    /// Branch / tag chips, left of the graph like GitKraken's.
    Refs,
    Graph,
    Change,
    Author,
}

impl Column {
    /// Narrowest width in rems, so a column can be found again.
    fn min(self) -> f32 {
        match self {
            Column::Refs | Column::Graph => 2.,
            Column::Change => 4.,
            Column::Author => 3.,
        }
    }

    /// Columns left of the description grow with a drag to the right; the
    /// ones right of it grow with a drag to the left.
    fn grows_right(self) -> bool {
        matches!(self, Column::Refs | Column::Graph | Column::Change)
    }
}

/// Widths of the header and every row, with the graph's automatic width resolved.
#[derive(Clone, Copy)]
pub(super) struct Columns {
    pub(super) refs: Rems,
    pub(super) graph: Rems,
    pub(super) change: Rems,
    pub(super) author: Rems,
    /// Too narrow for names: the author column shows avatars only.
    pub(super) compact_author: bool,
    /// The optional columns Settings shows; a hidden one is 0 wide.
    pub(super) show_refs: bool,
    pub(super) show_change: bool,
    pub(super) show_author: bool,
}

/// The narrowest description before the author column gives way.
const MIN_DESCRIPTION: f32 = 11.;
/// The author column with avatars only.
const COMPACT_AUTHOR: f32 = 1.75;
/// Row padding and the gaps between the columns, in rems.
const ROW_CHROME: f32 = 3.5;

/// Whether the author column shows avatars only: when `width` rems leave
/// the description less than its least beside the names. An unknown width
/// (0) keeps the names.
pub(super) fn avatars_only(width: f32, fixed: f32, author: f32) -> bool {
    width > 0. && width - ROW_CHROME - fixed - MIN_DESCRIPTION < author
}

/// What a column handle drags; which column is in `RepoView::column_drag`.
pub(super) struct ColumnResize;

/// Nothing follows the pointer while a column is resized.
struct ResizeGhost;

impl Render for ResizeGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

// ----- rows ------------------------------------------------------------------------------------

/// How many rows before the end of the loaded ones the next page is asked
/// for, so scrolling rarely reaches the end before it comes.
const PRELOAD_ROWS: usize = 200;

/// One log row. `rem` is the window's rem, which the graph's avatars are
/// sized by; `avatar` is the author's picture when one is known.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_row(
    row: &RowData,
    graph: &Graph,
    ix: usize,
    selected: bool,
    avatar: Option<SharedString>,
    rem: gpui_kit::Pixels,
    cols: Columns,
    refs_open: bool,
    owner: &WeakEntity<RepoView>,
    dim: bool,
    cx: &App,
) -> ListItem {
    let theme = cx.theme();
    let color = graph.node_color(ix, cx);
    ListItem::new(("revision", ix))
        .selected(selected)
        .h(row_height())
        .pl_3()
        .pr_3()
        .py_0()
        .w_full()
        // a find leaves the rows it does not match dimmed
        .when(dim, |row| row.opacity(0.35))
        .child(
            h_flex()
                .relative()
                .h_full()
                .gap_2()
                .min_w_0()
                .flex_1()
                .text_sm()
                // the row's branch colour, from its node to the end of the row
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        // a node clipped by a narrow graph starts the tint at the edge
                        .left(cols.refs + gpui_kit::rems(graph.node_x(ix).0.min(cols.graph.0)))
                        .bg(color.opacity(0.07)),
                )
                // no gap between the two, so the chip's line runs on to its node
                .child(
                    h_flex()
                        .flex_none()
                        .h_full()
                        .when(cols.show_refs, |this| {
                            this.child(render_ref_column(
                                row, ix, color, cols.refs, refs_open, owner, cx,
                            ))
                        })
                        .child(graph.row_element(
                            ix,
                            row.author.clone(),
                            avatar.clone(),
                            !row.refs.is_empty(),
                            cols.graph,
                            rem,
                            cx,
                        )),
                )
                .when(cols.show_change, |this| {
                    this.child(
                        h_flex()
                            .w(cols.change)
                            .flex_none()
                            .font_family(theme.mono_font_family.clone())
                            .child(
                                div()
                                    .font_semibold()
                                    .text_color(theme.primary)
                                    .child(row.change_prefix.clone()),
                            )
                            .child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .child(row.change_rest.clone()),
                            ),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(match &row.summary {
                            Some(summary) => h_flex()
                                .min_w_0()
                                .gap_2()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .when(row.is_working_copy, |this| this.font_medium())
                                        .child(summary.clone()),
                                )
                                // the rest of the description, in what room is left
                                .when_some(row.body.clone(), |this, body| {
                                    this.child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(theme.muted_foreground)
                                            .child(body),
                                    )
                                }),
                            None => div()
                                .min_w_0()
                                .truncate()
                                .italic()
                                .text_color(theme.muted_foreground)
                                .child("(no description set)"),
                        }),
                )
                .when(cols.show_author, |this| {
                    this.child(if cols.compact_author {
                        div()
                            .w(cols.author)
                            .flex_none()
                            .flex()
                            .justify_center()
                            .child(crate::graph::avatar(
                                &row.author,
                                avatar,
                                rem * 1.25,
                                theme.border,
                                false,
                                cx,
                            ))
                    } else {
                        div()
                            .w(cols.author)
                            .flex_none()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(row.author.clone())
                    })
                })
                // where an age group starts, a hairline across the row's top
                // that stops short of the age, in small text on the line: on
                // the line, it is clear of the row's text below and above
                // (row 0 has no line above, so its age sits at its top)
                .when(row.bucket_start, |this| {
                    this.child(
                        h_flex()
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(px(if ix == 0 { 0. } else { -7. }))
                            .h(px(14.))
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(1.))
                                    .when(ix > 0, |line| line.bg(theme.border)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(11.))
                                    .whitespace_nowrap()
                                    .text_color(theme.muted_foreground)
                                    .child(row.bucket_label.clone()),
                            ),
                    )
                }),
        )
}

/// The branch / tag column: the row's first ref as a chip in its branch
/// colour, `+N` for the rest, and a line on to the node. With more than one
/// ref, hovering the chip spreads them all out over the rows below, as
/// GitKraken does; a single ref's full name is in the tooltip.
fn render_ref_column(
    row: &RowData,
    ix: usize,
    color: gpui_kit::Hsla,
    width: Rems,
    open: bool,
    owner: &WeakEntity<RepoView>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let Some(first) = row.refs.first() else {
        return div().w(width).flex_none().into_any_element();
    };
    let mark_unpushed = crate::settings::get(cx).mark_unpushed;
    let several = row.refs.len() > 1;
    h_flex()
        .id(("refs", ix))
        .relative()
        .w(width)
        .flex_none()
        .h_full()
        .items_center()
        .map(|this| {
            if several {
                let owner = owner.clone();
                this.on_hover(move |hovered, _, cx| {
                    let _ = owner.update(cx, |view, cx| view.hover_refs(ix, false, *hovered, cx));
                })
            } else {
                let name: SharedString = if mark_unpushed && first.unpushed {
                    format!("{} (not pushed)", first.name).into()
                } else {
                    first.name.clone()
                };
                this.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(name.clone()).build(window, cx)
                })
            }
        })
        // a double click on the chip itself, as on GitKraken's branch label
        .child(
            ref_chip(first, color, mark_unpushed, cx)
                .id("ref")
                .flex_shrink(1.)
                .on_click(switch_on_double_click(first, &row.id, owner)),
        )
        .when(several, |this| {
            this.child(
                div()
                    .flex_none()
                    .ml_1()
                    .px_1()
                    .rounded(theme.radius)
                    .bg(color.opacity(0.16))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("+{}", row.refs.len() - 1)),
            )
        })
        .child(div().flex_1().min_w_1().h(gpui_kit::px(1.5)).bg(color))
        .when(several && open, |this| {
            this.child(spread_refs(row, ix, color, width, mark_unpushed, owner, cx))
        })
        .into_any_element()
}

/// A row's refs, all of them, one under another from where its chip is.
/// Deferred, so it is drawn over the rows below rather than under them.
fn spread_refs(
    row: &RowData,
    ix: usize,
    color: gpui_kit::Hsla,
    width: Rems,
    mark_unpushed: bool,
    owner: &WeakEntity<RepoView>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    // the first chip where the row's own is: both are centred in the row
    let inset = gpui_kit::rems((row_height().0 - CHIP_HEIGHT) / 2.);
    let pad = gpui_kit::rems(0.125);
    let hover_owner = owner.clone();
    let click_owner = owner.clone();
    deferred(
        div()
            .absolute()
            .top(inset - pad)
            .left(-pad)
            // short of the column, so the line on to the node still shows
            .w(width - gpui_kit::rems(0.75))
            .child(
                v_flex()
                    .id(("refs-spread", ix))
                    .occlude()
                    .p(pad)
                    .gap_0p5()
                    .rounded(theme.radius)
                    .bg(theme.background)
                    .shadow_md()
                    .on_hover(move |hovered, _, cx| {
                        let _ = hover_owner
                            .update(cx, |view, cx| view.hover_refs(ix, true, *hovered, cx));
                    })
                    .on_click(move |_, _, cx| {
                        let _ = click_owner.update(cx, |view, cx| view.select(ix, cx));
                    })
                    .children(row.refs.iter().enumerate().map(|(n, chip)| {
                        ref_chip(chip, color, mark_unpushed, cx)
                            .id(("spread-ref", n))
                            .w_full()
                            .on_click(switch_on_double_click(chip, &row.id, owner))
                    })),
            ),
    )
}

/// Switches to `chip` on a double click, as GitKraken checks a branch out
/// (see [`RepoView::switch_to`]); `id` is its revision.
fn switch_on_double_click(
    chip: &RefChip,
    id: &RevId,
    owner: &WeakEntity<RepoView>,
) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    let (name, local, id, owner) = (
        chip.name.clone(),
        chip.kind == RefKind::Local,
        id.clone(),
        owner.clone(),
    );
    move |event, window, cx| {
        if event.click_count() >= 2 {
            let _ = owner.update(cx, |view, cx| {
                view.switch_to(name.clone(), local, id.clone(), window, cx)
            });
        }
    }
}

/// The height of a ref chip, in rems (`h_5`).
const CHIP_HEIGHT: f32 = 1.25;

/// One ref as a chip in its row's branch colour: a check on the bookmark the
/// user is on, the kind's icon, the name, and a mark when it is not pushed.
fn ref_chip(chip: &RefChip, color: gpui_kit::Hsla, mark_unpushed: bool, cx: &App) -> gpui_kit::Div {
    let theme = cx.theme();
    let icon = match chip.kind {
        RefKind::Local => IconName::GitBranch,
        RefKind::Remote => IconName::CloudDownload,
        RefKind::Tag => IconName::Tag,
    };
    h_flex()
        .min_w_0()
        .h(gpui_kit::rems(CHIP_HEIGHT))
        .px_1p5()
        .gap_1()
        .rounded(theme.radius)
        .border_1()
        .border_color(color)
        // a tag's chip is lighter, as a tag is not a branch
        .when(chip.kind == RefKind::Tag, |chip| chip.border_dashed())
        .bg(color.opacity(0.16))
        .text_xs()
        .text_color(theme.foreground)
        .when(chip.current, |this| {
            this.child(
                Icon::new(IconName::Check)
                    .xsmall()
                    .text_color(theme.foreground),
            )
        })
        .child(Icon::new(icon).xsmall().text_color(color))
        .child(
            div()
                .min_w_0()
                .truncate()
                .when(chip.current, |this| this.font_semibold())
                .child(chip.name.clone()),
        )
        .when(mark_unpushed && chip.unpushed, |this| {
            this.child(
                Icon::new(IconName::Upload)
                    .xsmall()
                    .text_color(theme.warning),
            )
        })
}

impl RepoView {
    /// The pointer entered or left row `ix`'s ref chip, or (`list`) the
    /// refs spread out from it.
    fn hover_refs(&mut self, ix: usize, list: bool, hovered: bool, cx: &mut Context<Self>) {
        if self.refs_hover.set(ix, list, hovered) {
            cx.notify();
        }
    }

    pub(super) fn render_log_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cols = self.columns(cx);
        let header = [
            header_cell("Branch / Tag", cols.refs, Column::Refs, false, cx),
            header_cell("Graph", cols.graph, Column::Graph, false, cx),
            header_cell("Change", cols.change, Column::Change, false, cx),
            if cols.compact_author {
                div()
                    .w(cols.author)
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(gpui_kit::component::Icon::new(IconName::User).xsmall())
                    .into_any_element()
            } else {
                header_cell("Author", cols.author, Column::Author, false, cx)
            },
        ];
        let [refs_cell, graph_cell, change_cell, author_cell] = header;
        let theme = cx.theme();
        // A typed revset is neither scope, so neither tab is selected.
        let scope = self.log_scope().map(usize::from);
        let presets = self
            .workspace
            .as_ref()
            .filter(|ws| !ws.presets.is_empty())
            .map(|ws| (ws.presets.clone(), ws.query.clone()));
        let view = cx.entity().downgrade();
        v_flex()
            .flex_none()
            // a header over the rows, in the panels' shade, as GitKraken's
            .bg(theme.list_head)
            .child(
                h_flex()
                    .h_12()
                    .px_4()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .text_base()
                            .font_medium()
                            .child("Revisions"),
                    )
                    // which revisions the log shows: a jj revset, not a search
                    // (Find is the toolbar's Search)
                    .child(
                        div().w_80().min_w_24().child(
                            Input::new(&self.revset)
                                .small()
                                .prefix(
                                    gpui_kit::component::Icon::new(IconName::Funnel)
                                        .xsmall()
                                        .text_color(theme.muted_foreground),
                                )
                                .cleanable(true),
                        ),
                    )
                    .child(div().flex_1())
                    .child(
                        TabBar::new("log-scope")
                            .segmented()
                            .xsmall()
                            .when_some(scope, |tabs, ix| tabs.selected_index(ix))
                            .on_click(cx.listener(|view, ix: &usize, window, cx| {
                                view.set_log_scope(*ix == 1, window, cx)
                            }))
                            .child("Mine")
                            .child("All branches"),
                    )
                    // the order, changed now and then: a menu, not a toggle in view
                    .child({
                        let by_date = settings::get(cx).display.log_by_date;
                        Button::new("log-order")
                            .ghost()
                            .xsmall()
                            .label(if by_date { "By date" } else { "Topological" })
                            .icon(IconName::ChevronDown)
                            .tooltip("Order of the revisions")
                            .dropdown_menu_with_anchor(
                                gpui_kit::Anchor::TopRight,
                                move |menu, _, _| {
                                    let item = |label: &'static str, date: bool| {
                                        gpui_kit::component::menu::PopupMenuItem::new(label)
                                            .checked(by_date == date)
                                            .on_click(move |_, _, cx| {
                                                settings::update_display(cx, |d| {
                                                    d.log_by_date = date
                                                })
                                            })
                                    };
                                    menu.item(item("Topological", false))
                                        .item(item("By date", true))
                                },
                            )
                    })
                    .when_some(presets, |bar, (presets, query)| {
                        bar.child(
                            Button::new("presets")
                                .ghost()
                                .xsmall()
                                .label("Presets")
                                .icon(IconName::ChevronDown)
                                .tooltip("Revsets from gg.presets")
                                .dropdown_menu_with_anchor(
                                    gpui_kit::Anchor::TopRight,
                                    move |mut menu, _, _| {
                                        for (title, revset) in &presets {
                                            let view = view.clone();
                                            let revset = revset.clone();
                                            menu = menu.item(
                                                gpui_kit::component::menu::PopupMenuItem::new(
                                                    title.clone(),
                                                )
                                                .checked(revset == query)
                                                .on_click(move |_, window, cx| {
                                                    let revset = revset.clone();
                                                    let _ = view.update(cx, |view, cx| {
                                                        view.set_query(revset, window, cx)
                                                    });
                                                }),
                                            );
                                        }
                                        menu
                                    },
                                ),
                        )
                    }),
            )
            // a revset jj refused: the log still shows the one before
            .when_some(self.query_error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .px_4()
                        .pb_2()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(gpui_kit::component::Icon::new(IconName::TriangleAlert).xsmall())
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(format!("Revset not used: {error}")),
                        ),
                )
            })
            .child(
                h_flex()
                    .relative()
                    .h_8()
                    .px_3()
                    .gap_2()
                    .border_y_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .on_drag_move(cx.listener(
                        |view, event: &gpui_kit::DragMoveEvent<ColumnResize>, window, cx| {
                            view.resize_column(event.event.position.x, window.rem_size(), cx)
                        },
                    ))
                    // no gap between the two, as in the rows
                    .child(
                        h_flex()
                            .flex_none()
                            .h_full()
                            .when(cols.show_refs, |this| this.child(refs_cell))
                            .child(graph_cell),
                    )
                    .when(cols.show_change, |this| this.child(change_cell))
                    .child(div().flex_1().min_w_0().truncate().child("Description"))
                    .when(cols.show_author, |this| this.child(author_cell))
                    // over the header's right end, so the columns stay in line
                    // with the rows', as GitKraken puts them
                    .child(
                        h_flex()
                            .absolute()
                            .right_1()
                            .top_0()
                            .bottom_0()
                            .items_center()
                            .gap_0p5()
                            .bg(theme.list_head)
                            .child(self.render_author_filter_button(cx))
                            .child(self.render_column_settings_button(cx)),
                    ),
            )
    }

    /// The ⚙ at the header's end: which optional columns show, and the
    /// widths back to their defaults.
    fn render_column_settings_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = settings::get(cx).display.visible_columns;
        let owner = cx.entity().downgrade();
        Button::new("column-settings")
            .ghost()
            .xsmall()
            .icon(IconName::Settings)
            .tooltip("Column settings")
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
                let column =
                    |label: &'static str,
                     on: bool,
                     set: fn(&mut settings::ColumnVisibility, bool)| {
                        gpui_kit::component::menu::PopupMenuItem::new(label)
                            .checked(on)
                            .on_click(move |_, _, cx| {
                                settings::update_display(cx, |d| set(&mut d.visible_columns, !on))
                            })
                    };
                let owner = owner.clone();
                menu.item(column("Branch / Tag", shown.refs, |c, v| c.refs = v))
                    .item(column("Change", shown.change, |c, v| c.change = v))
                    .item(column("Author", shown.author, |c, v| c.author = v))
                    .separator()
                    .item(
                        gpui_kit::component::menu::PopupMenuItem::new("Reset column widths")
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |view, cx| view.reset_columns(cx));
                            }),
                    )
            })
    }

    /// Every column back to its default width, saved for every tab.
    fn reset_columns(&mut self, cx: &mut Context<Self>) {
        self.columns = settings::ColumnWidths::default();
        let columns = self.columns;
        settings::update_display(cx, |display| display.columns = columns);
        cx.notify();
    }

    /// The funnel at the header's end: pick authors, and the revisions of
    /// the others are dimmed. On while some are picked.
    fn render_author_filter_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = !self.author_filter.is_empty();
        let owner = cx.entity().downgrade();
        Button::new("author-filter")
            .ghost()
            // on: filled with the accent, as GitKraken's funnel is
            .when(active, |button| button.primary())
            .xsmall()
            .icon(IconName::Funnel)
            .tooltip(if active {
                "Filtered by author: the others are dimmed"
            } else {
                "Filter by author"
            })
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, _, cx| {
                let Some(view) = owner.upgrade() else {
                    return menu;
                };
                let (authors, picked) = {
                    let view = view.read(cx);
                    (
                        author_choices(
                            view.row_data
                                .iter()
                                .map(|row| (&row.author, &row.author_email)),
                        ),
                        view.author_filter.clone(),
                    )
                };
                if !picked.is_empty() {
                    let owner = owner.clone();
                    menu = menu
                        .item(
                            gpui_kit::component::menu::PopupMenuItem::new("Clear filter").on_click(
                                move |_, _, cx| {
                                    let _ =
                                        owner.update(cx, |view, cx| view.clear_author_filter(cx));
                                },
                            ),
                        )
                        .separator();
                }
                menu = menu.label("Authors of the loaded revisions");
                for (email, name, count) in authors {
                    let owner = owner.clone();
                    let checked = picked.contains(&email);
                    menu = menu.item(
                        gpui_kit::component::menu::PopupMenuItem::new(format!(
                            "{name} · {email} ({count})"
                        ))
                        .checked(checked)
                        .on_click(move |_, _, cx| {
                            let _ = owner.update(cx, |view, cx| view.toggle_author(&email, cx));
                        }),
                    );
                }
                menu.scrollable(true).max_h(gpui_kit::px(420.))
            })
    }

    fn toggle_author(&mut self, email: &SharedString, cx: &mut Context<Self>) {
        let filter = Rc::make_mut(&mut self.author_filter);
        if !filter.remove(email) {
            filter.insert(email.clone());
        }
        cx.notify();
    }

    fn clear_author_filter(&mut self, cx: &mut Context<Self>) {
        self.author_filter = Rc::default();
        cx.notify();
    }

    /// Resolved widths of the columns.
    pub(super) fn columns(&self, cx: &App) -> Columns {
        let shown = settings::get(cx).display.visible_columns;
        let c = self.columns;
        let rems = gpui_kit::rems;
        let width = |show: bool, width: f32| if show { width } else { 0. };
        let (refs, change, author) = (
            width(shown.refs, c.refs),
            width(shown.change, c.change),
            width(shown.author, c.author),
        );
        let graph = c.graph.map(rems).unwrap_or_else(|| self.graph.width());
        let compact_author = avatars_only(self.log_width.get(), refs + graph.0 + change, author);
        Columns {
            refs: rems(refs),
            graph,
            change: rems(change),
            author: rems(if compact_author && shown.author {
                COMPACT_AUTHOR
            } else {
                author
            }),
            compact_author: compact_author && shown.author,
            show_refs: shown.refs,
            show_change: shown.change,
            show_author: shown.author,
        }
    }

    fn column_width(&self, column: Column, cx: &App) -> f32 {
        let cols = self.columns(cx);
        match column {
            Column::Refs => cols.refs.0,
            Column::Graph => cols.graph.0,
            Column::Change => cols.change.0,
            Column::Author => cols.author.0,
        }
    }

    fn start_column_resize(&mut self, column: Column, x: gpui_kit::Pixels, cx: &App) {
        self.column_drag = Some((column, x, self.column_width(column, cx)));
    }

    fn resize_column(
        &mut self,
        x: gpui_kit::Pixels,
        rem: gpui_kit::Pixels,
        cx: &mut Context<Self>,
    ) {
        let Some((column, start_x, start)) = self.column_drag else {
            return;
        };
        let moved = f32::from(x - start_x) / f32::from(rem);
        let width = if column.grows_right() {
            start + moved
        } else {
            start - moved
        }
        .clamp(column.min(), 60.);
        let c = &mut self.columns;
        match column {
            Column::Refs => c.refs = width,
            Column::Graph => c.graph = Some(width),
            Column::Change => c.change = width,
            Column::Author => c.author = width,
        }
        cx.notify();
    }

    /// Saves the dragged widths, which every tab then uses.
    pub(super) fn finish_column_resize(&mut self, cx: &mut Context<Self>) {
        if self.column_drag.take().is_some() {
            let columns = self.columns;
            settings::update_display(cx, |display| display.columns = columns);
        }
    }

    /// A double click on a handle puts the column back to its default width
    /// (the graph back to fitting its lanes).
    fn reset_column(&mut self, column: Column, cx: &mut Context<Self>) {
        let default = settings::ColumnWidths::default();
        let c = &mut self.columns;
        match column {
            Column::Refs => c.refs = default.refs,
            Column::Graph => c.graph = default.graph,
            Column::Change => c.change = default.change,
            Column::Author => c.author = default.author,
        }
        let columns = self.columns;
        settings::update_display(cx, |display| display.columns = columns);
    }

    pub(super) fn render_log(&self, cx: &mut Context<Self>) -> AnyElement {
        let cols = self.columns(cx);
        let rows = self.row_data.clone();
        let graph = self.graph.clone();
        let focus = self.log_focus.clone();
        let selected = self.selected;
        let avatars = self.avatars.clone();
        let github = self.github_active();
        let owner = cx.entity().downgrade();
        let refs_open = self.refs_hover.open_row();
        let find_bar = self.find.as_ref().map(|find| self.render_find(find, cx));
        let wants_more = self.has_more && !self.loading_more && self.more_error.is_none();
        let more_owner = owner.clone();
        // rows to dim: those a find with text does not match, and those of
        // authors the filter leaves out
        let author_filter = (!self.author_filter.is_empty()).then(|| self.author_filter.clone());
        let find_matches = self
            .find
            .as_ref()
            .filter(|find| !find.input.read(cx).value().trim().is_empty())
            .map(|find| find.matches.clone());

        v_flex()
            .size_full()
            .min_h_0()
            .child(self.render_log_header(cx))
            .when_some(self.pick.as_ref(), |this, pick| {
                this.child(self.render_pick_bar(pick, cx))
            })
            // the find bar sits over the list but outside it: inside, its
            // field would be in the list's key context, and letters typed
            // there would run the list's shortcuts (d, e, r…)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("log")
                            .relative()
                            .size_full()
                            .track_focus(&self.log_focus)
                            .key_context(LOG_CONTEXT)
                            .role(gpui_kit::Role::List)
                            .aria_label("Revisions")
                            .on_mouse_down(
                                gpui_kit::MouseButton::Left,
                                cx.listener(|view, _, window, cx| view.log_focus.focus(window, cx)),
                            )
                            .on_action(cx.listener(|view, _: &actions::SelectPrevious, _, cx| {
                                view.move_selection(-1, cx)
                            }))
                            .on_action(cx.listener(|view, _: &actions::SelectNext, _, cx| {
                                view.move_selection(1, cx)
                            }))
                            .on_action(cx.listener(|view, _: &actions::ConfirmPick, window, cx| {
                                view.confirm_pick(window, cx)
                            }))
                            .on_action(cx.listener(|view, _: &actions::CancelPick, _, cx| {
                                view.cancel_pick(cx)
                            }))
                            .on_action(cx.listener(|view, _: &actions::SelectFirst, _, cx| {
                                view.move_selection(isize::MIN, cx)
                            }))
                            .on_action(cx.listener(|view, _: &actions::SelectLast, _, cx| {
                                view.move_selection(isize::MAX, cx)
                            }))
                            .child(
                                uniform_list("revisions", rows.len(), move |range, window, cx| {
                                    let rem = window.rem_size();
                                    // infinite scroll: the next page is asked for
                                    // while the rows drawn are still this far from the end
                                    if wants_more && range.end + PRELOAD_ROWS >= rows.len() {
                                        let owner = more_owner.clone();
                                        cx.defer(move |cx| {
                                            let _ = owner.update(cx, |view, cx| view.load_more(cx));
                                        });
                                    }
                                    range
                                        .map(|ix| {
                                            let owner = owner.clone();
                                            let menu_owner = owner.clone();
                                            let drop_owner = owner.clone();
                                            let focus = focus.clone();
                                            let email = rows[ix].author_email.as_ref();
                                            let avatar =
                                                avatars.get(email).cloned().or_else(|| {
                                                    github
                                                        .then(|| {
                                                            crate::github::noreply_avatar(email)
                                                        })
                                                        .flatten()
                                                        .map(SharedString::from)
                                                });
                                            render_row(
                                                &rows[ix],
                                                &graph,
                                                ix,
                                                selected == Some(ix),
                                                avatar,
                                                rem,
                                                cols,
                                                refs_open == Some(ix),
                                                &owner,
                                                find_matches
                                                    .as_ref()
                                                    .is_some_and(|m| m.binary_search(&ix).is_err())
                                                    || author_filter.as_ref().is_some_and(|f| {
                                                        !f.contains(&rows[ix].author_email)
                                                    }),
                                                cx,
                                            )
                                            .on_click(move |_, _, cx| {
                                                let _ = owner
                                                    .update(cx, |view, cx| view.select(ix, cx));
                                            })
                                            // Right-click selects the row first, so the menu acts on it.
                                            .on_mouse_down(
                                                gpui_kit::MouseButton::Right,
                                                move |_, window, cx| {
                                                    let _ = menu_owner.update(cx, |view, cx| {
                                                        view.select(ix, cx);
                                                        view.log_focus.focus(window, cx);
                                                    });
                                                },
                                            )
                                            .on_drag(
                                                DraggedRevision {
                                                    id: rows[ix].id.clone(),
                                                    label: rows[ix].change_prefix.clone(),
                                                },
                                                |dragged, _, _, cx| {
                                                    cx.new(|_| DragChip {
                                                        label: format!("Rebase {}", dragged.label)
                                                            .into(),
                                                        icon: IconName::GitCommitHorizontal,
                                                    })
                                                },
                                            )
                                            .drag_over::<DraggedRevision>(|style, _, _, cx| {
                                                style.bg(cx.theme().drop_target)
                                            })
                                            .drag_over::<DraggedRef>(|style, _, _, cx| {
                                                style.bg(cx.theme().drop_target)
                                            })
                                            .on_drop({
                                                let owner = drop_owner.clone();
                                                move |dragged: &DraggedRevision, window, cx| {
                                                    let _ = owner.update(cx, |view, cx| {
                                                        view.rebase_onto(
                                                            dragged.id.clone(),
                                                            ix,
                                                            window,
                                                            cx,
                                                        )
                                                    });
                                                }
                                            })
                                            .on_drop({
                                                let owner = drop_owner.clone();
                                                move |dragged: &DraggedRef, window, cx| {
                                                    let _ = owner.update(cx, |view, cx| {
                                                        view.move_ref_to(
                                                            dragged.store.clone(),
                                                            ix,
                                                            window,
                                                            cx,
                                                        )
                                                    });
                                                }
                                            })
                                            .drag_over::<DraggedFile>(|style, _, _, cx| {
                                                style.bg(cx.theme().drop_target)
                                            })
                                            .on_drop({
                                                let owner = drop_owner.clone();
                                                move |dragged: &DraggedFile, window, cx| {
                                                    let _ = owner.update(cx, |view, cx| {
                                                        view.move_file_to(
                                                            dragged.path.clone(),
                                                            dragged.from.clone(),
                                                            ix,
                                                            window,
                                                            cx,
                                                        )
                                                    });
                                                }
                                            })
                                            .context_menu(move |menu, _, _| {
                                                revision_menu(menu, focus.clone())
                                            })
                                            .into_any_element()
                                        })
                                        .collect()
                                })
                                .size_full()
                                .track_scroll(&self.scroll),
                            )
                            .child(Scrollbar::vertical(&self.scroll))
                            // measures the list, so the columns can fit its width
                            .child({
                                let width = self.log_width.clone();
                                canvas(
                                    move |bounds, window, _| {
                                        let rems = f32::from(bounds.size.width)
                                            / f32::from(window.rem_size());
                                        if (width.get() - rems).abs() > 0.25 {
                                            width.set(rems);
                                            window.refresh();
                                        }
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full()
                            }),
                    )
                    // after the list, so it is drawn over it
                    .children(find_bar),
            )
            .into_any_element()
    }
}

/// A header cell with a drag handle on the edge next to the description side
/// (right of the left columns, left of the right ones). Drag to resize, double
/// click to restore the default width.
fn header_cell(
    label: &'static str,
    width: Rems,
    column: Column,
    align_end: bool,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let theme = cx.theme();
    let (line, hover) = (theme.border, theme.primary);
    // between the branch / tag and graph columns there is no gap, so the
    // handle straddles the edge; elsewhere it sits in the gap
    let outside = if column == Column::Refs {
        px(4.)
    } else {
        px(8.)
    };
    let handle = div()
        .id(("column-resize", column as usize))
        .absolute()
        .top_0()
        .bottom_0()
        .w(px(8.))
        .map(|handle| {
            if column.grows_right() {
                handle.right(-outside)
            } else {
                handle.left(-outside)
            }
        })
        .flex()
        .justify_center()
        .cursor(gpui_kit::CursorStyle::ResizeColumn)
        .hover(move |style| style.bg(hover.opacity(0.25)))
        .child(div().w(px(1.)).h_full().bg(line))
        .on_mouse_down(
            gpui_kit::MouseButton::Left,
            cx.listener(move |view, event: &gpui_kit::MouseDownEvent, _, cx| {
                if event.click_count >= 2 {
                    view.reset_column(column, cx);
                } else {
                    // where the drag below measures from
                    view.column_press = Some(event.position.x);
                }
            }),
        )
        // the resize starts with the drag itself, so a press that never
        // becomes a drag leaves nothing behind
        .on_drag(ColumnResize, {
            let view = cx.entity().downgrade();
            move |_, _, window, cx| {
                let moved_to = window.mouse_position().x;
                let _ = view.update(cx, |view, cx| {
                    let x = view.column_press.take().unwrap_or(moved_to);
                    view.start_column_resize(column, x, cx)
                });
                cx.new(|_| ResizeGhost)
            }
        });
    div()
        .relative()
        .w(width)
        .flex_none()
        .h_full()
        .flex()
        .items_center()
        .when(align_end, |cell| cell.justify_end())
        .child(div().min_w_0().truncate().child(label))
        .child(handle)
        .into_any_element()
}

/// The authors for the author filter, from each row's (name, lowercase
/// email): (email, name, revisions), the most revisions first.
fn author_choices<'a>(
    authors: impl Iterator<Item = (&'a SharedString, &'a SharedString)>,
) -> Vec<(SharedString, SharedString, usize)> {
    let mut by_email: HashMap<SharedString, (SharedString, usize)> = HashMap::new();
    for (name, email) in authors {
        by_email
            .entry(email.clone())
            .or_insert_with(|| (name.clone(), 0))
            .1 += 1;
    }
    let mut authors: Vec<_> = by_email
        .into_iter()
        .map(|(email, (name, count))| (email, name, count))
        .collect();
    authors.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
    authors
}

#[cfg(test)]
mod tests {
    use super::avatars_only;
    use gpui_kit::SharedString;

    #[test]
    fn the_author_filter_lists_the_busiest_first() {
        let pairs: Vec<(SharedString, SharedString)> =
            [("b", "b@x"), ("a", "a@x"), ("b", "b@x"), ("c", "c@x")]
                .into_iter()
                .map(|(name, email)| (name.into(), email.into()))
                .collect();
        let listed: Vec<(String, usize)> =
            super::author_choices(pairs.iter().map(|(name, email)| (name, email)))
                .into_iter()
                .map(|(email, _, count)| (email.to_string(), count))
                .collect();
        // most revisions first, then by name
        assert_eq!(
            listed,
            [("b@x".into(), 2), ("a@x".into(), 1), ("c@x".into(), 1)]
        );
    }

    #[test]
    fn columns_give_way_as_the_list_narrows() {
        // fixed columns 20rem, author 7rem: 3.5 + 20 + 11 = 34.5 before the author
        assert!(!avatars_only(80., 20., 7.), "wide: names");
        assert!(!avatars_only(41.5, 20., 7.), "just room for the names");
        assert!(avatars_only(40., 20., 7.), "avatars only");
        assert!(!avatars_only(0., 20., 7.), "not measured yet");
    }
}
