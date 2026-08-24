//! Minimal theme bootstrap.
//!
//! Zed wires fonts and density through the `theme_settings` crate, which drags
//! in `settings` and its whole chain. `theme` deliberately keeps that at arm's
//! length behind `ThemeSettingsProvider`, so a standalone app can supply its
//! own five values and use the entire `ui` design system without it.

use gpui::{App, Font, Pixels, px};
use theme::{LoadThemes, ThemeSettingsProvider, UiDensity};

struct MockThemeSettings {
    ui_font: Font,
    buffer_font: Font,
}

impl ThemeSettingsProvider for MockThemeSettings {
    fn ui_font<'a>(&'a self, _cx: &'a App) -> &'a Font {
        &self.ui_font
    }
    fn buffer_font<'a>(&'a self, _cx: &'a App) -> &'a Font {
        &self.buffer_font
    }
    fn ui_font_size(&self, _cx: &App) -> Pixels {
        px(13.)
    }
    fn buffer_font_size(&self, _cx: &App) -> Pixels {
        px(12.)
    }
    fn ui_density(&self, _cx: &App) -> UiDensity {
        UiDensity::Default
    }
}

pub fn init(cx: &mut App) {
    theme::init(LoadThemes::JustBase, cx);
    theme::set_theme_settings_provider(
        Box::new(MockThemeSettings {
            // Zed ships Zed Plex via its `assets` crate; we fall back to system
            // faces, so this reads close to the screenshots but not identically.
            ui_font: gpui::font("SF Pro Text"),
            buffer_font: gpui::font("SF Mono"),
        }),
        cx,
    );
}
