//! Joins: cardinality, which keys and rows survive, and the record G003 and `describe`
//! read.

use super::filter::apply_flags;
use super::{Analyzer, JoinRecord, JoinSide, UpstreamFacts, column_names};
use crate::evidence::{Evidence, EvidenceArena, EvidenceId};
use crate::expr::{col_index, unalias};
use crate::facts::{
    Cardinality, ColSet, Deleted, Fd, GrainStatus, Ind, JoinSite, Key, MAX_KEYS, RelationFacts,
    Unit,
};
use cardinal_dbt::RelationId;
use cardinal_sql::datafusion_common::{Column, DFSchema, NullEquality};
use cardinal_sql::datafusion_expr::logical_plan::{Join, JoinType};
use cardinal_sql::datafusion_expr::utils::split_conjunction;
use cardinal_sql::datafusion_expr::{BinaryExpr, Expr, LogicalPlan, Operator};
use cardinal_sql::{CardinalTable, SourceText, SqlSpan, Tokens};
use std::collections::BTreeMap;
use std::sync::Arc;

impl Analyzer<'_> {
    pub(super) fn join(&mut self, j: &Join) -> RelationFacts {
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
        // A side is unique on its join columns when they are a superkey of it. With plain
        // `=`, a null matches nothing, so a key that holds only among non-null values (an
        // outer join's, or a `unique` test's) is enough: rows with a null there match no
        // row at all. Non-equality conditions never make a side unique on their own.
        let null_rejecting = j.null_equality == NullEquality::NullEqualsNothing;
        let unique_why = |f: &RelationFacts, cols: &ColSet| -> Option<Vec<EvidenceId>> {
            if null_rejecting && !cols.is_empty() {
                f.null_exempt_superkey_why(cols)
            } else {
                f.superkey_why(cols)
            }
        };
        let left_unique_why = unique_why(&l, &a_cols);
        let right_unique_why = unique_why(&r, &b_cols);
        let left_unique = left_unique_why.is_some();
        let right_unique = right_unique_why.is_some();
        let left_unique_why = left_unique_why.unwrap_or_default();
        let right_unique_why = right_unique_why.unwrap_or_default();
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
            sides: [
                self.join_side(&l, pairs.iter().map(|p| p.0), left_unique),
                self.join_side(&r, pairs.iter().map(|p| p.1), right_unique),
            ],
        });
        let record = self.joins.len() - 1;

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
        let needs_reference = |side: &RelationFacts, left: bool| {
            !extra
                && !pairs.is_empty()
                && pairs
                    .iter()
                    .all(|p| side.not_null.contains(&if left { p.0 } else { p.1 }))
        };
        let left_needs = left_kept.is_err()
            && matches!(jt, JoinType::Inner | JoinType::Right | JoinType::LeftSemi)
            && needs_reference(&l, true);
        let right_needs = right_kept.is_err()
            && matches!(jt, JoinType::Inner | JoinType::Left | JoinType::RightSemi)
            && needs_reference(&r, false);
        self.joins[record].sides[0].needs_reference = left_needs;
        self.joins[record].sides[1].needs_reference = right_needs;
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
        // A side whose rows each appear at most once, but which the join fills with nulls
        // on the other side's unmatched rows, keeps its keys among the rows where they
        // are non-null: every unmatched row holds nulls there. Its null-exempt keys hold
        // whenever its rows appear at most once.
        let mut null_keys: Vec<Key> = Vec::new();
        if right_unique {
            if !preserve_left {
                for k in &l.keys {
                    kept(
                        k,
                        k.cols.clone(),
                        &right_unique_why,
                        &mut self.arena,
                        &mut null_keys,
                    );
                }
            }
            for k in &l.null_keys {
                kept(
                    k,
                    k.cols.clone(),
                    &right_unique_why,
                    &mut self.arena,
                    &mut null_keys,
                );
            }
        }
        if left_unique {
            if !preserve_right {
                for k in &r.keys {
                    kept(
                        k,
                        shift(&k.cols),
                        &left_unique_why,
                        &mut self.arena,
                        &mut null_keys,
                    );
                }
            }
            for k in &r.null_keys {
                kept(
                    k,
                    shift(&k.cols),
                    &left_unique_why,
                    &mut self.arena,
                    &mut null_keys,
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
            null_keys,
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
            measures: l
                .measures
                .iter()
                .cloned()
                .chain(r.measures.iter().filter_map(|d| d.remap(|c| Some(c + nl))))
                .collect(),
            units: l
                .units
                .iter()
                .cloned()
                .chain(r.units.iter().map(|u| Unit {
                    col: u.col + nl,
                    unit: u.unit.as_ref().map(&shift),
                    site: u.site.clone(),
                }))
                .collect(),
            aggregated: l
                .aggregated
                .iter()
                .copied()
                .chain(r.aggregated.iter().map(|c| c + nl))
                .collect(),
            deleted: {
                // A join condition that reads a flag (`and not o._fivetran_deleted`)
                // applies it.
                let mut d: Vec<Deleted> = l
                    .deleted
                    .iter()
                    .cloned()
                    .chain(r.deleted.iter().map(|d| d.remap(|c| Some(c + nl))))
                    .collect();
                apply_flags(
                    &mut d,
                    j.on.iter()
                        .flat_map(|(a, b)| [a, b])
                        .chain(j.filter.as_ref()),
                    &j.schema,
                );
                d
            },
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
    /// Lineage of one side's join columns `cols`, in pair order.
    fn join_side(
        &self,
        side: &RelationFacts,
        cols: impl Iterator<Item = usize>,
        unique: bool,
    ) -> JoinSide {
        let mut out = JoinSide {
            unique,
            ..JoinSide::default()
        };
        for c in cols {
            let pick = |inds: &[Ind], proven_only: bool| -> Vec<(RelationId, String)> {
                let mut v: Vec<(RelationId, String)> = inds
                    .iter()
                    .filter(|i| !proven_only || self.arena.assumptions(i.why).is_empty())
                    .flat_map(|i| {
                        i.pairs
                            .iter()
                            .filter(|(x, _)| *x == c)
                            .map(|(_, n)| (i.relation.clone(), n.clone()))
                    })
                    .collect();
                v.sort();
                v.dedup();
                v
            };
            out.from.push(pick(&side.within, true));
            out.covers.push(pick(&side.covers, false));
        }
        out
    }

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
    /// clause in the text that names the right side on its line.
    fn find_join_span(&self, text: &SourceText<'_>, right: &LogicalPlan) -> Option<SqlSpan> {
        let names = side_aliases(right);
        let t = Tokens::new(text.text);
        for join in t.words("join") {
            let at = t.get(join).end;
            let line_end = text.text[at..]
                .find('\n')
                .map_or(text.text.len(), |n| at + n);
            let names_right = (join + 1..t.len())
                .take_while(|&k| t.get(k).start < line_end)
                .filter_map(|k| t.name(k))
                .any(|w| names.iter().any(|n| w.eq_ignore_ascii_case(n)));
            if names_right {
                let s = text.join_clause_before(text.span(at, at))?;
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
