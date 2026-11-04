//! Node selection: `name`, `path/`, `tag:x`, with `+` prefix for upstream and `+` suffix
//! for downstream, as in dbt.

use crate::project::{Project, RelationId};
use std::collections::BTreeSet;

pub fn select(project: &Project, selectors: &[String]) -> Result<BTreeSet<RelationId>, String> {
    let mut out = BTreeSet::new();
    for raw in selectors.iter().flat_map(|s| s.split_whitespace()) {
        let upstream = raw.starts_with('+');
        let downstream = raw.ends_with('+') && raw.len() > 1;
        let core = raw.trim_start_matches('+').trim_end_matches('+');
        let base: Vec<RelationId> = if let Some(tag) = core.strip_prefix("tag:") {
            project
                .models()
                .filter(|r| r.tags.iter().any(|t| t == tag))
                .map(|r| r.id.clone())
                .collect()
        } else if let Some(path) = core
            .strip_prefix("path:")
            .or(core.contains('/').then_some(core))
        {
            let p = path.trim_end_matches('/');
            project
                .checked()
                .filter(|r| {
                    r.file == p
                        || r.file.starts_with(&format!("{p}/"))
                        || r.file.trim_end_matches(".sql") == p
                })
                .map(|r| r.id.clone())
                .collect()
        } else {
            project
                .find_model(core)
                .map(|r| r.id.clone())
                .into_iter()
                .collect()
        };
        if base.is_empty() {
            return Err(format!("selector `{raw}` matched no models"));
        }
        for id in base {
            if upstream {
                out.extend(
                    project
                        .ancestors(&id)
                        .into_iter()
                        .filter(|a| project.get(a).is_some_and(|r| r.is_model())),
                );
            }
            if downstream {
                out.extend(project.descendants(&id));
            }
            out.insert(id);
        }
    }
    Ok(out)
}

/// Models whose files changed since `files` (paths relative to the project root),
/// plus their descendants.
pub fn changed(project: &Project, files: &[String]) -> BTreeSet<RelationId> {
    let mut out = BTreeSet::new();
    for r in project.checked() {
        if files
            .iter()
            .any(|f| f == &r.file || f.ends_with(&format!("/{}", r.file)))
        {
            out.insert(r.id.clone());
            out.extend(project.descendants(&r.id));
        }
    }
    out
}
