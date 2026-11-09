//! The inspector (selected revision, description editor, changed files) and the
//! changes panel (diff of the selected file).

use super::*;

impl RepoView {
    pub(super) fn render_inspector(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(detail) = &self.detail else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if self.detail_loading {
                    Spinner::new().into_any_element()
                } else {
                    "Select a revision".into_any_element()
                })
                .into_any_element();
        };
        let header = &detail.header;
        let mut lines = header.description.lines.iter().map(String::as_str);
        let subject = lines.next().filter(|line| !line.trim().is_empty());
        let body: String = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
        let (additions, deletions) = detail.files.iter().fold((0, 0), |(a, d), file| {
            (a + file.additions, d + file.deletions)
        });
        let refs = ref_chips(&header.refs);

        let fact = |label: &'static str, value: AnyElement| {
            v_flex()
                .gap_0p5()
                .min_w_0()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(div().text_sm().min_w_0().truncate().child(value))
        };

        v_flex()
            .id("inspector")
            .size_full()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .px_4()
                    .pt_4()
                    .pb_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_2xl()
                                    .child(
                                        div()
                                            .font_semibold()
                                            .text_color(theme.primary)
                                            .child(header.id.change.prefix.clone()),
                                    )
                                    .child(div().text_color(theme.muted_foreground).child(
                                        header.id.change.rest.chars().take(6).collect::<String>(),
                                    )),
                            )
                            .child(div().flex_1())
                            .when(header.is_working_copy, |this| {
                                this.child(Tag::primary().small().child("Working copy"))
                            })
                            .when(header.is_immutable, |this| {
                                this.child(Tag::secondary().small().child("Immutable"))
                            })
                            .when(header.has_conflict, |this| {
                                this.child(Tag::danger().small().child("Conflict"))
                            }),
                    )
                    // Immutable history is read-only; anything else is described in place.
                    .when(header.is_immutable, |this| {
                        this.child(match subject {
                            Some(subject) => {
                                div().text_base().font_medium().child(subject.to_owned())
                            }
                            None => div()
                                .text_base()
                                .italic()
                                .text_color(theme.muted_foreground)
                                .child("(no description set)"),
                        })
                        .when(!body.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(body.clone()),
                            )
                        })
                    })
                    .when(!header.is_immutable, |this| {
                        let dirty = self.description_dirty(cx);
                        this.child(Textarea::new(&self.description_input).small())
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(if dirty {
                                                "Unsaved · ⌘↩ to save"
                                            } else {
                                                "Press D to edit"
                                            }),
                                    )
                                    .child(
                                        Button::new("save-description")
                                            .small()
                                            .label("Save")
                                            .when(dirty, |button| button.primary())
                                            .disabled(!dirty || !self.busy.is_empty())
                                            .on_click(cx.listener(|view, _, window, cx| {
                                                view.save_description(window, cx)
                                            })),
                                    ),
                            )
                    })
                    .when(!refs.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .flex_wrap()
                                .children(refs.iter().map(|chip| {
                                    Tag::secondary().xsmall().child(chip.name.clone())
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .grid()
                    .grid_cols(2)
                    .gap_x_4()
                    .gap_y_3()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(fact(
                        "Author",
                        header.author.name.clone().into_any_element(),
                    ))
                    .child(fact(
                        "Date",
                        header
                            .author
                            .timestamp
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                            .into_any_element(),
                    ))
                    .child(fact(
                        "Commit",
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(
                                header.id.commit.prefix.clone()
                                    + &header.id.commit.rest[..6.min(header.id.commit.rest.len())],
                            )
                            .into_any_element(),
                    ))
                    .child(fact(
                        "Parents",
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(if detail.parents.is_empty() {
                                "none".to_owned()
                            } else {
                                detail.parents.join(", ")
                            })
                            .into_any_element(),
                    )),
            )
            .child(
                h_flex()
                    .h_10()
                    .px_4()
                    .gap_2()
                    .child(div().text_sm().font_medium().child("Changed files"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail.files.len().to_string()),
                    )
                    .child(div().flex_1())
                    .when(detail.conflicts > 0, |this| {
                        this.child(
                            Tag::danger()
                                .xsmall()
                                .child(format!("{} conflicts", detail.conflicts)),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(count_color(
                                additions,
                                theme.success,
                                theme.muted_foreground,
                            ))
                            .child(format!("+{additions}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(count_color(
                                deletions,
                                theme.danger,
                                theme.muted_foreground,
                            ))
                            .child(format!("−{deletions}")),
                    ),
            )
            .children(detail.files.iter().enumerate().map(|(ix, file)| {
                let (letter, color) = match file.kind {
                    ChangeKind::Added => ("A", theme.success),
                    ChangeKind::Deleted => ("D", theme.danger),
                    ChangeKind::Modified => ("M", theme.warning),
                    ChangeKind::None => ("·", theme.muted_foreground),
                };
                ListItem::new(("file", ix))
                    .px_4()
                    .py_1()
                    .selected(self.selected_file == Some(ix))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_3()
                            .child(
                                div()
                                    .w_3()
                                    .flex_none()
                                    .text_xs()
                                    .font_semibold()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(if file.has_conflict {
                                        theme.danger
                                    } else {
                                        color
                                    })
                                    .child(letter),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().truncate().child(file.name.clone()))
                                    .when(!file.dir.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .truncate()
                                                .text_color(theme.muted_foreground)
                                                .child(file.dir.clone()),
                                        )
                                    }),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap_2()
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(
                                        div()
                                            .w_8()
                                            .flex()
                                            .justify_end()
                                            .text_color(count_color(
                                                file.additions,
                                                theme.success,
                                                theme.muted_foreground,
                                            ))
                                            .child(format!("+{}", file.additions)),
                                    )
                                    .child(
                                        div()
                                            .w_8()
                                            .flex()
                                            .justify_end()
                                            .text_color(count_color(
                                                file.deletions,
                                                theme.danger,
                                                theme.muted_foreground,
                                            ))
                                            .child(format!("−{}", file.deletions)),
                                    ),
                            ),
                    )
                    // a double click opens the file's full view, as in GitKraken
                    .on_click(
                        cx.listener(move |view, event: &gpui_kit::ClickEvent, window, cx| {
                            view.select_file(Some(ix), cx);
                            if view.focus.is_some() {
                                // the full view follows the file picked here
                                view.retarget_focus(cx);
                            } else if event.click_count() >= 2 {
                                view.open_focus(FocusTab::Diff, false, window, cx);
                            }
                        }),
                    )
                    .on_mouse_down(
                        gpui_kit::MouseButton::Right,
                        cx.listener(move |view, _, window, cx| {
                            view.select_file(Some(ix), cx);
                            view.command_focus().focus(window, cx);
                        }),
                    )
                    .on_drag(
                        DraggedFile {
                            path: file.tree_path.clone(),
                            from: header.id.clone(),
                            label: file.name.clone(),
                        },
                        |dragged, _, _, cx| {
                            cx.new(|_| DragChip {
                                label: format!("Move {}", dragged.label).into(),
                                icon: IconName::File,
                            })
                        },
                    )
                    .context_menu({
                        let focus = self.command_focus();
                        let has_conflict = file.has_conflict;
                        let immutable = header.is_immutable;
                        move |menu, _, _| file_menu(menu, has_conflict, immutable, focus.clone())
                    })
            }))
            .into_any_element()
    }
    pub(super) fn render_changes(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let file = self
            .selected_file
            .and_then(|ix| self.detail.as_ref()?.files.get(ix));
        let body = if self.detail_loading && self.detail.is_none() {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(Spinner::new())
                .into_any_element()
        } else if file.is_none() {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_1()
                .child(div().text_sm().font_medium().child("No changes"))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("This revision leaves every file as it was."),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                // Fill the panel: without wrapping the diff would otherwise shrink to
                // its longest line and clip the file header of a short file.
                .child(
                    Diff::new(&self.diff)
                        .line_number(true)
                        .soft_wrap(false)
                        .size_full(),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .h_10()
                    .px_4()
                    .gap_3()
                    .flex_none()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        gpui_kit::component::Icon::new(IconName::FileDiff)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().text_sm().font_medium().child("Changes"))
                    .when_some(file, |this, file| {
                        this.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(file.path.clone()),
                        )
                    })
                    .child(div().flex_1())
                    .when(file.is_some() && !self.detail_is_immutable(), |this| {
                        if self.selected_hunk().is_some() {
                            this.child(
                                Button::new("squash-hunk")
                                    .small()
                                    .outline()
                                    .icon(IconName::ArrowDownToLine)
                                    .label("Squash hunk into parent")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.squash_hunk_into_parent(window, cx)
                                    })),
                            )
                        } else {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Select lines to pick a hunk"),
                            )
                        }
                    })
                    .child(
                        TabBar::new("diff-mode")
                            .segmented()
                            .xsmall()
                            .selected_index(if self.diff_mode == DiffMode::Split {
                                1
                            } else {
                                0
                            })
                            .on_click(cx.listener(|view, ix: &usize, _, cx| {
                                view.set_diff_mode(
                                    if *ix == 1 {
                                        DiffMode::Split
                                    } else {
                                        DiffMode::Unified
                                    },
                                    cx,
                                )
                            }))
                            .child("Unified")
                            .child("Split"),
                    )
                    .when(file.is_some(), |this| {
                        this.child(
                            Button::new("open-full-view")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Maximize2)
                                .tooltip("Full view: diff, file, blame and history (⇧⌘D)")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.open_focus(FocusTab::Diff, false, window, cx)
                                })),
                        )
                    }),
            )
            .child(body)
            .into_any_element()
    }
}
