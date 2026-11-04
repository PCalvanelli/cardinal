//! The analysis pipeline: one DAG level at a time, models in a level in parallel.
//! A model's finished facts become the schema and facts its dependents' scans read.

use crate::config::Config;
use cardinal_cache::{Cache, KeyBuilder};
use cardinal_check::{CheckContext, Locator, Shape, Shapes, model_checks, plan_failure};
use cardinal_dbt::{Kind, Project, Relation, RelationId};
use cardinal_diag::sourcemap::SourceMap;
use cardinal_diag::{Diagnostic, Label};
use cardinal_grain::{
    AnalyzedModel, Cardinality, JoinSide, RelationSummary, UnknownReason, UpstreamFacts, Verdict,
    analyze, summarize, summary_from_declared, verdict,
};
use cardinal_sql::provider::{Functions, TableLookup};
use cardinal_sql::sqlparser::dialect::Dialect;
use cardinal_sql::{CardinalContext, CardinalTable, SqlSpan, plan_sql};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JoinSummary {
    pub left: String,
    pub right: String,
    pub on: Vec<String>,
    pub cardinality: Option<Cardinality>,
    /// Lineage of each side's join columns, for `cardinal guess`: left, then right.
    #[serde(default)]
    pub sides: Vec<JoinSide>,
}

/// Everything later stages need from one relation; this is what the cache stores.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelResult {
    pub summary: RelationSummary,
    pub diagnostics: Vec<Diagnostic>,
    pub joins: Vec<JoinSummary>,
    /// Exactly-once verdicts: every meta.cardinal.exactly_once target, then each parent.
    #[serde(default)]
    pub verdicts: Vec<Verdict>,
    /// True when the SQL parsed and planned.
    pub planned: bool,
    #[serde(skip)]
    pub from_cache: bool,
    /// The cache key it was computed under; empty when nothing was planned.
    #[serde(skip)]
    pub cache_key: String,
}

pub struct RunResult {
    pub results: BTreeMap<RelationId, ModelResult>,
    pub elapsed: Duration,
    pub cache_hits: usize,
}

pub struct Engine<'a> {
    pub project: &'a Project,
    pub functions: Functions,
    pub dialect: Box<dyn Dialect + Send + Sync>,
    pub cache: Cache,
    config_fingerprint: String,
    /// Prefix for cache entries, so a second build of the project keeps its own.
    cache_prefix: &'static str,
    /// Results to reuse where the cache key matches: the head's, when analyzing the
    /// base branch's artifacts, which mostly hold the same models.
    reuse: Option<&'a BTreeMap<RelationId, ModelResult>>,
}

struct Lookup<'a> {
    project: &'a Project,
    done: &'a BTreeMap<RelationId, ModelResult>,
}

impl TableLookup for Lookup<'_> {
    fn lookup(&self, parts: &[String]) -> Option<Arc<CardinalTable>> {
        let id = self.project.resolve(parts)?;
        let r = self.done.get(id)?;
        Some(Arc::new(CardinalTable {
            relation: id.0.clone(),
            schema: r.summary.schema(),
        }))
    }
}

impl UpstreamFacts for Lookup<'_> {
    fn summary(&self, id: &RelationId) -> Option<&RelationSummary> {
        self.done.get(id).map(|r| &r.summary)
    }
}

/// Maps compiled-SQL spans to the model's own source file.
pub struct ProjectLocator<'a> {
    pub project: &'a Project,
}

impl Locator for ProjectLocator<'_> {
    fn label(&self, relation: &RelationId, span: Option<SqlSpan>, message: String) -> Label {
        let Some(rel) = self.project.get(relation) else {
            return Label {
                file: relation.0.clone(),
                span: None,
                message,
            };
        };
        let Some(span) = span else {
            return Label {
                file: rel.file.clone(),
                span: None,
                message,
            };
        };
        if let (Some(raw), Some(compiled)) = (&rel.raw_sql, &rel.compiled_sql)
            && let Some(mapped) = SourceMap::new(raw, compiled).map_span(span, compiled, raw)
        {
            return Label {
                file: rel.file.clone(),
                span: Some(mapped),
                message,
            };
        }
        Label {
            file: rel.compiled_path.clone().unwrap_or(rel.file.clone()),
            span: Some(span),
            message,
        }
    }

    fn display(&self, relation: &RelationId) -> String {
        self.project
            .get(relation)
            .map(|r| r.display.clone())
            .unwrap_or_else(|| relation.0.clone())
    }
}

impl<'a> Engine<'a> {
    pub fn new(project: &'a Project, config: &Config, use_cache: bool) -> Self {
        let adapter = config
            .dialect
            .clone()
            .unwrap_or_else(|| project.adapter.clone());
        let (dialect, _) = cardinal_sql::dialect::dialect_for_adapter(&adapter);
        Engine {
            project,
            functions: Functions::new(&adapter, &config.stub),
            dialect,
            cache: Cache::new(&project.root, use_cache),
            config_fingerprint: format!(
                "{adapter}:{}:{}",
                config.analysis_fingerprint(),
                build_id()
            ),
            cache_prefix: "",
            reuse: None,
        }
    }

    /// An engine for the base branch's artifacts (`--state`). It reuses the head's
    /// results for models whose SQL and inputs are unchanged, and caches the others
    /// apart from the head's, so neither evicts the other.
    pub fn for_state(
        project: &'a Project,
        config: &Config,
        use_cache: bool,
        head: &'a RunResult,
    ) -> Self {
        Engine {
            cache_prefix: "state:",
            reuse: Some(&head.results),
            ..Engine::new(project, config, use_cache)
        }
    }

    /// Analyze `needed` (which must be closed under ancestors) in DAG order.
    pub fn run(&self, needed: &BTreeSet<RelationId>) -> RunResult {
        let start = Instant::now();
        let mut done: BTreeMap<RelationId, ModelResult> = BTreeMap::new();
        for level in &self.project.levels {
            let ids: Vec<&RelationId> = level.iter().filter(|id| needed.contains(*id)).collect();
            let results: Vec<(RelationId, ModelResult)> = {
                let done_ref = &done;
                ids.par_iter()
                    .filter_map(|id| {
                        let rel = self.project.get(id)?;
                        Some(((*id).clone(), self.analyze_one(rel, done_ref)))
                    })
                    .collect()
            };
            done.extend(results);
        }
        let cache_hits = done.values().filter(|r| r.from_cache).count();
        RunResult {
            results: done,
            elapsed: start.elapsed(),
            cache_hits,
        }
    }

    fn cache_key(&self, rel: &Relation, done: &BTreeMap<RelationId, ModelResult>) -> String {
        let mut k = KeyBuilder::new();
        k.add(self.config_fingerprint.as_bytes())
            .add(rel.compiled_sql.as_deref().unwrap_or("").as_bytes())
            .add(rel.raw_sql.as_deref().unwrap_or("").as_bytes())
            .add(rel.file.as_bytes())
            .add_json(&rel.declared_keys)
            .add_json(&rel.not_null)
            .add_json(&rel.columns)
            .add_json(&rel.incremental)
            .add_json(&rel.references)
            .add_json(&rel.exactly_once)
            .add_json(&rel.measures)
            .add_json(&rel.units)
            .add(cardinal_check::CHECKS_FINGERPRINT.as_bytes());
        for d in &rel.depends_on {
            k.add(d.0.as_bytes());
            if let Some(r) = done.get(d) {
                k.add_json(&r.summary);
            }
        }
        k.finish()
    }

    fn analyze_one(&self, rel: &Relation, done: &BTreeMap<RelationId, ModelResult>) -> ModelResult {
        let unplanned = |reason: UnknownReason, diagnostics: Vec<Diagnostic>| ModelResult {
            summary: summary_from_declared(rel, reason),
            diagnostics,
            joins: vec![],
            verdicts: vec![],
            planned: false,
            from_cache: false,
            cache_key: String::new(),
        };
        match rel.kind {
            Kind::Source | Kind::Seed | Kind::Snapshot => {
                return unplanned(UnknownReason::SourceWithoutKey(rel.id.clone()), vec![]);
            }
            Kind::PythonModel => {
                return unplanned(UnknownReason::PythonModel(rel.id.clone()), vec![]);
            }
            Kind::Model { .. } | Kind::Ephemeral => {}
        }
        let locator = ProjectLocator {
            project: self.project,
        };
        let Some(sql) = rel.compiled_sql.as_deref() else {
            return unplanned(
                UnknownReason::PlanError(rel.id.clone()),
                vec![plan_failure(rel, None, &locator)],
            );
        };
        let key = self.cache_key(rel, done);
        if let Some(prev) = self.reuse.and_then(|r| r.get(&rel.id))
            && prev.cache_key == key
        {
            return ModelResult {
                from_cache: true,
                ..prev.clone()
            };
        }
        let cache_id = format!("{}{}", self.cache_prefix, rel.id.0);
        if let Some(mut hit) = self.cache.get::<ModelResult>(&cache_id, &key) {
            hit.from_cache = true;
            hit.cache_key = key;
            return hit;
        }
        let lookup = Lookup {
            project: self.project,
            done,
        };
        let ctx = CardinalContext {
            functions: &self.functions,
            tables: &lookup,
        };
        let result = match plan_sql(sql, self.dialect.as_ref(), &ctx) {
            Err(f) => {
                tracing::debug!(model = %rel.id, error = %f.message, "plan failed");
                unplanned(
                    UnknownReason::PlanError(rel.id.clone()),
                    vec![plan_failure(rel, Some(&f), &locator)],
                )
            }
            Ok(plan) => {
                let model = analyze(
                    rel.id.clone(),
                    rel.display.clone(),
                    sql.to_string(),
                    plan,
                    &lookup,
                );
                let schema = model.plan.schema().as_arrow().clone();
                let (mut summary, declared) = summarize(rel, &schema, model.output(), &model.arena);
                summary.losses = model.losses.clone();
                let verdicts = verdicts(rel, &model, done);
                let cx = CheckContext {
                    relation: rel,
                    locator: &locator,
                    functions: &self.functions,
                    declared: &declared,
                };
                let mut diagnostics = Vec::new();
                for check in model_checks() {
                    check.run(&cx, &model, &mut diagnostics);
                }
                let joins = model
                    .joins
                    .iter()
                    .map(|j| JoinSummary {
                        left: j.left.clone(),
                        right: j.right.clone(),
                        on: j.on_left.clone(),
                        cardinality: j.cardinality,
                        sides: j.sides.to_vec(),
                    })
                    .collect();
                ModelResult {
                    summary,
                    diagnostics,
                    joins,
                    verdicts,
                    planned: true,
                    from_cache: false,
                    cache_key: String::new(),
                }
            }
        };
        self.cache.put(&cache_id, &key, &result);
        ModelResult {
            cache_key: key,
            ..result
        }
    }
}

/// Verdicts for each declared exactly_once target, then for each parent with a key.
fn verdicts(
    rel: &Relation,
    model: &AnalyzedModel,
    done: &BTreeMap<RelationId, ModelResult>,
) -> Vec<Verdict> {
    let mut out = Vec::new();
    let declared: Vec<&RelationId> = rel
        .exactly_once
        .iter()
        .filter_map(|e| e.relation.as_ref())
        .collect();
    for id in &declared {
        if out.iter().any(|v: &Verdict| &&v.relation == id) {
            continue;
        }
        out.push(match done.get(*id) {
            Some(r) => verdict(model, &r.summary, true),
            None => Verdict::not_read((*id).clone(), id.0.clone(), true),
        });
    }
    for d in &rel.depends_on {
        if declared.contains(&d) {
            continue;
        }
        if let Some(r) = done.get(d)
            && !r.summary.keys.is_empty()
        {
            out.push(verdict(model, &r.summary, false));
        }
    }
    out
}

/// Identifies this build of Cardinal, so a rebuilt binary never reuses facts computed by
/// different analysis code. Size and mtime are cheap to read and change on every build.
fn build_id() -> String {
    std::env::current_exe()
        .and_then(std::fs::metadata)
        .map(|m| {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
            format!("{}-{}", m.len(), t.map(|d| d.as_nanos()).unwrap_or(0))
        })
        .unwrap_or_default()
}

impl RunResult {
    /// No analysis: what graph and node checks read when no SQL is analyzed.
    pub fn empty() -> Self {
        RunResult {
            results: BTreeMap::new(),
            elapsed: Duration::ZERO,
            cache_hits: 0,
        }
    }

    /// The shape of every analyzed relation, for project-wide checks and the shapes file.
    pub fn shapes(&self, project: &Project) -> Shapes {
        let mut shapes = Shapes::new(&project.adapter, project.invocation_id.clone());
        shapes.relations = self
            .results
            .iter()
            .map(|(id, r)| {
                (
                    id.clone(),
                    Shape {
                        planned: r.planned,
                        summary: r.summary.clone(),
                        verdicts: r.verdicts.clone(),
                    },
                )
            })
            .collect();
        shapes
    }
}
