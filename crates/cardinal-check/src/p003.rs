//! P003: a guarantee the base branch proves is no longer proven.
//!
//! With `--state`, the base branch's artifacts are analyzed too, and each model is
//! compared with its base version on two kinds of guarantee:
//!
//! - rows: for each parent with a key, that the model keeps each of its rows at least
//!   once, and at most once (the two halves of an exactly-once verdict);
//! - grain: each key the base's SQL proves, while its columns still exist.
//!
//! Only guarantees the base proves count, and only losses with a cause the analysis can
//! name: for rows, a filter or join that may drop them or a join that repeats them; for
//! grain, columns that are no longer a key of a model it fully analyzed. One change often breaks guarantees
//! in many downstream models, so each cause is reported once, at its root, with the
//! downstream models it also affects in a note.

use crate::Locator;
use crate::p001::site_labels;
use cardinal_dbt::{Project, RelationId};
use cardinal_diag::{Code, Diagnostic, Fix, Label, Severity, normalize_text};
use cardinal_grain::{Reason, RelationSummary, UnknownReason, Verdict};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bound {
    AtLeastOnce,
    AtMostOnce,
}

impl Bound {
    fn label(self) -> &'static str {
        match self {
            Bound::AtLeastOnce => "at least once",
            Bound::AtMostOnce => "at most once",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Guarantee {
    /// The model keeps each row of `parent` at least once, or at most once.
    Rows {
        parent: RelationId,
        display: String,
        bound: Bound,
    },
    /// No two rows of the model agree on `cols`.
    Grain { cols: Vec<String> },
}

/// A guarantee the base proves and the head does not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lost {
    pub guarantee: Guarantee,
    /// For rows, what the base proves: "exactly once", "at least once" or "at most once".
    pub before: &'static str,
    /// Why the head does not prove it. Never empty.
    pub reasons: Vec<Reason>,
}

impl Lost {
    /// An error, except rows dropped only by filters: someone wrote that predicate, so
    /// losing the rows is likely intended.
    pub fn severity(&self) -> Severity {
        let filters_only = matches!(
            self.guarantee,
            Guarantee::Rows {
                bound: Bound::AtLeastOnce,
                ..
            }
        ) && self
            .reasons
            .iter()
            .all(|r| matches!(r, Reason::Loss { site } if site.what.starts_with("filter")));
        if filters_only {
            Severity::Warning
        } else {
            Code::P003.default_severity()
        }
    }

    fn fingerprint(&self) -> String {
        match &self.guarantee {
            Guarantee::Rows { display, bound, .. } => format!("rows {display} {}", bound.label()),
            Guarantee::Grain { cols } => format!("grain ({})", cols.join(", ")),
        }
    }
}

/// How one model's guarantees changed from the base to the head.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ModelChange {
    pub lost: Vec<Lost>,
    /// Not proven on the base, proven on the head.
    pub gained: usize,
    /// Proven on both.
    pub kept: usize,
}

/// One version of a model: its summary and verdicts.
#[derive(Clone, Copy)]
pub struct Side<'a> {
    pub summary: &'a RelationSummary,
    pub verdicts: &'a [Verdict],
}

/// Whether the head's facts are complete enough that a missing key means something:
/// no operator the analysis does not support, and no input it could not analyze.
fn fully_analyzed(s: &RelationSummary) -> bool {
    !matches!(
        s.status.reason(),
        Some(
            UnknownReason::UnsupportedOperator(_)
                | UnknownReason::PlanError(_)
                | UnknownReason::PythonModel(_)
        )
    )
}

/// The reasons that name a place: a join or filter that may drop rows, or a join that
/// repeats them. The rest say the analysis cannot tell (no key, key not carried, or
/// columns it cannot show unique, as when a left join pads them with nulls), so they
/// are never reported as a loss.
fn causes(reasons: &[Reason]) -> Vec<Reason> {
    reasons
        .iter()
        .filter(|r| matches!(r, Reason::Loss { .. } | Reason::Repeats { .. }))
        .cloned()
        .collect()
}

/// Compare a model's base and head versions.
pub fn compare(base: Side<'_>, head: Side<'_>) -> ModelChange {
    let mut out = ModelChange::default();
    for vb in base.verdicts {
        // A parent that is no longer keyed, or no longer read, has no head verdict. A
        // declared contract is P001's to report.
        let Some(vh) = head
            .verdicts
            .iter()
            .find(|v| v.relation == vb.relation && !v.declared)
        else {
            continue;
        };
        for (bound, b, h) in [
            (Bound::AtLeastOnce, &vb.at_least_once, &vh.at_least_once),
            (Bound::AtMostOnce, &vb.at_most_once, &vh.at_most_once),
        ] {
            match (b.is_proven(), h.is_proven()) {
                (true, true) => out.kept += 1,
                (false, true) => out.gained += 1,
                (true, false) => {
                    let reasons = causes(h.reasons());
                    if !reasons.is_empty() {
                        out.lost.push(Lost {
                            guarantee: Guarantee::Rows {
                                parent: vh.relation.clone(),
                                display: vh.display.clone(),
                                bound,
                            },
                            before: vb.label(),
                            reasons,
                        });
                    }
                }
                (false, false) => {}
            }
        }
    }

    let (b, h) = (base.summary, head.summary);
    for k in b.proven_keys() {
        let cols = b.names(&k.cols);
        if !cols.iter().all(|c| h.col(c).is_some()) {
            continue;
        }
        if h.is_key(&cols) {
            out.kept += 1;
            continue;
        }
        if !fully_analyzed(h) {
            continue;
        }
        let mut reasons = Vec::new();
        for c in &cols {
            if let Some(Some(site)) = h.col(c).and_then(|i| h.repeats.get(i)) {
                let r = Reason::Repeats { site: site.clone() };
                if !reasons.contains(&r) {
                    reasons.push(r);
                }
            }
        }
        if reasons.is_empty() {
            reasons.push(Reason::NotUnique { cols: cols.clone() });
        }
        out.lost.push(Lost {
            guarantee: Guarantee::Grain { cols },
            before: "",
            reasons,
        });
    }
    for k in h.proven_keys() {
        let cols = h.names(&k.cols);
        if cols.iter().all(|c| b.col(c).is_some()) && !b.is_key(&cols) {
            out.gained += 1;
        }
    }
    out
}

/// Whether the model's own SQL changed. Compiled SQL names relations by database and
/// schema, which differ between environments (production artifacts against a CI
/// build), so when those names differ only the model's source file is compared.
pub fn sql_changed(base: &Project, head: &Project, id: &RelationId) -> bool {
    let (Some(b), Some(h)) = (base.get(id), head.get(id)) else {
        return true;
    };
    let same_names = std::iter::once(id)
        .chain(&h.depends_on)
        .all(|d| base.get(d).map(|r| &r.relation_parts) == head.get(d).map(|r| &r.relation_parts));
    if same_names {
        b.compiled_sql != h.compiled_sql
    } else {
        b.raw_sql != h.raw_sql
    }
}

/// Which models' losses to report, and where the others belong. A model's losses are
/// a root cause when its SQL changed or none of its ancestors lost a guarantee;
/// otherwise they follow from upstream, and go to the nearest root among its ancestors.
/// Returns each root with the downstream models attached to it.
pub fn roots(
    lossy: &BTreeSet<RelationId>,
    ancestors_of: &dyn Fn(&RelationId) -> BTreeSet<RelationId>,
    changed: &dyn Fn(&RelationId) -> bool,
) -> BTreeMap<RelationId, BTreeSet<RelationId>> {
    let ancestors: BTreeMap<&RelationId, BTreeSet<RelationId>> =
        lossy.iter().map(|m| (m, ancestors_of(m))).collect();
    let is_root = |m: &RelationId| changed(m) || ancestors[m].is_disjoint(lossy);
    let mut out: BTreeMap<RelationId, BTreeSet<RelationId>> = lossy
        .iter()
        .filter(|m| is_root(m))
        .map(|m| (m.clone(), BTreeSet::new()))
        .collect();
    for m in lossy.iter().filter(|m| !is_root(m)) {
        let candidates: Vec<&RelationId> =
            out.keys().filter(|r| ancestors[m].contains(*r)).collect();
        // The nearest: not an ancestor of another candidate.
        let nearest: Vec<RelationId> = candidates
            .iter()
            .filter(|r| !candidates.iter().any(|c| ancestors[*c].contains(**r)))
            .map(|r| (*r).clone())
            .collect();
        for r in nearest {
            out.entry(r).or_default().insert(m.clone());
        }
    }
    out
}

/// One finding: the guarantees lost at one place.
struct Group<'a> {
    primary: Label,
    secondary: Vec<Label>,
    severity: Severity,
    items: Vec<&'a Lost>,
}

/// P003 findings: for each root cause, one per place where guarantees were lost, with
/// the downstream models it also affects. `lost` must hold every model with losses.
pub fn lost_guarantees(
    base: &Project,
    head: &Project,
    lost: &BTreeMap<RelationId, Vec<Lost>>,
    locator: &dyn Locator,
) -> Vec<Diagnostic> {
    let lossy: BTreeSet<RelationId> = lost
        .iter()
        .filter(|(_, l)| !l.is_empty())
        .map(|(m, _)| m.clone())
        .collect();
    let changed = |m: &RelationId| sql_changed(base, head, m);
    let mut out = Vec::new();
    for (root, affected) in roots(&lossy, &|m| head.ancestors(m), &changed) {
        let mut findings = model_findings(&root, &lost[&root], locator);
        if !affected.is_empty()
            && let Some(first) = findings.iter_mut().max_by_key(|d| d.severity)
        {
            let mut names: Vec<String> = affected.iter().map(|m| locator.display(m)).collect();
            names.sort();
            let n = names.len();
            let mut shown: Vec<String> = names.into_iter().take(3).collect();
            if n > 3 {
                shown.push("…".into());
            }
            first.notes.push(format!(
                "also affects {n} downstream {}: {}",
                if n == 1 { "model" } else { "models" },
                shown.join(", ")
            ));
            let worst = affected
                .iter()
                .flat_map(|m| &lost[m])
                .map(Lost::severity)
                .max()
                .unwrap_or(Severity::Info);
            first.severity = first.severity.max(worst);
        }
        out.extend(findings);
    }
    out
}

fn model_findings(model: &RelationId, lost: &[Lost], locator: &dyn Locator) -> Vec<Diagnostic> {
    let name = locator.display(model);
    let display = |id: &RelationId| locator.display(id);
    let mut groups: Vec<Group<'_>> = Vec::new();
    for l in lost {
        let of = match &l.guarantee {
            Guarantee::Rows { display, .. } => display.clone(),
            Guarantee::Grain { .. } => name.clone(),
        };
        let (primary, secondary) = site_labels(model, &of, &l.reasons, locator);
        let primary =
            primary.unwrap_or_else(|| locator.label(model, None, l.reasons[0].text(&of, &display)));
        let severity = l.severity();
        match groups.iter_mut().find(|g| {
            g.primary.file == primary.file
                && g.primary.span == primary.span
                && g.severity == severity
        }) {
            Some(g) => {
                for s in secondary {
                    if !g.secondary.contains(&s) {
                        g.secondary.push(s);
                    }
                }
                g.items.push(l);
            }
            None => groups.push(Group {
                primary,
                secondary,
                severity,
                items: vec![l],
            }),
        }
    }
    groups
        .into_iter()
        .map(|g| finding(model, &name, g, &display))
        .collect()
}

/// "dropped", "repeat" or "dropped or repeat", from the bounds lost for one parent.
fn what_happens(bounds: &[Bound]) -> (&'static str, &'static str) {
    let lost = |b| bounds.contains(&b);
    match (lost(Bound::AtLeastOnce), lost(Bound::AtMostOnce)) {
        (true, true) => ("drop or repeat", "be dropped or repeat"),
        (true, false) => ("drop", "be dropped"),
        _ => ("repeat", "repeat"),
    }
}

fn finding(
    model: &RelationId,
    name: &str,
    g: Group<'_>,
    display: &dyn Fn(&RelationId) -> String,
) -> Diagnostic {
    let mut headline = None;
    let mut notes = Vec::new();
    // Rows of one parent, both halves at once when both are lost here.
    let mut parents: Vec<(&RelationId, &str, &str, Vec<Bound>)> = Vec::new();
    for l in &g.items {
        if let Guarantee::Rows {
            parent,
            display,
            bound,
        } = &l.guarantee
        {
            match parents.iter_mut().find(|p| p.0 == parent) {
                Some(p) => p.3.push(*bound),
                None => parents.push((parent, display, l.before, vec![*bound])),
            }
        }
    }
    for (_, of, before, bounds) in &parents {
        let (verb, now) = what_happens(bounds);
        headline.get_or_insert_with(|| format!("{name} may now {verb} rows of {of}"));
        let kept = match *before {
            "exactly once" => format!("kept each {of} row exactly once"),
            "at least once" => format!("kept every {of} row"),
            _ => format!("kept each {of} row at most once"),
        };
        notes.push(format!("on main, {name} {kept}; now rows may {now}"));
    }
    for l in &g.items {
        if let Guarantee::Grain { cols } = &l.guarantee {
            let (had, now) = if cols.is_empty() {
                ("had at most one row".to_string(), "it can have more")
            } else {
                (
                    format!("had one row per ({})", cols.join(", ")),
                    "it can have several",
                )
            };
            headline.get_or_insert_with(|| {
                if cols.is_empty() {
                    format!("{name} can now have more than one row")
                } else {
                    format!("{name} can now have several rows per ({})", cols.join(", "))
                }
            });
            notes.push(format!("on main, {name} {had}; now {now}"));
        }
    }
    // Why a join or filter may drop rows, when the analysis can say more.
    for l in &g.items {
        if let Guarantee::Rows { display: of, .. } = &l.guarantee {
            for r in &l.reasons {
                if let Reason::Loss { site } = r
                    && site.because.is_some()
                {
                    let t = r.text(of, display);
                    if !notes.contains(&t) {
                        notes.push(t);
                    }
                }
            }
        }
    }
    let mut parts: Vec<String> = g.items.iter().map(|l| l.fingerprint()).collect();
    parts.sort();
    parts.dedup();
    let allow = match g.primary.span {
        Some(_) => "on that line".to_string(),
        None => format!("anywhere in {}", g.primary.file),
    };
    let intended = format!("if this is intended, add `-- cardinal: allow(P003) reason: …` {allow}");
    let fix = match fix_for(&g.items[0].reasons[0]) {
        Some(f) => format!("{f}; {intended}"),
        None => intended,
    };
    Diagnostic {
        code: Code::P003,
        severity: g.severity,
        relation: model.0.clone(),
        message: headline.unwrap_or_else(|| Code::P003.title().into()),
        primary: g.primary,
        secondary: g.secondary,
        notes,
        fix: Some(Fix {
            message: fix,
            replacement: None,
        }),
        evidence: vec![],
        fingerprint: normalize_text(&parts.join("; ")),
    }
}

/// How to get the guarantee back, when there is a usual way.
fn fix_for(r: &Reason) -> Option<String> {
    match r {
        Reason::Loss { site } if site.what.contains("join") => Some(
            "use a left join, or declare the reference that guarantees a match with meta.cardinal.references"
                .into(),
        ),
        Reason::Repeats { site } => Some(format!(
            "reduce {} to one row per join key before the join",
            site.repeated_by
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cardinal_dbt::Assumption;
    use cardinal_grain::{
        Cardinality, GrainStatus, Half, JoinSite, LossSite, Site, SummaryColumn, SummaryKey,
    };

    fn id(s: &str) -> RelationId {
        RelationId(format!("model.t.{s}"))
    }

    /// A model with columns `cols` and keys given by column names; `trusted` keys rest
    /// on their own declaration.
    fn summary(
        name: &str,
        cols: &[&str],
        keys: &[&[&str]],
        trusted: &[&[&str]],
    ) -> RelationSummary {
        let pos = |k: &[&str]| -> Vec<usize> {
            k.iter()
                .map(|c| cols.iter().position(|x| x == c).unwrap())
                .collect()
        };
        let mut all: Vec<SummaryKey> = keys
            .iter()
            .map(|k| SummaryKey {
                cols: pos(k),
                why: "group by".into(),
                declared: None,
                assumptions: vec![],
            })
            .collect();
        all.extend(trusted.iter().map(|k| SummaryKey {
            cols: pos(k),
            why: String::new(),
            declared: Some(cardinal_dbt::DeclOrigin::MetaGrain),
            assumptions: vec![Assumption::Key {
                relation: id(name),
                cols: k.iter().map(|c| c.to_string()).collect(),
            }],
        }));
        RelationSummary {
            relation: id(name),
            display: name.into(),
            columns: cols
                .iter()
                .map(|c| SummaryColumn {
                    name: c.to_string(),
                    data_type: "Int64".into(),
                })
                .collect(),
            keys: all,
            fds: vec![],
            not_null: vec![],
            status: GrainStatus::Known,
            repeats: vec![None; cols.len()],
            columns_known: true,
            not_null_why: Default::default(),
            covers: vec![],
            within: vec![],
            ranks: vec![],
            losses: vec![],
            measures: vec![],
            units: vec![],
            aggregated: vec![],
        }
    }

    fn join(model: &str) -> JoinSite {
        JoinSite {
            relation: id(model),
            span: None,
            cardinality: Cardinality::OneToMany,
            repeated_by: "payments".into(),
        }
    }

    fn loss(model: &str, what: &str) -> Reason {
        Reason::Loss {
            site: LossSite {
                model: id(model),
                span: None,
                what: what.into(),
                because: None,
            },
        }
    }

    fn verdict(parent: &str, least: Half, most: Half) -> Verdict {
        Verdict {
            relation: id(parent),
            display: parent.into(),
            declared: false,
            site: Site::Output,
            key: vec!["order_id".into()],
            cols: vec!["order_id".into()],
            at_least_once: least,
            at_most_once: most,
            assumptions: vec![],
        }
    }

    fn not(r: Reason) -> Half {
        Half::NotProven { reasons: vec![r] }
    }

    #[test]
    fn rows_lost_only_when_base_proves_and_head_names_a_cause() {
        let s = summary("m", &["order_id"], &[&["order_id"]], &[]);
        let base = [
            verdict("orders", Half::Proven, Half::Proven),
            verdict("items", not(loss("m", "inner join on x")), Half::Proven),
        ];
        let head = [
            verdict(
                "orders",
                Half::Proven,
                not(Reason::Repeats { site: join("m") }),
            ),
            // Now proven at least once: gained. At most once kept.
            verdict("items", Half::Proven, Half::Proven),
        ];
        let c = compare(
            Side {
                summary: &s,
                verdicts: &base,
            },
            Side {
                summary: &s,
                verdicts: &head,
            },
        );
        assert_eq!(c.lost.len(), 1);
        assert_eq!(
            c.lost[0].guarantee,
            Guarantee::Rows {
                parent: id("orders"),
                display: "orders".into(),
                bound: Bound::AtMostOnce
            }
        );
        assert_eq!(c.lost[0].before, "exactly once");
        assert_eq!(c.lost[0].severity(), Severity::Error);
        assert_eq!(c.gained, 1);
        // orders at least once, items at most once, and the grain (order_id).
        assert_eq!(c.kept, 3);
    }

    #[test]
    fn unsure_reasons_and_declared_contracts_are_not_losses() {
        let s = summary("m", &["order_id"], &[&["order_id"]], &[]);
        let base = [
            verdict("orders", Half::Proven, Half::Proven),
            verdict("items", Half::Proven, Half::Proven),
        ];
        let mut contract = verdict("items", not(loss("m", "LIMIT")), Half::Proven);
        contract.declared = true;
        let head = [
            verdict(
                "orders",
                not(Reason::NotCarried {
                    key: vec!["order_id".into()],
                }),
                not(Reason::NotRead),
            ),
            contract,
        ];
        let c = compare(
            Side {
                summary: &s,
                verdicts: &base,
            },
            Side {
                summary: &s,
                verdicts: &head,
            },
        );
        assert!(c.lost.is_empty(), "{:?}", c.lost);

        // A left join pads the joined side with nulls, so its key is not unique in the
        // output, though no row repeats: not a loss.
        let base = [verdict("payments", Half::Proven, Half::Proven)];
        let head = [verdict(
            "payments",
            Half::Proven,
            not(Reason::NotUnique {
                cols: vec!["payment_id".into()],
            }),
        )];
        let side = |v| Side {
            summary: &s,
            verdicts: v,
        };
        assert!(compare(side(&base), side(&head)).lost.is_empty());
    }

    #[test]
    fn filter_only_losses_are_warnings() {
        let s = summary("m", &["order_id"], &[&["order_id"]], &[]);
        let base = [verdict("orders", Half::Proven, Half::Proven)];
        let head = [verdict(
            "orders",
            Half::NotProven {
                reasons: vec![loss("m", "filter `where x`"), loss("m", "filter `where y`")],
            },
            Half::Proven,
        )];
        let side = |v| Side {
            summary: &s,
            verdicts: v,
        };
        let c = compare(side(&base), side(&head));
        assert_eq!(c.lost.len(), 1);
        assert_eq!(c.lost[0].severity(), Severity::Warning);

        let head = [verdict(
            "orders",
            Half::NotProven {
                reasons: vec![loss("m", "filter `where x`"), loss("m", "inner join on y")],
            },
            Half::Proven,
        )];
        let c = compare(side(&base), side(&head));
        assert_eq!(c.lost[0].severity(), Severity::Error);
    }

    #[test]
    fn grain_lost_points_at_the_join_that_repeats_rows() {
        let base = summary("m", &["order_id", "amount"], &[&["order_id"]], &[]);
        let mut head = summary("m", &["order_id", "amount"], &[], &[]);
        head.repeats[0] = Some(join("m"));
        let c = compare(
            Side {
                summary: &base,
                verdicts: &[],
            },
            Side {
                summary: &head,
                verdicts: &[],
            },
        );
        assert_eq!(c.lost.len(), 1);
        assert_eq!(
            c.lost[0].guarantee,
            Guarantee::Grain {
                cols: vec!["order_id".into()]
            }
        );
        assert_eq!(c.lost[0].reasons, vec![Reason::Repeats { site: join("m") }]);
    }

    #[test]
    fn grain_compares_proven_keys_whose_columns_remain() {
        // A trusted key is not proven on the base; a smaller key on the head still
        // makes (a, b) unique; a renamed column cannot be compared.
        let base = summary("m", &["a", "b", "c"], &[&["a", "b"], &["c"]], &[&["b"]]);
        let head = summary("m", &["a", "b", "d"], &[&["a"], &["d"]], &[]);
        let c = compare(
            Side {
                summary: &base,
                verdicts: &[],
            },
            Side {
                summary: &head,
                verdicts: &[],
            },
        );
        assert!(c.lost.is_empty(), "{:?}", c.lost);
        // (a, b) kept; (a) gained, since a alone was not a key on the base.
        assert_eq!((c.kept, c.gained), (1, 1));

        // A model the head cannot fully analyze says nothing about its grain.
        let mut unknown = summary("m", &["a", "b", "c"], &[], &[]);
        unknown.status = GrainStatus::Unknown(UnknownReason::UnsupportedOperator("UNNEST".into()));
        let c = compare(
            Side {
                summary: &base,
                verdicts: &[],
            },
            Side {
                summary: &unknown,
                verdicts: &[],
            },
        );
        assert!(c.lost.is_empty(), "{:?}", c.lost);
    }

    #[test]
    fn cascades_go_to_the_nearest_root() {
        use std::collections::BTreeSet;
        let ids = |v: &[&str]| v.iter().map(|m| id(m)).collect::<BTreeSet<_>>();
        // a -> b -> c -> d -> f, and e alone. All but b lost guarantees; c changed.
        let chain = ["a", "b", "c", "d", "f"];
        let ancestors = |m: &RelationId| -> BTreeSet<RelationId> {
            match chain.iter().position(|x| id(x) == *m) {
                Some(i) => ids(&chain[..i]),
                None => BTreeSet::new(),
            }
        };
        let lossy = ids(&["a", "c", "d", "e", "f"]);
        let r = roots(&lossy, &ancestors, &|m| *m == id("c"));
        assert_eq!(
            r.keys().cloned().collect::<BTreeSet<_>>(),
            ids(&["a", "c", "e"])
        );
        assert_eq!(r[&id("a")], ids(&[]));
        assert_eq!(r[&id("c")], ids(&["d", "f"]), "c is nearer than a");
        assert_eq!(r[&id("e")], ids(&[]));

        // Unchanged models below a root follow it.
        let r = roots(&lossy, &ancestors, &|_| false);
        assert_eq!(r.keys().cloned().collect::<BTreeSet<_>>(), ids(&["a", "e"]));
        assert_eq!(r[&id("a")], ids(&["c", "d", "f"]));
    }
}
