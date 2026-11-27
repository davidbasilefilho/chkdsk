#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(feature = "scenes")]
use chkdsk::fixtures;
use chkdsk::{collector, ui};
use rune::{App, AppSpec};

#[cfg(windows)]
mod elevation;

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
    if std::env::args().nth(1).as_deref() == Some("--build-profile") {
        println!("{}", chkdsk::CPU_PROFILE);
        return;
    }
    if std::env::args().any(|arg| arg == "--version") {
        println!("chkdsk {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let resume = args
        .iter()
        .find_map(|arg| u64::from_str_radix(arg.strip_prefix("--resume-drive=")?, 16).ok());
    args.retain(|arg| !arg.starts_with("--resume-drive="));
    let fixture = args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--snap" | "--scene" | "--fixture" | "--smoke"));
    #[cfg(windows)]
    let theme = match args.as_slice() {
        [] => Some(None),
        [flag, value]
            if flag == "--theme" && ["dark", "light", "system"].contains(&value.as_str()) =>
        {
            Some(Some(value.clone()))
        }
        _ => None,
    };
    let app = App::new(AppSpec {
        min_size: Some((360.0, 420.0)),
        scenes: SCENES,
        families: FAMILIES,
        on_key: Some(ui::on_key),
        ..AppSpec::new(TITLE, (1100.0, 720.0), ui::Dashboard::default, ui::view)
    });
    let app = app.args(args).titlebar(rune::Titlebar::new(40.0));
    let handle = app.handle();
    // Collection starts after the first frame; headless captures never start it.
    let mut started = false;
    let restore = resume;
    #[cfg(windows)]
    let attempted = resume.is_some();
    let app = app.on_ready(move |_| {
        if !fixture && !std::mem::replace(&mut started, true) {
            let handle = handle.clone();
            #[cfg(windows)]
            let theme = theme.clone();
            let mut restore = restore;
            #[cfg(windows)]
            let mut attempted = attempted;
            collector::spawn(move |update| {
                let now = ui::now_unix();
                let selected = match &update {
                    collector::Update::Disks(disks) if !disks.is_empty() => restore.take(),
                    _ => None,
                };
                #[cfg(windows)]
                let prompt = if !attempted && theme.is_some() && update.needs_permission() {
                    attempted = true;
                    Some((handle.clone(), theme.clone().flatten()))
                } else {
                    None
                };
                handle.update_if(move |state, tx| {
                    let changed = state.deliver(update, now, tx);
                    if let Some(key) = selected
                        && let Some(i) = state
                            .cards
                            .iter()
                            .position(|card| ui::disk_key(&card.id) == key)
                    {
                        state.select(i, tx);
                    }
                    #[cfg(windows)]
                    if let Some((handle, theme)) = prompt {
                        let key = state
                            .cards
                            .get(state.selected)
                            .map_or(0, |c| ui::disk_key(&c.id));
                        std::thread::spawn(move || {
                            if elevation::relaunch(key, theme.as_deref()) {
                                handle.update_if(|_, tx| {
                                    tx.quit();
                                    true
                                });
                            }
                        });
                    }
                    changed
                });
            });
        }
    });
    if let Err(error) = app.try_run() {
        report_start_failure(&error);
        std::process::exit(1);
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
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() };
        let message = wide(chkdsk::locale::tr(
            "Não foi possível abrir a janela do chkdsk. Verifique se o driver de vídeo está atualizado e se a placa de vídeo é compatível com DirectX 12.",
            "Could not open the chkdsk window. Check that your graphics driver is up to date and supports DirectX 12.",
        ));
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
