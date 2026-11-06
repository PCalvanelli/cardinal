//! `--state`: which directory holds the base branch's dbt artifacts, and the comparison
//! with them, logged for `-v`.

use cardinal_engine::{Analysis, BaseShapes, Comparison, Config, Severities};
use std::path::{Path, PathBuf};

/// Where a relative state directory is looked for, in order: the working directory,
/// as dbt resolves it, then the project root.
fn candidates(dir: &Path, root: &Path) -> Vec<PathBuf> {
    if dir.is_absolute() {
        return vec![dir.to_path_buf()];
    }
    let mut out = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join(dir));
    }
    out.push(root.join(dir));
    out
}

fn has_manifest(dir: &Path) -> bool {
    dir.join("manifest.json").is_file()
}

/// The state directory to compare with: `--state`, or else `DBT_STATE`. A relative path
/// is resolved against the working directory first, then the project root.
///
/// `--state` is explicit, so if it has no manifest.json the path is still returned and
/// loading it reports the error. `DBT_STATE` is ambient (dbt reads it too), so it is
/// used only when it names a directory with a manifest.json, and otherwise ignored.
pub fn state_dir(explicit: Option<&Path>, root: &Path) -> Option<PathBuf> {
    if let Some(d) = explicit {
        let c = candidates(d, root);
        return Some(
            c.iter()
                .find(|p| has_manifest(p))
                .or(c.iter().find(|p| p.is_dir()))
                .unwrap_or(c.last().unwrap())
                .clone(),
        );
    }
    let env = std::env::var_os("DBT_STATE").filter(|v| !v.is_empty())?;
    state_from_env(Path::new(&env), root)
}

fn state_from_env(dir: &Path, root: &Path) -> Option<PathBuf> {
    let found = candidates(dir, root).into_iter().find(|p| has_manifest(p));
    if found.is_none() {
        tracing::debug!(
            "ignoring DBT_STATE={}: no manifest.json there",
            dir.display()
        );
    }
    found
}

/// Analyze the base artifacts in `dir` and compare the selected models with their base
/// versions; see [`Analysis::compare`].
pub fn compare(
    head: &Analysis<'_>,
    config: &Config,
    severities: &Severities,
    dir: &Path,
    use_cache: bool,
) -> Result<Comparison, String> {
    let c = head.compare(config, severities, dir, use_cache)?;
    match &c.base {
        BaseShapes::Saved(path) => tracing::debug!("--state: using {}", path.display()),
        BaseShapes::Analyzed(why) => tracing::debug!("--state: analyzing main's artifacts: {why}"),
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cardinal-state-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn env_state_without_manifest_is_ignored() {
        let root = tmp("env");
        assert_eq!(state_from_env(&root.join("missing"), &root), None);
        std::fs::create_dir_all(root.join("empty")).unwrap();
        assert_eq!(state_from_env(Path::new("empty"), &root), None);
        std::fs::create_dir_all(root.join("base")).unwrap();
        std::fs::write(root.join("base/manifest.json"), "{}").unwrap();
        assert_eq!(
            state_from_env(Path::new("base"), &root),
            Some(root.join("base")),
            "relative to the project root when the working directory has none"
        );
        assert_eq!(
            state_from_env(&root.join("base"), Path::new("/nowhere")),
            Some(root.join("base"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_state_is_kept_even_when_missing() {
        let root = tmp("explicit");
        assert_eq!(
            state_dir(Some(Path::new("not-there")), &root),
            Some(root.join("not-there")),
            "loading it then reports the missing manifest"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
