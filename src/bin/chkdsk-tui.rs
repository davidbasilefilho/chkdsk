use chkdsk::{
    collector::{self, DiskSnapshot, Update},
    present::{Body, Card},
};
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::{
    io::{self, Write},
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn render(disks: &[DiskSnapshot], selected: usize, now: u64) -> Vec<String> {
    let mut lines = vec!["chkdsk · Saúde dos discos".to_string()];
    for (i, disk) in disks.iter().enumerate() {
        let card = Card::new(disk, now);
        let cursor = if i == selected { ">" } else { " " };
        lines.push(format!(
            "{cursor} {} · {}",
            card.list_name,
            card.status_label()
        ));
        if i == selected {
            lines.push(card.subtitle);
            match card.body {
                Body::Loading => lines.push("Lendo…".into()),
                Body::Message(text) => lines.push(text),
                Body::Health(h) => {
                    lines.push(format!(
                        "Nota: {} · {}",
                        h.score.as_deref().unwrap_or("—"),
                        h.status
                    ));
                    lines.push(format!(
                        "Desgaste: {} · Temperatura: {}",
                        h.wear, h.temperature
                    ));
                    lines.push(format!(
                        "Tempo ligado: {} · Vida estimada: {}",
                        h.power_on, h.lifetime
                    ));
                    lines.push(h.last_reading);
                    lines.extend(h.warnings.into_iter().map(String::from));
                    for row in h.attributes {
                        lines.push(format!(
                            "SMART {} · atual {} · pior {} · bruto {}",
                            row.id, row.current, row.worst, row.raw
                        ));
                    }
                }
            }
        }
    }
    if disks.is_empty() {
        lines.push("Nenhum disco disponível.".into());
    }
    // Device labels are untrusted text, including on a terminal with VT enabled.
    for line in &mut lines {
        line.retain(|c| !c.is_control());
    }
    lines
}

struct Screen;
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

fn main() -> io::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("--version") => {
            println!("chkdsk {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--build-profile") => {
            println!("{}", chkdsk::CPU_PROFILE);
            return Ok(());
        }
        #[cfg(feature = "scenes")]
        Some("--fixture") => {
            println!(
                "{}",
                render(&chkdsk::fixtures::disks(), 0, chkdsk::fixtures::NOW).join("\n")
            );
            return Ok(());
        }
        Some(_) => return Err(io::Error::other("opção desconhecida")),
        None => {}
    }
    terminal::enable_raw_mode()?;
    let _screen = Screen;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, Hide)?;
    let (tx, rx) = mpsc::channel();
    collector::spawn(move |update| {
        let _ = tx.send(update);
    });
    let mut disks = Vec::new();
    let mut notice = "Procurando discos…";
    let mut selected = 0usize;
    let mut scroll = 0usize;
    loop {
        for update in rx.try_iter() {
            match update {
                Update::Disks(items) => {
                    disks = items;
                    notice = "";
                }
                Update::DiscoveryFailed => notice = "Falha ao descobrir discos.",
            }
        }
        selected = selected.min(disks.len().saturating_sub(1));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let lines = render(&disks, selected, now);
        let (width, height) = terminal::size()?;
        let rows = usize::from(height.saturating_sub(2));
        scroll = scroll.min(lines.len().saturating_sub(rows));
        execute!(out, MoveTo(0, 0), Clear(ClearType::All))?;
        for line in lines.iter().skip(scroll).take(rows) {
            write!(
                out,
                "{}\r\n",
                line.chars()
                    .take(width.saturating_sub(1).into())
                    .collect::<String>()
            )?;
        }
        write!(out, "{notice}\r\n←/→ disco · ↑/↓ rolar · q sair")?;
        out.flush()?;
        if event::poll(Duration::from_millis(400))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Left => {
                    selected = selected.saturating_sub(1);
                    scroll = 0;
                }
                KeyCode::Right => {
                    selected = selected.saturating_add(1);
                    scroll = 0;
                }
                KeyCode::Up => scroll = scroll.saturating_sub(1),
                KeyCode::Down => scroll = scroll.saturating_add(1),
                _ => {}
            }
        }
    }
}
