//! Assumptions: the declared facts Cardinal's proofs rest on, and whether anything
//! checks them when dbt runs.
//!
//! Cardinal proves facts about models from facts about their inputs. At the edge of the
//! project (sources, seeds, snapshots, Python models) and wherever the SQL cannot prove a
//! declared fact, the declaration is taken as given. Each such fact is an assumption. A
//! proof is only as good as its assumptions, so every proof lists them, and this module
//! says which ones a dbt test or the warehouse checks.

use crate::project::{DeclOrigin, Kind, NotNullOrigin, Project, Relation, RelationId, TestKind};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Assumption {
    /// No two rows of `relation` agree on `cols`.
    Key {
        relation: RelationId,
        cols: Vec<String>,
    },
    /// `column` of `relation` is never null.
    NotNull {
        relation: RelationId,
        column: String,
    },
    /// Every non-null value of `relation.cols` appears in `to.to_cols`.
    References {
        relation: RelationId,
        cols: Vec<String>,
        to: RelationId,
        to_cols: Vec<String>,
    },
}

impl Assumption {
    pub fn relation(&self) -> &RelationId {
        match self {
            Assumption::Key { relation, .. }
            | Assumption::NotNull { relation, .. }
            | Assumption::References { relation, .. } => relation,
        }
    }

    /// "key (order_id) of shop.orders"
    pub fn describe(&self, display: &dyn Fn(&RelationId) -> String) -> String {
        match self {
            Assumption::Key { relation, cols } => {
                format!("key ({}) of {}", cols.join(", "), display(relation))
            }
            Assumption::NotNull { relation, column } => {
                format!("{}.{column} is not null", display(relation))
            }
            Assumption::References {
                relation,
                cols,
                to,
                to_cols,
            } => format!(
                "{}.{} references {}.{}",
                display(relation),
                tuple(cols),
                display(to),
                tuple(to_cols)
            ),
        }
    }
}

fn tuple(cols: &[String]) -> String {
    if cols.len() == 1 {
        cols[0].clone()
    } else {
        format!("({})", cols.join(", "))
    }
}

/// How an assumption is checked when dbt runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckedBy {
    /// A dbt data test that fails the build.
    Test(String),
    /// A dbt data test with severity: warn, which reports but does not fail the build.
    WarnTest(String),
    /// An enforced model contract whose constraint this warehouse enforces.
    Warehouse(String),
}

impl fmt::Display for CheckedBy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckedBy::Test(t) => write!(f, "test {t}"),
            CheckedBy::WarnTest(t) => write!(f, "test {t} (severity: warn)"),
            CheckedBy::Warehouse(w) => write!(f, "{w}"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// Where the assumption is declared, e.g. "meta.cardinal.grain".
    pub declared_by: Vec<String>,
    pub checked_by: Vec<CheckedBy>,
    /// Tests that look related but do not check it, and why.
    pub caveats: Vec<String>,
    /// Tests of exactly this fact that do not fail the build (severity: warn, or a
    /// `where` filter): fix these rather than adding another.
    #[serde(default)]
    pub weak_tests: Vec<String>,
}

impl Status {
    /// Checked by something that fails the build.
    pub fn is_checked(&self) -> bool {
        self.checked_by
            .iter()
            .any(|c| !matches!(c, CheckedBy::WarnTest(_)))
    }
}

/// Adapters whose warehouse enforces a primary_key constraint in an enforced contract.
const ENFORCES_PRIMARY_KEY: &[&str] = &["postgres"];

impl Project {
    /// Where an assumption is declared and what checks it.
    pub fn assumption_status(&self, a: &Assumption) -> Status {
        let mut st = Status::default();
        let Some(rel) = self.get(a.relation()) else {
            return st;
        };
        let filtered_note =
            |t: &str| format!("test {t} has a `where` filter, so it checks only some rows");
        match a {
            Assumption::Key { cols, .. } => {
                let mut want = cols.clone();
                want.sort();
                for d in &rel.declared_keys {
                    let mut c = d.cols.clone();
                    c.sort();
                    if c == want {
                        st.declared_by.push(d.origin.describe());
                        if d.origin == DeclOrigin::ContractPrimaryKey {
                            self.contract_check(rel, "primary_key", ENFORCES_PRIMARY_KEY, &mut st);
                        }
                    }
                }
                for t in &rel.tests {
                    if let TestKind::Unique(tc) = &t.kind {
                        let mut tc = tc.clone();
                        tc.sort();
                        if tc == want {
                            push_test(t, &mut st, filtered_note(&t.name));
                        }
                    }
                }
            }
            Assumption::NotNull { column, .. } => {
                for o in rel.not_null.get(column).into_iter().flatten() {
                    st.declared_by.push(o.describe());
                    if matches!(
                        o,
                        NotNullOrigin::Contract | NotNullOrigin::ContractPrimaryKey
                    ) {
                        // Every supported warehouse enforces not_null in a contract.
                        self.contract_check(rel, "not_null", &[], &mut st);
                    }
                }
                for t in &rel.tests {
                    if matches!(&t.kind, TestKind::NotNull(c) if c == column) {
                        push_test(t, &mut st, filtered_note(&t.name));
                    }
                }
            }
            Assumption::References {
                cols, to, to_cols, ..
            } => {
                if rel
                    .references
                    .iter()
                    .any(|r| &r.cols == cols && &r.to == to && &r.to_cols == to_cols)
                {
                    st.declared_by.push("meta.cardinal.references".into());
                }
                for t in &rel.tests {
                    if let TestKind::Relationships {
                        column,
                        to: tt,
                        field,
                    } = &t.kind
                        && cols.len() == 1
                        && column == &cols[0]
                        && tt == to
                        && field == &to_cols[0]
                    {
                        push_test(t, &mut st, filtered_note(&t.name));
                    }
                }
            }
        }
        st.declared_by.dedup();
        st
    }

    fn contract_check(&self, rel: &Relation, what: &str, only: &[&str], st: &mut Status) {
        let adapter = self.adapter.to_ascii_lowercase();
        let model = matches!(rel.kind, Kind::Model { .. });
        if !model {
            st.caveats.push(format!(
                "{what} constraints on a {} are not applied",
                kind_name(&rel.kind)
            ));
        } else if !rel.contract_enforced {
            st.caveats.push(format!(
                "the {what} constraint is not applied: the model's contract is not enforced"
            ));
        } else if !only.is_empty() && !only.contains(&adapter.as_str()) {
            st.caveats
                .push(format!("{adapter} does not enforce {what} constraints"));
        } else {
            let c = CheckedBy::Warehouse(format!("the {what} constraint ({adapter})"));
            if !st.checked_by.contains(&c) {
                st.checked_by.push(c);
            }
        }
    }
}

fn kind_name(k: &Kind) -> &'static str {
    match k {
        Kind::Source => "source",
        Kind::Seed => "seed",
        Kind::Snapshot => "snapshot",
        Kind::Ephemeral => "ephemeral model",
        Kind::PythonModel => "Python model",
        Kind::Model { .. } => "model",
    }
}

fn push_test(t: &crate::project::DataTest, st: &mut Status, filtered_note: String) {
    if t.filtered {
        st.caveats.push(filtered_note);
        st.weak_tests.push(t.name.clone());
    } else if t.warn_only {
        st.checked_by.push(CheckedBy::WarnTest(t.name.clone()));
        st.weak_tests.push(t.name.clone());
    } else {
        st.checked_by.push(CheckedBy::Test(t.name.clone()));
    }
}
