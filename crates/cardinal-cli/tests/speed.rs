//! Speed targets from the spec, on a synthetic 1,000-model project. Only meaningful in
//! release builds, so they are ignored by default; CI runs them with
//! `cargo test --release -p cardinal-cli --test speed -- --ignored`.

use cardinal_engine::{Config, Engine};
use cardinal_synth::{SynthSpec, edit_model, generate};
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

/// `check --state` on 1,000 models: analyzing main's artifacts, against reading the
/// shapes main's own check wrote. Both must agree.
#[test]
#[ignore]
fn state_with_and_without_shapes() {
    use cardinal_diag::baseline::Baseline;
    use cardinal_engine::{Analysis, Options};

    let dir = std::env::temp_dir().join(format!("cardinal-speed-state-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = generate(&dir, SynthSpec::default()).unwrap();
    unsafe { std::env::set_var("CARDINAL_ALLOW_STALE", "1") };
    let state = root.join("main-state");
    std::fs::create_dir_all(&state).unwrap();
    for f in ["manifest.json", "catalog.json"] {
        std::fs::copy(root.join("target").join(f), state.join(f)).unwrap();
    }
    // The branch changes a model near the bottom of the DAG.
    edit_model(&root, 10, 1).unwrap();

    let config = Config::default();
    let sev = config.severities(&[], &[]).unwrap();
    let head = cardinal_dbt::load_project(&root).unwrap();
    let selected: BTreeSet<_> = head.models().map(|r| r.id.clone()).collect();
    let head = Analysis::new(&head, &config, selected, false);

    let t = Instant::now();
    let analyzed = head.compare(&config, &sev, &state, false).unwrap();
    let analyzing = t.elapsed();

    // Main's shapes, as main's own `cardinal check` writes them.
    let base = cardinal_dbt::load_state(&root, &state).unwrap();
    let base_selected: BTreeSet<_> = base.models().map(|r| r.id.clone()).collect();
    let base = Analysis::new(&base, &config, base_selected, false);
    let mut shapes = base.shapes();
    let findings = base.findings(&shapes, &Options::all_findings(&sev));
    shapes.findings = Baseline::from_findings(&findings.all);
    shapes.reported_for = base.selected.clone();
    shapes
        .save(&state.join(cardinal_check::SHAPES_FILE))
        .unwrap();

    let t = Instant::now();
    let from_file = head.compare(&config, &sev, &state, false).unwrap();
    let reading = t.elapsed();
    eprintln!(
        "--state on 1,000 models: analyzing main {analyzing:?}, reading main's shapes {reading:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        (analyzed.lost, analyzed.gained, analyzed.kept),
        (from_file.lost, from_file.gained, from_file.kept)
    );
    assert_eq!(analyzed.on_main.entries, from_file.on_main.entries);
    assert!(
        reading < Duration::from_secs(2),
        "--state from shapes {reading:?} exceeds 2 s"
    );
}
