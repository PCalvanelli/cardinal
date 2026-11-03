//! Grain inference: facts computed bottom-up over a model's logical plan, one rule per
//! plan node, on the plan exactly as SqlToRel produced it.

use crate::evidence::{Evidence, EvidenceArena, EvidenceId};
use crate::facts::{
    Cardinality, ColSet, Fd, GrainStatus, JoinSite, Key, MAX_KEYS, RelationFacts, UnknownReason,
    closure, minimize,
};
use crate::summary::RelationSummary;
use derive_dbt::RelationId;
use derive_sql::datafusion_common::tree_node::TreeNode;
use derive_sql::datafusion_common::{Column, DFSchema, ScalarValue};
use derive_sql::datafusion_expr::expr::{Alias, WindowFunction};
use derive_sql::datafusion_expr::logical_plan::{
    Aggregate, Distinct, DistinctOn, Filter, Join, JoinType, Projection, TableScan, Union, Window,
};
use derive_sql::datafusion_expr::utils::split_conjunction;
use derive_sql::datafusion_expr::{
    BinaryExpr, Expr, LogicalPlan, Operator, WindowFunctionDefinition,
};
use derive_sql::{DeriveTable, SourceText, SqlSpan};
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
}

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
    };
    a.visit(&plan);
    let Analyzer {
        arena,
        facts,
        joins,
        ..
    } = a;
    AnalyzedModel {
        relation,
        display,
        sql: sql.clone(),
        plan,
        arena,
        facts,
        joins,
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
    let mut f = RelationFacts {
        width: m,
        keys,
        fds,
        not_null,
        status: input.status.clone(),
        repeats,
    };
    f.normalize();
    f.settle_status(input.status.clone());
    f
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
                    width,
                    keys: vec![Key {
                        cols: ColSet::new(),
                        why,
                    }],
                    fds: vec![],
                    not_null: ColSet::new(),
                    status: GrainStatus::Known,
                    repeats: vec![None; width],
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
        let Some(t) = s.source.as_ref().downcast_ref::<DeriveTable>() else {
            return RelationFacts::unknown(
                width,
                UnknownReason::UnsupportedOperator("an unknown table".into()),
            );
        };
        let id = RelationId(t.relation.clone());
        let Some(summary) = self.upstream.summary(&id) else {
            return RelationFacts::unknown(width, UnknownReason::PlanError(id));
        };
        let full = summary.to_facts(&mut self.arena);
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
        let mut facts = (*self.visit(&f.input)).clone();
        let schema = f.input.schema();
        let text = SourceText::new(self.sql);
        for conj in split_conjunction(&f.predicate) {
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
                                facts.not_null.insert(c);
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
                                facts.not_null.insert(a);
                                facts.not_null.insert(b);
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
                        facts.not_null.extend(lc.into_iter().chain(rc));
                    }
                }
                Expr::IsNotNull(e)
                | Expr::Like(derive_sql::datafusion_expr::expr::Like { expr: e, .. }) => {
                    if let Expr::Column(c) = unalias(e)
                        && let Some(i) = col_index(schema, c)
                    {
                        facts.not_null.insert(i);
                    }
                }
                Expr::Between(b) if !b.negated => {
                    if let Expr::Column(c) = unalias(&b.expr)
                        && let Some(i) = col_index(schema, c)
                    {
                        facts.not_null.insert(i);
                    }
                }
                Expr::InList(l) if !l.negated => {
                    if let Expr::Column(c) = unalias(&l.expr)
                        && let Some(i) = col_index(schema, c)
                    {
                        facts.not_null.insert(i);
                    }
                }
                _ => {}
            }
        }
        facts.normalize();
        facts
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
        f.normalize();
        f.status = GrainStatus::Known;
        f
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
        let mut f = RelationFacts {
            width,
            keys: vec![],
            fds: vec![],
            not_null,
            status: GrainStatus::Known,
            repeats: vec![None; width],
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
        let left_only = matches!(
            j.join_type,
            JoinType::LeftSemi | JoinType::LeftAnti | JoinType::LeftMark
        );
        let right_only = matches!(
            j.join_type,
            JoinType::RightSemi | JoinType::RightAnti | JoinType::RightMark
        );
        if left_only || right_only {
            let side = if left_only { &l } else { &r };
            let mut f = (**side).clone();
            f.width = width;
            f.repeats.resize(width, None);
            return f;
        }

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
        let kept = |k: &Key, cols: ColSet, arena: &mut EvidenceArena, keys: &mut Vec<Key>| {
            let why = arena.push(Evidence::JoinKept {
                at: span,
                cardinality: card,
                on: on_names.clone(),
                from: vec![k.why],
            });
            keys.push(Key { cols, why });
        };
        let preserve_left = matches!(j.join_type, JoinType::Inner | JoinType::Left);
        let preserve_right = matches!(j.join_type, JoinType::Inner | JoinType::Right);
        if right_unique && preserve_left {
            for k in &l.keys {
                kept(k, k.cols.clone(), &mut self.arena, &mut keys);
            }
        }
        if left_unique && preserve_right {
            for k in &r.keys {
                kept(k, shift(&k.cols), &mut self.arena, &mut keys);
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
                    });
                    keys.push(Key {
                        cols: kl.cols.union(&shift(&kr.cols)).copied().collect(),
                        why,
                    });
                }
            }
        }

        let mut not_null = ColSet::new();
        if matches!(j.join_type, JoinType::Inner | JoinType::Left) {
            not_null.extend(l.not_null.iter().copied());
        }
        if matches!(j.join_type, JoinType::Inner | JoinType::Right) {
            not_null.extend(shift(&r.not_null));
        }
        if j.join_type == JoinType::Inner {
            for (a, b) in &pairs {
                not_null.insert(*a);
                not_null.insert(b + nl);
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
        };
        f.normalize();
        f.settle_status(GrainStatus::combine([&l.status, &r.status]));
        f
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
            .downcast_ref::<DeriveTable>()
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
