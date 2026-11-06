//! One analysis of a project: the selected models, analyzed with everything upstream of
//! them, and the findings and shapes drawn from it. `Analysis::compare`, the comparison
//! with the base branch, is in `state`.

use crate::config::Config;
use crate::engine::{Engine, RunResult};
use crate::pipeline::{Findings, Options, collect};
use crate::selection::closure;
use cardinal_check::Shapes;
use cardinal_dbt::{Project, RelationId};
use std::collections::BTreeSet;

/// The selected models of a project, analyzed with everything upstream of them.
pub struct Analysis<'p> {
    pub project: &'p Project,
    /// The models findings are reported for.
    pub selected: BTreeSet<RelationId>,
    pub run: RunResult,
}

impl<'p> Analysis<'p> {
    /// Analyze `selected` and its ancestors, in DAG order.
    pub fn new(
        project: &'p Project,
        config: &Config,
        selected: BTreeSet<RelationId>,
        use_cache: bool,
    ) -> Self {
        let run = Engine::new(project, config, use_cache).run(&closure(project, &selected));
        Analysis {
            project,
            selected,
            run,
        }
    }

    /// No SQL analyzed: what the graph and node checks read, from the manifest and
    /// catalog alone.
    pub fn without_sql(project: &'p Project, selected: BTreeSet<RelationId>) -> Self {
        Analysis {
            project,
            selected,
            run: RunResult::empty(),
        }
    }

    /// The base branch's artifacts (`--state`), reusing `head`'s results for models
    /// whose SQL and inputs are unchanged.
    pub(crate) fn for_state(
        base: &'p Project,
        config: &Config,
        selected: BTreeSet<RelationId>,
        use_cache: bool,
        head: &RunResult,
    ) -> Self {
        let run = Engine::for_state(base, config, use_cache, head).run(&closure(base, &selected));
        Analysis {
            project: base,
            selected,
            run,
        }
    }

    /// The shape of every analyzed relation, for project-wide checks and the shapes file.
    pub fn shapes(&self) -> Shapes {
        self.run.shapes(self.project)
    }

    /// The findings for the selected models.
    pub fn findings(&self, shapes: &Shapes, opts: &Options<'_>) -> Findings {
        collect(self.project, &self.run, shapes, &self.selected, opts)
    }
}
