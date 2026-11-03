//! The analysis pipeline: one DAG level at a time, models in a level in parallel.
//! A model's finished facts become the schema and facts its dependents' scans read.

use crate::config::Config;
use derive_cache::{Cache, KeyBuilder};
use derive_check::{CheckContext, Locator, model_checks, plan_failure};
use derive_dbt::{Kind, Project, Relation, RelationId};
use derive_diag::sourcemap::SourceMap;
use derive_diag::{Diagnostic, Label};
use derive_grain::{
    Cardinality, GrainStatus, RelationSummary, UnknownReason, UpstreamFacts, analyze, summarize,
    summary_from_declared,
};
use derive_sql::provider::{Functions, TableLookup};
use derive_sql::sqlparser::dialect::Dialect;
use derive_sql::{DeriveContext, DeriveTable, SqlSpan, plan_sql};
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
}

/// Everything later stages need from one relation; this is what the cache stores.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelResult {
    pub summary: RelationSummary,
    pub diagnostics: Vec<Diagnostic>,
    pub joins: Vec<JoinSummary>,
    /// True when the SQL parsed and planned.
    pub planned: bool,
    #[serde(skip)]
    pub from_cache: bool,
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
}

struct Lookup<'a> {
    project: &'a Project,
    done: &'a BTreeMap<RelationId, ModelResult>,
}

impl TableLookup for Lookup<'_> {
    fn lookup(&self, parts: &[String]) -> Option<Arc<DeriveTable>> {
        let id = self.project.resolve(parts)?;
        let r = self.done.get(id)?;
        Some(Arc::new(DeriveTable {
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
        let (dialect, _) = derive_sql::dialect::dialect_for_adapter(&adapter);
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
            .add_json(&rel.columns);
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
            planned: false,
            from_cache: false,
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
        if let Some(mut hit) = self.cache.get::<ModelResult>(&rel.id.0, &key) {
            hit.from_cache = true;
            return hit;
        }
        let lookup = Lookup {
            project: self.project,
            done,
        };
        let ctx = DeriveContext {
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
                let (summary, declared) = summarize(rel, &schema, model.output(), &model.arena);
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
                    })
                    .collect();
                ModelResult {
                    summary,
                    diagnostics,
                    joins,
                    planned: true,
                    from_cache: false,
                }
            }
        };
        self.cache.put(&rel.id.0, &key, &result);
        result
    }
}

/// Identifies this build of Derive, so a rebuilt binary never reuses facts computed by
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

/// Statuses of every analyzed relation, for root-cause grouping.
pub fn statuses(run: &RunResult) -> BTreeMap<RelationId, GrainStatus> {
    run.results
        .iter()
        .map(|(k, v)| (k.clone(), v.summary.status.clone()))
        .collect()
}
