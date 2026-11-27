#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(feature = "scenes")]
use disksonar::fixtures;
use disksonar::{collector, ui};
use rune::{App, AppSpec};

#[cfg(windows)]
mod elevation;

const TITLE: &str = "disksonar";

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
        println!("{}", disksonar::CPU_PROFILE);
        return;
    }
    if std::env::args().any(|arg| arg == "--version") {
        println!("disksonar {}", env!("CARGO_PKG_VERSION"));
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
    let mut consent = Some(disksonar::consent::Consent::new(&args, resume.is_some()));
    let app = App::new(AppSpec {
        min_size: Some((360.0, 420.0)),
        scenes: SCENES,
        families: FAMILIES,
        on_key: Some(ui::on_key),
        ..AppSpec::new(TITLE, (1100.0, 720.0), ui::Dashboard::default, ui::view)
    });
    let app = app.init(move || {
        let mut state = ui::Dashboard::default();
        if !fixture && let Some(path) = disksonar::settings::user_file() {
            state.preferences = disksonar::settings::Settings::load(&path);
            state.preferences_path = Some(path);
        }
        state
    });
    let app = app.args(args).titlebar(rune::Titlebar::new(40.0));
    let handle = app.handle();
    // Collection starts after the first frame; headless captures never start it.
    let mut started = false;
    let restore = resume;
    let app = app.on_ready(move |_| {
        if !fixture && !std::mem::replace(&mut started, true) {
            let handle = handle.clone();
            #[cfg(windows)]
            let mut consent = consent.take().expect("collector starts once");
            #[cfg(windows)]
            {
                let motion_handle = handle.clone();
                disksonar::settings::observe_motion(move |reduced| {
                    motion_handle.update_if(move |s, tx| {
                        s.system_motion_off = Some(reduced);
                        if s.preferences.motion != disksonar::settings::Motion::System {
                            return false;
                        }
                        if reduced {
                            tx.settle();
                        }
                        true
                    });
                });
            }
            let mut restore = restore;
            collector::spawn(move |update| {
                let now = ui::now_unix();
                let selected = match &update {
                    collector::Update::Disks(disks) if !disks.is_empty() => restore.take(),
                    _ => None,
                };
                #[cfg(windows)]
                let prompt = consent
                    .request(update.needs_permission())
                    .map(|args| (handle.clone(), args));
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
                    if let Some((handle, arguments)) = prompt {
                        let key = state
                            .cards
                            .get(state.selected)
                            .map_or(0, |c| ui::disk_key(&c.id));
                        std::thread::spawn(move || {
                            if elevation::relaunch(key, &arguments) {
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
        eprintln!("disksonar: {error}");
        return;
    }
    eprintln!("disksonar: could not open the window: {error}");
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() };
        let message = wide(disksonar::locale::tr(
            "Não foi possível abrir a janela do disksonar. Verifique se o driver de vídeo está atualizado e se a placa de vídeo é compatível com DirectX 12.",
            "Could not open the disksonar window. Check that your graphics driver is up to date and supports DirectX 12.",
        ));
        let title = wide("disksonar");
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
