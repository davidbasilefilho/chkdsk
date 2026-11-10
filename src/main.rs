#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code))]

mod ata;
mod backend;
mod collector;
mod elevation;
#[cfg(any(test, feature = "scenes"))]
mod fixtures;
mod health;
mod history;
mod model;
mod parse;
mod present;
mod ui;

use rune::{App, AppSpec};

const TITLE: &str = concat!("chkdsk v", env!("CARGO_PKG_VERSION"));

/// Windows draws with its own UI font when it is installed; Inter is the fallback.
const FAMILIES: (&str, &str) = if cfg!(windows) {
    ("Segoe UI Variable Text, Segoe UI, Inter", "Inter")
} else {
    ("Inter", "Inter")
};

#[cfg(feature = "scenes")]
const SCENES: &[ui::Scene] = fixtures::SCENES;
#[cfg(not(feature = "scenes"))]
const SCENES: &[ui::Scene] = &[];

fn main() {
    if std::env::args().any(|arg| arg == "--version") {
        println!("chkdsk {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // Headless captures never touch disks, so they are never elevated.
    if !rune::is_headless()
        && let Some(code) = elevate_or_exit_code()
    {
        std::process::exit(code);
    }

    let app = App::new(AppSpec {
        min_size: Some((640.0, 420.0)),
        scenes: SCENES,
        families: FAMILIES,
        on_key: Some(ui::on_key),
        ..AppSpec::new(TITLE, (1100.0, 720.0), ui::Dashboard::default, ui::view)
    });
    let handle = app.handle();
    // Collection starts after the first frame; headless captures never start it.
    let mut started = false;
    let app = app.on_ready(move |_| {
        if !std::mem::replace(&mut started, true) {
            let handle = handle.clone();
            collector::spawn(move |update| {
                let now = ui::now_unix();
                handle.update_if(move |state, tx| {
                    tx.instant();
                    state.deliver(update, now, tx)
                });
            });
        }
    });
    if let Err(error) = app.try_run() {
        report_start_failure(&error);
        std::process::exit(1);
    }
}

/// Elevate for SMART access. `Some(code)` when the process must exit instead of
/// opening the window: the elevated copy took over, or elevation failed. A
/// Windows GUI process has no console, so a failure is told in a message box.
fn elevate_or_exit_code() -> Option<i32> {
    let decision = elevation::ensure_elevated();
    match &decision {
        elevation::StartupDecision::Cancelled => show_error(
            "A elevação foi cancelada. Abra o chkdsk novamente e autorize o acesso de administrador.",
        ),
        elevation::StartupDecision::Failed(error) => {
            eprintln!("chkdsk: could not elevate: {error}");
            show_error("Não foi possível obter privilégios de administrador para ler os discos.");
        }
        elevation::StartupDecision::Continue | elevation::StartupDecision::Relaunched => {}
    }
    match elevation::startup_action(&decision) {
        elevation::StartupAction::RunInterface => None,
        elevation::StartupAction::ExitSuccess => Some(0),
        elevation::StartupAction::ExitFailure => Some(1),
    }
}

/// The run failed. Headless captures (`--snap`) report on stderr only. A window
/// that could not open (no GPU or DirectX 12) also gets a message box, since a
/// Windows GUI process has no console.
fn report_start_failure(error: &str) {
    if rune::is_headless() {
        eprintln!("chkdsk: {error}");
        return;
    }
    eprintln!("chkdsk: could not open the window: {error}");
    show_error(
        "Não foi possível abrir a janela do chkdsk. Verifique se o driver de vídeo está atualizado e se a placa de vídeo é compatível com DirectX 12.",
    );
}

#[cfg(windows)]
fn show_error(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() };
    let message = wide(message);
    let title = wide("chkdsk");
    // SAFETY: both buffers are NUL-terminated and outlive the call; no owner window.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
fn show_error(_: &str) {}
