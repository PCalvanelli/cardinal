use super::{EXIT_TOOL, eprint_error, eprint_warning, root};
use crate::config::template;
use derive_diag::baseline::Baseline;
use std::path::PathBuf;

pub fn run(dir: Option<PathBuf>) -> u8 {
    let root = match root(dir) {
        Ok(r) => r,
        Err(c) => return c,
    };
    let path = root.join("derive.toml");
    if path.exists() {
        eprint_error("derive.toml already exists; edit it or delete it first");
        return 1;
    }
    let dialect = match derive_dbt::load_project(&root) {
        Ok(p) => p.adapter,
        Err(e) => {
            eprint_warning(&format!(
                "{e}\n  using the generic dialect; set `dialect` in derive.toml later"
            ));
            "generic".into()
        }
    };
    if let Err(e) = std::fs::write(&path, template(&dialect)) {
        eprint_error(&format!("could not write derive.toml: {e}"));
        return EXIT_TOOL;
    }
    let baseline = root.join(".derive/baseline.json");
    if !baseline.exists()
        && let Err(e) = (Baseline {
            version: 1,
            ..Default::default()
        })
        .save(&baseline)
    {
        eprint_error(&format!("could not write {}: {e}", baseline.display()));
        return EXIT_TOOL;
    }
    println!("Wrote derive.toml (dialect: {dialect}) and an empty .derive/baseline.json.");
    println!("Add .derive/cache/ to .gitignore; commit derive.toml and the baseline.");
    0
}
