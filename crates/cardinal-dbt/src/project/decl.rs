//! What a relation declares in its YAML: keys from constraints and tests, and the
//! grain, references, measures and units under `meta.cardinal`.

use super::{
    Column, DeclOrigin, DeclProblem, DeclProblemKind, DeclaredKey, DeclaredMeasure, DeclaredUnit,
    Incremental, MeasureDecl, NotNullOrigin,
};
use crate::artifacts::{ColumnInfo, NodeConfig};
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) fn string_list(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.to_lowercase()],
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str())
            .map(|s| s.to_lowercase())
            .collect(),
        _ => vec![],
    }
}

pub(super) fn incremental_config(config: &NodeConfig) -> Incremental {
    let raw: Vec<String> = match &config.unique_key {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str())
            .map(str::to_string)
            .collect(),
        _ => vec![],
    };
    let cols: Option<Vec<String>> = raw.iter().map(|s| plain_column(s)).collect();
    Incremental {
        unique_key: cols.unwrap_or_default(),
        unique_key_raw: (!raw.is_empty()).then(|| {
            if raw.len() == 1 {
                raw[0].clone()
            } else {
                format!("[{}]", raw.join(", "))
            }
        }),
        strategy: config.incremental_strategy.clone(),
    }
}

pub(super) fn meta_grain(meta: Option<&Value>) -> Option<Vec<String>> {
    let g = meta?.get("cardinal")?.get("grain")?;
    let cols = string_list(g);
    (!cols.is_empty()).then_some(cols)
}

/// A plain column name (possibly quoted), or None for an expression like `lower(email)`.
pub(super) fn plain_column(s: &str) -> Option<String> {
    let t = s
        .trim()
        .trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']');
    (!t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_')).then(|| t.to_lowercase())
}

pub(super) fn yaml_columns(cols: &BTreeMap<String, ColumnInfo>) -> Vec<Column> {
    cols.iter()
        .map(|(k, c)| Column {
            name: c.name.clone().unwrap_or_else(|| k.clone()).to_lowercase(),
            data_type: c.data_type.clone(),
        })
        .collect()
}

pub(super) fn constraint_facts(
    model_constraints: &[crate::artifacts::Constraint],
    cols: &BTreeMap<String, ColumnInfo>,
    keys: &mut Vec<DeclaredKey>,
    not_null: &mut BTreeMap<String, Vec<NotNullOrigin>>,
) {
    for c in model_constraints {
        if c.kind.as_deref() == Some("primary_key")
            && let Some(cs) = &c.columns
        {
            keys.push(DeclaredKey {
                cols: cs.iter().map(|s| s.to_lowercase()).collect(),
                origin: DeclOrigin::ContractPrimaryKey,
            });
            for c in cs {
                add_not_null(
                    not_null,
                    c.to_lowercase(),
                    NotNullOrigin::ContractPrimaryKey,
                );
            }
        }
    }
    for (k, info) in cols {
        let name = info
            .name
            .clone()
            .unwrap_or_else(|| k.clone())
            .to_lowercase();
        for c in &info.constraints {
            match c.kind.as_deref() {
                Some("primary_key") => {
                    keys.push(DeclaredKey {
                        cols: vec![name.clone()],
                        origin: DeclOrigin::ContractPrimaryKey,
                    });
                    add_not_null(not_null, name.clone(), NotNullOrigin::ContractPrimaryKey);
                }
                Some("not_null") => {
                    add_not_null(not_null, name.clone(), NotNullOrigin::Contract);
                }
                _ => {}
            }
        }
    }
}

pub(super) fn add_not_null(
    not_null: &mut BTreeMap<String, Vec<NotNullOrigin>>,
    col: String,
    origin: NotNullOrigin,
) {
    let v = not_null.entry(col).or_default();
    if !v.contains(&origin) {
        v.push(origin);
    }
}

/// meta.cardinal.grain: a key, and (as a primary key) non-null columns.
pub(super) fn grain_facts(
    meta: Option<Vec<String>>,
    keys: &mut Vec<DeclaredKey>,
    not_null: &mut BTreeMap<String, Vec<NotNullOrigin>>,
) {
    if let Some(cols) = meta {
        for c in &cols {
            add_not_null(not_null, c.clone(), NotNullOrigin::MetaGrain);
        }
        keys.push(DeclaredKey {
            cols,
            origin: DeclOrigin::MetaGrain,
        });
    }
}

/// A meta.cardinal.references entry before its target is resolved.
#[derive(Clone, Debug)]
pub(super) struct RawReference {
    pub(super) cols: Vec<String>,
    pub(super) to: String,
    /// Empty: same names as `cols`, or (column-level string form) still to be split.
    pub(super) to_cols: Vec<String>,
}

fn cardinal_meta<'a>(meta: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    meta?.get("cardinal")?.get(key)
}

/// Relation-level `references`: a map or a list of maps with `columns`, `to` and
/// optional `to_columns`.
fn relation_references(v: &Value) -> Result<Vec<RawReference>, String> {
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![v],
        _ => return Err("expected a list of {columns, to, to_columns}".into()),
    };
    items
        .into_iter()
        .map(|it| {
            let cols = it
                .get("columns")
                .or_else(|| it.get("column"))
                .map(string_list)
                .unwrap_or_default();
            let to = it
                .get("to")
                .and_then(Value::as_str)
                .ok_or("an entry has no `to`")?;
            let to_cols = it
                .get("to_columns")
                .or_else(|| it.get("to_column"))
                .map(string_list)
                .unwrap_or_default();
            if cols.is_empty() {
                return Err(format!("the entry for {to} has no `columns`"));
            }
            Ok(RawReference {
                cols,
                to: to.to_string(),
                to_cols,
            })
        })
        .collect()
}

/// Column-level `references`: "relation", "relation.column", or {to, column}.
fn column_reference(col: &str, v: &Value) -> Result<RawReference, String> {
    match v {
        Value::String(s) => Ok(RawReference {
            cols: vec![col.to_string()],
            to: s.clone(),
            to_cols: vec![],
        }),
        Value::Object(_) => {
            let to = v
                .get("to")
                .and_then(Value::as_str)
                .ok_or("expected {to, column}")?;
            let to_col = v
                .get("column")
                .or_else(|| v.get("to_column"))
                .and_then(Value::as_str)
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| col.to_string());
            Ok(RawReference {
                cols: vec![col.to_string()],
                to: to.to_string(),
                to_cols: vec![to_col],
            })
        }
        _ => Err("expected a relation name or {to, column}".into()),
    }
}

/// Every meta.cardinal.references entry on a relation and its columns.
pub(super) fn raw_references(
    meta: Option<&Value>,
    config_meta: Option<&Value>,
    cols: &BTreeMap<String, ColumnInfo>,
    problems: &mut Vec<DeclProblem>,
) -> Vec<RawReference> {
    let bad = |m: String| DeclProblem::new("references", DeclProblemKind::Malformed, m);
    let mut out = Vec::new();
    if let Some(v) =
        cardinal_meta(meta, "references").or_else(|| cardinal_meta(config_meta, "references"))
    {
        match relation_references(v) {
            Ok(r) => out.extend(r),
            Err(e) => problems.push(bad(e)),
        }
    }
    for (k, info) in cols {
        let name = info
            .name
            .clone()
            .unwrap_or_else(|| k.clone())
            .to_lowercase();
        let cm = info.config.as_ref().and_then(|c| c.get("meta"));
        if let Some(v) = cardinal_meta(info.meta.as_ref(), "references")
            .or_else(|| cardinal_meta(cm, "references"))
        {
            match column_reference(&name, v) {
                Ok(r) => out.push(r),
                Err(e) => problems.push(bad(format!("column {name}: {e}"))),
            }
        }
    }
    out
}

/// Every meta.cardinal.measure and meta.cardinal.unit on a relation's columns. Names in
/// `over` and `unit` must be columns of the relation when its columns are known.
pub(super) fn column_measures(
    cols: &BTreeMap<String, ColumnInfo>,
    columns: &[Column],
    problems: &mut Vec<DeclProblem>,
) -> (Vec<DeclaredMeasure>, Vec<DeclaredUnit>) {
    use DeclProblemKind::{Malformed, MissingColumn};
    let known = |n: &str| columns.is_empty() || columns.iter().any(|c| c.name == n);
    let mut measures = Vec::new();
    let mut units = Vec::new();
    for (k, info) in cols {
        let name = info
            .name
            .clone()
            .unwrap_or_else(|| k.clone())
            .to_lowercase();
        let cm = info.config.as_ref().and_then(|c| c.get("meta"));
        let get =
            |key: &str| cardinal_meta(info.meta.as_ref(), key).or_else(|| cardinal_meta(cm, key));
        if let Some(v) = get("measure") {
            match measure_decl(v) {
                Ok(kind) => {
                    let missing: Vec<&String> = match &kind {
                        MeasureDecl::Stock { over } => over.iter().filter(|o| !known(o)).collect(),
                        _ => vec![],
                    };
                    if missing.is_empty() {
                        measures.push(DeclaredMeasure {
                            column: name.clone(),
                            kind,
                        });
                    } else {
                        problems.push(DeclProblem::new(
                            "measure",
                            MissingColumn,
                            format!(
                                "column {name}: measure is over {}, which is not a column",
                                missing
                                    .iter()
                                    .map(|s| s.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        ));
                    }
                }
                Err(e) => problems.push(DeclProblem::new(
                    "measure",
                    Malformed,
                    format!("column {name}: measure {e}"),
                )),
            }
        }
        if let Some(v) = get("unit") {
            match v.as_str().and_then(plain_column) {
                Some(unit) if known(&unit) => units.push(DeclaredUnit {
                    column: name.clone(),
                    unit,
                }),
                Some(unit) => problems.push(DeclProblem::new(
                    "unit",
                    MissingColumn,
                    format!("column {name}: its unit column {unit} is not a column"),
                )),
                None => problems.push(DeclProblem::new(
                    "unit",
                    Malformed,
                    format!("column {name}: unit should name the column that holds its unit"),
                )),
            }
        }
    }
    (measures, units)
}

/// `non_additive`, `{kind: non_additive}`, or `{kind: stock, over: [col, …]}`.
fn measure_decl(v: &Value) -> Result<MeasureDecl, String> {
    let (kind, over) = match v {
        Value::String(s) => (s.as_str(), None),
        Value::Object(_) => (
            v.get("kind")
                .and_then(Value::as_str)
                .ok_or("has no `kind`")?,
            v.get("over"),
        ),
        _ => return Err("should be {kind: stock, over: [...]} or {kind: non_additive}".into()),
    };
    match kind.to_ascii_lowercase().replace('-', "_").as_str() {
        "additive" => Ok(MeasureDecl::Additive),
        "non_additive" | "nonadditive" => Ok(MeasureDecl::NonAdditive),
        "stock" | "semi_additive" | "balance" => {
            let over: Vec<String> = over
                .map(string_list)
                .unwrap_or_default()
                .iter()
                .filter_map(|s| plain_column(s))
                .collect();
            if over.is_empty() {
                Err(format!(
                    "kind {kind} needs `over`: the columns it can't be added across, such as a snapshot date"
                ))
            } else {
                Ok(MeasureDecl::Stock { over })
            }
        }
        other => Err(format!(
            "kind `{other}` is unknown (expected stock, non_additive or additive)"
        )),
    }
}

pub(super) fn exactly_once_names(meta: Option<&Value>, config_meta: Option<&Value>) -> Vec<String> {
    cardinal_meta(meta, "exactly_once")
        .or_else(|| cardinal_meta(config_meta, "exactly_once"))
        .map(|v| match v {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a
                .iter()
                .filter_map(|x| x.as_str())
                .map(str::to_string)
                .collect(),
            _ => vec![],
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reference_forms() {
        let v: Value = serde_json::json!([
            {"columns": ["order_id"], "to": "shop.orders"},
            {"columns": "a", "to": "x", "to_columns": "b"}
        ]);
        let r = relation_references(&v).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].cols, vec!["order_id"]);
        assert!(r[0].to_cols.is_empty(), "defaults to the same names later");
        assert_eq!(r[1].to_cols, vec!["b"]);
        assert!(relation_references(&serde_json::json!([{"to": "x"}])).is_err());
        assert!(relation_references(&serde_json::json!("x")).is_err());

        let c = column_reference("customer_id", &serde_json::json!("shop.customers")).unwrap();
        assert_eq!(
            (c.cols, c.to, c.to_cols.len()),
            (vec!["customer_id".into()], "shop.customers".into(), 0)
        );
        let c = column_reference(
            "cust",
            &serde_json::json!({"to": "customers", "column": "ID"}),
        )
        .unwrap();
        assert_eq!(c.to_cols, vec!["id"]);
        assert!(column_reference("c", &serde_json::json!(3)).is_err());
    }
}
