//! The inspector (selected revision, description editor, changed files) and the
//! changes panel (diff of the selected file).

use gpui_kit::component::ElementExt as _;

use super::included::Inclusion;
use super::*;
use crate::harness_mark::HarnessMark as _;

impl RepoView {
    pub(super) fn render_inspector(&self, cx: &mut Context<Self>) -> AnyElement {
        // the working copy as the next commit: a message, and its changes
        // with a box each for whether the commit includes it
        let commit_mode = self.commit_mode();
        let commit_box = commit_mode.then(|| self.render_commit_box(cx));
        let theme = cx.theme();
        let Some(detail) = &self.inspected.detail else {
            return v_flex()
                .size_full()
                .bg(theme.sidebar)
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if self.inspected.loading {
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
        // what is being written, for an editable revision; what is saved otherwise
        let description_text = if header.is_immutable {
            header.description.lines.join("\n")
        } else {
            self.description.editor.text(cx)
        };
        let issues = crate::issue_links::find(&description_text, cx);
        let subject_limits = settings::get(cx).display.subject_limits;

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
            .bg(theme.sidebar)
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .px_4()
                    .pt_4()
                    .pb_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    // the change id as a label, GitKraken's "commit: b8daab": the
                    // description below is what the panel is about
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("change")
                            .child(
                                h_flex()
                                    .font_family(theme.mono_font_family.clone())
                                    .child(
                                        div()
                                            .font_semibold()
                                            .text_color(theme.primary)
                                            .child(header.id.change.prefix.clone()),
                                    )
                                    .child(
                                        header.id.change.rest.chars().take(6).collect::<String>(),
                                    ),
                            )
                            .child(div().flex_1())
                            .when(header.is_working_copy, |this| {
                                // another workspace's is named: GG marks every one
                                this.child(Tag::primary().small().child(
                                    match &header.working_copy_of {
                                        Some(name) => format!("Working copy of {name}"),
                                        None => "Working copy".to_owned(),
                                    },
                                ))
                            })
                            .when(header.has_conflict, |this| {
                                this.child(Tag::danger().small().child("Conflict"))
                            })
                            .when(header.is_immutable, |this| {
                                this.child(
                                    div()
                                        .id("immutable")
                                        .child(
                                            gpui_kit::component::Icon::new(IconName::Lock).xsmall(),
                                        )
                                        .tooltip(|window, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new("Immutable")
                                                .build(window, cx)
                                        }),
                                )
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
                    // the working copy's description is the next commit's message
                    .children(commit_box)
                    .when(!header.is_immutable && !commit_mode, |this| {
                        let dirty = self.description_dirty(cx);
                        this.child(self.description.editor.render(subject_limits, cx))
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
                    // issue keys of the description, by Settings' issue link rules
                    .when(!issues.is_empty(), |this| {
                        this.child(h_flex().gap_1().flex_wrap().children(
                            issues.into_iter().enumerate().map(|(ix, issue)| {
                                let url = issue.url.clone();
                                Button::new(("issue-link", ix))
                                    .outline()
                                    .xsmall()
                                    .icon(IconName::ExternalLink)
                                    .label(issue.key)
                                    .tooltip(issue.url)
                                    .on_click(move |_, _, cx| cx.open_url(&url))
                            }),
                        ))
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
                    // every file in the commit, or none
                    .when(commit_mode && !detail.files.is_empty(), |this| {
                        let all = self.next_commit.included.all_files(&detail.files);
                        this.child(
                            inclusion_box(
                                "include-all",
                                all,
                                if all == Inclusion::All {
                                    "Leave every change out of the commit"
                                } else {
                                    "Include every change in the commit"
                                },
                                theme,
                            )
                            .on_click(cx.listener(
                                move |view, _, _, cx| view.include_all(all != Inclusion::All, cx),
                            )),
                        )
                    })
                    .child(div().text_sm().font_medium().child("Changed files"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail.files.len().to_string()),
                    )
                    .when(self.inspected.marked.len() > 1, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("· {} picked", self.inspected.marked.len())),
                        )
                    })
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
                let marked = self.inspected.marked.contains(&file.path);
                ListItem::new(("file", ix))
                    .harness_mark(|| format!("file:{}", file.tree_path.repo_path))
                    .px_4()
                    .py_1()
                    .selected(self.inspected.file == Some(ix) || marked)
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            // whether the commit includes the file, as GitHub
                            // Desktop's box beside each
                            .when(commit_mode, |row| {
                                let state = self
                                    .next_commit
                                    .included
                                    .file(&file.tree_path.repo_path, &file.hunks);
                                row.child(
                                    inclusion_box(
                                        ("include-file", ix),
                                        state,
                                        match state {
                                            Inclusion::Nothing => "Include in the commit",
                                            Inclusion::Some => "Include every change in the commit",
                                            Inclusion::All => "Leave out of the commit",
                                        },
                                        theme,
                                    )
                                    .harness_mark(|| {
                                        format!("include:{}", file.tree_path.repo_path)
                                    })
                                    .on_click(cx.listener(
                                        move |view, _, _, cx| {
                                            cx.stop_propagation();
                                            view.include_file(ix, state != Inclusion::All, cx)
                                        },
                                    )),
                                )
                            })
                            .child(file_summary(file, theme)),
                    )
                    // a double click opens the file's full view, as in GitKraken;
                    // ⌘ and ⇧ pick files for Split
                    .on_click(
                        cx.listener(move |view, event: &gpui_kit::ClickEvent, window, cx| {
                            view.click_file(ix, event.modifiers(), cx);
                            if event.modifiers().secondary(cx) || event.modifiers().shift {
                                return;
                            }
                            if view.center.file().is_some() {
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
                            // the menu acts on the picked files when this is one of them
                            if marked {
                                view.select_file(Some(ix), cx);
                            } else {
                                view.click_file(ix, gpui_kit::Modifiers::default(), cx);
                            }
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
                        // the working copy picks no files for Split (its
                        // boxes are for Split off): the file clicked
                        let picked = Some(if marked {
                            self.inspected.marked.len()
                        } else {
                            1
                        });
                        move |menu, _, _| {
                            file_menu(menu, has_conflict, immutable, picked, focus.clone())
                        }
                    })
            }))
            .into_any_element()
    }

    /// The commit message box of the working copy's inspector, with Commit:
    /// the changes included, or every one when none is.
    fn render_commit_box(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let files = self
            .inspected
            .detail
            .as_ref()
            .map_or(&[][..], |detail| &detail.files[..]);
        let included = &self.next_commit.included;
        let typed = !self.next_commit.message.text(cx).is_empty();
        let hint = if files.is_empty() {
            "No changes to commit".to_owned()
        } else if self.next_commit.lines_left_out {
            "Included lines changed and were left out · ⌘↩".to_owned()
        } else if included.is_empty() {
            "Nothing included: commits every change · ⌘↩".to_owned()
        } else if included.all_files(files) == Inclusion::All {
            "Commits every change · ⌘↩".to_owned()
        } else {
            "The rest stays in the working copy · ⌘↩".to_owned()
        };
        v_flex()
            .gap_2()
            .child(
                self.next_commit
                    .message
                    .render(settings::get(cx).display.subject_limits, cx),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(if self.next_commit.lines_left_out {
                                theme.warning
                            } else {
                                theme.muted_foreground
                            })
                            .child(hint),
                    )
                    // `jj split`: what is included goes, with this message;
                    // the working copy keeps the rest, its description and
                    // its bookmarks
                    .when(!included.is_empty(), |this| {
                        this.child(
                            Button::new("split-included")
                                .harness_mark(|| "split-off")
                                .small()
                                .outline()
                                .label("Split off")
                                .tooltip(
                                    "Split what is included off with this message: the working \
                                     copy keeps the rest, its description and bookmarks (jj split)",
                                )
                                .disabled(!typed || !self.busy.is_empty())
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.split_included(window, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("commit")
                            .small()
                            .primary()
                            .label(if included.is_empty() {
                                "Commit all".to_owned()
                            } else {
                                format!("Commit {}", included.summary(files))
                            })
                            .tooltip(
                                "Commit with this message: the new working copy has what is \
                                 not included (jj commit)",
                            )
                            .disabled(!typed || files.is_empty() || !self.busy.is_empty())
                            .on_click(
                                cx.listener(|view, _, window, cx| view.commit_changes(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The selected lines' buttons for a diff's header (in the working
    /// copy's, include them in the commit or leave them out, else Squash
    /// into parent; and Split off…), or how to get them.
    pub(super) fn render_line_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let command = self
            .shown_diff_file()
            .and_then(|file| self.line_command(&file));
        let Some(selection) = self.selected_lines() else {
            return div()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(match command {
                    Some(LineCommand::Include) => {
                        "Tick lines, or drag over their numbers, to include them in the commit"
                    }
                    _ => "Drag over line numbers to squash or split lines off",
                })
                .into_any_element();
        };
        let one_hunk = selection.hunks.len() == 1;
        let main = match command {
            Some(LineCommand::Include) => {
                let all_in = self.selected_lines_included();
                Button::new("include-lines")
                    .harness_mark(|| "include-lines")
                    .small()
                    .outline()
                    .icon(if all_in {
                        IconName::Minus
                    } else {
                        IconName::Plus
                    })
                    .label(if all_in {
                        "Leave lines out"
                    } else {
                        "Include lines"
                    })
                    .tooltip(if all_in {
                        "Leave the selected lines out of the commit"
                    } else {
                        "Include the selected lines in the commit"
                    })
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_selected_lines(cx)))
            }
            _ => Button::new("squash-lines")
                .small()
                .outline()
                .icon(IconName::ArrowDownToLine)
                .label("Squash into parent")
                .disabled(!one_hunk || selection.parent.is_none())
                .tooltip(if one_hunk {
                    "Move the selected lines into the parent revision"
                } else {
                    "Select lines in one hunk to squash them"
                })
                .on_click(
                    cx.listener(|view, _, window, cx| view.squash_hunk_into_parent(window, cx)),
                ),
        };
        h_flex()
            .gap_2()
            // a press anywhere else clears the diff's line selection, which
            // these buttons act on
            .keep_text_selection("line-actions")
            .child(main)
            .child(
                Button::new("split-lines")
                    .small()
                    .outline()
                    .icon(IconName::GitCommitHorizontal)
                    .label("Split off…")
                    .tooltip("Split the selected lines into a new commit")
                    .on_click(
                        cx.listener(|view, _, window, cx| view.prompt_split_lines(window, cx)),
                    ),
            )
            .into_any_element()
    }

    /// What a changed line's button does in `file`'s diff: in the working
    /// copy's, include it in the commit or leave it out, elsewhere squash it
    /// into the parent. `None` where the revision cannot be rewritten.
    pub(super) fn line_command(&self, file: &super::lines::ShownFile) -> Option<LineCommand> {
        if file.immutable {
            return None;
        }
        match (self.includes_lines(file), &file.parent) {
            (true, _) => Some(LineCommand::Include),
            (false, Some(_)) => Some(LineCommand::Squash),
            (false, None) => None,
        }
    }

    /// `diff` with a button beside each changed line of `file`. In the
    /// working copy's it is the line's box in the commit, ticked while the
    /// commit includes the line; elsewhere it shows while the pointer is on
    /// the line, and squashes the line into the parent.
    pub(super) fn with_line_buttons(
        &self,
        diff: gpui_kit::component::diff::Diff,
        file: &super::lines::ShownFile,
        cx: &mut Context<Self>,
    ) -> gpui_kit::component::diff::Diff {
        use gpui_kit::component::diff::{DiffGutterMarker, DiffLinePosition, DiffSide};
        let Some(command) = self.line_command(file) else {
            return diff;
        };
        let included = &self.next_commit.included;
        let markers = changed_line_positions(file.hunks)
            .into_iter()
            .enumerate()
            .map(|(ix, (side, line))| {
                let label = match command {
                    LineCommand::Include if included.line(&file.path.repo_path, (side, line)) => {
                        "Leave this line out of the commit"
                    }
                    LineCommand::Include => "Include this line in the commit",
                    LineCommand::Squash => "Squash this line into the parent",
                };
                let side = match side {
                    Side::Original => DiffSide::Original,
                    Side::Modified => DiffSide::Modified,
                };
                DiffGutterMarker::new(
                    ("line-command", ix),
                    DiffLinePosition::new(file.diff_path.clone(), side, line),
                )
                .with_label(label)
            });
        let owner = cx.entity().downgrade();
        let path = file.path.repo_path.clone();
        let picked: std::collections::HashSet<(Side, usize)> = match command {
            LineCommand::Include => changed_line_positions(file.hunks)
                .into_iter()
                .filter(|&key| included.line(&path, key))
                .collect(),
            LineCommand::Squash => Default::default(),
        };
        diff.gutter_markers(markers)
            .render_gutter_marker(move |marker, _, cx| {
                let side = super::lines::side_of(marker.position().side());
                let line = marker.position().line();
                let owner = owner.clone();
                match command {
                    LineCommand::Include => {
                        let state = if picked.contains(&(side, line)) {
                            Inclusion::All
                        } else {
                            Inclusion::Nothing
                        };
                        div()
                            .harness_mark(move || {
                                let side = match side {
                                    Side::Original => "old",
                                    Side::Modified => "new",
                                };
                                format!("line:{side}:{line}")
                            })
                            .child(
                                inclusion_box(
                                    marker.id().clone(),
                                    state,
                                    marker.label().clone(),
                                    cx.theme(),
                                )
                                .on_click(move |_, _, cx| {
                                    let _ = owner.update(cx, |view, cx| {
                                        view.toggle_lines((side, line), (side, line), cx)
                                    });
                                }),
                            )
                    }
                    LineCommand::Squash => div()
                        .invisible()
                        .group_hover("diff-line", |style| style.visible())
                        .child(
                            Button::new(marker.id().clone())
                                .xsmall()
                                .success()
                                .icon(IconName::Plus)
                                .tooltip(marker.label().clone())
                                .on_click(move |_, window, cx| {
                                    let _ = owner.update(cx, |view, cx| {
                                        view.squash_line(side, line, window, cx)
                                    });
                                }),
                        ),
                }
            })
    }

    pub(super) fn render_changes(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut styled = split_diff::styled_diff(&self.diff, cx).line_number(true);
        if let Some(file) = self.shown_diff_file() {
            styled = self.with_line_buttons(styled, &file, cx);
        }
        let diff = self.split_diff("changes-split", styled, &self.diff, cx);
        let theme = cx.theme();
        let file = self
            .inspected
            .file
            .and_then(|ix| self.inspected.detail.as_ref()?.files.get(ix));
        let body = if self.inspected.loading && self.inspected.detail.is_none() {
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
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No changes")
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                // Fill the panel: without wrapping the diff would otherwise shrink to
                // its longest line and clip the file header of a short file.
                .child(diff)
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
                    .bg(theme.list_head)
                    .border_b_1()
                    .border_color(theme.border)
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
                        this.child(self.render_line_actions(cx))
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

/// What a changed file's row shows: its change letter, name and directory,
/// and its added and removed lines.
fn file_summary(file: &FileItem, theme: &gpui_kit::component::Theme) -> gpui_kit::Div {
    let (letter, color) = match file.kind {
        ChangeKind::Added => ("A", theme.success),
        ChangeKind::Deleted => ("D", theme.danger),
        ChangeKind::Modified => ("M", theme.warning),
        ChangeKind::None => ("·", theme.muted_foreground),
    };
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
        )
}

/// What the buttons beside a diff's changed lines do (see
/// [`RepoView::line_command`]).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LineCommand {
    /// Include the line in the next commit, or leave it out.
    Include,
    Squash,
}

/// A box for whether the commit includes a file or a line: empty, ticked,
/// or (for a file some of whose lines are in) with a dash, as a checkbox
/// shows a mixed state.
fn inclusion_box(
    id: impl Into<gpui_kit::ElementId>,
    state: Inclusion,
    tooltip: impl Into<SharedString>,
    theme: &gpui_kit::component::Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let on = state != Inclusion::Nothing;
    let tooltip = tooltip.into();
    div()
        .id(id)
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
        })
        // as a button's press: the keyboard stays where it was (D still
        // goes to the message), and no text selection starts
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            gpui_kit::component::GlobalState::suppress_text_selection(cx);
        })
        .flex_none()
        .size_4()
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radius / 2.)
        .border_1()
        .border_color(if on { theme.primary } else { theme.input })
        .when(on, |this| this.bg(theme.primary))
        .cursor_pointer()
        .when(!on, |this| {
            this.hover(|style| style.border_color(theme.ring))
        })
        .when(on, |this| {
            this.child(
                gpui_kit::component::Icon::new(if state == Inclusion::All {
                    IconName::Check
                } else {
                    IconName::Minus
                })
                .xsmall()
                .text_color(theme.primary_foreground),
            )
        })
}
