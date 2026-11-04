//! The adapter a project's dbt profile uses, for when there is no manifest yet to read
//! it from. Read from profiles.yml without a YAML parser, like dbt_project.yml.

use crate::project::{profile_name, strip_yaml_comment};
use std::path::{Path, PathBuf};

/// The adapter type of the project's profile, and the profiles.yml it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileAdapter {
    pub adapter: String,
    pub path: PathBuf,
}

/// Where dbt looks for profiles.yml: `$DBT_PROFILES_DIR`, the project, then `~/.dbt`.
fn profiles_candidates(root: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = std::env::var_os("DBT_PROFILES_DIR").filter(|d| !d.is_empty()) {
        v.push(PathBuf::from(d).join("profiles.yml"));
    }
    v.push(root.join("profiles.yml"));
    if let Some(h) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        v.push(PathBuf::from(h).join(".dbt/profiles.yml"));
    }
    v
}

/// The adapter of the profile named in dbt_project.yml, from the first profiles.yml
/// that defines that profile.
pub fn profile_adapter(root: &Path) -> Option<ProfileAdapter> {
    let profile = profile_name(root)?;
    profiles_candidates(root).into_iter().find_map(|path| {
        let text = std::fs::read_to_string(&path).ok()?;
        let adapter = adapter_in(&text, &profile)?;
        Some(ProfileAdapter { adapter, path })
    })
}

struct Line<'a> {
    indent: usize,
    key: &'a str,
    value: &'a str,
}

fn lines(text: &str) -> Vec<Line<'_>> {
    text.lines()
        .filter_map(|l| {
            let l = strip_yaml_comment(l).trim_end();
            let body = l.trim_start();
            if body.is_empty() || body.starts_with('-') {
                return None;
            }
            let (key, value) = body.split_once(':')?;
            Some(Line {
                indent: l.len() - body.len(),
                key: unquote(key),
                value: unquote(value),
            })
        })
        .collect()
}

fn unquote(s: &str) -> &str {
    s.trim().trim_matches(|c| c == '"' || c == '\'')
}

/// The lines nested under `lines[at]`: those after it, up to the next line indented
/// no deeper than it.
fn children<'a, 'b>(lines: &'b [Line<'a>], at: usize) -> &'b [Line<'a>] {
    let indent = lines[at].indent;
    let rest = &lines[at + 1..];
    let end = rest
        .iter()
        .position(|l| l.indent <= indent)
        .unwrap_or(rest.len());
    &rest[..end]
}

/// The direct children of a block: lines at the block's shallowest indent.
fn direct<'a, 'b>(block: &'b [Line<'a>]) -> impl Iterator<Item = (usize, &'b Line<'a>)> {
    let indent = block.iter().map(|l| l.indent).min().unwrap_or(0);
    block
        .iter()
        .enumerate()
        .filter(move |(_, l)| l.indent == indent)
}

/// `type` of the profile's target output. When the target is templated or missing,
/// the type all of its outputs share, if they share one.
fn adapter_in(text: &str, profile: &str) -> Option<String> {
    let all = lines(text);
    let at = all.iter().position(|l| l.indent == 0 && l.key == profile)?;
    let block = children(&all, at);
    let target = direct(block)
        .find(|(_, l)| l.key == "target")
        .map(|(_, l)| l.value)
        .filter(|v| !v.is_empty() && !v.contains("{{"));
    let (oi, _) = direct(block).find(|(_, l)| l.key == "outputs")?;
    let outputs = children(block, oi);
    let mut types: Vec<(&str, &str)> = Vec::new();
    for (i, out) in direct(outputs) {
        let ty = direct(children(outputs, i))
            .find(|(_, l)| l.key == "type")
            .map(|(_, l)| l.value)
            .filter(|v| !v.is_empty() && !v.contains("{{"));
        if let Some(ty) = ty {
            types.push((out.key, ty));
        }
    }
    if let Some(t) = target
        && let Some((_, ty)) = types.iter().find(|(name, _)| *name == t)
    {
        return Some(ty.to_ascii_lowercase());
    }
    let first = types.first()?.1;
    types
        .iter()
        .all(|(_, ty)| ty.eq_ignore_ascii_case(first))
        .then(|| first.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::adapter_in;

    const PROFILES: &str = r#"
config:
  send_anonymous_usage_stats: false

shop:
  target: dev   # the default
  outputs:
    dev:
      type: duckdb
      path: warehouse.duckdb
    prod:
      type: "snowflake"
      account: abc

other:
  outputs:
    a:
      type: bigquery
"#;

    #[test]
    fn reads_the_target_outputs_type() {
        assert_eq!(adapter_in(PROFILES, "shop").as_deref(), Some("duckdb"));
    }

    #[test]
    fn falls_back_to_the_type_every_output_shares() {
        assert_eq!(adapter_in(PROFILES, "other").as_deref(), Some("bigquery"));
        let templated = PROFILES.replace("target: dev", "target: \"{{ env_var('T') }}\"");
        // dev and prod differ, so a templated target decides nothing.
        assert_eq!(adapter_in(&templated, "shop"), None);
    }

    #[test]
    fn unknown_profile_is_none() {
        assert_eq!(adapter_in(PROFILES, "missing"), None);
    }

    #[test]
    fn a_type_key_in_another_block_does_not_count() {
        let text = "p:\n  target: dev\n  outputs:\n    dev:\n      threads: 1\n      extra:\n        type: postgres\n";
        assert_eq!(adapter_in(text, "p"), None);
    }
}
