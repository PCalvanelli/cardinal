//! Grain inference: facts computed bottom-up over a model's logical plan, one rule per
//! plan node, on the plan exactly as SqlToRel produced it.

use crate::evidence::{Evidence, EvidenceArena, EvidenceId};
use crate::facts::{
    Cardinality, ColSet, DistinctCount, DistinctSite, Fd, GrainStatus, Ind, JoinSite, Key,
    LossSite, MAX_KEYS, RelationFacts, RowLoss, UnknownReason, closure, minimize,
};
use crate::summary::RelationSummary;
use cardinal_dbt::RelationId;
use cardinal_sql::datafusion_common::tree_node::TreeNode;
use cardinal_sql::datafusion_common::{Column, DFSchema, ScalarValue};
use cardinal_sql::datafusion_expr::expr::{Alias, WindowFunction};
use cardinal_sql::datafusion_expr::logical_plan::{
    Aggregate, Distinct, DistinctOn, Filter, Join, JoinType, Projection, TableScan, Union, Window,
};
use cardinal_sql::datafusion_expr::utils::split_conjunction;
use cardinal_sql::datafusion_expr::{
    BinaryExpr, Expr, LogicalPlan, Operator, WindowFunctionDefinition,
};
use cardinal_sql::{CardinalTable, SourceText, SqlSpan};
use std::collections::{BTreeMap, BTreeSet, HashMap};
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

/// How one output column of a projection-like node is computed from its input.
#[derive(Clone, Debug)]
pub(crate) enum Out {
    Col(usize),
    Lit { non_null: bool },
    Expr { cols: ColSet, non_null: bool },
    Opaque { non_null: bool },
}

pub(crate) fn unalias(e: &Expr) -> &Expr {
    match e {
        Expr::Alias(Alias { expr, .. }) => unalias(expr),
        other => other,
    }
}

pub(crate) fn col_index(schema: &DFSchema, c: &Column) -> Option<usize> {
    schema.maybe_index_of_column(c).or_else(|| {
        if c.relation.is_some() {
            return None;
        }
        // Unqualified reference that is unique by name.
        let hits: Vec<usize> = schema
            .fields()
            .iter()
            .enumerate()
            .filter(|(_, f)| f.name() == &c.name)
            .map(|(i, _)| i)
            .collect();
        (hits.len() == 1).then(|| hits[0])
    })
}

/// Input positions of every column an expression reads; None if one cannot be resolved.
pub(crate) fn expr_cols(e: &Expr, schema: &DFSchema) -> Option<ColSet> {
    e.column_refs()
        .into_iter()
        .map(|c| col_index(schema, c))
        .collect()
}

/// Aggregates that count distinct values: summing their results across groups can count
/// a value once per group.
pub const DISTINCT_COUNTS: &[&str] = &["approx_distinct", "approx_count_distinct", "hll"];

pub fn is_distinct_count(name: &str, distinct: bool) -> bool {
    let n = name.to_ascii_lowercase();
    (n == "count" && distinct) || DISTINCT_COUNTS.contains(&n.as_str())
}

/// The input column whose values a distinct-count argument counts: the column itself, or
/// a CASE whose every result is that column or NULL (`case when active then user_id end`).
/// None for any other expression, since it could map different values to one.
fn counted_col(e: &Expr, schema: &DFSchema) -> Option<usize> {
    match unalias(e) {
        Expr::Column(c) => col_index(schema, c),
        Expr::Case(case) => {
            let mut col = None;
            for (_, then) in &case.when_then_expr {
                let c = counted_col(then, schema)?;
                if col.is_some_and(|p| p != c) {
                    return None;
                }
                col = Some(c);
            }
            match case.else_expr.as_deref().map(unalias) {
                None => {}
                Some(Expr::Literal(v, _)) if v.is_null() => {}
                Some(other) => {
                    if counted_col(other, schema)? != col? {
                        return None;
                    }
                }
            }
            col
        }
        _ => None,
    }
}

fn has_nondeterminism(e: &Expr) -> bool {
    e.is_volatile()
        || e.exists(|x| {
            Ok(matches!(
                x,
                Expr::ScalarSubquery(_)
                    | Expr::Exists(_)
                    | Expr::InSubquery(_)
                    | Expr::AggregateFunction(_)
                    | Expr::WindowFunction(_)
                    | Expr::OuterReferenceColumn(..)
                    | Expr::Placeholder(_)
            ))
        })
        .unwrap_or(true)
}

pub(crate) fn classify_out(e: &Expr, schema: &DFSchema, input: &RelationFacts) -> Out {
    let e = unalias(e);
    match e {
        Expr::Column(c) => match col_index(schema, c) {
            Some(p) => Out::Col(p),
            None => Out::Opaque { non_null: false },
        },
        Expr::Literal(v, _) => Out::Lit {
            non_null: !v.is_null(),
        },
        Expr::Cast(c) if matches!(unalias(&c.expr), Expr::Column(_)) => {
            match expr_cols(&c.expr, schema) {
                Some(cols) => {
                    let nn = cols.iter().all(|p| input.not_null.contains(p));
                    Out::Expr { cols, non_null: nn }
                }
                None => Out::Opaque { non_null: false },
            }
        }
        _ if has_nondeterminism(e) => Out::Opaque { non_null: false },
        _ => match expr_cols(e, schema) {
            Some(cols) => Out::Expr {
                cols,
                non_null: false,
            },
            None => Out::Opaque { non_null: false },
        },
    }
}

/// Facts of a node whose columns are each computed from one input row (projection,
/// the group-by part of an aggregate). Works in an extended space of input columns
/// followed by output columns, then reads the output facts back out.
pub(crate) fn project_facts(input: &RelationFacts, outs: &[Out]) -> RelationFacts {
    let n = input.width;
    let m = outs.len();
    let w = n + m;
    let input_all: ColSet = (0..n).collect();
    let mut ext: Vec<Fd> = input.fds.clone();
    for k in &input.keys {
        ext.push(Fd {
            lhs: k.cols.clone(),
            rhs: input_all.clone(),
            why: k.why,
            entity: true,
        });
    }
    let dummy = EvidenceId(0);
    let mut computed = Vec::new();
    for (j, o) in outs.iter().enumerate() {
        let oj = n + j;
        match o {
            Out::Col(p) => {
                ext.push(Fd {
                    lhs: [*p].into(),
                    rhs: [oj].into(),
                    why: dummy,
                    entity: false,
                });
                ext.push(Fd {
                    lhs: [oj].into(),
                    rhs: [*p].into(),
                    why: dummy,
                    entity: false,
                });
            }
            Out::Lit { .. } => {
                ext.push(Fd {
                    lhs: ColSet::new(),
                    rhs: [oj].into(),
                    why: dummy,
                    entity: false,
                });
                computed.push(oj);
            }
            Out::Expr { cols, .. } => {
                ext.push(Fd {
                    lhs: cols.clone(),
                    rhs: [oj].into(),
                    why: dummy,
                    entity: false,
                });
                computed.push(oj);
            }
            Out::Opaque { .. } => computed.push(oj),
        }
    }
    let outs_ext: ColSet = (n..w).collect();
    let cl = |x: &ColSet| closure(w, &[], &ext, x);
    // Drop computed outputs first, then the columns that determine the most, so a
    // dependency keeps its nearest determinant (customer_id, not order_id).
    let mut drop_order = computed.clone();
    let mut plain: Vec<(usize, usize)> = (n..w)
        .filter(|c| !computed.contains(c))
        .map(|c| (cl(&ColSet::from([c])).len(), c))
        .collect();
    plain.sort_by(|a, b| b.cmp(a));
    drop_order.extend(plain.into_iter().map(|(_, c)| c));
    let min = |x: &ColSet, target: &ColSet| minimize(w, &[], &ext, x, target, &drop_order);
    let to_out = |s: &ColSet| -> ColSet { s.iter().filter(|c| **c >= n).map(|c| c - n).collect() };
    let cl_all = cl(&outs_ext);

    // Prefer outputs that copy the target's own columns, so equivalent keys (a.id and
    // b.id after a one-to-one join) stay distinct instead of collapsing into one.
    let direct = |target: &ColSet| -> ColSet {
        outs.iter()
            .enumerate()
            .filter(|(_, o)| matches!(o, Out::Col(p) if target.contains(p)))
            .map(|(j, _)| n + j)
            .collect()
    };
    let best = |target: &ColSet| -> ColSet {
        let d = direct(target);
        if target.is_subset(&cl(&d)) {
            min(&d, target)
        } else {
            min(&outs_ext, target)
        }
    };
    let mut keys = Vec::new();
    for k in &input.keys {
        if k.cols.is_subset(&cl_all) {
            keys.push(Key {
                cols: to_out(&best(&k.cols)),
                why: k.why,
            });
        }
    }
    let mut fds = Vec::new();
    let mut sources: Vec<&Fd> = input.fds.iter().collect();
    let key_fds = input.key_fds();
    sources.extend(key_fds.iter());
    for f in sources {
        if f.lhs.is_subset(&cl_all) {
            let lhs_ext = best(&f.lhs);
            let rhs = to_out(&cl(&lhs_ext));
            let lhs = to_out(&lhs_ext);
            let rhs: ColSet = rhs.difference(&lhs).copied().collect();
            if !rhs.is_empty() {
                fds.push(Fd {
                    lhs,
                    rhs,
                    why: f.why,
                    entity: f.entity,
                });
            }
        }
    }
    for (j, o) in outs.iter().enumerate() {
        match o {
            Out::Lit { .. } => fds.push(Fd {
                lhs: ColSet::new(),
                rhs: [j].into(),
                why: dummy,
                entity: false,
            }),
            Out::Expr { cols, .. } => {
                let mut others = outs_ext.clone();
                others.remove(&(n + j));
                if cols.is_subset(&cl(&others)) {
                    let lhs = to_out(&min(&others, cols));
                    fds.push(Fd {
                        lhs,
                        rhs: [j].into(),
                        why: dummy,
                        entity: false,
                    });
                }
            }
            _ => {}
        }
    }
    let not_null = outs
        .iter()
        .enumerate()
        .filter(|(_, o)| match o {
            Out::Col(p) => input.not_null.contains(p),
            Out::Lit { non_null } | Out::Expr { non_null, .. } | Out::Opaque { non_null } => {
                *non_null
            }
        })
        .map(|(j, _)| j)
        .collect();
    let repeats = outs
        .iter()
        .map(|o| match o {
            Out::Col(p) => input.repeats.get(*p).cloned().flatten(),
            Out::Expr { cols, .. } => cols
                .iter()
                .find_map(|p| input.repeats.get(*p).cloned().flatten()),
            _ => None,
        })
        .collect();
    // Each input column's first plain copy among the outputs.
    let mut copy_of: BTreeMap<usize, usize> = BTreeMap::new();
    for (j, o) in outs.iter().enumerate() {
        if let Out::Col(p) = o {
            copy_of.entry(*p).or_insert(j);
        }
    }
    let to = |c: usize| copy_of.get(&c).copied();
    let nn_why = input
        .nn_why
        .iter()
        .filter_map(|(c, w)| to(*c).map(|j| (j, *w)))
        .collect();
    let mut f = RelationFacts {
        width: m,
        keys,
        fds,
        not_null,
        status: input.status.clone(),
        repeats,
        covers: input.covers.iter().filter_map(|i| i.remap(to)).collect(),
        within: input.within.iter().filter_map(|i| i.remap(to)).collect(),
        ranks: input
            .ranks
            .iter()
            .filter_map(|(r, p)| {
                let p: Option<ColSet> = p.iter().map(|c| to(*c)).collect();
                Some((to(*r)?, p?))
            })
            .collect(),
        nn_why,
        distinct: input.distinct.iter().filter_map(|d| d.remap(to)).collect(),
    };
    f.normalize();
    f.settle_status(input.status.clone());
    f
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

/// How a filter conjunct treats the rows of a relation the input covers.
enum Keeps {
    /// True for every input row.
    All,
    /// Keeps at least one row of every partition of this rank column.
    OnePerPartition(ColSet),
    /// May drop rows.
    Some,
}

/// Classify one filter conjunct against the filter's input facts.
fn keeps(conj: &Expr, schema: &DFSchema, input: &RelationFacts) -> Keeps {
    let col = |e: &Expr| match unalias(e) {
        Expr::Column(c) => col_index(schema, c),
        _ => None,
    };
    match conj {
        Expr::IsNotNull(e) if col(e).is_some_and(|c| input.not_null.contains(&c)) => Keeps::All,
        Expr::Literal(ScalarValue::Boolean(Some(true)), _) => Keeps::All,
        Expr::BinaryExpr(BinaryExpr { left, op, right }) => {
            // Put the column on the left: `1 = rn` is `rn = 1`, `3 >= rn` is `rn <= 3`.
            let (c, op, v) = match (col(left), col(right)) {
                (Some(c), None) => (c, *op, int_literal(right)),
                (None, Some(c)) => match op.swap() {
                    Some(o) => (c, o, int_literal(left)),
                    None => return Keeps::Some,
                },
                _ => return Keeps::Some,
            };
            let Some(v) = v else { return Keeps::Some };
            let Some((_, p)) = input.ranks.iter().find(|(r, _)| *r == c) else {
                return Keeps::Some;
            };
            // Every partition has a row ranked 1.
            let keeps_rank_one = match op {
                Operator::Eq => v == 1,
                Operator::LtEq => v >= 1,
                Operator::Lt => v >= 2,
                _ => false,
            };
            if keeps_rank_one {
                Keeps::OnePerPartition(p.clone())
            } else {
                Keeps::Some
            }
        }
        _ => Keeps::Some,
    }
}

fn literal_of(e: &Expr) -> Option<&ScalarValue> {
    match unalias(e) {
        Expr::Literal(v, _) => Some(v),
        Expr::Cast(c) => literal_of(&c.expr),
        _ => None,
    }
}

fn is_row_number(e: &Expr) -> Option<&WindowFunction> {
    match unalias(e) {
        Expr::WindowFunction(wf) => match &wf.fun {
            WindowFunctionDefinition::WindowUDF(u) if u.name() == "row_number" => Some(wf),
            _ => None,
        },
        _ => None,
    }
}

/// row_number, rank or dense_rank: every partition has a row where the value is 1.
fn is_rank(e: &Expr) -> bool {
    match unalias(e) {
        Expr::WindowFunction(wf) => match &wf.fun {
            WindowFunctionDefinition::WindowUDF(u) => {
                matches!(u.name(), "row_number" | "rank" | "dense_rank")
            }
            _ => false,
        },
        _ => false,
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
        let facts = Arc::new(self.rule(plan));
        debug_assert_eq!(
            facts.width,
            plan.schema().fields().len(),
            "width mismatch at {}",
            plan.display()
        );
        self.facts.insert(node_id(plan), facts.clone());
        facts
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
            LogicalPlan::Filter(f) => self.filter(f),
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

    fn table_scan(&mut self, s: &TableScan) -> RelationFacts {
        let width = s.projected_schema.fields().len();
        let Some(t) = s.source.as_ref().downcast_ref::<CardinalTable>() else {
            return RelationFacts::unknown(
                width,
                UnknownReason::UnsupportedOperator("an unknown table".into()),
            );
        };
        let id = RelationId(t.relation.clone());
        let Some(summary) = self.upstream.summary(&id) else {
            return RelationFacts::unknown(width, UnknownReason::PlanError(id));
        };
        let mut full = summary.to_facts(&mut self.arena);
        self.losses.extend(summary.losses.iter().cloned());
        let pairs: Vec<(usize, String)> = summary
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| (i, c.name.clone()))
            .collect();
        if !pairs.is_empty() {
            let why = self.arena.push(Evidence::Identity {
                relation: id.clone(),
            });
            let ident = Ind {
                relation: id.clone(),
                pairs,
                why,
            };
            // SqlToRel never pushes filters or limits into a scan, but stay safe.
            if s.filters.is_empty() && s.fetch.is_none() {
                full.covers.insert(0, ident.clone());
            }
            full.within.insert(0, ident);
        }
        match &s.projection {
            None => full,
            Some(p) => {
                let outs: Vec<Out> = p.iter().map(|i| Out::Col(*i)).collect();
                project_facts(&full, &outs)
            }
        }
    }

    fn projection(&mut self, p: &Projection) -> RelationFacts {
        let input = self.visit(&p.input);
        let schema = p.input.schema();
        let outs: Vec<Out> = p
            .expr
            .iter()
            .map(|e| classify_out(e, schema, &input))
            .collect();
        project_facts(&input, &outs)
    }

    fn filter(&mut self, f: &Filter) -> RelationFacts {
        let input = self.visit(&f.input);
        let mut facts = (*input).clone();
        let schema = f.input.schema();
        let text = SourceText::new(self.sql);
        let conjuncts = split_conjunction(&f.predicate);
        self.filter_covers(&conjuncts, schema, &input, &mut facts);
        for conj in conjuncts {
            match conj {
                Expr::BinaryExpr(BinaryExpr { left, op, right }) => {
                    let lc = match unalias(left) {
                        Expr::Column(c) => col_index(schema, c),
                        _ => None,
                    };
                    let rc = match unalias(right) {
                        Expr::Column(c) => col_index(schema, c),
                        _ => None,
                    };
                    let span = conj
                        .column_refs()
                        .into_iter()
                        .find_map(|c| c.spans().first())
                        .and_then(|s| SqlSpan::from_df(&s));
                    if *op == Operator::Eq {
                        match (lc, rc, literal_of(left), literal_of(right)) {
                            (Some(c), None, _, Some(v)) | (None, Some(c), Some(v), _)
                                if !v.is_null() =>
                            {
                                let why = self.arena.push(Evidence::EqualityFilter {
                                    at: span,
                                    text: span.map(|s| text.slice(s).to_string()).unwrap_or_else(
                                        || format!("{} = {}", schema.field(c).name(), v),
                                    ),
                                });
                                facts.fds.push(Fd {
                                    lhs: ColSet::new(),
                                    rhs: [c].into(),
                                    why,
                                    entity: false,
                                });
                                facts.prove_not_null(c);
                            }
                            (Some(a), Some(b), _, _) => {
                                let why = self.arena.push(Evidence::EqualityFilter {
                                    at: span,
                                    text: format!(
                                        "{} = {}",
                                        schema.field(a).name(),
                                        schema.field(b).name()
                                    ),
                                });
                                facts.fds.push(Fd {
                                    lhs: [a].into(),
                                    rhs: [b].into(),
                                    why,
                                    entity: false,
                                });
                                facts.fds.push(Fd {
                                    lhs: [b].into(),
                                    rhs: [a].into(),
                                    why,
                                    entity: false,
                                });
                                facts.prove_not_null(a);
                                facts.prove_not_null(b);
                            }
                            _ => {}
                        }
                    } else if matches!(
                        op,
                        Operator::Lt
                            | Operator::LtEq
                            | Operator::Gt
                            | Operator::GtEq
                            | Operator::NotEq
                            | Operator::LikeMatch
                            | Operator::ILikeMatch
                    ) {
                        for c in lc.into_iter().chain(rc) {
                            facts.prove_not_null(c);
                        }
                    }
                }
                Expr::IsNotNull(e)
                | Expr::Like(cardinal_sql::datafusion_expr::expr::Like { expr: e, .. }) => {
                    if let Expr::Column(c) = unalias(e)
                        && let Some(i) = col_index(schema, c)
                    {
                        facts.prove_not_null(i);
                    }
                }
                Expr::Between(b) if !b.negated => {
                    if let Expr::Column(c) = unalias(&b.expr)
                        && let Some(i) = col_index(schema, c)
                    {
                        facts.prove_not_null(i);
                    }
                }
                Expr::InList(l) if !l.negated => {
                    if let Expr::Column(c) = unalias(&l.expr)
                        && let Some(i) = col_index(schema, c)
                    {
                        facts.prove_not_null(i);
                    }
                }
                _ => {}
            }
        }
        facts.normalize();
        facts
    }

    /// Which covers facts survive a filter: all of them when every conjunct is true for
    /// every row, those a rank filter keeps one row per partition of, or none.
    fn filter_covers(
        &mut self,
        conjuncts: &[&Expr],
        schema: &DFSchema,
        input: &RelationFacts,
        facts: &mut RelationFacts,
    ) {
        let mut partition: Option<ColSet> = None;
        let mut drops: Option<&Expr> = None;
        let mut ranked: Option<&Expr> = None;
        for conj in conjuncts {
            match keeps(conj, schema, input) {
                Keeps::All => {}
                Keeps::OnePerPartition(p) if partition.is_none() => {
                    partition = Some(p);
                    ranked = Some(conj);
                }
                // Two rank filters can keep different rows and together keep none.
                _ => {
                    drops = drops.or(Some(conj));
                }
            }
        }
        let span_of = |e: &Expr| {
            e.column_refs()
                .into_iter()
                .find_map(|c| c.spans().first())
                .and_then(|s| SqlSpan::from_df(&s))
        };
        if drops.is_none() && partition.is_none() {
            return;
        }
        facts.ranks.clear();
        let all = std::mem::take(&mut facts.covers);
        let (kept, lost): (Vec<Ind>, Vec<Ind>) = match (&partition, drops) {
            (Some(p), None) => {
                // Keep the pairs on columns the partition determines: each partition's
                // kept row has the same values there as every row of the partition.
                let (cl, why) = input.closure_why(p);
                let mut k = Vec::new();
                let mut l = Vec::new();
                for c in all {
                    match c.remap(|x| cl.contains(&x).then_some(x)) {
                        Some(kept) => k.push(kept),
                        None => l.push(c),
                    }
                }
                let at = ranked.and_then(span_of);
                let text = self
                    .line_of(at)
                    .map(|(_, t)| t)
                    .unwrap_or_else(|| "a rank filter".into());
                let k = k
                    .into_iter()
                    .map(|mut c| {
                        let mut from = vec![c.why];
                        from.extend(why.iter().copied());
                        c.why = self.arena.push(Evidence::Preserved {
                            at,
                            what: format!("`{text}` keeps a row of every partition"),
                            from,
                        });
                        c
                    })
                    .collect();
                (k, l)
            }
            _ => (vec![], all),
        };
        facts.covers = kept;
        if !lost.is_empty() {
            let at = drops.or(ranked).and_then(span_of);
            let (span, what) = match self.line_of(at) {
                Some((s, t)) => (Some(s), format!("filter `{t}`")),
                None => (
                    None,
                    format!(
                        "filter `{}`",
                        drops.or(ranked).map(|e| e.to_string()).unwrap_or_default()
                    ),
                ),
            };
            let because = match (&partition, drops) {
                (Some(_), None) => Some(
                    "the rank filter keeps one row per partition, which does not determine these columns"
                        .to_string(),
                ),
                _ => None,
            };
            self.lose(&lost, span, &what, because);
        }
    }

    fn window(&mut self, w: &Window) -> RelationFacts {
        let input = self.visit(&w.input);
        let schema = w.input.schema();
        let n = input.width;
        let mut f = (*input).clone();
        f.width = n + w.window_expr.len();
        f.repeats.resize(f.width, None);
        let text = SourceText::new(self.sql);
        for (j, e) in w.window_expr.iter().enumerate() {
            let out = n + j;
            let Expr::WindowFunction(wf) = unalias(e) else {
                continue;
            };
            let partition: Option<ColSet> = wf
                .params
                .partition_by
                .iter()
                .map(|p| match unalias(p) {
                    Expr::Column(c) => col_index(schema, c),
                    _ => None,
                })
                .collect();
            if let Some(p) = &partition
                && is_rank(e)
            {
                f.ranks.push((out, p.clone()));
            }
            if is_row_number(e).is_some() {
                f.not_null.insert(out);
                if let Some(p) = &partition {
                    let span = wf
                        .params
                        .partition_by
                        .iter()
                        .flat_map(|p| p.column_refs())
                        .find_map(|c| c.spans().first())
                        .and_then(|s| SqlSpan::from_df(&s))
                        .and_then(|s| text.enclosing_call(s, "over").or(Some(s)));
                    let why = self.arena.push(Evidence::RowNumber {
                        at: span,
                        partition: column_names(schema, p),
                    });
                    let mut cols = p.clone();
                    cols.insert(out);
                    f.keys.push(Key { cols, why });
                    // (partition, row_number) is a key whatever the input is.
                    f.status = GrainStatus::Known;
                }
            } else if wf.params.order_by.is_empty()
                && let Some(p) = partition
                && !has_nondeterminism_args(&wf.params.args)
            {
                // Without ORDER BY the value depends only on the partition.
                let why = self.arena.push(Evidence::Derived { from: vec![] });
                f.fds.push(Fd {
                    lhs: p,
                    rhs: [out].into(),
                    why,
                    entity: false,
                });
            }
        }
        f.normalize();
        f
    }

    fn aggregate(&mut self, a: &Aggregate) -> RelationFacts {
        let input = self.visit(&a.input);
        let schema = a.input.schema();
        if a.group_expr
            .iter()
            .any(|g| matches!(g, Expr::GroupingSet(_)))
        {
            return RelationFacts::unknown(
                a.schema.fields().len(),
                UnknownReason::UnsupportedOperator("GROUPING SETS / ROLLUP / CUBE".into()),
            );
        }
        let mut outs: Vec<Out> = a
            .group_expr
            .iter()
            .map(|e| classify_out(e, schema, &input))
            .collect();
        for e in &a.aggr_expr {
            let non_null =
                matches!(unalias(e), Expr::AggregateFunction(f) if f.func.name() == "count");
            outs.push(Out::Opaque { non_null });
        }
        let mut f = project_facts(&input, &outs);
        let g = a.group_expr.len();
        let group: ColSet = (0..g).collect();
        let at = a
            .group_expr
            .iter()
            .flat_map(|e| e.column_refs())
            .find_map(|c| c.spans().first())
            .and_then(|s| SqlSpan::from_df(&s));
        let why = self.arena.push(Evidence::GroupBy {
            at,
            cols: a
                .group_expr
                .iter()
                .map(|e| e.schema_name().to_string())
                .collect(),
        });
        f.keys.push(Key { cols: group, why });
        f.repeats = vec![None; f.width];
        self.distinct_counts(a, &input, &mut f);
        f.normalize();
        f.status = GrainStatus::Known;
        f
    }

    /// Record each `count(distinct x)` of an aggregate, with the group columns `x`
    /// determines: two groups that count the same value agree on those. Only when the
    /// input's grain is fully known, so a missing dependency cannot invent an overlap.
    fn distinct_counts(&mut self, a: &Aggregate, input: &RelationFacts, f: &mut RelationFacts) {
        if !input.status.is_known() || input.keys.is_empty() {
            return;
        }
        let schema = a.input.schema();
        let g = a.group_expr.len();
        let text = SourceText::new(self.sql);
        for (j, e) in a.aggr_expr.iter().enumerate() {
            let Expr::AggregateFunction(af) = unalias(e) else {
                continue;
            };
            let name = af.func.name().to_ascii_lowercase();
            if !is_distinct_count(&name, af.params.distinct) {
                continue;
            }
            let Some(x) = af
                .params
                .args
                .iter()
                .map(|a| counted_col(a, schema))
                .collect::<Option<ColSet>>()
            else {
                continue;
            };
            if x.is_empty() {
                continue;
            }
            let cl = input.closure(&x);
            let mut agree_on = ColSet::new();
            for (i, ge) in a.group_expr.iter().enumerate() {
                if expr_cols(ge, schema).is_some_and(|c| c.is_subset(&cl)) {
                    agree_on.insert(i);
                }
            }
            if agree_on.len() == g {
                // Every group holds its own values: the counts never overlap.
                continue;
            }
            let span = af
                .params
                .args
                .iter()
                .flat_map(|a| a.column_refs())
                .find_map(|c| c.spans().first())
                .and_then(|s| SqlSpan::from_df(&s))
                .map(|s| text.enclosing_call(s, &name).unwrap_or(s));
            let per: Vec<String> = a
                .group_expr
                .iter()
                .map(|e| match unalias(e) {
                    Expr::Column(c) => c.name.clone(),
                    other => other.schema_name().to_string(),
                })
                .collect();
            let site = DistinctSite {
                relation: self.relation.clone(),
                span,
                counted: column_names(schema, &x),
                across: per
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !agree_on.contains(i))
                    .map(|(_, n)| n.clone())
                    .collect(),
                per,
            };
            f.distinct.push(DistinctCount {
                col: g + j,
                agree_on,
                site: Arc::new(site),
            });
        }
    }

    fn distinct_on(&mut self, d: &DistinctOn) -> RelationFacts {
        let input = self.visit(&d.input);
        let schema = d.input.schema();
        let outs: Vec<Out> = d
            .select_expr
            .iter()
            .map(|e| classify_out(e, schema, &input))
            .collect();
        let mut f = project_facts(&input, &outs);
        let key: Option<ColSet> = d
            .on_expr
            .iter()
            .map(|on| d.select_expr.iter().position(|s| unalias(s) == unalias(on)))
            .collect();
        f.ranks.clear();
        let all = std::mem::take(&mut f.covers);
        let (kept, lost): (Vec<Ind>, Vec<Ind>) = match &key {
            Some(cols) => {
                let cl = f.closure(cols);
                let mut k = Vec::new();
                let mut l = Vec::new();
                for c in all {
                    match c.remap(|x| cl.contains(&x).then_some(x)) {
                        Some(kept) => k.push(kept),
                        None => l.push(c),
                    }
                }
                (k, l)
            }
            None => (vec![], all),
        };
        f.covers = kept;
        self.lose(
            &lost,
            None,
            "DISTINCT ON",
            Some("it keeps one row per value of the DISTINCT ON columns".into()),
        );
        match key {
            Some(cols) => {
                let why = self.arena.push(Evidence::Distinct { at: None });
                f.keys.push(Key { cols, why });
                f.repeats = vec![None; f.width];
                f.normalize();
                f.status = GrainStatus::Known;
            }
            None => f.settle_status(GrainStatus::Unknown(UnknownReason::UnsupportedOperator(
                "DISTINCT ON an expression that is not selected".into(),
            ))),
        }
        f
    }

    fn union(&mut self, u: &Union) -> RelationFacts {
        let branches: Vec<Arc<RelationFacts>> = u.inputs.iter().map(|i| self.visit(i)).collect();
        let width = u.schema.fields().len();
        let status = GrainStatus::combine(branches.iter().map(|b| &b.status));
        let not_null: ColSet = (0..width)
            .filter(|c| branches.iter().all(|b| b.not_null.contains(c)))
            .collect();
        let mut nn_why = BTreeMap::new();
        for c in &not_null {
            let from: Vec<EvidenceId> = branches
                .iter()
                .filter_map(|b| b.nn_why.get(c).copied())
                .collect();
            if !from.is_empty() {
                nn_why.insert(*c, self.arena.push(Evidence::Derived { from }));
            }
        }
        // Every branch's rows are in the result.
        let covers: Vec<Ind> = branches.iter().flat_map(|b| b.covers.clone()).collect();
        // A value comes from a relation only if it does in every branch.
        let mut within = Vec::new();
        for w in &branches[0].within {
            let mut pairs = w.pairs.clone();
            let mut from = vec![w.why];
            for b in &branches[1..] {
                let best = b
                    .within
                    .iter()
                    .filter(|o| o.relation == w.relation)
                    .map(|o| {
                        let common: Vec<(usize, String)> = pairs
                            .iter()
                            .filter(|p| o.pairs.contains(p))
                            .cloned()
                            .collect();
                        (common, o.why)
                    })
                    .max_by_key(|(c, _)| c.len());
                match best {
                    Some((common, why)) => {
                        pairs = common;
                        from.push(why);
                    }
                    None => pairs.clear(),
                }
            }
            if !pairs.is_empty() {
                within.push(Ind {
                    relation: w.relation.clone(),
                    pairs,
                    why: self.arena.push(Evidence::Derived { from }),
                });
            }
        }
        let mut f = RelationFacts {
            not_null,
            status: GrainStatus::Known,
            covers,
            within,
            nn_why,
            ..RelationFacts::unknown(width, UnknownReason::UnsupportedOperator(String::new()))
        };
        // A constant discriminator column with distinct literals per branch, plus a key
        // shared by every branch, keeps (key, discriminator) unique.
        'disc: for c in 0..width {
            let lits: Option<Vec<ScalarValue>> = u
                .inputs
                .iter()
                .map(|b| branch_literal(b, c).cloned())
                .collect();
            let Some(lits) = lits else { continue };
            if lits.iter().any(|v| v.is_null()) {
                continue;
            }
            for i in 0..lits.len() {
                for j in i + 1..lits.len() {
                    if lits[i] == lits[j] {
                        continue 'disc;
                    }
                }
            }
            for k in &branches[0].keys {
                if branches.iter().all(|b| b.is_superkey(&k.cols)) {
                    let froms: Vec<EvidenceId> = branches
                        .iter()
                        .filter_map(|b| b.key_covered_by(&k.cols).map(|k| k.why))
                        .collect();
                    let why = self.arena.push(Evidence::UnionDiscriminator {
                        column: u.schema.field(c).name().clone(),
                        from: froms,
                    });
                    let mut cols = k.cols.clone();
                    cols.insert(c);
                    f.keys.push(Key { cols, why });
                    break 'disc;
                }
            }
        }
        f.normalize();
        if f.keys.is_empty() {
            let reason = status
                .reason()
                .cloned()
                .unwrap_or(UnknownReason::UnsupportedOperator(
                    "UNION ALL without a discriminator column".into(),
                ));
            f.status = GrainStatus::Unknown(reason);
        } else {
            f.settle_status(status);
        }
        f
    }

    fn join(&mut self, j: &Join) -> RelationFacts {
        let l = self.visit(&j.left);
        let r = self.visit(&j.right);
        let (ls, rs) = (j.left.schema(), j.right.schema());
        let (nl, nr) = (l.width, r.width);

        // Equality pairs between the two sides, from USING/ON pairs and ON conjuncts.
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        let mut extra = false;
        let side_of = |c: &Column| -> Option<(bool, usize)> {
            match (col_index(ls, c), col_index(rs, c)) {
                (Some(i), None) => Some((true, i)),
                (None, Some(i)) => Some((false, i)),
                _ => None,
            }
        };
        let push_eq = |a: &Expr, b: &Expr, pairs: &mut Vec<(usize, usize)>| -> bool {
            if let (Expr::Column(ca), Expr::Column(cb)) = (unalias(a), unalias(b)) {
                match (side_of(ca), side_of(cb)) {
                    (Some((true, i)), Some((false, k))) | (Some((false, k)), Some((true, i))) => {
                        pairs.push((i, k));
                        return true;
                    }
                    _ => {}
                }
            }
            false
        };
        for (a, b) in &j.on {
            if !push_eq(a, b, &mut pairs) {
                extra = true;
            }
        }
        let mut span_cols: Vec<&Column> =
            j.on.iter()
                .flat_map(|(a, b)| a.column_refs().into_iter().chain(b.column_refs()))
                .collect();
        if let Some(filter) = &j.filter {
            for conj in split_conjunction(filter) {
                span_cols.extend(conj.column_refs());
                match conj {
                    Expr::BinaryExpr(BinaryExpr {
                        left,
                        op: Operator::Eq,
                        right,
                    }) if push_eq(left, right, &mut pairs) => {}
                    _ => extra = true,
                }
            }
        }
        let a_cols: ColSet = pairs.iter().map(|p| p.0).collect();
        let b_cols: ColSet = pairs.iter().map(|p| p.1).collect();
        // Non-equality conditions never make a side unique on their own.
        let left_unique = l.is_superkey(&a_cols);
        let right_unique = r.is_superkey(&b_cols);
        let left_decided = left_unique || l.status.is_known();
        let right_decided = right_unique || r.status.is_known();
        let cardinality =
            (left_decided && right_decided).then_some(match (left_unique, right_unique) {
                (true, true) => Cardinality::OneToOne,
                (false, true) => Cardinality::ManyToOne,
                (true, false) => Cardinality::OneToMany,
                (false, false) => Cardinality::ManyToMany,
            });

        let text = SourceText::new(self.sql);
        let left_name = side_name(&j.left, self.upstream);
        let right_name = side_name(&j.right, self.upstream);
        let span = span_cols
            .iter()
            .find_map(|c| c.spans().first())
            .and_then(|s| SqlSpan::from_df(&s))
            .and_then(|s| text.join_clause_before(s))
            .or_else(|| self.find_join_span(&text, &j.right));
        if let Some(s) = span {
            self.used_join_spans.insert(s);
        }
        let on_names: Vec<String> = {
            let mut v: Vec<String> = pairs
                .iter()
                .map(|(a, _)| ls.field(*a).name().clone())
                .collect();
            v.dedup();
            v
        };
        let grain = |f: &RelationFacts, schema: &DFSchema| -> Option<Vec<String>> {
            f.keys.first().map(|k| column_names(schema, &k.cols))
        };
        self.joins.push(JoinRecord {
            node: j as *const Join as usize,
            span,
            cardinality,
            left: left_name.clone(),
            right: right_name.clone(),
            on_left: pairs
                .iter()
                .map(|(a, _)| ls.field(*a).name().clone())
                .collect(),
            on_right: pairs
                .iter()
                .map(|(_, b)| rs.field(*b).name().clone())
                .collect(),
            left_grain: grain(&l, ls),
            right_grain: grain(&r, rs),
            join_type: j.join_type,
        });

        let shift = |s: &ColSet| -> ColSet { s.iter().map(|c| c + nl).collect() };
        let width = j.schema.fields().len();

        // Does every row of each side survive the join?
        let jt = j.join_type;
        let swapped: Vec<(usize, usize)> = pairs.iter().map(|(a, b)| (*b, *a)).collect();
        let what = format!(
            "{} join{}",
            join_label(jt),
            if on_names.is_empty() {
                String::new()
            } else {
                format!(" on {}", on_names.join(", "))
            }
        );
        let left_kept: Result<Option<EvidenceId>, String> = match jt {
            JoinType::Left | JoinType::Full | JoinType::LeftMark => Ok(None),
            JoinType::Inner | JoinType::Right | JoinType::LeftSemi => self
                .match_proof(
                    (&l, ls, &left_name),
                    (&r, rs, &right_name),
                    &pairs,
                    extra,
                    span,
                )
                .map(Some),
            JoinType::LeftAnti => Err("an anti join drops every row that has a match".into()),
            _ => Err(String::new()),
        };
        let right_kept: Result<Option<EvidenceId>, String> = match jt {
            JoinType::Right | JoinType::Full | JoinType::RightMark => Ok(None),
            JoinType::Inner | JoinType::Left | JoinType::RightSemi => self
                .match_proof(
                    (&r, rs, &right_name),
                    (&l, ls, &left_name),
                    &swapped,
                    extra,
                    span,
                )
                .map(Some),
            JoinType::RightAnti => Err("an anti join drops every row that has a match".into()),
            _ => Err(String::new()),
        };
        let kept_covers = |side: &RelationFacts,
                           kept: &Result<Option<EvidenceId>, String>,
                           offset: usize,
                           this: &mut Self|
         -> Vec<Ind> {
            match kept {
                Ok(proof) => side
                    .covers
                    .iter()
                    .map(|c| {
                        let mut c = c.remap(|x| Some(x + offset)).expect("non-empty");
                        if let Some(p) = proof {
                            c.why = this.arena.push(Evidence::Derived {
                                from: vec![c.why, *p],
                            });
                        }
                        c
                    })
                    .collect(),
                Err(because) => {
                    let because = (!because.is_empty()).then(|| because.clone());
                    this.lose(&side.covers, span, &what, because);
                    vec![]
                }
            }
        };

        let left_only = matches!(
            j.join_type,
            JoinType::LeftSemi | JoinType::LeftAnti | JoinType::LeftMark
        );
        let right_only = matches!(
            j.join_type,
            JoinType::RightSemi | JoinType::RightAnti | JoinType::RightMark
        );
        if left_only || right_only {
            let (side, kept) = if left_only {
                (&l, &left_kept)
            } else {
                (&r, &right_kept)
            };
            let mut f = (**side).clone();
            f.covers = kept_covers(side, kept, 0, self);
            if !matches!(jt, JoinType::LeftMark | JoinType::RightMark) {
                f.ranks.clear();
            }
            f.width = width;
            f.repeats.resize(width, None);
            return f;
        }
        let mut covers = kept_covers(&l, &left_kept, 0, self);
        covers.extend(kept_covers(&r, &right_kept, nl, self));
        let mut within: Vec<Ind> = l.within.clone();
        within.extend(r.within.iter().filter_map(|w| w.remap(|c| Some(c + nl))));
        if jt == JoinType::Inner {
            // Every output row has a = b, so either column can stand for the other (a
            // USING join outputs only one of them).
            for i in covers.iter_mut().chain(within.iter_mut()) {
                let mut extra_pairs = Vec::new();
                for (a, b) in &pairs {
                    let b = b + nl;
                    for (c, x) in &i.pairs {
                        if *c == b && !i.has_pair(*a, x) {
                            extra_pairs.push((*a, x.clone()));
                        }
                        if c == a && !i.has_pair(b, x) {
                            extra_pairs.push((b, x.clone()));
                        }
                    }
                }
                i.pairs.extend(extra_pairs);
                i.pairs.sort();
                i.pairs.dedup();
            }
        }
        let mut ranks = Vec::new();
        // Ranks survive where each row of their side appears exactly once.
        if left_kept.is_ok() && right_unique {
            ranks.extend(l.ranks.iter().cloned());
        }
        if right_kept.is_ok() && left_unique {
            ranks.extend(r.ranks.iter().map(|(c, p)| (c + nl, shift(p))));
        }
        let left_unique_why = l.superkey_why(&a_cols).unwrap_or_default();
        let right_unique_why = r.superkey_why(&b_cols).unwrap_or_default();

        let left_all: ColSet = (0..nl).collect();
        let right_all: ColSet = (nl..nl + nr).collect();
        let mut fds: Vec<Fd> = Vec::new();
        let (keep_left_fds, keep_right_fds) = (true, true);
        if keep_left_fds {
            fds.extend(l.fds.iter().cloned());
            fds.extend(l.keys.iter().map(|k| Fd {
                lhs: k.cols.clone(),
                rhs: left_all.clone(),
                why: k.why,
                entity: true,
            }));
        }
        if keep_right_fds {
            fds.extend(r.fds.iter().map(|f| Fd {
                lhs: shift(&f.lhs),
                rhs: shift(&f.rhs),
                why: f.why,
                entity: f.entity,
            }));
            fds.extend(r.keys.iter().map(|k| Fd {
                lhs: shift(&k.cols),
                rhs: right_all.clone(),
                why: k.why,
                entity: true,
            }));
        }
        let pair_why = self.arena.push(Evidence::Derived { from: vec![] });
        for (a, b) in &pairs {
            let (a, b) = (*a, b + nl);
            match j.join_type {
                JoinType::Inner => {
                    fds.push(Fd {
                        lhs: [a].into(),
                        rhs: [b].into(),
                        why: pair_why,
                        entity: false,
                    });
                    fds.push(Fd {
                        lhs: [b].into(),
                        rhs: [a].into(),
                        why: pair_why,
                        entity: false,
                    });
                }
                // With only equalities in ON, whether a row matches depends on the key
                // alone, so the preserved side's key still determines the other's.
                JoinType::Left if !extra => fds.push(Fd {
                    lhs: [a].into(),
                    rhs: [b].into(),
                    why: pair_why,
                    entity: false,
                }),
                JoinType::Right if !extra => fds.push(Fd {
                    lhs: [b].into(),
                    rhs: [a].into(),
                    why: pair_why,
                    entity: false,
                }),
                _ => {}
            }
        }

        let mut keys: Vec<Key> = Vec::new();
        let card = cardinality.unwrap_or(Cardinality::ManyToMany);
        let kept = |k: &Key,
                    cols: ColSet,
                    uses: &[EvidenceId],
                    arena: &mut EvidenceArena,
                    keys: &mut Vec<Key>| {
            let why = arena.push(Evidence::JoinKept {
                at: span,
                cardinality: card,
                on: on_names.clone(),
                from: vec![k.why],
                uses: uses.to_vec(),
            });
            keys.push(Key { cols, why });
        };
        let preserve_left = matches!(j.join_type, JoinType::Inner | JoinType::Left);
        let preserve_right = matches!(j.join_type, JoinType::Inner | JoinType::Right);
        if right_unique && preserve_left {
            for k in &l.keys {
                kept(
                    k,
                    k.cols.clone(),
                    &right_unique_why,
                    &mut self.arena,
                    &mut keys,
                );
            }
        }
        if left_unique && preserve_right {
            for k in &r.keys {
                kept(
                    k,
                    shift(&k.cols),
                    &left_unique_why,
                    &mut self.arena,
                    &mut keys,
                );
            }
        }
        let one_side_kept = (right_unique && preserve_left) || (left_unique && preserve_right);
        if !one_side_kept {
            'outer: for kl in &l.keys {
                for kr in &r.keys {
                    if keys.len() >= MAX_KEYS {
                        break 'outer;
                    }
                    let why = self.arena.push(Evidence::JoinKept {
                        at: span,
                        cardinality: card,
                        on: on_names.clone(),
                        from: vec![kl.why, kr.why],
                        uses: vec![],
                    });
                    keys.push(Key {
                        cols: kl.cols.union(&shift(&kr.cols)).copied().collect(),
                        why,
                    });
                }
            }
        }

        let mut not_null = ColSet::new();
        let mut nn_why = BTreeMap::new();
        if matches!(j.join_type, JoinType::Inner | JoinType::Left) {
            not_null.extend(l.not_null.iter().copied());
            nn_why.extend(l.nn_why.iter().map(|(c, w)| (*c, *w)));
        }
        if matches!(j.join_type, JoinType::Inner | JoinType::Right) {
            not_null.extend(shift(&r.not_null));
            nn_why.extend(r.nn_why.iter().map(|(c, w)| (c + nl, *w)));
        }
        if j.join_type == JoinType::Inner {
            for (a, b) in &pairs {
                not_null.insert(*a);
                not_null.insert(b + nl);
                nn_why.remove(a);
                nn_why.remove(&(b + nl));
            }
        }

        let site = |repeated_by: &str| {
            Arc::new(JoinSite {
                relation: self.relation.clone(),
                span,
                cardinality: card,
                repeated_by: repeated_by.to_string(),
            })
        };
        let mut repeats: Vec<Option<Arc<JoinSite>>> = Vec::with_capacity(width);
        let left_site = (cardinality.is_some() && !right_unique).then(|| site(&right_name));
        let right_site = (cardinality.is_some() && !left_unique).then(|| site(&left_name));
        for i in 0..nl {
            repeats.push(
                left_site
                    .clone()
                    .or_else(|| l.repeats.get(i).cloned().flatten()),
            );
        }
        for i in 0..nr {
            repeats.push(
                right_site
                    .clone()
                    .or_else(|| r.repeats.get(i).cloned().flatten()),
            );
        }
        repeats.resize(width, None);

        let mut f = RelationFacts {
            width,
            keys,
            fds,
            not_null,
            status: GrainStatus::Known,
            repeats,
            covers,
            within,
            ranks,
            nn_why,
            // A row repeated by the join repeats its whole side, agree_on included, so
            // the fact holds; the repetition itself is G001's to report.
            distinct: l
                .distinct
                .iter()
                .cloned()
                .chain(r.distinct.iter().filter_map(|d| d.remap(|c| Some(c + nl))))
                .collect(),
        };
        f.normalize();
        f.settle_status(GrainStatus::combine([&l.status, &r.status]));
        f
    }

    /// Evidence that every row of `side` has a match in `other` under the equality
    /// `pairs` (side column, other column), or why that cannot be shown.
    ///
    /// A row of `side` matches when its join columns are non-null and some row of
    /// `other` has the same values. Both follow when `side` takes those values from a
    /// relation R (a within fact) and `other` contains every row of R (a covers fact)
    /// on the same columns.
    fn match_proof(
        &mut self,
        (side, side_schema, side_name): (&RelationFacts, &DFSchema, &str),
        (other, other_schema, other_name): (&RelationFacts, &DFSchema, &str),
        pairs: &[(usize, usize)],
        extra: bool,
        at: Option<SqlSpan>,
    ) -> Result<EvidenceId, String> {
        if extra {
            return Err("the ON clause has conditions besides column equalities".into());
        }
        if pairs.is_empty() {
            return Err("the join has no column equality".into());
        }
        let name = |schema: &DFSchema, c: usize| schema.field(c).name().clone();
        let nullable: Vec<String> = pairs
            .iter()
            .filter(|(a, _)| !side.not_null.contains(a))
            .map(|(a, _)| name(side_schema, *a))
            .collect();
        if !nullable.is_empty() {
            return Err(format!(
                "{side_name}.{} can be null, and a null never matches",
                nullable.join(", ")
            ));
        }
        for w in &side.within {
            for c in other.covers.iter().filter(|c| c.relation == w.relation) {
                let all = pairs
                    .iter()
                    .all(|(a, b)| w.pairs.iter().any(|(wc, x)| wc == a && c.has_pair(*b, x)));
                if all {
                    let a_cols: ColSet = pairs.iter().map(|(a, _)| *a).collect();
                    let mut from = vec![w.why, c.why];
                    from.extend(side.not_null_why(&a_cols));
                    let cols: Vec<String> =
                        pairs.iter().map(|(a, _)| name(side_schema, *a)).collect();
                    return Ok(self.arena.push(Evidence::Preserved {
                        at,
                        what: format!(
                            "every row of {side_name} matches {other_name} on {}",
                            cols.join(", ")
                        ),
                        from,
                    }));
                }
            }
        }
        let a: Vec<String> = pairs.iter().map(|(a, _)| name(side_schema, *a)).collect();
        let b: Vec<String> = pairs.iter().map(|(_, b)| name(other_schema, *b)).collect();
        let tuple = |v: &[String]| {
            if v.len() == 1 {
                v[0].clone()
            } else {
                format!("({})", v.join(", "))
            }
        };
        Err(format!(
            "nothing shows that every {side_name}.{} appears in {other_name}.{}",
            tuple(&a),
            tuple(&b)
        ))
    }

    /// Fallback when no join key carries a span (USING joins): the first unused JOIN
    /// clause in the text that names the right side.
    fn find_join_span(&self, text: &SourceText<'_>, right: &LogicalPlan) -> Option<SqlSpan> {
        let names = side_aliases(right);
        let lower = text.text.to_ascii_lowercase();
        let mut from = 0;
        while let Some(i) = lower[from..].find("join") {
            let at = from + i;
            from = at + 4;
            let line_end = lower[at..]
                .find('\n')
                .map(|n| at + n)
                .unwrap_or(lower.len());
            let clause = &lower[at..line_end];
            if names.iter().any(|n| {
                clause
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .any(|w| w == n)
            }) {
                let s = text.join_clause_before(text.span(at + 4, at + 4))?;
                if !self.used_join_spans.contains(&s) {
                    return Some(s);
                }
            }
        }
        None
    }
}

fn join_label(t: JoinType) -> &'static str {
    match t {
        JoinType::Inner => "inner",
        JoinType::Left => "left",
        JoinType::Right => "right",
        JoinType::Full => "full",
        JoinType::LeftSemi | JoinType::RightSemi => "semi",
        JoinType::LeftAnti | JoinType::RightAnti => "anti",
        JoinType::LeftMark | JoinType::RightMark => "mark",
    }
}

fn has_nondeterminism_args(args: &[Expr]) -> bool {
    args.iter().any(has_nondeterminism)
}

/// The literal at output position `c` of a union branch, if the branch is a projection
/// that selects a constant there.
fn branch_literal(plan: &LogicalPlan, c: usize) -> Option<&ScalarValue> {
    match plan {
        LogicalPlan::Projection(p) => p.expr.get(c).and_then(literal_of),
        LogicalPlan::SubqueryAlias(s) => branch_literal(&s.input, c),
        LogicalPlan::Sort(s) => branch_literal(&s.input, c),
        LogicalPlan::Limit(l) => branch_literal(&l.input, c),
        LogicalPlan::Distinct(Distinct::All(i)) => branch_literal(i, c),
        _ => None,
    }
}

/// A readable name for one side of a join: the relation it scans, or its alias.
fn side_name(plan: &LogicalPlan, upstream: &dyn UpstreamFacts) -> String {
    match plan {
        LogicalPlan::TableScan(s) => s
            .source
            .as_ref()
            .downcast_ref::<CardinalTable>()
            .and_then(|t| upstream.summary(&RelationId(t.relation.clone())))
            .map(|s| s.display.clone())
            .unwrap_or_else(|| s.table_name.table().to_string()),
        LogicalPlan::SubqueryAlias(s) => match s.input.as_ref() {
            LogicalPlan::TableScan(_) => side_name(&s.input, upstream),
            _ => s.alias.table().to_string(),
        },
        LogicalPlan::Projection(p) => side_name(&p.input, upstream),
        LogicalPlan::Filter(f) => side_name(&f.input, upstream),
        LogicalPlan::Join(j) => side_name(&j.left, upstream),
        _ => "a subquery".into(),
    }
}

fn side_aliases(plan: &LogicalPlan) -> Vec<String> {
    match plan {
        LogicalPlan::TableScan(s) => vec![s.table_name.table().to_lowercase()],
        LogicalPlan::SubqueryAlias(s) => {
            let mut v = vec![s.alias.table().to_lowercase()];
            v.extend(side_aliases(&s.input));
            v
        }
        LogicalPlan::Projection(p) => side_aliases(&p.input),
        LogicalPlan::Filter(f) => side_aliases(&f.input),
        _ => vec![],
    }
}
