//! Finding a dbt project and reading the few settings Cardinal needs from
//! dbt_project.yml.

use std::path::{Path, PathBuf};

/// Walk up from `start` to the directory holding dbt_project.yml.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("dbt_project.yml").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Directories at most `depth` levels below `start` that hold a dbt_project.yml, for
/// pointing at the project when Cardinal runs from a monorepo's root. Skips dbt's
/// installed packages, build output and hidden directories.
pub fn find_projects_below(start: &Path, depth: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![(start.to_path_buf(), 0)];
    while let Some((dir, d)) = stack.pop() {
        if d > 0 && dir.join("dbt_project.yml").is_file() {
            found.push(dir);
            continue;
        }
        if d == depth {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            let skip = name.starts_with('.')
                || matches!(
                    name.as_ref(),
                    "target" | "dbt_packages" | "dbt_modules" | "node_modules" | "logs"
                );
            if !skip && e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push((e.path(), d + 1));
            }
        }
    }
    found.sort();
    found
}

/// The `profile:` dbt_project.yml names.
pub(crate) fn profile_name(root: &Path) -> Option<String> {
    Some(read_project_yml(root).profile).filter(|p| !p.is_empty())
}

/// Values from dbt_project.yml that Cardinal needs, read without a YAML parser.
pub(super) struct ProjectYml {
    pub(super) name: String,
    profile: String,
    pub(super) target_path: String,
    pub(super) model_paths: Vec<String>,
    pub(super) test_paths: Vec<String>,
}

pub(super) fn read_project_yml(root: &Path) -> ProjectYml {
    let text = std::fs::read_to_string(root.join("dbt_project.yml")).unwrap_or_default();
    let mut yml = ProjectYml {
        name: String::new(),
        profile: String::new(),
        target_path: "target".into(),
        model_paths: vec!["models".into()],
        test_paths: vec!["tests".into()],
    };
    let unquote = |s: &str| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
    // The paths key whose block list (`- models` lines) follows, with the items so far.
    let mut block: Option<(&str, Vec<String>)> = None;
    for line in text.lines() {
        let line = strip_yaml_comment(line).trim_end();
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with([' ', '\t', '-']) {
            if let Some((_, items)) = &mut block
                && let Some(item) = line.trim_start().strip_prefix('-')
            {
                items.push(unquote(item));
            }
            continue;
        }
        if let Some((key, items)) = block.take() {
            yml.set_paths(key, items);
        }
        let Some((key, v)) = line.split_once(':') else {
            continue;
        };
        match key.trim_end() {
            "name" => yml.name = unquote(v),
            "profile" => yml.profile = unquote(v),
            "target-path" => yml.target_path = unquote(v),
            key @ ("model-paths" | "test-paths") if v.trim().is_empty() => {
                block = Some((key, Vec::new()));
            }
            key @ ("model-paths" | "test-paths") => {
                let v = v.trim().trim_start_matches('[').trim_end_matches(']');
                yml.set_paths(key, v.split(',').map(unquote).collect());
            }
            _ => {}
        }
    }
    if let Some((key, items)) = block {
        yml.set_paths(key, items);
    }
    // dbt reads DBT_TARGET_PATH before target-path.
    if let Some(t) = std::env::var_os("DBT_TARGET_PATH").filter(|t| !t.is_empty()) {
        yml.target_path = t.to_string_lossy().into_owned();
    }
    yml
}

impl ProjectYml {
    /// Set `model-paths` or `test-paths`, keeping the default when no path is given.
    fn set_paths(&mut self, key: &str, paths: Vec<String>) {
        let paths: Vec<String> = paths.into_iter().filter(|p| !p.is_empty()).collect();
        if paths.is_empty() {
            return;
        }
        if key == "model-paths" {
            self.model_paths = paths;
        } else {
            self.test_paths = paths;
        }
    }
}

/// `line` without its YAML comment: a `#` at the start or after whitespace, outside
/// quotes. dbt's older starter project has `target-path: "target"  # directory ...`.
pub(crate) fn strip_yaml_comment(line: &str) -> &str {
    let mut quote = None;
    let mut after_space = true;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && after_space => return &line[..i],
            None => {}
        }
        after_space = c.is_whitespace();
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_project_yml_with_comments_and_block_lists() {
        let root = std::env::temp_dir().join(format!("cardinal-yml-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("dbt_project.yml"),
            "# Name your project!\n\
             name: 'jaffle_shop'  # lowercase\n\
             target-path: \"target\"  # directory which will store compiled SQL files\n\
             model-paths:\n  - \"transform\"   # where models live\n  - staging\n\
             test-paths: [\"checks\"]\n\
             clean-targets:         # directories to be removed by `dbt clean`\n  - \"target\"\n\
             vars:\n  hash: 'a#b'\n",
        )
        .unwrap();
        let yml = read_project_yml(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(yml.name, "jaffle_shop");
        assert_eq!(yml.target_path, "target");
        assert_eq!(yml.model_paths, vec!["transform", "staging"]);
        assert_eq!(yml.test_paths, vec!["checks"]);
        assert_eq!(strip_yaml_comment("x: 'a # b' # c"), "x: 'a # b' ");
        assert_eq!(strip_yaml_comment("x: a#b"), "x: a#b");
    }
}
