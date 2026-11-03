use super::check::run_loaded;
use super::{EXIT_TOOL, eprint_error, eprint_warning, load};
use crate::cli::CheckArgs;
use notify::{RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// Re-run check whenever target/manifest.json changes. Facts are cached, so a re-run
/// re-analyzes only the models whose compiled SQL or inputs changed.
pub fn run(dir: Option<PathBuf>, args: CheckArgs) -> u8 {
    let (mut project, mut config) = match load(dir.clone()) {
        Ok(x) => x,
        Err(c) => return c,
    };
    let target = project.target_dir.clone();
    let (tx, rx) = mpsc::channel();
    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res
                && ev
                    .paths
                    .iter()
                    .any(|p| p.file_name().is_some_and(|n| n == "manifest.json"))
            {
                let _ = tx.send(());
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                eprint_error(&format!("could not watch {}: {e}", target.display()));
                return EXIT_TOOL;
            }
        };
    if let Err(e) = watcher.watch(&target, RecursiveMode::NonRecursive) {
        eprint_error(&format!("could not watch {}: {e}", target.display()));
        return EXIT_TOOL;
    }
    loop {
        print!("\x1b[2J\x1b[H");
        run_loaded(&project, &config, &args);
        eprintln!(
            "\nWatching {} for changes (Ctrl-C to stop)…",
            target.join("manifest.json").display()
        );
        if rx.recv().is_err() {
            return 0;
        }
        // dbt writes the manifest in several steps; wait for it to settle.
        while rx.recv_timeout(Duration::from_millis(300)).is_ok() {}
        match load(dir.clone()) {
            Ok((p, c)) => {
                project = p;
                config = c;
            }
            Err(_) => {
                eprint_warning("keeping the previous results until the next successful compile")
            }
        }
    }
}
