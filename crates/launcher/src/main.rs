mod app;
mod config;
mod remote;
mod runpod;
mod ssh;
mod style;
mod theme;
mod ui;

#[cfg(test)]
mod snapshot_tests;

use anyhow::Result;
use app::App;
use crossterm::{
    event::{Event, EventStream},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::stdout;
use std::time::Duration;

struct CleanTerminalGuard;
impl Drop for CleanTerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
}

/// Repository root, two levels above this crate's manifest directory.
///
/// Every script and bundle the launcher runs lives relative to it; a missing
/// parent is a broken checkout, not a user error, so the path degrades to the
/// current directory instead of panicking.
pub fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let manual_endpoint = if args.len() >= 3 {
        let host = args[1].clone();
        let port: u16 = args[2].parse().unwrap_or(22);
        Some((host, port))
    } else {
        None
    };

    // Setup terminal
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let _guard = CleanTerminalGuard;

    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(manual_endpoint, tx);

    // Initial load
    if app.runpod_client.is_some() {
        app.refresh_pods_and_volumes();
    }

    let mut event_reader = EventStream::new();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(50));

    // Consume the first immediate tick so we don't fire instantaneously on start
    tick_interval.tick().await;

    let mut last_pod_refresh = std::time::Instant::now();
    let mut last_server_poll = std::time::Instant::now();

    loop {
        terminal.draw(|f| ui::draw(f, &app))?;

        tokio::select! {
            // Asynchronous crossterm input event
            maybe_event = event_reader.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        app.handle_key_event(key);
                    }
                    Some(Ok(Event::Resize(_, _))) => {
                        // Terminal will automatically redraw on next loop iteration
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => {}
                    None => break,
                }
            }

            // Asynchronous background task events (API responses, logs, telemetry)
            Some(event) = rx.recv() => {
                app.handle_async_event(event);
                // Drain any additional events already queued in channel
                while let Ok(extra) = rx.try_recv() {
                    app.handle_async_event(extra);
                }
            }

            // Animation tick (50ms) for smooth spinner
            _ = tick_interval.tick() => {
                app.tick_spinner();

                // Periodic auto-refresh pod inventory if enabled
                let refresh_secs = app.config.pod_refresh_interval_secs;
                if refresh_secs > 0 && last_pod_refresh.elapsed() >= Duration::from_secs(refresh_secs) {
                    if app.busy_operation.is_none() && app.runpod_client.is_some() {
                        app.refresh_pods_and_volumes();
                    }
                    last_pod_refresh = std::time::Instant::now();
                }

                // Studio server health + telemetry, and the pod's workspace
                // volume over SSH. Both self-gate; 0 disables the server poll.
                let poll_secs = app.config.server_poll_interval_secs;
                if poll_secs > 0 && last_server_poll.elapsed() >= Duration::from_secs(poll_secs) {
                    app.poll_remote_status();
                    last_server_poll = std::time::Instant::now();
                }
                app.poll_workspace_usage();
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}
