use std::{
    io, panic,
    time::{Duration, Instant},
};

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use crossterm::{
    event::{self, Event as CEvent, KeyCode, KeyEvent},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use tokio::sync::mpsc::{Receiver, Sender};
use tui::{backend::CrosstermBackend, Terminal};

#[derive(Debug, Parser)] // requires `derive` feature
#[clap(name = "nebulous")]
#[clap(about = "A fictional versioning CLI", long_about = None, author = "Jonathan Rothberg")]
struct Cli {
    #[clap(subcommand)]
    subcmd: Option<SubCmd>,
}

#[derive(Debug, Subcommand)]
enum SubCmd {}

#[tokio::main]
async fn main() -> Result<()> {
    println!("Hello, world!");
    let args = Cli::parse();

    match run(&args).await {
        Ok(_) => Ok(()),
        Err(e) => {
            eprintln!("Error in main: {:?}", e);
            Err(e)
        }
    }
}

async fn run(_args: &Cli) -> Result<()> {
    run_ui().await?;

    Ok(())
}

#[derive(Clone, Debug)]
pub enum Event<I> {
    Input(I),
    Tick,
}

async fn run_ui() -> Result<()> {
    enable_raw_mode()?;

    panic::set_hook(Box::new(|info| {
        println!("Panic: {}", info);
        disable_raw_mode().expect("restore terminal raw mode");
    }));

    let mut rx = start_key_events();
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    io::stdout().execute(EnterAlternateScreen)?;
    terminal.clear()?;

    let (mut ui_tx, mut ui_rx): (Sender<Event<KeyEvent>>, Receiver<Event<KeyEvent>>) =
        tokio::sync::mpsc::channel(1);

    loop {
        tokio::select! {
        Some(event) = rx.recv() => {
            match event {
                Event::Input(event) =>
                    match event.code {
                        KeyCode::Char('q') => {
                            disable_raw_mode()?;
                                        io::stdout().execute(LeaveAlternateScreen)?;
                                        terminal.show_cursor()?;
                            break;
                    },
                        _ => {}

                    }

                Event::Tick => {}
            }
        }
        }
    }

    Ok(())
}

fn start_key_events() -> tokio::sync::mpsc::Receiver<Event<KeyEvent>> {
    let (mut tx, mut rx) = tokio::sync::mpsc::channel(1);
    let tick_rate = Duration::from_millis(200);
    tokio::spawn(async move {
        let mut last_tick = Instant::now();
        loop {
            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));

            if event::poll(timeout).expect("poll works") {
                if let CEvent::Key(key) = event::read().expect("can read events") {
                    let _ = tx.send(Event::Input(key)).await;
                }
            }

            if last_tick.elapsed() >= tick_rate {
                if let Ok(_) = tx.send(Event::Tick).await {
                    last_tick = Instant::now();
                }
            }
        }
    });

    rx
}
