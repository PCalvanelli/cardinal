//! Which models a run reports on, and what analysis needs to read for them.

use cardinal_dbt::{Project, RelationId, select};
use std::collections::BTreeSet;

/// Models to report on, from --select, positional selectors and --changed-since.
pub fn selection(
    project: &Project,
    selectors: &[String],
    changed_since: Option<&str>,
) -> Result<BTreeSet<RelationId>, String> {
    let mut sel = if selectors.is_empty() && changed_since.is_none() {
        project.checked().map(|r| r.id.clone()).collect()
    } else {
        select::select(project, selectors)?
    };
    if let Some(r) = changed_since {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&project.root)
            .args(["diff", "--name-only", "--relative", r])
            .output()
            .map_err(|e| format!("could not run git: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git diff {r} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|s| s.to_string())
            .collect();
        sel.extend(select::changed(project, &files));
    }
    Ok(sel)
}

/// The selected models plus everything upstream of them, which analysis needs.
pub(crate) fn closure(project: &Project, selected: &BTreeSet<RelationId>) -> BTreeSet<RelationId> {
    let mut needed = selected.clone();
    for id in selected {
        needed.extend(project.ancestors(id));
    }
    needed
}
