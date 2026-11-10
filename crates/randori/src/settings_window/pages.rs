//! The Settings window's pages: what each shows, and the choices of its
//! menus (jj's tools, the installed terminal applications, the code fonts).

use super::fields::{committed_input, issue_links_editor};
use super::*;

pub(super) fn general_page(cx: &App) -> SettingPage {
    let theme_choice = cx
        .try_global::<MainApp>()
        .and_then(|main| main.0.upgrade())
        .map(|app| app.read(cx).saved_theme());
    SettingPage::new("General")
        .icon(IconName::Settings)
        .resettable(false)
        .group(
            SettingGroup::new()
                .title("Appearance")
                .item(SettingItem::new(
                    "Theme",
                    SettingField::dropdown(
                        vec![
                            ("light".into(), "Light".into()),
                            ("dark".into(), "Dark".into()),
                            ("system".into(), "Follow system".into()),
                        ],
                        move |_| {
                            match theme_choice.unwrap_or_default() {
                                ThemeChoice::Light => "light",
                                ThemeChoice::Dark => "dark",
                                ThemeChoice::System => "system",
                            }
                            .into()
                        },
                        |value, cx| {
                            let choice = match value.as_ref() {
                                "light" => ThemeChoice::Light,
                                "system" => ThemeChoice::System,
                                _ => ThemeChoice::Dark,
                            };
                            if let Some(app) =
                                cx.try_global::<MainApp>().and_then(|m| m.0.upgrade())
                            {
                                app.update(cx, |app, cx| app.request_theme(choice, cx));
                            }
                        },
                    ),
                )),
        )
        .group(
            SettingGroup::new()
                .title("Revisions")
                .item(
                    SettingItem::new(
                        "Order",
                        SettingField::render(|_, _, cx| {
                            let by_date = display(cx).log_by_date;
                            RadioGroup::horizontal("settings-order")
                                .selected_index(Some(usize::from(by_date)))
                                .child("Topological")
                                .child("By date")
                                .on_click(|ix, _, cx| {
                                    settings::update_display(cx, |d| d.log_by_date = *ix == 1)
                                })
                        }),
                    )
                    .description(
                        "Topological shows one branch at a time, as jj log does; by date \
                         lists the newest first across branches. The toolbar switches it too.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Mark tabs with changes",
                        SettingField::switch(
                            |cx| display(cx).mark_changed_tabs,
                            |value, cx| {
                                settings::update_display(cx, |d| d.mark_changed_tabs = value)
                            },
                        ),
                    )
                    .description("A dot on a tab whose working copy changes files."),
                ),
        )
        .group(
            SettingGroup::new()
                .title("Workspaces")
                .item(
                    SettingItem::render(|_, _, cx| source_folders(cx)).description(
                        "The workspace picker (in the toolbar) lists the jj repositories \
                             in these folders, two levels down.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Fetch automatically",
                        SettingField::dropdown(
                            AUTO_FETCH_CHOICES
                                .iter()
                                .map(|(mins, label)| (mins.to_string().into(), (*label).into()))
                                .collect(),
                            |cx| display(cx).auto_fetch_mins.to_string().into(),
                            |value, cx| {
                                let mins = value.parse().unwrap_or(0);
                                settings::update_display(cx, |d| d.auto_fetch_mins = mins)
                            },
                        ),
                    )
                    .description(
                        "Runs jj git fetch for every open tab. A fetch that fails (no network, \
                         a remote that asks for a password) is skipped quietly.",
                    ),
                ),
        )
}

fn source_folders(cx: &App) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let folders = display(cx).source_folders.clone();
    v_flex()
        .w_full()
        .gap_1()
        .child(div().text_sm().font_medium().child("Source folders"))
        .children(folders.into_iter().enumerate().map(|(ix, folder)| {
            h_flex()
                .gap_2()
                .text_sm()
                .child(
                    gpui_kit::component::Icon::new(IconName::Folder)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(div().flex_1().min_w_0().truncate().child(folder.clone()))
                .child(
                    Button::new(("remove-folder", ix))
                        .ghost()
                        .xsmall()
                        .icon(IconName::X)
                        .tooltip("Remove")
                        .on_click(move |_, _, cx| {
                            let folder = folder.clone();
                            settings::update_display(cx, |d| {
                                d.source_folders.retain(|other| *other != folder)
                            })
                        }),
                )
        }))
        .child(
            Button::new("add-folder")
                .outline()
                .xsmall()
                .icon(IconName::Plus)
                .label("Add folder…")
                .on_click(|_, _, cx| add_source_folders(cx)),
        )
        .into_any_element()
}

fn add_source_folders(cx: &mut App) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: true,
        prompt: Some("Add".into()),
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = paths.await else {
            return;
        };
        cx.update(|cx| {
            settings::update_display(cx, |d| {
                for path in paths {
                    let path = path.display().to_string();
                    if !d.source_folders.contains(&path) {
                        d.source_folders.push(path);
                    }
                }
            })
        });
    })
    .detach();
}

pub(super) fn revisions_page() -> SettingPage {
    let column = |title: &'static str,
                  get: fn(&DisplaySettings) -> bool,
                  set: fn(&mut DisplaySettings, bool)| {
        SettingItem::new(
            title,
            SettingField::switch(
                move |cx| get(display(cx)),
                move |value, cx| settings::update_display(cx, |d| set(d, value)),
            )
            .default_value(true),
        )
    };
    SettingPage::new("Revisions")
        .icon(IconName::GitGraph)
        .group(
            SettingGroup::new()
                .title("Columns")
                .description("The graph and the description are always shown.")
                .item(column(
                    "Branch / Tag",
                    |d| d.visible_columns.refs,
                    |d, v| d.visible_columns.refs = v,
                ))
                .item(column(
                    "Change",
                    |d| d.visible_columns.change,
                    |d, v| d.visible_columns.change = v,
                ))
                .item(column(
                    "Author",
                    |d| d.visible_columns.author,
                    |d, v| d.visible_columns.author = v,
                )),
        )
        .group(
            SettingGroup::new().title("Bookmarks").item(
                SettingItem::new(
                    "Mark bookmarks that are not pushed",
                    SettingField::switch(
                        |cx| settings::get(cx).mark_unpushed,
                        |value, cx| {
                            let result = settings::set_mark_unpushed(cx, value);
                            report(result, cx)
                        },
                    ),
                )
                .description("gg.ui.mark-unpushed-bookmarks in your jj config."),
            ),
        )
}

pub(super) fn diff_page(cx: &App) -> SettingPage {
    let default_font = cx
        .try_global::<ThemeCodeFont>()
        .cloned()
        .unwrap_or(ThemeCodeFont {
            family: cx.theme().mono_font_family.clone(),
            size: f32::from(cx.theme().mono_font_size),
        });
    let fonts = font_choices(&default_font.family, cx);
    let family = default_font.family.clone();
    SettingPage::new("Diff")
        .icon(IconName::FileDiff)
        .group(
            SettingGroup::new()
                .title("Layout")
                .item(SettingItem::new(
                    "Diffs open as",
                    SettingField::dropdown(
                        vec![
                            ("unified".into(), "Unified".into()),
                            ("split".into(), "Split".into()),
                        ],
                        |cx| match display(cx).diff {
                            DiffLayout::Unified => "unified".into(),
                            DiffLayout::Split => "split".into(),
                        },
                        |value, cx| {
                            let layout = if value == "split" {
                                DiffLayout::Split
                            } else {
                                DiffLayout::Unified
                            };
                            settings::update_display(cx, |d| d.diff = layout)
                        },
                    )
                    .default_value(SharedString::from("unified")),
                ))
                .item(SettingItem::new(
                    "Mark changed lines with",
                    SettingField::dropdown(
                        vec![
                            ("signs".into(), "+ and − signs".into()),
                            ("bars".into(), "A coloured bar".into()),
                            ("none".into(), "Nothing (colour only)".into()),
                        ],
                        |cx| match display(cx).change_marks {
                            ChangeMarks::Signs => "signs".into(),
                            ChangeMarks::Bars => "bars".into(),
                            ChangeMarks::None => "none".into(),
                        },
                        |value, cx| {
                            let marks = match value.as_ref() {
                                "bars" => ChangeMarks::Bars,
                                "none" => ChangeMarks::None,
                                _ => ChangeMarks::Signs,
                            };
                            settings::update_display(cx, |d| d.change_marks = marks)
                        },
                    )
                    .default_value(SharedString::from("signs")),
                ))
                .item(SettingItem::new(
                    "Syntax highlighting",
                    SettingField::switch(
                        |cx| display(cx).syntax_highlight,
                        |value, cx| settings::update_display(cx, |d| d.syntax_highlight = value),
                    )
                    .default_value(true),
                )),
        )
        .group(
            SettingGroup::new()
                .title("Code font")
                .description("For diffs, the file view and change ids.")
                .item(SettingItem::new(
                    "Font",
                    SettingField::scrollable_dropdown(
                        fonts,
                        move |cx| {
                            display(cx)
                                .code_font
                                .clone()
                                .map(SharedString::from)
                                .unwrap_or_else(|| family.clone())
                        },
                        {
                            let family = default_font.family.clone();
                            move |value, cx| {
                                let font = (value != family).then(|| value.to_string());
                                settings::update_display(cx, |d| d.code_font = font)
                            }
                        },
                    )
                    .default_value(default_font.family.clone()),
                ))
                .item(SettingItem::new(
                    "Size",
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: 9.,
                            max: 24.,
                            step: 1.,
                        },
                        move |cx| {
                            f64::from(display(cx).code_font_size.unwrap_or(default_font.size))
                        },
                        move |value, cx| {
                            let size = value as f32;
                            let size = ((size - default_font.size).abs() > 0.01).then_some(size);
                            settings::update_display(cx, |d| d.code_font_size = size)
                        },
                    )
                    .default_value(f64::from(default_font.size)),
                )),
        )
}

/// The installed code fonts, the theme's first and the chosen ones kept
/// even when the lookup has not finished or missed them.
fn font_choices(default: &SharedString, cx: &App) -> Vec<(SharedString, SharedString)> {
    let mut names: Vec<String> = vec![default.to_string()];
    let display = display(cx);
    names.extend(display.code_font.clone());
    names.extend(display.terminal_font.clone());
    if let Some(fonts) = cx.try_global::<CodeFonts>() {
        names.extend(fonts.0.iter().cloned());
    }
    let mut seen = HashSet::new();
    names
        .into_iter()
        .filter(|name| seen.insert(name.clone()))
        .map(|name| {
            let label = if name == default.as_ref() {
                format!("{name} (theme)")
            } else {
                name.clone()
            };
            (name.into(), label.into())
        })
        .collect()
}

pub(super) fn describe_page() -> SettingPage {
    let limit = |title: &'static str,
                 get: fn((u32, u32)) -> u32,
                 set: fn(&mut (u32, u32), u32),
                 default: u32| {
        SettingItem::new(
            title,
            SettingField::number_input(
                NumberFieldOptions {
                    min: 20.,
                    max: 200.,
                    step: 1.,
                },
                move |cx| f64::from(get(display(cx).subject_limits)),
                move |value, cx| {
                    settings::update_display(cx, |d| set(&mut d.subject_limits, value as u32))
                },
            )
            .default_value(f64::from(default)),
        )
    };
    SettingPage::new("Describe")
        .icon(IconName::TextCursorInput)
        .group(
            SettingGroup::new()
                .title("Subject length")
                .description(
                    "The description editor counts the first line's characters and colours \
                     the count past these limits.",
                )
                .item(limit("Long past", |l| l.0, |l, v| l.0 = v, 50))
                .item(limit("Too long past", |l| l.1, |l, v| l.1 = v, 72)),
        )
        .group(
            SettingGroup::new()
                .title("Issue links")
                .description(
                    "One rule a line: a regular expression, a space, then the URL, where $0 \
                     is the match. Issue keys in descriptions and pull requests become links, \
                     e.g. MELON-\\d+ https://example.atlassian.net/browse/$0",
                )
                .item(SettingItem::render(|_, window, cx| {
                    issue_links_editor(window, cx)
                })),
        )
}

pub(super) fn jj_page() -> SettingPage {
    SettingPage::new("jj")
        .icon(IconName::GitCommitHorizontal)
        .group(
            SettingGroup::new()
                .title("User")
                .description(
                    "user.name and user.email in your jj config: new changes are authored by \
                     them. Open tabs load their workspace again to pick them up.",
                )
                .item(SettingItem::new(
                    "Name",
                    committed_input(
                        "user-name",
                        |cx| settings::get(cx).user_name.clone(),
                        |value, cx| {
                            let result = settings::set_jj_string(cx, USER_NAME_KEY, Some(value));
                            report(result, cx)
                        },
                    ),
                ))
                .item(SettingItem::new(
                    "Email",
                    committed_input(
                        "user-email",
                        |cx| settings::get(cx).user_email.clone(),
                        |value, cx| {
                            let result = settings::set_jj_string(cx, USER_EMAIL_KEY, Some(value));
                            report(result, cx)
                        },
                    ),
                )),
        )
        .group(
            SettingGroup::new().title("Snapshots").item(
                SettingItem::new(
                    "Snapshot when Randori comes to the front",
                    SettingField::dropdown(
                        vec![
                            ("auto".into(), "Automatic".into()),
                            ("always".into(), "Always".into()),
                            ("never".into(), "Never".into()),
                        ],
                        |cx| match settings::get(cx).auto_snapshot {
                            None => "auto".into(),
                            Some(true) => "always".into(),
                            Some(false) => "never".into(),
                        },
                        |value, cx| {
                            let value = match value.as_ref() {
                                "always" => Some(true),
                                "never" => Some(false),
                                _ => None,
                            };
                            let result = settings::set_auto_snapshot(cx, value);
                            report(result, cx)
                        },
                    ),
                )
                .description(
                    "Automatic skips it in repositories over 100k commits \
                     (gg.queries.auto-snapshot). A terminal's git commands are picked up by a \
                     snapshot, so Never also leaves them for the next jj command.",
                ),
            ),
        )
}

/// jj's merge tools that resolve conflicts (`merge-args`), in its defaults.
const MERGE_TOOLS: [(&str, &str); 8] = [
    // jj's default, first
    (":builtin", "jj's built-in editor (:builtin)"),
    ("kdiff3", "KDiff3"),
    ("meld", "Meld"),
    ("mergiraf", "Mergiraf"),
    ("smerge", "Sublime Merge"),
    ("vimdiff", "vimdiff"),
    ("vscode", "VS Code"),
    ("vscodium", "VSCodium"),
];

/// jj's diff formats and the tools in its defaults that show a diff.
const DIFF_TOOLS: [(&str, &str); 8] = [
    // jj's default, first
    (":color-words", "Coloured words (:color-words)"),
    (":git", "Git format (:git)"),
    (":summary", "File list (:summary)"),
    ("difft", "difftastic"),
    ("diffedit3", "diffedit3"),
    ("meld", "Meld"),
    ("vscode", "VS Code"),
    ("vscodium", "VSCodium"),
];

/// Terminal applications the terminal button can open instead of the panel.
const TERMINAL_APPS: [&str; 7] = [
    "Terminal",
    "iTerm",
    "Ghostty",
    "WezTerm",
    "Warp",
    "kitty",
    "Alacritty",
];

pub(super) fn integration_page(cx: &App) -> SettingPage {
    let current = settings::get(cx);
    // the config read includes jj's defaults, the first of each list
    let merge = tool_choices(&MERGE_TOOLS, current.merge_editor.as_deref());
    let diff = tool_choices(&DIFF_TOOLS, current.diff_formatter.as_deref());
    let default_font = cx
        .try_global::<ThemeCodeFont>()
        .map(|font| font.family.clone())
        .unwrap_or_else(|| cx.theme().mono_font_family.clone());
    let fonts = font_choices(&default_font, cx);
    let code_size = cx
        .try_global::<ThemeCodeFont>()
        .map(|font| font.size)
        .unwrap_or(13.);
    SettingPage::new("Integration")
        .icon(IconName::SquareTerminal)
        .group(
            SettingGroup::new()
                .title("Terminal")
                .item(
                    SettingItem::new(
                        "Open",
                        SettingField::dropdown(
                            terminal_choices(),
                            |cx| display(cx).external_terminal.clone().unwrap_or_default().into(),
                            |value, cx| {
                                let app = (!value.is_empty()).then(|| value.to_string());
                                settings::update_display(cx, |d| d.external_terminal = app)
                            },
                        )
                        .default_value(SharedString::default()),
                    )
                    .description(
                        "What the toolbar's terminal button and ⌃` open, in the workspace's folder.",
                    ),
                )
                .item(SettingItem::new(
                    "Font",
                    SettingField::scrollable_dropdown(
                        fonts,
                        {
                            let default_font = default_font.clone();
                            move |cx| {
                                display(cx)
                                    .terminal_font
                                    .clone()
                                    .map(SharedString::from)
                                    .unwrap_or_else(|| default_font.clone())
                            }
                        },
                        {
                            let default_font = default_font.clone();
                            move |value, cx| {
                                let font = (value != default_font).then(|| value.to_string());
                                settings::update_display(cx, |d| d.terminal_font = font)
                            }
                        },
                    )
                    .default_value(default_font.clone()),
                ))
                .item(SettingItem::new(
                    "Size",
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: 9.,
                            max: 28.,
                            step: 1.,
                        },
                        move |cx| {
                            let display = display(cx);
                            f64::from(
                                display
                                    .terminal_font_size
                                    .or(display.code_font_size)
                                    .unwrap_or(code_size),
                            )
                        },
                        move |value, cx| {
                            let size = value as f32;
                            let size = ((size - code_size).abs() > 0.01).then_some(size);
                            settings::update_display(cx, |d| d.terminal_font_size = size)
                        },
                    )
                    .default_value(f64::from(code_size)),
                )),
        )
        .group(
            SettingGroup::new()
                .title("Tools")
                .description(
                    "ui.merge-editor and ui.diff-formatter in your jj config, for \"Resolve in \
                     merge tool\" and \"Open in diff tool\" (and the jj CLI). Open tabs load \
                     their workspace again to pick them up.",
                )
                .item(SettingItem::new(
                    "Merge tool",
                    SettingField::dropdown(
                        merge,
                        |cx| tool_value(&MERGE_TOOLS, &settings::get(cx).merge_editor),
                        |value, cx| {
                            // the menu sends the chosen item again too
                            if value == tool_value(&MERGE_TOOLS, &settings::get(cx).merge_editor) {
                                return;
                            }
                            let result =
                                settings::set_jj_string(cx, MERGE_EDITOR_KEY, tool_setting(&MERGE_TOOLS, &value));
                            report(result, cx)
                        },
                    ),
                ))
                .item(SettingItem::new(
                    "Diff tool",
                    SettingField::dropdown(
                        diff,
                        |cx| tool_value(&DIFF_TOOLS, &settings::get(cx).diff_formatter),
                        |value, cx| {
                            if value == tool_value(&DIFF_TOOLS, &settings::get(cx).diff_formatter) {
                                return;
                            }
                            let result = settings::set_jj_string(
                                cx,
                                DIFF_FORMATTER_KEY,
                                tool_setting(&DIFF_TOOLS, &value),
                            );
                            report(result, cx)
                        },
                    ),
                )),
        )
        .group(
            SettingGroup::new()
                .title("GitHub")
                .item(
                    SettingItem::new(
                        "Pull requests and avatars",
                        SettingField::switch(
                            |cx| display(cx).github,
                            |value, cx| settings::update_display(cx, |d| d.github = value),
                        )
                        .default_value(true),
                    )
                    .description("Network requests to the GitHub host of the workspace's remote."),
                )
                .item(SettingItem::new(
                    "Refresh pull requests",
                    SettingField::dropdown(
                        PULL_REFRESH_CHOICES
                            .iter()
                            .map(|(secs, label)| (secs.to_string().into(), (*label).into()))
                            .collect(),
                        |cx| display(cx).pull_refresh_secs.to_string().into(),
                        |value, cx| {
                            let secs = value.parse().unwrap_or(60);
                            settings::update_display(cx, |d| d.pull_refresh_secs = secs)
                        },
                    )
                    .default_value(SharedString::from("60")),
                )),
        )
}

/// The known tools (jj's default first), and a value set by hand that is
/// none of them, so it shows as chosen.
fn tool_choices(
    known: &[(&str, &str)],
    current: Option<&str>,
) -> Vec<(SharedString, SharedString)> {
    let mut choices: Vec<(SharedString, SharedString)> = Vec::new();
    choices.extend(
        known
            .iter()
            .map(|(name, label)| ((*name).into(), (*label).into())),
    );
    if let Some(current) = current.filter(|current| !known.iter().any(|(name, _)| name == current))
    {
        choices.push((
            current.to_owned().into(),
            format!("{current} (set by hand)").into(),
        ));
    }
    choices
}

/// The tool shown as chosen: the one set, else jj's default.
fn tool_value(known: &[(&str, &str)], current: &Option<String>) -> SharedString {
    current
        .clone()
        .unwrap_or_else(|| known[0].0.to_owned())
        .into()
}

/// What to write for the tool picked: nothing for jj's default, so a newer
/// jj's default applies.
fn tool_setting(known: &[(&str, &str)], picked: &str) -> Option<String> {
    (picked != known[0].0).then(|| picked.to_owned())
}

/// The panel, then the terminal applications that are installed.
fn terminal_choices() -> Vec<(SharedString, SharedString)> {
    // looked for once: every page is built whenever a setting changes
    static INSTALLED: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    let installed = INSTALLED.get_or_init(|| {
        if cfg!(target_os = "macos") {
            TERMINAL_APPS
                .iter()
                .copied()
                .filter(|app| application_installed(app))
                .collect()
        } else {
            Vec::new()
        }
    });
    let mut choices: Vec<(SharedString, SharedString)> =
        vec![("".into(), "The terminal panel (built in)".into())];
    choices.extend(installed.iter().map(|app| ((*app).into(), (*app).into())));
    choices
}

fn application_installed(name: &str) -> bool {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let bundle = format!("{name}.app");
    [
        Path::new("/Applications").to_path_buf(),
        Path::new("/System/Applications/Utilities").to_path_buf(),
    ]
    .into_iter()
    .chain(home.map(|home| home.join("Applications")))
    .any(|dir| dir.join(&bundle).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_set_by_hand_stays_a_choice() {
        let choices = tool_choices(&MERGE_TOOLS, Some("my-tool"));
        assert_eq!(choices.first().unwrap().0, ":builtin");
        assert_eq!(choices.last().unwrap().0, "my-tool");
        // jj's default and the known tools are no hand-set value
        assert_eq!(
            tool_choices(&MERGE_TOOLS, Some("meld")).len(),
            MERGE_TOOLS.len()
        );
        assert_eq!(
            tool_choices(&DIFF_TOOLS, Some(":color-words")).len(),
            DIFF_TOOLS.len()
        );
        // picking jj's default removes the setting
        assert_eq!(tool_setting(&DIFF_TOOLS, ":color-words"), None);
        assert_eq!(tool_setting(&DIFF_TOOLS, ":git"), Some(":git".into()));
        assert_eq!(tool_value(&MERGE_TOOLS, &None), ":builtin");
    }
}
