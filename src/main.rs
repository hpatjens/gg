mod app;
mod git;
mod tree;
mod ui;

use anyhow::{Context, Result};
use app::App;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::stdout;
use std::time::Duration;

fn main() -> Result<()> {
    if let Err(e) = git::repo_root() {
        eprintln!("gi: not inside a git repo ({})", e);
        std::process::exit(1);
    }

    enable_raw_mode().context("enable raw mode")?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture).context("enter alt screen")?;

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture);
        prev_hook(info);
    }));

    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend).context("creating terminal")?;

    let result = run(&mut terminal);

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture).ok();
    terminal.show_cursor().ok();

    if let Err(e) = result {
        eprintln!("gi error: {:?}", e);
        std::process::exit(1);
    }
    Ok(())
}

fn run(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    let mut app = App::new()?;
    loop {
        terminal.draw(|f| ui::render(f, &app))?;
        if app.quit {
            break;
        }
        let timeout = if app.pending.is_some() {
            Duration::from_millis(80)
        } else {
            Duration::from_millis(200)
        };
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == event::KeyEventKind::Press {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }
                    if let Err(e) = app.handle_key(key) {
                        app.toast(format!("error: {}", e));
                    }
                }
            }
        }
        app.poll();
    }
    Ok(())
}
