//! Relation names: splitting compiled names, and resolving the names declarations use
//! (`ref(...)`, `source(...)`, plain names) to relations.

use super::decl::RawReference;
use super::{DeclProblem, DeclProblemKind, DeclaredReference, Kind, Relation};
use crate::names::RelationId;
use std::collections::BTreeMap;

/// Split a relation_name like `"db"."main"."orders"` or `` `p.d.t` `` into lowercased parts.
pub fn split_relation_name(name: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in name.chars() {
        match (quote, c) {
            (None, '"' | '`' | '[') => quote = Some(if c == '[' { ']' } else { c }),
            (Some(q), c) if c == q => quote = None,
            (None, '.') => parts.push(std::mem::take(&mut cur)),
            (Some('`'), '.') => parts.push(std::mem::take(&mut cur)),
            (_, c) => cur.push(c),
        }
    }
    parts.push(cur);
    parts
        .into_iter()
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Strip `ref('x')` and `source('a', 'b')` to `x` and `a.b`, so either spelling works.
fn plain_relation_name(name: &str) -> String {
    let t = name.trim();
    let args = |inner: &str| -> Vec<String> {
        inner
            .split(',')
            .map(|a| a.trim().trim_matches(|c| c == '\'' || c == '"').to_string())
            .filter(|a| !a.is_empty())
            .collect()
    };
    for (prefix, sep) in [("ref(", ""), ("source(", ".")] {
        if let Some(inner) = t.strip_prefix(prefix).and_then(|r| r.strip_suffix(')')) {
            let a = args(inner);
            return if sep.is_empty() {
                a.last().cloned().unwrap_or_default()
            } else {
                a.join(sep)
            };
        }
    }
    t.to_string()
}

/// Resolve a relation name as a user writes it in YAML: unique id, `source.table` for
/// sources, or the name of a model, seed or snapshot. Names are case-insensitive.
pub(super) fn resolve_name(
    relations: &BTreeMap<RelationId, Relation>,
    name: &str,
) -> Option<RelationId> {
    let n = plain_relation_name(name);
    if let Some(r) = relations.get(&RelationId(n.clone())) {
        return Some(r.id.clone());
    }
    let lower = n.to_lowercase();
    let hits: Vec<&Relation> = relations
        .values()
        .filter(|r| r.display.to_lowercase() == lower)
        .collect();
    // A model and a source can share a display name only in odd projects; prefer the model.
    match hits.as_slice() {
        [one] => Some(one.id.clone()),
        [] => None,
        many => many
            .iter()
            .filter(|r| r.kind != Kind::Source)
            .map(|r| r.id.clone())
            .next(),
    }
}

/// Turn raw references into resolved ones, checking that every column exists when the
/// column lists are known.
pub(super) fn resolve_references(
    relations: &mut BTreeMap<RelationId, Relation>,
    raw: BTreeMap<RelationId, Vec<RawReference>>,
) {
    for (id, refs) in raw {
        let display = relations[&id].display.clone();
        let mut resolved = Vec::new();
        let mut problems = Vec::new();
        for r in refs {
            let mut to_name = r.to.clone();
            let mut to_cols = r.to_cols.clone();
            let mut to = resolve_name(relations, &to_name);
            // Column-level "relation.column" form.
            if to.is_none()
                && to_cols.is_empty()
                && let Some((rel, col)) = plain_relation_name(&r.to).rsplit_once('.')
                && let Some(t) = resolve_name(relations, rel)
            {
                to = Some(t);
                to_name = rel.to_string();
                to_cols = vec![col.to_lowercase()];
            }
            let Some(to) = to else {
                problems.push(DeclProblem::new(
                    "references",
                    DeclProblemKind::UnknownRelation,
                    format!("`{}` is not a model, seed, snapshot or source", r.to),
                ));
                continue;
            };
            if to_cols.is_empty() {
                to_cols = r.cols.clone();
            }
            let to_cols: Vec<String> = to_cols.iter().map(|c| c.to_lowercase()).collect();
            if to_cols.len() != r.cols.len() {
                problems.push(DeclProblem::new(
                    "references",
                    DeclProblemKind::Malformed,
                    format!(
                        "{} columns reference {} columns of {to_name}",
                        r.cols.len(),
                        to_cols.len()
                    ),
                ));
                continue;
            }
            let missing = |rel: &Relation, cols: &[String]| -> Vec<String> {
                if rel.columns.is_empty() {
                    return vec![];
                }
                cols.iter()
                    .filter(|c| !rel.columns.iter().any(|x| &x.name == *c))
                    .cloned()
                    .collect()
            };
            let m1 = missing(&relations[&id], &r.cols);
            let m2 = missing(&relations[&to], &to_cols);
            if !m1.is_empty() || !m2.is_empty() {
                let mut parts = Vec::new();
                if !m1.is_empty() {
                    parts.push(format!("{display} has no column {}", m1.join(", ")));
                }
                if !m2.is_empty() {
                    parts.push(format!(
                        "{} has no column {}",
                        relations[&to].display,
                        m2.join(", ")
                    ));
                }
                problems.push(DeclProblem::new(
                    "references",
                    DeclProblemKind::MissingColumn,
                    parts.join("; "),
                ));
                continue;
            }
            let d = DeclaredReference {
                cols: r.cols.clone(),
                to,
                to_cols,
                guess: None,
            };
            if !resolved.contains(&d) {
                resolved.push(d);
            }
        }
        if let Some(rel) = relations.get_mut(&id) {
            rel.references = resolved;
            rel.decl_problems.extend(problems);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relation_names_accept_ref_and_source_calls() {
        assert_eq!(plain_relation_name("ref('stg_orders')"), "stg_orders");
        assert_eq!(
            plain_relation_name("ref(\"pkg\", \"stg_orders\")"),
            "stg_orders"
        );
        assert_eq!(
            plain_relation_name("source('shop', 'orders')"),
            "shop.orders"
        );
        assert_eq!(plain_relation_name(" shop.orders "), "shop.orders");
    }

    #[test]
    fn splits_relation_names() {
        assert_eq!(
            split_relation_name("\"Dev\".\"main\".\"orders\""),
            vec!["dev", "main", "orders"]
        );
        assert_eq!(
            split_relation_name("`proj.ds.tbl`"),
            vec!["proj", "ds", "tbl"]
        );
        assert_eq!(
            split_relation_name("ANALYTICS.DBT.ORDERS"),
            vec!["analytics", "dbt", "orders"]
        );
    }
}
