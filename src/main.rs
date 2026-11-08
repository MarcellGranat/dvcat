use std::{
    io::{Write, stdout},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use ratatui::{
    DefaultTerminal,
    crossterm::{
        event::{self, Event as TermEvent, KeyEventKind},
        execute,
        terminal::{EnterAlternateScreen, enable_raw_mode},
    },
};

use dvcat::{
    app::{App, Event, PickRequest},
    cli, graph,
    project::Project,
    ui,
};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// How long the key thread waits for input before it looks at the `paused` flag again.
const KEY_POLL: Duration = Duration::from_millis(100);

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
    // An absolute path gives the header a real project name even for `dvcat .`.
    let dir = dir
        .canonicalize()
        .with_context(|| format!("cannot open project directory {}", dir.display()))?;
    let project = Project::load(&dir)?;
    let graph = graph::build(&project.stages)?; // a cycle error converts into anyhow via `?`

    // `init` enters raw mode + the alternate screen and installs a panic hook that restores them.
    let mut terminal = ratatui::init();
    let _guard = TerminalGuard; // lives until the end of `main`, then `drop` runs
    let mut app = App::new(project, graph);
    app.enable_history();
    let result = run(&mut terminal, app);
    // Restore the terminal first, so the message lands on the normal screen and stays visible.
    drop(_guard);
    if let Ok(Some(message)) = &result {
        eprintln!("dvcat: {message}");
    }
    result.map(|_| ())
}

fn run(terminal: &mut DefaultTerminal, mut app: App) -> Result<Option<String>> {
    let (tx, rx) = mpsc::channel();
    // While an editor owns the terminal, the key thread must not read (it would steal keystrokes).
    // An `Arc<AtomicBool>` is a flag both threads can see; `Arc` shares ownership, the atomic
    // makes reading and writing it safe without a lock.
    let paused = Arc::new(AtomicBool::new(false));
    // Each producer gets its own clone of the sender; `tx` stays here for later jobs.
    spawn_key_thread(tx.clone(), Arc::clone(&paused));
    app.spawn_status_check(&tx);
    app.reattach(&tx);

    while !app.should_quit {
        // The closure borrows `app` immutably; the borrow ends when `draw` returns...
        app.tick();
        terminal.draw(|frame| ui::view(&app, frame))?;
        // ...so we may borrow it mutably here.
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(event) => app.dispatch(event, &tx),
            Err(RecvTimeoutError::Timeout) => {} // nothing happened; just redraw
            Err(RecvTimeoutError::Disconnected) => break, // the key thread died
        }
        if let Some(path) = app.editor_request.take() {
            let result = open_in_editor(terminal, &paused, &path);
            app.editor_closed(result.err(), &tx);
        }
        if let Some(request) = app.pick_request.take() {
            let result = pick_with_fzf(terminal, &paused, &request);
            app.file_picked(request.target, result);
        }
    }
    // A run keeps going without us; tell the user once the terminal is back to normal.
    Ok(app.shutdown())
}

/// Give the terminal to `$VISUAL`/`$EDITOR` until it exits, then take it back.
/// `Err` carries a message for the footer.
fn open_in_editor(
    terminal: &mut DefaultTerminal,
    paused: &AtomicBool,
    path: &Path,
) -> Result<(), String> {
    let command = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned());
    // `EDITOR="code -w"` is common, so split the program from its arguments.
    let mut words = command.split_whitespace();
    let program = words.next().unwrap_or("vi");

    paused.store(true, Ordering::SeqCst);
    // Let a key-thread poll that is already in flight finish before the editor starts reading.
    thread::sleep(KEY_POLL * 2);
    ratatui::restore();
    let status = Command::new(program).args(words).arg(path).status();
    // Back to raw mode and the alternate screen; the old frame is stale, so clear it.
    let back = enable_raw_mode().and_then(|()| execute!(stdout(), EnterAlternateScreen));
    paused.store(false, Ordering::SeqCst);
    back.map_err(|error| format!("cannot restore the terminal: {error}"))?;
    let _ = terminal.clear();

    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("{program} exited with {status}")),
        Err(error) => Err(format!("cannot start '{program}': {error} (set $EDITOR)")),
    }
}

/// Give the terminal to `fzf` over the request's items, then take it back. An empty list means
/// nothing was chosen.
fn pick_with_fzf(
    terminal: &mut DefaultTerminal,
    paused: &AtomicBool,
    request: &PickRequest,
) -> Result<Vec<String>, String> {
    paused.store(true, Ordering::SeqCst);
    thread::sleep(KEY_POLL * 2);
    ratatui::restore();
    // fzf draws on the terminal itself; only its answer comes back through the pipe.
    let mut command = Command::new("fzf");
    command.args([
        "--prompt",
        &format!("{} > ", request.prompt),
        "--height",
        "40%",
        "--reverse",
    ]);
    if request.multi {
        command.arg("--multi");
    }
    let spawned = command.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn();
    let result = match spawned {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let list = request.items.join("\n");
                // A thread, so a long list cannot fill the pipe while fzf waits for us to read.
                thread::spawn(move || {
                    let _ = stdin.write_all(list.as_bytes());
                });
            }
            child
                .wait_with_output()
                .map_err(|error| format!("fzf failed: {error}"))
                .map(|output| {
                    // Esc and Ctrl+C make fzf exit non-zero with nothing on stdout.
                    if !output.status.success() {
                        return Vec::new();
                    }
                    String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err("fzf not found; type the path instead".to_owned())
        }
        Err(error) => Err(format!("cannot start fzf: {error}")),
    };
    let back = enable_raw_mode().and_then(|()| execute!(stdout(), EnterAlternateScreen));
    paused.store(false, Ordering::SeqCst);
    back.map_err(|error| format!("cannot restore the terminal: {error}"))?;
    let _ = terminal.clear();
    result
}

/// Forward key presses from the terminal into our event channel.
fn spawn_key_thread(tx: Sender<Event>, paused: Arc<AtomicBool>) {
    thread::spawn(move || {
        loop {
            if paused.load(Ordering::SeqCst) {
                thread::sleep(KEY_POLL / 2);
                continue;
            }
            // `poll` waits a short while instead of blocking forever, so the flag gets rechecked.
            match event::poll(KEY_POLL) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => break,
            }
            let Ok(term_event) = event::read() else {
                break;
            };
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
