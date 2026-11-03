use super::{EXIT_TOOL, eprint_error, eprint_warning, root};
use crate::config::template;
use cardinal_diag::baseline::Baseline;
use std::path::PathBuf;

pub fn run(dir: Option<PathBuf>) -> u8 {
    let root = match root(dir) {
        Ok(r) => r,
        Err(c) => return c,
    };
    let path = root.join("cardinal.toml");
    if path.exists() {
        eprint_error("cardinal.toml already exists; edit it or delete it first");
        return 1;
    }
    let dialect = match cardinal_dbt::load_project(&root) {
        Ok(p) => p.adapter,
        Err(e) => {
            eprint_warning(&format!(
                "{e}\n  using the generic dialect; set `dialect` in cardinal.toml later"
            ));
            "generic".into()
        }
    };
    if let Err(e) = std::fs::write(&path, template(&dialect)) {
        eprint_error(&format!("could not write cardinal.toml: {e}"));
        return EXIT_TOOL;
    }
    let baseline = root.join(".cardinal/baseline.json");
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
    println!("Wrote cardinal.toml (dialect: {dialect}) and an empty .cardinal/baseline.json.");
    println!("Add .cardinal/cache/ to .gitignore; commit cardinal.toml and the baseline.");
    0
}
