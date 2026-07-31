mod app;
mod config;
mod db;
mod editor;
mod highlight;
mod history;
mod results;
mod sqlsplit;
mod tree;
mod ui;
mod util;

use std::io::stdout;
use std::time::Duration;

use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;

use crate::app::{AfterConnect, App};
use crate::config::Config;

fn setup_terminal(mouse: bool) -> std::io::Result<bool> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
    if mouse {
        execute!(stdout(), EnableMouseCapture)?;
    }
    // Kitty keyboard protocol lets us distinguish Ctrl+Enter from Enter.
    let enhanced = matches!(supports_keyboard_enhancement(), Ok(true));
    if enhanced {
        execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    Ok(enhanced)
}

fn restore_terminal(mouse: bool, enhanced: bool) {
    if enhanced {
        let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    }
    if mouse {
        let _ = execute!(stdout(), DisableMouseCapture);
    }
    let _ = execute!(stdout(), DisableBracketedPaste, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

// Two workers is plenty: the work is all async I/O, and keeping a worker free
// means row conversion for a large result never blocks the render loop.
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::io::Result<()> {
    let (cfg, startup_msg) = Config::load();
    let mouse = cfg.settings.mouse;

    // Restore the terminal on panic so the shell isn't left in raw mode.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal(mouse, true);
        default_hook(info);
    }));

    let enhanced = setup_terminal(mouse)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let (db_tx, mut db_rx) = mpsc::unbounded_channel();
    let mut app = App::new(cfg, db_tx, startup_msg);

    // `pgtui <connection>` connects immediately.
    if let Some(name) = std::env::args().nth(1) {
        if let Some(cc) = app.cfg.connections.iter().find(|c| c.name == name).cloned() {
            app.ensure_connected(
                &cc.name,
                &cc.dbname(),
                vec![AfterConnect::SetActive, AfterConnect::ListDatabases],
            );
        } else {
            app.set_status(format!("Unknown connection '{name}'"), true);
        }
    }

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(120));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        terminal.draw(|f| ui::draw(f, &mut app))?;
        tokio::select! {
            ev = events.next() => {
                match ev {
                    Some(Ok(ev)) => app.on_term_event(ev),
                    Some(Err(_)) => {}
                    None => break,
                }
            }
            Some(ev) = db_rx.recv() => {
                app.on_db_event(ev);
                // Drain any additional queued events before redrawing.
                while let Ok(ev) = db_rx.try_recv() {
                    app.on_db_event(ev);
                }
            }
            _ = tick.tick() => app.on_tick(),
        }
        if app.should_quit {
            break;
        }
    }

    crate::history::compact(&app.history, app.cfg.settings.history_limit);
    restore_terminal(mouse, enhanced);
    Ok(())
}
