//! The inspector (selected revision, description editor, changed files) and the
//! changes panel (diff of the selected file).

use super::*;

impl RepoView {
    pub(super) fn render_inspector(&self, cx: &mut Context<Self>) -> AnyElement {
        // the working copy as GitKraken's work in progress: a commit message,
        // and its changes unstaged and staged
        let staging_mode = self.staging_mode();
        let commit_box = staging_mode.then(|| self.render_commit_box(cx));
        let staged_section = staging_mode.then(|| self.render_staged_section(cx));
        let theme = cx.theme();
        let Some(detail) = &self.detail else {
            return v_flex()
                .size_full()
                .bg(theme.sidebar)
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
        // what is being written, for an editable revision; what is saved otherwise
        let typed = (!header.is_immutable).then(|| self.description_input.read(cx).value());
        let description_text: String = match &typed {
            Some(typed) => typed.to_string(),
            None => header.description.lines.join("\n"),
        };
        let issues = crate::issue_links::find(&description_text, cx);
        let subject_len = description_text
            .lines()
            .next()
            .map_or(0, |line| line.chars().count());
        let (long, too_long) = settings::get(cx).display.subject_limits;
        let subject_color = if subject_len > too_long as usize {
            theme.danger
        } else if subject_len > long as usize {
            theme.warning
        } else {
            theme.muted_foreground
        };

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
                                            gpui_kit::component::Icon::new(IconName::Lock)
                                                .xsmall(),
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
                    .when(!header.is_immutable && !staging_mode, |this| {
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
                                    // the subject's length against Settings' limits
                                    .when(subject_len > 0, |this| {
                                        this.child(
                                            div()
                                                .id("subject-length")
                                                .text_xs()
                                                .text_color(subject_color)
                                                .child(format!("{subject_len} / {long}"))
                                                .tooltip(move |window, cx| {
                                                    gpui_kit::component::tooltip::Tooltip::new(
                                                        format!(
                                                            "First line: {subject_len} characters \
                                                             (long past {long}, too long past {too_long})"
                                                        ),
                                                    )
                                                    .build(window, cx)
                                                }),
                                        )
                                    })
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
                    .child(div().text_sm().font_medium().child(if staging_mode {
                        "Unstaged files"
                    } else {
                        "Changed files"
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail.files.len().to_string()),
                    )
                    .when(self.marked_files.len() > 1, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("· {} picked", self.marked_files.len())),
                        )
                    })
                    .child(div().flex_1())
                    .when(staging_mode && !detail.files.is_empty(), |this| {
                        this.child(
                            Button::new("stage-all")
                                .ghost()
                                .xsmall()
                                .label("Stage all")
                                .tooltip("Stage every change of the working copy")
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.stage_all(window, cx)),
                                ),
                        )
                    })
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
                let marked = self.marked_files.contains(&file.path);
                ListItem::new(("file", ix))
                    .px_4()
                    .py_1()
                    .selected(self.selected_file == Some(ix) || marked)
                    .group("file-row")
                    .child(file_summary(file, theme).when(staging_mode, |row| {
                        // GitKraken's stage button, there while the pointer is on the row
                        row.child(
                            div()
                                .flex_none()
                                .invisible()
                                .group_hover("file-row", |style| style.visible())
                                .child(
                                    Button::new(("stage-file", ix))
                                        .success()
                                        .xsmall()
                                        .icon(IconName::Plus)
                                        .tooltip("Stage this file")
                                        .on_click(cx.listener(move |view, _, window, cx| {
                                            cx.stop_propagation();
                                            view.stage_file(ix, window, cx)
                                        })),
                                ),
                        )
                    }))
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
                        let picked = if marked { self.marked_files.len() } else { 1 };
                        move |menu, _, _| {
                            file_menu(menu, has_conflict, immutable, picked, focus.clone())
                        }
                    })
            }))
            .children(staged_section)
            .into_any_element()
    }

    /// The commit message box of the working copy's inspector, with Commit:
    /// what is staged, or everything when nothing is.
    fn render_commit_box(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let staged = self.staged.as_ref().map_or(0, |staged| staged.files.len());
        let changed = self
            .detail
            .as_ref()
            .is_some_and(|detail| !detail.files.is_empty());
        let typed = !self.commit_input.read(cx).value().trim().is_empty();
        v_flex()
            .gap_2()
            .child(Textarea::new(&self.commit_input).small())
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(match (staged, changed) {
                                (0, false) => "No changes to commit",
                                (0, true) => "Nothing staged: commits every change · ⌘↩",
                                _ => "Commits the staged changes · ⌘↩",
                            }),
                    )
                    .child(
                        Button::new("commit")
                            .small()
                            .primary()
                            .label(match staged {
                                0 => "Commit all".to_owned(),
                                1 => "Commit 1 file".to_owned(),
                                n => format!("Commit {n} files"),
                            })
                            .disabled(!typed || (staged == 0 && !changed) || !self.busy.is_empty())
                            .on_click(
                                cx.listener(|view, _, window, cx| view.commit_changes(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The staging revision's files, as GitKraken lists staged files: a
    /// click shows one's diff, its − (on hover) unstages it.
    fn render_staged_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let files = self
            .staged
            .as_ref()
            .map_or(&[][..], |staged| &staged.files[..]);
        v_flex()
            .border_t_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .h_10()
                    .px_4()
                    .gap_2()
                    .child(div().text_sm().font_medium().child("Staged files"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(files.len().to_string()),
                    )
                    .child(div().flex_1())
                    .when(!files.is_empty(), |this| {
                        this.child(
                            Button::new("unstage-all")
                                .ghost()
                                .xsmall()
                                .label("Unstage all")
                                .tooltip("Put every staged change back in the working copy")
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.unstage_all(window, cx)),
                                ),
                        )
                    }),
            )
            .when(files.is_empty(), |this| {
                this.child(
                    div()
                        .px_4()
                        .pb_3()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Stage files or lines with their +"),
                )
            })
            .children(files.iter().enumerate().map(|(ix, file)| {
                ListItem::new(("staged-file", ix))
                    .px_4()
                    .py_1()
                    .group("staged-row")
                    .selected(self.staged_file == Some(ix))
                    .child(
                        file_summary(file, theme).child(
                            div()
                                .flex_none()
                                .invisible()
                                .group_hover("staged-row", |style| style.visible())
                                .child(
                                    Button::new(("unstage-file", ix))
                                        .danger()
                                        .xsmall()
                                        .icon(IconName::Minus)
                                        .tooltip("Unstage this file")
                                        .on_click(cx.listener(move |view, _, window, cx| {
                                            cx.stop_propagation();
                                            view.unstage_file(ix, window, cx)
                                        })),
                                ),
                        ),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| view.select_staged_file(ix, cx)))
                    .into_any_element()
            }))
            .into_any_element()
    }
    /// The selected lines' buttons for a diff's header (Stage or Unstage
    /// in the working copy's, else Squash into parent; and Split off…), or
    /// how to get them.
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
                    Some(LineCommand::Stage) => "Drag over line numbers to stage lines",
                    Some(LineCommand::Unstage) => "Drag over line numbers to unstage lines",
                    _ => "Drag over line numbers to squash or split lines off",
                })
                .into_any_element();
        };
        let one_hunk = selection.hunks.len() == 1;
        let main = match command {
            Some(LineCommand::Stage) => Button::new("stage-lines")
                .small()
                .outline()
                .icon(IconName::Plus)
                .label("Stage lines")
                .tooltip("Stage the selected lines")
                .on_click(cx.listener(|view, _, window, cx| view.stage_selected_lines(window, cx))),
            Some(LineCommand::Unstage) => Button::new("unstage-lines")
                .small()
                .outline()
                .icon(IconName::Minus)
                .label("Unstage lines")
                .tooltip("Put the selected lines back in the working copy")
                .on_click(cx.listener(|view, _, window, cx| view.stage_selected_lines(window, cx))),
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
            // the press clears the diff's selection before the click: kept
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|view, _, _, _| view.keep_line_selection = true),
            )
            .child(main)
            // a staged file's lines are the staging revision's: not split there
            .when(command != Some(LineCommand::Unstage), |this| {
                this.child(
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
            })
            .into_any_element()
    }

    /// What a changed line's button does in `file`'s diff: in the working
    /// copy's, stage it, in a staged file's, unstage it, elsewhere squash it
    /// into the parent. `None` where the revision cannot be rewritten.
    pub(super) fn line_command(&self, file: &super::ops::ShownFile) -> Option<LineCommand> {
        if file.immutable {
            return None;
        }
        if file.staged {
            return Some(LineCommand::Unstage);
        }
        let working_copy = self.staging_mode()
            && self
                .detail
                .as_ref()
                .is_some_and(|detail| detail.header.id.commit.hex == file.id.commit.hex);
        match (working_copy, &file.parent) {
            (true, _) => Some(LineCommand::Stage),
            (false, Some(_)) => Some(LineCommand::Squash),
            (false, None) => None,
        }
    }

    /// `diff` with a button beside each changed line of `file`, shown while
    /// the pointer is on the line, as GitKraken's "Stage this line": + stages
    /// it (or squashes it into the parent outside the working copy), − in a
    /// staged file unstages it.
    pub(super) fn with_line_buttons(
        &self,
        diff: gpui_kit::component::diff::Diff,
        file: &super::ops::ShownFile,
        cx: &mut Context<Self>,
    ) -> gpui_kit::component::diff::Diff {
        use gpui_kit::component::diff::{DiffGutterMarker, DiffLinePosition, DiffSide};
        let Some(command) = self.line_command(file) else {
            return diff;
        };
        let label = match command {
            LineCommand::Stage => "Stage this line",
            LineCommand::Unstage => "Unstage this line",
            LineCommand::Squash => "Squash this line into the parent",
        };
        let markers = changed_line_positions(file.hunks)
            .into_iter()
            .enumerate()
            .map(|(ix, (side, line))| {
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
        diff.gutter_markers(markers)
            .render_gutter_marker(move |marker, _, _| {
                let side = super::ops::side_of(marker.position().side());
                let line = marker.position().line();
                let owner = owner.clone();
                let button = Button::new(marker.id().clone())
                    .xsmall()
                    .tooltip(marker.label().clone());
                let button = match command {
                    LineCommand::Unstage => button.danger().icon(IconName::Minus),
                    LineCommand::Stage | LineCommand::Squash => {
                        button.success().icon(IconName::Plus)
                    }
                };
                div()
                    .invisible()
                    .group_hover("diff-line", |style| style.visible())
                    .child(button.on_click(move |_, window, cx| {
                        let _ = owner.update(cx, |view, cx| match command {
                            LineCommand::Squash => view.squash_line(side, line, window, cx),
                            LineCommand::Stage | LineCommand::Unstage => {
                                view.stage_lines((side, line), (side, line), window, cx)
                            }
                        });
                    }))
            })
    }

    pub(super) fn render_changes(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut styled = split::styled_diff(&self.diff, cx).line_number(true);
        if let Some(file) = self.shown_diff_file() {
            styled = self.with_line_buttons(styled, &file, cx);
        }
        let diff = self.split_diff("changes-split", styled, &self.diff, cx);
        let theme = cx.theme();
        // the working copy's file shown, or a staged one
        let file = self
            .selected_file
            .and_then(|ix| self.detail.as_ref()?.files.get(ix))
            .or_else(|| {
                self.staged_file
                    .and_then(|ix| self.staged.as_ref()?.files.get(ix))
            });
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
    Stage,
    Unstage,
    Squash,
}
