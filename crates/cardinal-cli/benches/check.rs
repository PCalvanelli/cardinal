//! Benchmarks against a synthetic project. The developer-experience targets are
//! enforced separately by tests/speed.rs, which CI runs in release mode.

use cardinal_cli::config::Config;
use cardinal_cli::engine::Engine;
use cardinal_synth::{SynthSpec, edit_model, generate};
use criterion::{Criterion, criterion_group, criterion_main};
use std::collections::BTreeSet;

fn bench(c: &mut Criterion) {
    let dir = std::env::temp_dir().join(format!("cardinal-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = generate(&dir, SynthSpec::default()).unwrap();
    unsafe { std::env::set_var("CARDINAL_ALLOW_STALE", "1") };
    let config = Config::default();
    let mut g = c.benchmark_group("check_1000_models");
    g.sample_size(10);
    g.bench_function("cold", |b| {
        b.iter(|| {
            let p = cardinal_dbt::load_project(&root).unwrap();
            let all: BTreeSet<_> = p.relations.keys().cloned().collect();
            Engine::new(&p, &config, false).run(&all)
        })
    });
    {
        let p = cardinal_dbt::load_project(&root).unwrap();
        let all: BTreeSet<_> = p.relations.keys().cloned().collect();
        Engine::new(&p, &config, true).run(&all);
    }
    g.bench_function("warm", |b| {
        b.iter(|| {
            let p = cardinal_dbt::load_project(&root).unwrap();
            let all: BTreeSet<_> = p.relations.keys().cloned().collect();
            Engine::new(&p, &config, true).run(&all)
        })
    });
    let mut marker = 0;
    g.bench_function("one_model_edited", |b| {
        b.iter(|| {
            marker += 1;
            edit_model(&root, 990, marker).unwrap();
            let p = cardinal_dbt::load_project(&root).unwrap();
            let all: BTreeSet<_> = p.relations.keys().cloned().collect();
            Engine::new(&p, &config, true).run(&all)
        })
    });
    g.finish();
    let _ = std::fs::remove_dir_all(&dir);
}

criterion_group!(benches, bench);
criterion_main!(benches);
