#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(not(windows))]
compile_error!("Este aplicativo funciona somente no Windows.");

mod backend;
mod health;
mod history;
mod model;
mod parse;
mod ui;

use r3bl_tui::{InputEvent, Key, KeyPress, SpecialKey, TerminalWindow};

#[tokio::main]
async fn main() {
    if std::env::args().any(|arg| arg == "--version") {
        println!("chkdsk {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let exit_keys = [
        InputEvent::Keyboard(KeyPress::Plain {
            key: Key::Character('q'),
        }),
        InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Esc),
        }),
    ];
    let app = ui::DashboardApp::new_boxed();
    let result = match TerminalWindow::main_event_loop(app, &exit_keys, ui::Dashboard::default()) {
        Ok(future) => future.await,
        Err(error) => Err(error),
    };
    if result.is_err() {
        eprintln!("Não foi possível iniciar a interface. Verifique o terminal e tente novamente.");
    }
}
