#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code))]

mod backend;
mod collector;
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

/// The window could not open (no GPU or DirectX 12). A Windows GUI process has
/// no console, so say it in a message box; elsewhere stderr is enough.
fn report_start_failure(error: &str) {
    eprintln!("chkdsk: {error}");
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() };
        let message = wide(
            "Não foi possível abrir a janela do chkdsk. Verifique se o driver de vídeo está atualizado e se a placa de vídeo é compatível com DirectX 12.",
        );
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
}
