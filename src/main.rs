use std::{
    sync::mpsc::{self, RecvTimeoutError, Sender},
    thread,
    time::Duration,
};

use anyhow::Result;
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event as TermEvent, KeyEventKind},
};

use dvc_tui::{
    app::{App, Event},
    cli, graph,
    project::Project,
    ui,
};

const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Restores the terminal when dropped: on normal exit, on `?` early return, and during panics.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

fn main() -> Result<()> {
    // Load first: if this fails, the error prints on the normal screen, not the alternate one.
    let dir = cli::project_dir_from_args(std::env::args())?;
    let project = Project::load(&dir)?;
    let graph = graph::build(&project.stages)?; // a cycle error converts into anyhow via `?`

    // `init` enters raw mode + the alternate screen and installs a panic hook that restores them.
    let mut terminal = ratatui::init();
    let _guard = TerminalGuard; // lives until the end of `main`, then `drop` runs
    run(&mut terminal, App::new(project, graph))
}

fn run(terminal: &mut DefaultTerminal, mut app: App) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    spawn_key_thread(tx); // `tx` is *moved* into the function: we can't use it here any more

    while !app.should_quit {
        // The closure borrows `app` immutably; the borrow ends when `draw` returns...
        terminal.draw(|frame| ui::view(&app, frame))?;
        // ...so we may borrow it mutably here.
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(event) => app.update(event),
            Err(RecvTimeoutError::Timeout) => {} // nothing happened; just redraw
            Err(RecvTimeoutError::Disconnected) => break, // the key thread died
        }
    }
    Ok(())
}

/// Forward key presses from the terminal into our event channel.
fn spawn_key_thread(tx: Sender<Event>) {
    thread::spawn(move || {
        // `read` blocks until something happens, which is why this lives on its own thread.
        while let Ok(term_event) = event::read() {
            // Some terminals report both press and release; only act on presses.
            if let TermEvent::Key(key) = term_event
                && key.kind == KeyEventKind::Press
                && tx.send(Event::Key(key)).is_err()
            {
                break; // receiver is gone: the app is shutting down
            }
        }
    });
}
