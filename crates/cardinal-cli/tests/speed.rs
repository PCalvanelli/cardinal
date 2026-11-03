//! Speed targets from the spec, on a synthetic 1,000-model project. Only meaningful in
//! release builds, so they are ignored by default; CI runs them with
//! `cargo test --release -p cardinal-cli --test speed -- --ignored`.

use cardinal_cli::config::Config;
use cardinal_cli::engine::Engine;
use cardinal_cli::synth::{SynthSpec, edit_model, generate};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

fn run(root: &std::path::Path, cache: bool) -> (Duration, usize) {
    let t = Instant::now();
    let p = cardinal_dbt::load_project(root).unwrap();
    let all: BTreeSet<_> = p.relations.keys().cloned().collect();
    let r = Engine::new(&p, &Config::default(), cache).run(&all);
    let planned = r.results.values().filter(|m| m.planned).count();
    (t.elapsed(), planned)
}

#[test]
#[ignore]
fn speed_targets() {
    let dir = std::env::temp_dir().join(format!("cardinal-speed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = generate(&dir, SynthSpec::default()).unwrap();
    unsafe { std::env::set_var("CARDINAL_ALLOW_STALE", "1") };

    let (cold, planned) = run(&root, true);
    assert_eq!(planned, 1000, "every synthetic model should plan");
    let (warm, _) = run(&root, true);
    edit_model(&root, 995, 1).unwrap();
    let (edited, _) = run(&root, true);
    eprintln!("1,000 models: cold {cold:?}, warm {warm:?}, one model edited {edited:?}");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        cold < Duration::from_secs(10),
        "cold full check {cold:?} exceeds 10 s"
    );
    assert!(
        warm < Duration::from_secs(2),
        "warm full check {warm:?} exceeds 2 s"
    );
    assert!(
        edited < Duration::from_millis(200),
        "one-model edit {edited:?} exceeds 200 ms"
    );
}
