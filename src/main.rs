mod composer;
mod model;
mod seed;
mod shell;
mod theme_setup;

use gpui::{App, AppContext, Bounds, KeyBinding, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

use crate::composer as key;

/// Keys the comment field claims. Scoped to the `Composer` context so they only
/// bind while it has focus.
fn bind_composer_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", key::Backspace, Some("Composer")),
        KeyBinding::new("delete", key::Delete, Some("Composer")),
        KeyBinding::new("left", key::Left, Some("Composer")),
        KeyBinding::new("right", key::Right, Some("Composer")),
        KeyBinding::new("shift-left", key::SelectLeft, Some("Composer")),
        KeyBinding::new("shift-right", key::SelectRight, Some("Composer")),
        KeyBinding::new("cmd-a", key::SelectAll, Some("Composer")),
        KeyBinding::new("cmd-v", key::Paste, Some("Composer")),
        KeyBinding::new("cmd-c", key::Copy, Some("Composer")),
        KeyBinding::new("cmd-x", key::Cut, Some("Composer")),
        KeyBinding::new("home", key::Home, Some("Composer")),
        KeyBinding::new("end", key::End, Some("Composer")),
        KeyBinding::new("enter", key::Submit, Some("Composer")),
        KeyBinding::new("escape", key::Cancel, Some("Composer")),
        KeyBinding::new("tab", key::Complete, Some("Composer")),
        KeyBinding::new("up", key::MoveUp, Some("Composer")),
        KeyBinding::new("down", key::MoveDown, Some("Composer")),
        KeyBinding::new("ctrl-cmd-space", key::ShowCharacterPalette, Some("Composer")),
    ]);
}

/// Write panics to a file as well as stderr.
///
/// Launched through `open`, the app has no stderr anyone can read, so a panic
/// in a click listener looks exactly like the window vanishing for no reason.
/// Persisting it means the next occurrence is diagnosable from the log alone.
fn install_panic_logger() -> std::path::PathBuf {
    let path = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
        .join("Library/Logs/delta-mock-panic.log");
    let target = path.clone();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        if let Some(dir) = target.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)
        {
            use std::io::Write;
            let _ = writeln!(file, "\n=== delta-mock panic ===\n{info}\n{backtrace}");
            let _ = file.flush();
        }
        previous(info);
    }));
    path
}

fn main() {
    let log = install_panic_logger();
    eprintln!("delta-mock: panics will be recorded to {}", log.display());

    application().run(|cx: &mut App| {
        theme_setup::init(cx);
        bind_composer_keys(cx);

        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|cx| {
                    let mut shell = shell::Shell::new(cx);
                    shell.maybe_start_demo(cx);
                    shell
                })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
