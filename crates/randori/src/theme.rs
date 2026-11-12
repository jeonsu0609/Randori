//! Randori's colours over gpui-kit's: its default theme is shadcn's
//! neutral, one near-black (or white) for every surface, so the tab strip,
//! toolbar, side panels and log are told apart by hairlines only. Here each
//! kind of surface has its own shade, in GitKraken's order (in the dark
//! theme the content darkest, the panels a step lighter, the toolbar the
//! lightest), on cool greys with a blue accent:
//!
//! - the log and diff: `background`
//! - side panels (refs, inspector) and headers over the rows: `sidebar`,
//!   `list.head`
//! - the toolbar: `tab.active`, so it continues the selected tab
//! - the tab strip and rail: `title_bar`; the status bar: `status_bar`
//! - menus and dialogs: `popover`, over all of them
//!
//! Colours not named in `theme.json` are gpui-kit's.

use std::rc::Rc;

use gpui_kit::App;
use gpui_kit::component::theme::{Theme, ThemeSet};

const THEMES: &str = include_str!("theme.json");

/// Makes Randori's light and dark themes the ones a mode change loads.
/// After `gpui_kit::init`, and before the first `Theme::change`.
pub fn install(cx: &mut App) {
    let set: ThemeSet = serde_json::from_str(THEMES).expect("theme.json is a theme set");
    let theme = Theme::global_mut(cx);
    for config in set.themes {
        let config = Rc::new(config);
        if config.mode.is_dark() {
            theme.dark_theme = config;
        } else {
            theme.light_theme = config;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::THEMES;

    #[test]
    fn every_colour_is_a_known_key_with_a_hex_value() {
        let raw: serde_json::Value = serde_json::from_str(THEMES).expect("json");
        let set: gpui_kit::component::theme::ThemeSet =
            serde_json::from_str(THEMES).expect("a theme set");
        assert_eq!(set.themes.len(), 2);
        for (raw, parsed) in raw["themes"]
            .as_array()
            .expect("themes")
            .iter()
            .zip(&set.themes)
        {
            let named = raw["colors"].as_object().expect("colors");
            // a key gpui-kit does not know is dropped, and its colour lost
            let kept = serde_json::to_value(&parsed.colors).expect("colors");
            for (key, value) in named {
                assert_eq!(&kept[key], value, "{}: {key}", parsed.name);
                let hex = value.as_str().expect("a string");
                assert!(
                    hex.starts_with('#')
                        && matches!(hex.len(), 7 | 9)
                        && hex[1..].chars().all(|c| c.is_ascii_hexdigit()),
                    "{}: {key} = {hex}",
                    parsed.name
                );
            }
        }
    }
}
