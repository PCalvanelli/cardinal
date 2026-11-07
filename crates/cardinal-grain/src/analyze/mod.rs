//! Grain inference: facts computed bottom-up over a model's logical plan, one rule per
//! plan node, on the plan exactly as SqlToRel produced it.

mod aggregate;
mod filter;
mod join;
mod project;
mod union;
mod window;

use crate::evidence::{Evidence, EvidenceArena};
use crate::expr::unalias;
use crate::facts::{
    Cardinality, ColSet, FlagDrop, GrainStatus, Ind, Key, LossSite, RelationFacts, RowLoss,
    UnknownReason,
};
use crate::summary::RelationSummary;
use cardinal_dbt::RelationId;
use cardinal_sql::datafusion_common::tree_node::{TreeNode, TreeNodeRecursion};
use cardinal_sql::datafusion_common::{DFSchema, ScalarValue};
use cardinal_sql::datafusion_expr::logical_plan::{Distinct, JoinType};
use cardinal_sql::datafusion_expr::{Expr, LogicalPlan};
use cardinal_sql::{SourceText, SqlSpan};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// Finished facts for upstream relations, looked up by id.
pub trait UpstreamFacts: Sync {
    fn summary(&self, id: &RelationId) -> Option<&RelationSummary>;
}

/// One join in a model, for G003 and `describe`.
#[derive(Clone, Debug)]
pub struct JoinRecord {
    pub node: usize,
    pub span: Option<SqlSpan>,
    /// None when one side's grain is unknown, so cardinality cannot be decided.
    pub cardinality: Option<Cardinality>,
    pub left: String,
    pub right: String,
    /// Join columns on each side, in pair order.
    pub on_left: Vec<String>,
    pub on_right: Vec<String>,
    /// Each side's smallest key, or None when unknown.
    pub left_grain: Option<Vec<String>>,
    pub right_grain: Option<Vec<String>>,
    pub join_type: JoinType,
    /// Lineage of each side's join columns: left, then right.
    pub sides: [JoinSide; 2],
}

/// What a join's equality columns on one side come from, for `cardinal guess`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinSide {
    /// For each join pair, the relation columns this side's value is read from, as the
    /// SQL proves it (no declared reference).
    pub from: Vec<Vec<(RelationId, String)>>,
    /// For each join pair, relations every row of which this side holds, with the
    /// column paired to the join column.
    pub covers: Vec<Vec<(RelationId, String)>>,
    /// No two rows of this side agree on the join columns.
    pub unique: bool,
    /// Rows of this side may be lost here only because nothing shows that its join
    /// values all appear on the other side: a reference would prove they are kept.
    pub needs_reference: bool,
}

/// A model after analysis: its plan with facts on every node.
pub struct AnalyzedModel {
    pub relation: RelationId,
    pub display: String,
    pub sql: String,
    pub plan: Arc<LogicalPlan>,
    pub arena: EvidenceArena,
    facts: HashMap<usize, Arc<RelationFacts>>,
    pub joins: Vec<JoinRecord>,
    /// Where rows of each relation read may be lost, here or upstream.
    pub losses: Vec<RowLoss>,
}

/// Cap on row losses kept per model.
pub const MAX_LOSSES: usize = 32;

pub fn node_id(p: &LogicalPlan) -> usize {
    p as *const LogicalPlan as usize
}

impl AnalyzedModel {
    pub fn facts(&self, node: &LogicalPlan) -> Option<&Arc<RelationFacts>> {
        self.facts.get(&node_id(node))
    }
    pub fn output(&self) -> &Arc<RelationFacts> {
        self.facts.get(&node_id(&self.plan)).expect("root analyzed")
    }
    pub fn text(&self) -> SourceText<'_> {
        SourceText::new(&self.sql)
    }
}

pub fn analyze(
    relation: RelationId,
    display: String,
    sql: String,
    plan: LogicalPlan,
    upstream: &dyn UpstreamFacts,
) -> AnalyzedModel {
    let plan = Arc::new(plan);
    let mut a = Analyzer {
        relation: relation.clone(),
        sql: &sql,
        upstream,
        arena: EvidenceArena::default(),
        facts: HashMap::new(),
        joins: Vec::new(),
        used_join_spans: BTreeSet::new(),
        losses: Vec::new(),
    };
    a.visit(&plan);
    let Analyzer {
        arena,
        facts,
        joins,
        mut losses,
        ..
    } = a;
    // Local losses first, so messages point at this model's SQL before upstream ones.
    losses.sort_by_key(|l| l.site.model != relation);
    let mut seen = BTreeSet::new();
    losses.retain(|l| seen.insert((l.relation.clone(), l.site.clone())));
    losses.truncate(MAX_LOSSES);
    AnalyzedModel {
        relation,
        display,
        sql: sql.clone(),
        plan,
        arena,
        facts,
        joins,
        losses,
    }
}

struct Analyzer<'a> {
    relation: RelationId,
    sql: &'a str,
    upstream: &'a dyn UpstreamFacts,
    arena: EvidenceArena,
    facts: HashMap<usize, Arc<RelationFacts>>,
    joins: Vec<JoinRecord>,
    used_join_spans: BTreeSet<SqlSpan>,
    losses: Vec<RowLoss>,
}

/// The integer value of a literal, through casts.
fn int_literal(e: &Expr) -> Option<i64> {
    match literal_of(e)? {
        ScalarValue::Int8(Some(v)) => Some(*v as i64),
        ScalarValue::Int16(Some(v)) => Some(*v as i64),
        ScalarValue::Int32(Some(v)) => Some(*v as i64),
        ScalarValue::Int64(Some(v)) => Some(*v),
        ScalarValue::UInt8(Some(v)) => Some(*v as i64),
        ScalarValue::UInt16(Some(v)) => Some(*v as i64),
        ScalarValue::UInt32(Some(v)) => Some(*v as i64),
        ScalarValue::UInt64(Some(v)) => i64::try_from(*v).ok(),
        _ => None,
    }
}

fn literal_of(e: &Expr) -> Option<&ScalarValue> {
    match unalias(e) {
        Expr::Literal(v, _) => Some(v),
        Expr::Cast(c) => literal_of(&c.expr),
        _ => None,
    }
}

fn column_names(schema: &DFSchema, cols: &ColSet) -> Vec<String> {
    cols.iter()
        .map(|c| schema.field(*c).name().clone())
        .collect()
}

impl Analyzer<'_> {
    fn visit(&mut self, plan: &LogicalPlan) -> Arc<RelationFacts> {
        if let Some(f) = self.facts.get(&node_id(plan)) {
            return f.clone();
        }
        let mut facts = self.rule(plan);
        // A soft-delete flag dropped by this node was dropped by this model.
        for d in &mut facts.deleted {
            if d.col.is_none() && d.dropped.is_none() {
                d.dropped = Some(FlagDrop {
                    model: self.relation.clone(),
                    via: d.via.clone(),
                });
            }
        }
        let facts = Arc::new(facts);
        debug_assert_eq!(
            facts.width,
            plan.schema().fields().len(),
            "width mismatch at {}",
            plan.display()
        );
        self.facts.insert(node_id(plan), facts.clone());
        facts
    }

    /// Facts for each `IN (subquery)` in a filter, so checks can ask whether the
    /// subquery's column can be null. The subquery's rows don't become the model's rows,
    /// so the joins and row losses inside it are not recorded as the model's.
    fn visit_in_subqueries(&mut self, predicate: &Expr) {
        let mut subqueries = Vec::new();
        let _ = predicate.apply(|e| {
            if let Expr::InSubquery(s) = e {
                subqueries.push(s.subquery.subquery.clone());
            }
            Ok(TreeNodeRecursion::Continue)
        });
        for plan in subqueries {
            let (joins, losses) = (self.joins.len(), self.losses.len());
            let spans = self.used_join_spans.clone();
            self.visit(&plan);
            self.joins.truncate(joins);
            self.losses.truncate(losses);
            self.used_join_spans = spans;
        }
    }

    fn unsupported(&mut self, plan: &LogicalPlan, op: &str) -> RelationFacts {
        for c in plan.inputs() {
            self.visit(c);
        }
        RelationFacts::unknown(
            plan.schema().fields().len(),
            UnknownReason::UnsupportedOperator(op.into()),
        )
    }

    /// Record that rows of every relation in `covers` may be lost here.
    fn lose(&mut self, covers: &[Ind], at: Option<SqlSpan>, what: &str, because: Option<String>) {
        for c in covers {
            self.losses.push(RowLoss {
                relation: c.relation.clone(),
                site: LossSite {
                    model: self.relation.clone(),
                    span: at,
                    what: what.to_string(),
                    because: because.clone(),
                },
            });
        }
    }

    /// The trimmed source line a span is on, as a span, and its text.
    fn line_of(&self, at: Option<SqlSpan>) -> Option<(SqlSpan, String)> {
        let at = at?;
        let text = SourceText::new(self.sql);
        let line = text.line(at.start.line);
        let lead = line.len() - line.trim_start().len();
        let body = line.trim();
        let start = text.offset(cardinal_sql::LineCol {
            line: at.start.line,
            col: 1,
        }) + lead;
        Some((text.span(start, start + body.len()), body.to_string()))
    }

    fn rule(&mut self, plan: &LogicalPlan) -> RelationFacts {
        match plan {
            LogicalPlan::TableScan(s) => self.table_scan(s),
            LogicalPlan::Projection(p) => self.projection(p),
            LogicalPlan::Filter(f) => {
                self.visit_in_subqueries(&f.predicate);
                self.filter(f)
            }
            LogicalPlan::Join(j) => self.join(j),
            LogicalPlan::Aggregate(a) => self.aggregate(a),
            LogicalPlan::Window(w) => self.window(w),
            LogicalPlan::Distinct(Distinct::All(input)) => {
                let mut f = (*self.visit(input)).clone();
                let why = self.arena.push(Evidence::Distinct { at: None });
                f.keys.push(Key { cols: f.all(), why });
                f.repeats = vec![None; f.width];
                f.normalize();
                f.status = GrainStatus::Known;
                f
            }
            LogicalPlan::Distinct(Distinct::On(d)) => self.distinct_on(d),
            LogicalPlan::Union(u) => self.union(u),
            LogicalPlan::Sort(s) => (*self.visit(&s.input)).clone(),
            LogicalPlan::SubqueryAlias(s) => (*self.visit(&s.input)).clone(),
            LogicalPlan::Repartition(r) => (*self.visit(&r.input)).clone(),
            LogicalPlan::Limit(l) => {
                let mut f = (*self.visit(&l.input)).clone();
                let skips = l.skip.as_deref().is_some_and(|e| int_literal(e) != Some(0));
                if l.fetch.is_some() || skips {
                    let covers = std::mem::take(&mut f.covers);
                    self.lose(&covers, None, "LIMIT", None);
                    f.ranks.clear();
                }
                let fetch = l
                    .fetch
                    .as_deref()
                    .and_then(literal_of)
                    .and_then(|v| match v {
                        ScalarValue::Int64(Some(n)) => Some(*n),
                        ScalarValue::UInt64(Some(n)) => Some(*n as i64),
                        _ => None,
                    });
                if matches!(fetch, Some(n) if n <= 1) {
                    let why = self.arena.push(Evidence::Derived { from: vec![] });
                    f.keys = vec![Key {
                        cols: ColSet::new(),
                        why,
                    }];
                    f.status = GrainStatus::Known;
                }
                f
            }
            LogicalPlan::EmptyRelation(e) => {
                let why = self.arena.push(Evidence::Derived { from: vec![] });
                let width = e.schema.fields().len();
                RelationFacts {
                    keys: vec![Key {
                        cols: ColSet::new(),
                        why,
                    }],
                    status: GrainStatus::Known,
                    ..RelationFacts::unknown(
                        width,
                        UnknownReason::UnsupportedOperator(String::new()),
                    )
                }
            }
            LogicalPlan::Values(_) => self.unsupported(plan, "VALUES"),
            LogicalPlan::Unnest(_) => self.unsupported(plan, "UNNEST"),
            LogicalPlan::RecursiveQuery(_) => self.unsupported(plan, "a recursive CTE"),
            LogicalPlan::Extension(_) => self.unsupported(plan, "an extension operator"),
            _ => self.unsupported(
                plan,
                plan.display()
                    .to_string()
                    .split(':')
                    .next()
                    .unwrap_or("operator"),
            ),
        }
    }
}
