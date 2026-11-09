//! The facts dbt data tests check, attached to the relations they test.

use super::decl::{add_not_null, plain_column, string_list};
use super::resolve::resolve_name;
use super::{DataTest, DeclOrigin, DeclaredKey, NotNullOrigin, Relation, TestKind};
use crate::artifacts::ManifestNode;
use crate::names::RelationId;
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) fn attach_test(n: &ManifestNode, relations: &mut BTreeMap<RelationId, Relation>) {
    let Some(tm) = &n.test_metadata else { return };
    let name = tm.name.as_deref().unwrap_or("");
    let deps: Vec<&String> = n
        .depends_on
        .nodes
        .iter()
        .filter(|d| !d.starts_with("macro."))
        .collect();
    // Tests on sources have no attached_node, and depends_on is sorted, so a test with
    // two inputs (relationships) names the tested relation only in its `model` argument.
    let from_model_arg = || {
        let m = tm.kwargs.get("model")?.as_str()?;
        let inner = m.find("source(").or_else(|| m.find("ref("))?;
        let call = &m[inner..];
        let call = &call[..=call.find(')')?];
        let id = resolve_name(relations, call)?;
        deps.iter().any(|d| **d == id.0).then_some(id.0)
    };
    let target = n
        .attached_node
        .clone()
        .or_else(from_model_arg)
        .or_else(|| deps.first().map(|d| d.to_string()));
    let Some(target) = target.map(RelationId) else {
        return;
    };
    let kw = &tm.kwargs;
    let column = kw
        .get("column_name")
        .and_then(Value::as_str)
        .or(n.column_name.as_deref())
        .or_else(|| kw.get("arg").and_then(Value::as_str))
        .and_then(plain_column);
    let test_name = n.name.clone();
    let other = n
        .depends_on
        .nodes
        .iter()
        .find(|d| !d.starts_with("macro.") && **d != target.0)
        .cloned()
        .map(RelationId);
    // A `where` config limits the test to some rows, so it declares nothing about the
    // relation as a whole.
    let filtered = n
        .config
        .where_
        .as_ref()
        .is_some_and(|w| !w.is_null() && w.as_str() != Some(""));
    let warn_only = n
        .config
        .severity
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("warn"));
    let Some(rel) = relations.get_mut(&target) else {
        return;
    };
    let kind = match name {
        "unique" => match column {
            Some(c) => {
                if !filtered {
                    rel.declared_keys.push(DeclaredKey {
                        cols: vec![c.clone()],
                        origin: DeclOrigin::UniqueTest(test_name.clone()),
                    });
                }
                TestKind::Unique(vec![c])
            }
            None => TestKind::Other,
        },
        "unique_combination_of_columns" => {
            let cols = kw
                .get("combination_of_columns")
                .map(string_list)
                .unwrap_or_default();
            let cols: Vec<String> = cols.iter().filter_map(|c| plain_column(c)).collect();
            if cols.is_empty() {
                TestKind::Other
            } else {
                if !filtered {
                    rel.declared_keys.push(DeclaredKey {
                        cols: cols.clone(),
                        origin: DeclOrigin::UniqueCombination(test_name.clone()),
                    });
                }
                TestKind::Unique(cols)
            }
        }
        "not_null" => match column {
            Some(c) => {
                if !filtered {
                    add_not_null(
                        &mut rel.not_null,
                        c.clone(),
                        NotNullOrigin::Test(test_name.clone()),
                    );
                }
                TestKind::NotNull(c)
            }
            None => TestKind::Other,
        },
        // Checks a declared reference; never declares one.
        "relationships" => {
            let field = kw
                .get("field")
                .and_then(Value::as_str)
                .and_then(plain_column);
            match (column, field, other) {
                (Some(column), Some(field), Some(to)) => {
                    TestKind::Relationships { column, to, field }
                }
                _ => TestKind::Other,
            }
        }
        _ => TestKind::Other,
    };
    rel.tests.push(DataTest {
        name: test_name,
        kind,
        filtered,
        warn_only,
    });
}
