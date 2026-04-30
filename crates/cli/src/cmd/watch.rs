//! `pyllow watch` — re-run `check` on file change.
//!
//! Coarse re-analysis (full project run) on any `.py` write within the
//! project root. Pyllow check is fast enough at current scale (~170ms on
//! a 550-file FastAPI repo) that incremental analysis isn't necessary;
//! when the user notices waiting, that's the signal to add it.

use crate::postprocess::PostFlags;
use crate::report::Format;
use anyhow::{Context, Result};
use colored::Colorize;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(200);

pub fn run(path: PathBuf, format: Format, post: PostFlags) -> Result<()> {
    let (config, project_root) = super::load_config(&path)?;
    let _ = config;

    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(tx)
        .context("creating filesystem watcher")?;
    watcher
        .watch(&project_root, RecursiveMode::Recursive)
        .with_context(|| format!("watching {}", project_root.display()))?;

    print_header(&project_root);
    run_check(&path, format, &post);

    let mut pending: Option<Instant> = None;
    loop {
        match rx.recv_timeout(DEBOUNCE) {
            Ok(Ok(event)) if event_is_relevant(&event) => {
                pending = Some(Instant::now());
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => eprintln!("watcher error: {e}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(t) = pending {
                    if t.elapsed() >= DEBOUNCE {
                        pending = None;
                        clear_screen();
                        print_header(&project_root);
                        run_check(&path, format, &post);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

fn event_is_relevant(event: &notify::Event) -> bool {
    if !matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) {
        return false;
    }
    event.paths.iter().any(|p| is_py_file(p))
}

fn is_py_file(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("py")
}

fn run_check(path: &Path, format: Format, post: &PostFlags) {
    if let Err(e) = super::check::run(path.to_path_buf(), false, format, post.clone()) {
        eprintln!("{} {e:#}", "watch error:".red().bold());
    }
}

fn print_header(project_root: &Path) {
    let now = chrono_now();
    println!(
        "{} {} {} {}",
        "==".dimmed(),
        "pyllow watch".bold(),
        project_root.display().to_string().cyan(),
        now.dimmed()
    );
    println!("{}", "(press Ctrl-C to exit)".dimmed());
}

/// Lightweight wall-clock formatter — avoids pulling in chrono just for the
/// watch header. Format: `HH:MM:SS` (24-hour, local time).
fn chrono_now() -> String {
    use std::time::SystemTime;
    let since_epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // We can't easily get local-tz offset without chrono/time; print epoch
    // seconds modulo a day in HH:MM:SS as UTC. Good enough for "see fresh
    // run started" — humans get the relative ordering.
    let secs_today = since_epoch % 86400;
    let h = (secs_today / 3600) % 24;
    let m = (secs_today / 60) % 60;
    let s = secs_today % 60;
    format!("[{h:02}:{m:02}:{s:02} UTC]")
}

fn clear_screen() {
    // ANSI: clear screen + move cursor home. Works in any modern terminal;
    // gracefully degrades to no-op on dumb terminals.
    print!("\x1B[2J\x1B[H");
}
