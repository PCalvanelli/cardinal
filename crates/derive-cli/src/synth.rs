//! Synthetic dbt projects for benchmarks and scale tests: a manifest and catalog with
//! `models` models over `sources` keyed sources, arranged in `depth` DAG levels, each
//! model reading up to `width` upstream relations through a rotation of SQL shapes.

use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub struct SynthSpec {
    pub models: usize,
    pub sources: usize,
    pub depth: usize,
    pub width: usize,
}

impl Default for SynthSpec {
    fn default() -> Self {
        SynthSpec {
            models: 1000,
            sources: 20,
            depth: 12,
            width: 2,
        }
    }
}

const COLS: &[&str] = &["id", "fk", "v1", "v2"];

fn rel(schema: &str, name: &str) -> String {
    format!("\"synth\".\"{schema}\".\"{name}\"")
}

fn shape(i: usize, a: &str, b: &str) -> String {
    match i % 6 {
        0 => format!("select x.id, x.fk, x.v1, y.v2\nfrom {a} x\nleft join {b} y on x.fk = y.id"),
        1 => format!("select id, fk, sum(v1) as v1, max(v2) as v2\nfrom {a}\ngroup by id, fk"),
        2 => format!("select id, fk, v1, v2\nfrom {a}\nwhere v1 > 0"),
        3 => format!(
            "select id, fk, v1, v2\nfrom {a}\nqualify row_number() over (partition by id order by v2 desc) = 1"
        ),
        4 => format!(
            "select x.id, x.fk, x.v1 + coalesce(y.v1, 0) as v1, x.v2\nfrom {a} x\nleft join {b} y on x.id = y.id"
        ),
        _ => format!(
            "select x.fk as id, x.fk, sum(x.v1) as v1, max(y.v2) as v2\nfrom {a} x\njoin {b} y on x.id = y.id\ngroup by x.fk"
        ),
    }
}

/// Write a synthetic project to `dir` and return its root.
pub fn generate(dir: &Path, spec: SynthSpec) -> std::io::Result<PathBuf> {
    let root = dir.to_path_buf();
    std::fs::create_dir_all(root.join("models"))?;
    std::fs::create_dir_all(root.join("target"))?;
    std::fs::write(
        root.join("dbt_project.yml"),
        "name: synth\nversion: \"1.0.0\"\nprofile: synth\n",
    )?;

    let mut nodes = Map::new();
    let mut sources = Map::new();
    let mut cat_nodes = Map::new();
    let mut cat_sources = Map::new();
    let catalog_cols = || {
        let mut m = Map::new();
        for (i, c) in COLS.iter().enumerate() {
            m.insert(
                c.to_string(),
                json!({"name": c, "type": "INTEGER", "index": i + 1}),
            );
        }
        Value::Object(m)
    };
    for s in 0..spec.sources {
        let id = format!("source.synth.raw.s_{s}");
        sources.insert(
            id.clone(),
            json!({
                "unique_id": id, "name": format!("s_{s}"), "source_name": "raw", "package_name": "synth",
                "original_file_path": "models/sources.yml", "relation_name": rel("raw", &format!("s_{s}")),
                "meta": {"derive": {"grain": ["id"]}}, "columns": {},
            }),
        );
        cat_sources.insert(id, json!({"columns": catalog_cols()}));
    }
    // Level 0 is the sources; models spread evenly over levels 1..=depth.
    let per_level = spec.models.div_ceil(spec.depth.max(1));
    let mut levels: Vec<Vec<(String, String)>> = vec![
        (0..spec.sources)
            .map(|s| {
                (
                    format!("source.synth.raw.s_{s}"),
                    rel("raw", &format!("s_{s}")),
                )
            })
            .collect(),
    ];
    let mut n = 0;
    for _ in 0..spec.depth {
        let mut level = Vec::new();
        for _ in 0..per_level {
            if n >= spec.models {
                break;
            }
            let prev = &levels[levels.len() - 1];
            let older = &levels[(n * 7) % levels.len()];
            let (pa, ra) = &prev[n % prev.len()];
            let (pb, rb) = &older[(n * 3 + 1) % older.len()];
            let parents: Vec<&String> = if spec.width > 1 {
                vec![pa, pb]
            } else {
                vec![pa]
            };
            let compiled = shape(n, ra, if spec.width > 1 { rb } else { ra });
            let raw = compiled
                .replace(ra, "{{ ref('a') }}")
                .replace(rb, "{{ ref('b') }}");
            let name = format!("m_{n}");
            let id = format!("model.synth.{name}");
            let file = format!("models/{name}.sql");
            std::fs::write(root.join(&file), &raw)?;
            nodes.insert(
                id.clone(),
                json!({
                    "unique_id": id, "resource_type": "model", "name": name, "package_name": "synth",
                    "original_file_path": file, "path": format!("{name}.sql"), "language": "sql",
                    "compiled_code": compiled, "raw_code": raw, "relation_name": rel("main", &name),
                    "depends_on": {"nodes": parents.iter().map(|p| p.to_string()).collect::<Vec<_>>()},
                    "config": {"materialized": "view"}, "columns": {}, "tags": [],
                }),
            );
            cat_nodes.insert(id.clone(), json!({"columns": catalog_cols()}));
            level.push((id, rel("main", &name)));
            n += 1;
        }
        levels.push(level);
    }
    let manifest = json!({
        "metadata": {"dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12.json", "adapter_type": "duckdb"},
        "nodes": nodes, "sources": sources,
    });
    std::fs::write(
        root.join("target/catalog.json"),
        serde_json::to_vec(&json!({"nodes": cat_nodes, "sources": cat_sources}))?,
    )?;
    std::fs::write(
        root.join("target/manifest.json"),
        serde_json::to_vec(&manifest)?,
    )?;
    Ok(root)
}

/// Simulate editing one model: change its compiled SQL in the manifest.
pub fn edit_model(root: &Path, index: usize, marker: usize) -> std::io::Result<()> {
    let path = root.join("target/manifest.json");
    let mut m: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    let node = &mut m["nodes"][format!("model.synth.m_{index}")];
    if let Some(code) = node["compiled_code"].as_str() {
        let base = code.split("\n-- edit").next().unwrap_or(code).to_string();
        node["compiled_code"] = Value::String(format!("{base}\n-- edit {marker}"));
    }
    std::fs::write(&path, serde_json::to_vec(&m)?)
}
