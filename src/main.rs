#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(not(windows))]
compile_error!("Este aplicativo funciona somente no Windows.");

mod ata;
mod backend;
mod elevation;
mod health;
mod history;
mod model;
mod parse;
mod ui;

use r3bl_tui::{InputEvent, Key, KeyPress, SpecialKey, TerminalWindow};
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    if std::env::args().any(|arg| arg == "--version") {
        println!("chkdsk {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    let elevation_decision = elevation::ensure_elevated();
    match elevation::startup_action(&elevation_decision) {
        elevation::StartupAction::RunInterface => {}
        elevation::StartupAction::ExitSuccess => return ExitCode::SUCCESS,
        elevation::StartupAction::ExitFailure => {
            match elevation_decision {
                elevation::StartupDecision::Cancelled => eprintln!(
                    "A elevação foi cancelada. Execute o CHKDSK novamente e autorize o acesso de administrador."
                ),
                elevation::StartupDecision::Failed(error) => {
                    eprintln!("Não foi possível obter privilégios de administrador: {error}");
                }
                _ => unreachable!("ação de saída incompatível com a decisão de elevação"),
            }
            return ExitCode::FAILURE;
        }
    }

    let _console_modes = elevation::enable_virtual_terminal_processing();

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
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
