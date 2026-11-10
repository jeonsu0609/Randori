//! The Settings window (⌘,), as Fork has one: a window of its own with its
//! pages listed on the left. A change applies at once, with no Save button.
//!
//! Randori's own preferences go to the session (`DisplaySettings`); the ones
//! jj and GG read (`user.*`, `ui.*`, `gg.*`) go to the user's jj config, so the
//! jj CLI sees the same values. A jj write that fails says so in a
//! notification instead of pretending the setting changed.

mod fields;
mod pages;

use std::collections::HashSet;
use std::path::Path;

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, StyledExt as _, Theme, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    notification::Notification,
    radio::RadioGroup,
    setting::{
        NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings as Pages,
    },
    v_flex,
};
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, Focusable as _, Global, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Subscription,
    WeakEntity, Window, WindowBounds, WindowOptions, div, prelude::FluentBuilder as _, px, size,
};

use crate::app_view::{RandoriApp, ThemeChoice};
use crate::settings::{
    self, AUTO_FETCH_CHOICES, ChangeMarks, DIFF_FORMATTER_KEY, DiffLayout, DisplaySettings,
    IssueLink, MERGE_EDITOR_KEY, PULL_REFRESH_CHOICES, USER_EMAIL_KEY, USER_NAME_KEY,
};
use pages::{describe_page, diff_page, general_page, integration_page, jj_page, revisions_page};

/// The main window's app, which keeps the appearance (it re-themes windows),
/// and the display it is on, which Settings opens on. The main window keeps
/// the display as it moves: Settings opens from inside it, where the window
/// cannot be asked.
pub struct MainApp {
    pub app: WeakEntity<RandoriApp>,
    pub display: std::rc::Rc<std::cell::Cell<Option<gpui_kit::DisplayId>>>,
}

impl Global for MainApp {}

/// The Settings window while it is open, so ⌘, brings it forward.
struct OpenWindow(AnyWindowHandle);

impl Global for OpenWindow {}

/// The theme's code font before Randori's setting replaced it, shown as the
/// default and put back when the setting is cleared.
#[derive(Clone)]
pub struct ThemeCodeFont {
    pub family: SharedString,
    pub size: f32,
}

impl Global for ThemeCodeFont {}

/// Installed fonts that look monospaced, for the font pickers. Looked up once,
/// off the UI thread.
#[derive(Default)]
pub struct CodeFonts(pub Vec<String>);

impl Global for CodeFonts {}

/// Whether `family` is an installed code font. Unknown until the lookup has
/// finished, so a saved font waits for it.
pub fn font_installed(family: &str, cx: &App) -> bool {
    cx.try_global::<CodeFonts>()
        .is_some_and(|fonts| fonts.0.iter().any(|font| font == family))
}

/// Lists the installed fonts once at startup, in the background (it takes a
/// while on a Mac with many): the code fonts for Settings and the symbol fonts
/// the terminal falls back on, from the one listing.
pub fn find_installed_fonts(cx: &mut App) {
    let text_system = cx.text_system().clone();
    cx.spawn(async move |cx| {
        let fonts = cx
            .background_spawn(async move {
                let names = text_system.all_font_names();
                crate::terminal::remember_symbol_fonts(&names);
                code_fonts(names)
            })
            .await;
        cx.update(|cx| cx.set_global(CodeFonts(fonts)));
    })
    .detach();
}

/// Names that read as a code font: a word programmers' fonts carry, or a
/// well-known family.
fn code_fonts(names: Vec<String>) -> Vec<String> {
    const WORDS: [&str; 6] = ["mono", "code", "nerd font", "courier", "consol", "terminal"];
    const FAMILIES: [&str; 9] = [
        "menlo",
        "monaco",
        "hack",
        "iosevka",
        "inconsolata",
        "d2coding",
        "pragmata",
        "input",
        "fira",
    ];
    let mut fonts: Vec<String> = names
        .into_iter()
        .filter(|name| !name.starts_with('.'))
        .filter(|name| {
            let lower = name.to_lowercase();
            WORDS.iter().any(|word| lower.contains(word))
                || FAMILIES.iter().any(|family| lower.starts_with(family))
        })
        .collect();
    fonts.sort_by_key(|name| name.to_lowercase());
    fonts.dedup();
    fonts
}

/// Opens the Settings window, or brings it forward when it is open.
pub fn open(cx: &mut App) {
    if let Some(handle) = cx.try_global::<OpenWindow>().map(|open| open.0)
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    // on the display Randori is on: without one GPUI picks the primary
    // display, another monitor than the main window's
    let display = cx
        .try_global::<MainApp>()
        .and_then(|main| main.display.get());
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds::centered(
            display,
            size(px(880.), px(640.)),
            cx,
        ))),
        display_id: display,
        window_min_size: Some(size(px(640.), px(420.))),
        titlebar: Some(gpui_kit::TitlebarOptions {
            title: Some("Settings".into()),
            ..Default::default()
        }),
        app_id: Some("io.github.jeonsu0609.randori".into()),
        ..Default::default()
    };
    match gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| SettingsWindow::new(window, cx))
    }) {
        Ok((handle, _)) => cx.set_global(OpenWindow(handle)),
        Err(err) => log::warn!("opening the Settings window: {err:#}"),
    }
}

/// The Settings window, while it is open (screenshots capture it).
#[cfg(feature = "screenshot")]
pub fn window_handle(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<OpenWindow>().map(|open| open.0)
}

/// Whether `handle` is the Settings window: closing the main window quits
/// even while it is open.
pub fn is_settings_window(handle: AnyWindowHandle, cx: &App) -> bool {
    cx.try_global::<OpenWindow>()
        .is_some_and(|open| open.0.window_id() == handle.window_id())
}

struct SettingsWindow {
    _subscriptions: Vec<Subscription>,
}

/// Edits typed but not yet written (they are written on Enter or when the
/// field loses focus), by field: closing the window writes them, since a
/// closing window may not tell its fields they lost focus.
#[derive(Default)]
struct PendingEdits(std::collections::HashMap<&'static str, WriteEdit>);

/// Writes one field's typed value to where it belongs.
type WriteEdit = std::rc::Rc<dyn Fn(&mut App)>;

impl Global for PendingEdits {}

fn pend(field: &'static str, write: WriteEdit, cx: &mut App) {
    cx.default_global::<PendingEdits>().0.insert(field, write);
}

/// Writes the edit of `field` now, if one is waiting.
fn write_pending(field: &'static str, cx: &mut App) {
    let write = cx.default_global::<PendingEdits>().0.remove(field);
    if let Some(write) = write {
        write(cx);
    }
}

fn write_all_pending(cx: &mut App) {
    let pending = std::mem::take(&mut cx.default_global::<PendingEdits>().0);
    for write in pending.into_values() {
        write(cx);
    }
}

impl SettingsWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // the pages read every value from the settings when they are drawn
        // what is typed but not written yet is written before the window goes
        window.on_window_should_close(cx, |_, cx| {
            write_all_pending(cx);
            true
        });
        let _subscriptions = vec![
            cx.observe_global::<settings::Settings>(|_, cx| cx.notify()),
            cx.observe_global::<Theme>(|_, cx| cx.notify()),
            cx.observe_global::<CodeFonts>(|_, cx| cx.notify()),
            cx.on_release_in(window, |_, _, cx| {
                cx.remove_global::<OpenWindow>();
            }),
        ];
        Self { _subscriptions }
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                Pages::new("randori-settings")
                    .sidebar_width(px(200.))
                    .default_selected_index(first_page())
                    .pages([
                        general_page(cx),
                        revisions_page(),
                        diff_page(cx),
                        describe_page(),
                        jj_page(),
                        integration_page(cx),
                    ]),
            )
    }
}

/// The page shown first. Screenshot builds take it from
/// `RANDORI_SETTINGS_PAGE`, so each page can be captured.
fn first_page() -> gpui_kit::component::setting::SelectIndex {
    #[cfg(feature = "screenshot")]
    let page_ix = std::env::var("RANDORI_SETTINGS_PAGE")
        .ok()
        .and_then(|page| page.parse().ok())
        .unwrap_or(0);
    #[cfg(not(feature = "screenshot"))]
    let page_ix = 0;
    gpui_kit::component::setting::SelectIndex {
        page_ix,
        group_ix: None,
    }
}

fn display(cx: &App) -> &DisplaySettings {
    &settings::get(cx).display
}

/// A jj config write failed: say so in the Settings window.
fn report(result: anyhow::Result<()>, cx: &mut App) {
    let Err(err) = result else {
        return;
    };
    log::warn!("changing the jj config: {err:#}");
    if let Some(handle) = cx.try_global::<OpenWindow>().map(|open| open.0) {
        let _ = handle.update(cx, |_, window, cx| {
            window.push_notification(
                Notification::error(format!("{err:#}")).title("Could not change the jj config"),
                cx,
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_fonts_are_picked_by_name() {
        let fonts = code_fonts(vec![
            "Helvetica".into(),
            "JetBrains Mono".into(),
            "Menlo".into(),
            ".SF NS Mono".into(),
            "Fira Code".into(),
            "menlo".into(),
        ]);
        assert_eq!(fonts, vec!["Fira Code", "JetBrains Mono", "Menlo", "menlo"]);
    }
}
