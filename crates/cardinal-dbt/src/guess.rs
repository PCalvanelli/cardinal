//! Cardinal's guesses: keys and references `cardinal guess` proposes for relations that
//! declare none, written as dbt singular tests so dbt checks them against the data.
//!
//! Each test carries its hypothesis in `config.meta.cardinal.guess`. When
//! target/run_results.json holds results for those tests, they are merged into
//! `.cardinal/guesses.json`, since the next `dbt compile` overwrites run_results.json.
//! A guess dbt passed is then used like a declaration, always labeled as a guess; a
//! guess dbt failed is never used, and never proposed again.

use crate::names::{RelationId, tuple};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a guess claims about the data.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Hypothesis {
    /// No row of `relation` has a null in `columns`, and no two rows agree on them.
    Key {
        relation: RelationId,
        columns: Vec<String>,
    },
    /// Every non-null value of `relation.columns` appears in `to.to_columns`.
    References {
        relation: RelationId,
        columns: Vec<String>,
        to: RelationId,
        to_columns: Vec<String>,
    },
}

impl Hypothesis {
    pub fn relation(&self) -> &RelationId {
        match self {
            Hypothesis::Key { relation, .. } | Hypothesis::References { relation, .. } => relation,
        }
    }

    /// Read `config.meta.cardinal.guess` of a test node.
    pub fn from_meta(meta: Option<&Value>) -> Option<Hypothesis> {
        let g = meta?.get("cardinal")?.get("guess")?;
        let h: Hypothesis = serde_json::from_value(g.clone()).ok()?;
        let lower = |v: &[String]| v.iter().map(|c| c.to_lowercase()).collect::<Vec<_>>();
        let h = match h {
            Hypothesis::Key { relation, columns } => Hypothesis::Key {
                relation,
                columns: lower(&columns),
            },
            Hypothesis::References {
                relation,
                columns,
                to,
                to_columns,
            } => Hypothesis::References {
                relation,
                columns: lower(&columns),
                to,
                to_columns: lower(&to_columns),
            },
        };
        let ok = match &h {
            Hypothesis::Key { columns, .. } => !columns.is_empty(),
            Hypothesis::References {
                columns,
                to_columns,
                ..
            } => !columns.is_empty() && columns.len() == to_columns.len(),
        };
        ok.then_some(h)
    }

    /// "app.customers has one row per customer_id"
    pub fn describe(&self, display: &dyn Fn(&RelationId) -> String) -> String {
        match self {
            Hypothesis::Key { relation, columns } => {
                format!("{} has one row per {}", display(relation), tuple(columns))
            }
            Hypothesis::References {
                relation,
                columns,
                to,
                to_columns,
            } => format!(
                "every {}.{} is in {}.{}",
                display(relation),
                tuple(columns),
                display(to),
                tuple(to_columns)
            ),
        }
    }

    /// What a failed test shows: "app.events is not unique on event_id".
    pub fn refuted(&self, display: &dyn Fn(&RelationId) -> String) -> String {
        match self {
            Hypothesis::Key { relation, columns } => {
                format!("{} is not unique on {}", display(relation), tuple(columns))
            }
            Hypothesis::References {
                relation,
                columns,
                to,
                to_columns,
            } => format!(
                "some {}.{} values are not in {}.{}",
                display(relation),
                tuple(columns),
                display(to),
                tuple(to_columns)
            ),
        }
    }

    /// The failing rows, in plain words: "duplicate or null rows".
    pub fn failing_rows(&self, n: u64) -> String {
        let rows = if n == 1 { "row" } else { "rows" };
        match self {
            Hypothesis::Key { .. } => format!("{n} duplicate or null {rows}"),
            Hypothesis::References { .. } => format!("{n} {rows} with no match"),
        }
    }

    /// The claim alone, as `cardinal guess` lists it: "one row per customer_id".
    pub fn claim(&self, display: &dyn Fn(&RelationId) -> String) -> String {
        match self {
            Hypothesis::Key { columns, .. } => format!("one row per {}", columns.join(", ")),
            Hypothesis::References {
                columns,
                to,
                to_columns,
                ..
            } => format!(
                "every {} is in {}.{}",
                columns.join(", "),
                display(to),
                to_columns.join(", ")
            ),
        }
    }

    /// The dbt test name for a guess: "cardinal_key_app_customers_customer_id_1a2b3c4d".
    pub fn test_name(&self, display: &dyn Fn(&RelationId) -> String) -> String {
        format!("{}_{}", readable_name(self, display), hypothesis_hash(self))
    }
}

// The file `cardinal guess` writes for each guess: a dbt singular test that returns the
// rows refuting it, with the hypothesis in `config.meta.cardinal.guess` for
// `Hypothesis::from_meta` to read back.

/// First line of every file `cardinal guess` writes; marks it as safe to rewrite.
pub const GUESS_MARKER: &str = "-- Cardinal's guess:";

/// A guess `cardinal guess` writes as a dbt test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Guess {
    pub hypothesis: Hypothesis,
    /// Why Cardinal guesses it, in plain words.
    pub why: Vec<String>,
    /// What it would let Cardinal check, in plain words.
    pub unlocks: String,
    /// The dbt test name, which is also the file name.
    pub test_name: String,
}

impl Guess {
    /// The singular test for a guess: a header in plain words, the hypothesis in
    /// `config.meta.cardinal.guess`, and SQL that returns the rows that refute it.
    /// `relation_sql` renders a relation as the test reads it (`{{ source(...) }}`).
    pub fn test_sql(
        &self,
        display: &dyn Fn(&RelationId) -> String,
        relation_sql: &dyn Fn(&RelationId) -> String,
    ) -> String {
        let h = &self.hypothesis;
        let mut out = format!("{GUESS_MARKER} {}.\n", h.describe(&display));
        for w in &self.why {
            out.push_str(&format!("-- Why: {w}.\n"));
        }
        out.push_str(&format!("-- If it holds, it {}.\n", self.unlocks));
        let meta = serde_json::to_string(h).unwrap_or_default();
        match h {
            Hypothesis::Key { relation, columns } => {
                out.push_str(&format!(
                    "-- This test returns every row whose {} is null or repeated.\n",
                    tuple(columns)
                ));
                out.push_str(TRAILER);
                out.push_str(&config_line(&meta));
                let cols = columns.join(", ");
                let nulls = columns
                    .iter()
                    .map(|c| format!("{c} is null"))
                    .collect::<Vec<_>>()
                    .join(" or ");
                out.push_str(&format!(
                    "\nselect *\nfrom (\n    select\n        {cols},\n        count(*) over (partition by {cols}) as cardinal_rows\n    from {}\n) rows_by_key\nwhere {nulls} or cardinal_rows > 1\n",
                    relation_sql(relation)
                ));
            }
            Hypothesis::References {
                relation,
                columns,
                to,
                to_columns,
            } => {
                out.push_str(&format!(
                    "-- This test returns every row whose {} is not null and has no match in {}.\n",
                    tuple(columns),
                    display(to)
                ));
                out.push_str(TRAILER);
                out.push_str(&config_line(&meta));
                let on = columns
                    .iter()
                    .zip(to_columns)
                    .map(|(c, p)| format!("child.{c} = parent.{p}"))
                    .collect::<Vec<_>>()
                    .join(" and ");
                let not_null = columns
                    .iter()
                    .map(|c| format!("child.{c} is not null"))
                    .collect::<Vec<_>>()
                    .join(" and ");
                out.push_str(&format!(
                    "\nselect {}\nfrom {} child\nleft join {} parent\n    on {on}\nwhere {not_null}\n    and parent.{} is null\n",
                    columns
                        .iter()
                        .map(|c| format!("child.{c}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    relation_sql(relation),
                    relation_sql(to),
                    to_columns[0]
                ));
            }
        }
        out
    }
}

const TRAILER: &str = "-- Written by `cardinal guess`. Cardinal uses the guess only after dbt passes this test,\n-- and stops guessing it if dbt fails it. Keep the file so dbt checks the guess on every run.\n";

fn config_line(meta_json: &str) -> String {
    format!("{{{{ config(tags=['cardinal'], meta={{'cardinal': {{'guess': {meta_json}}}}}) }}}}\n")
}

fn sanitize(s: &str) -> String {
    let mut out: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

/// A short, stable hash of the whole hypothesis (relation ids, columns, and target), so
/// hypotheses whose sanitized names collide still get distinct test names.
fn hypothesis_hash(h: &Hypothesis) -> String {
    let id = match h {
        Hypothesis::Key { relation, columns } => {
            format!("key\0{}\0{}", relation.0, columns.join("\0"))
        }
        Hypothesis::References {
            relation,
            columns,
            to,
            to_columns,
        } => format!(
            "references\0{}\0{}\0{}\0{}",
            relation.0,
            columns.join("\0"),
            to.0,
            to_columns.join("\0")
        ),
    };
    blake3::hash(id.as_bytes()).to_hex()[..8].to_string()
}

fn readable_name(h: &Hypothesis, display: &dyn Fn(&RelationId) -> String) -> String {
    match h {
        Hypothesis::Key { relation, columns } => format!(
            "cardinal_key_{}_{}",
            sanitize(&display(relation)),
            sanitize(&columns.join("_"))
        ),
        Hypothesis::References {
            relation,
            columns,
            to,
            ..
        } => format!(
            "cardinal_ref_{}_{}_to_{}",
            sanitize(&display(relation)),
            sanitize(&columns.join("_")),
            sanitize(&display(to))
        ),
    }
}

/// A Cardinal guess test found in the manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuessTest {
    pub unique_id: String,
    pub name: String,
    /// Relative to the project root.
    pub file: String,
    pub hypothesis: Hypothesis,
}

/// What dbt found when it ran one guess test.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuessResult {
    pub hypothesis: Hypothesis,
    pub passed: bool,
    /// Rows the test returned.
    pub failures: u64,
    /// The dbt test's name.
    pub test: String,
    /// run_results.json's `metadata.generated_at`, or the file's modification time when
    /// it has none (RFC 3339).
    pub generated_at: String,
    /// The dbt target the test ran against (run_results.json's `args.target`), if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

impl GuessResult {
    /// The day dbt ran the test: "2026-11-04".
    pub fn date(&self) -> &str {
        self.generated_at.get(..10).unwrap_or(&self.generated_at)
    }

    /// Where and when dbt ran the test: "target prod on 2026-11-04", or just the date.
    pub fn when(&self) -> String {
        when(self.target.as_deref(), self.date())
    }

    /// "dbt test cardinal_key_app_customers_customer_id passed on 2026-11-04"
    pub fn passed_note(&self) -> GuessNote {
        GuessNote {
            test: self.test.clone(),
            date: self.date().to_string(),
            target: self.target.clone(),
        }
    }
}

fn when(target: Option<&str>, date: &str) -> String {
    match target {
        Some(t) => format!("target {t} on {date}"),
        None => date.to_string(),
    }
}

/// The test that confirmed a guess, and when: part of the guess's declaration origin.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GuessNote {
    pub test: String,
    pub date: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

impl GuessNote {
    /// "guessed by Cardinal; dbt test X passed on target prod on 2026-11-04"
    pub fn describe(&self) -> String {
        format!(
            "guessed by Cardinal; dbt test {} passed on {}",
            self.test,
            when(self.target.as_deref(), &self.date)
        )
    }
}

/// `.cardinal/guesses.json`: every guess dbt has run, newest result per hypothesis.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuessLog {
    #[serde(default)]
    pub guesses: Vec<GuessResult>,
}

impl GuessLog {
    pub fn path(root: &Path) -> PathBuf {
        root.join(".cardinal").join("guesses.json")
    }

    /// The log at `root`, empty when there is none.
    pub fn load(root: &Path) -> Result<GuessLog, String> {
        let p = Self::path(root);
        match std::fs::read(&p) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(GuessLog::default()),
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    }

    /// Write atomically, so concurrent runs never read half a file.
    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let p = Self::path(root);
        std::fs::create_dir_all(p.parent().unwrap())?;
        let tmp = p.with_extension(format!("json.{}", std::process::id()));
        let mut body = serde_json::to_string_pretty(self).unwrap_or_default();
        body.push('\n');
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &p)
    }

    pub fn get(&self, h: &Hypothesis) -> Option<&GuessResult> {
        self.guesses.iter().find(|g| &g.hypothesis == h)
    }

    /// Add results; for each hypothesis the newest wins. Returns whether anything changed.
    pub fn merge(&mut self, results: Vec<GuessResult>) -> bool {
        let mut changed = false;
        for r in results {
            match self
                .guesses
                .iter_mut()
                .find(|g| g.hypothesis == r.hypothesis)
            {
                Some(g) if newer_or_same(&r.generated_at, &g.generated_at) && *g != r => {
                    *g = r;
                    changed = true;
                }
                Some(_) => {}
                None => {
                    self.guesses.push(r);
                    changed = true;
                }
            }
        }
        self.guesses.sort_by(|a, b| a.hypothesis.cmp(&b.hypothesis));
        changed
    }

    pub fn passed(&self) -> impl Iterator<Item = &GuessResult> {
        self.guesses.iter().filter(|g| g.passed)
    }

    pub fn failed(&self) -> impl Iterator<Item = &GuessResult> {
        self.guesses.iter().filter(|g| !g.passed)
    }
}

/// Whether timestamp `a` is at or after `b`, compared as times. A timestamp that does
/// not parse is older than any that does; two that don't parse compare as strings.
fn newer_or_same(a: &str, b: &str) -> bool {
    match (parse_rfc3339(a), parse_rfc3339(b)) {
        (Some(x), Some(y)) => x >= y,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => a >= b,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if m <= 2 {
            yoe + era * 400 + 1
        } else {
            yoe + era * 400
        },
        m,
        d,
    )
}

/// An RFC 3339 timestamp ("2026-11-04T10:00:00.123Z", "...+02:00", or without an
/// offset, read as UTC as dbt writes it) as (seconds since the epoch, nanoseconds).
fn parse_rfc3339(s: &str) -> Option<(i64, u32)> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        t.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| t.parse().ok())?
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut rest = &s[19..];
    let mut nanos = 0u32;
    if let Some(r) = rest.strip_prefix('.') {
        let n = r.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        let digits = &r[..n.min(9)];
        nanos = digits.parse::<u32>().ok()? * 10u32.pow(9 - digits.len() as u32);
        rest = &r[n..];
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        o if o.len() == 6 && (o.starts_with('+') || o.starts_with('-')) && &o[3..4] == ":" => {
            let oh: i64 = o[1..3].parse().ok()?;
            let om: i64 = o[4..6].parse().ok()?;
            let v = oh * 3600 + om * 60;
            if o.starts_with('-') { -v } else { v }
        }
        _ => return None,
    };
    let secs = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - offset;
    Some((secs, nanos))
}

/// A file time as RFC 3339 in UTC: "2026-11-04T10:00:00.123456Z".
pub(crate) fn rfc3339(t: std::time::SystemTime) -> Option<String> {
    let d = t.duration_since(std::time::UNIX_EPOCH).ok()?;
    let secs = d.as_secs() as i64;
    let (y, m, day) = civil_from_days(secs.div_euclid(86400));
    let r = secs.rem_euclid(86400);
    Some(format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{:06}Z",
        r / 3600,
        r % 3600 / 60,
        r % 60,
        d.subsec_micros()
    ))
}

/// Results for guess tests in a run_results.json. Tests that errored or were skipped
/// say nothing about the data, so they are left out. So is a whole run made with
/// `--empty`: its tests ran on no rows, so a pass proves nothing. `file_time` stands in
/// for `metadata.generated_at` when the file has none.
pub fn results_from_run(
    run_results: &Value,
    tests: &[GuessTest],
    file_time: Option<&str>,
) -> Vec<GuessResult> {
    let args = run_results.get("args");
    if args
        .and_then(|a| a.get("empty"))
        .is_some_and(|e| e.as_bool() == Some(true) || e.as_str() == Some("true"))
    {
        return Vec::new();
    }
    let target = args
        .and_then(|a| a.get("target"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let generated_at = run_results
        .pointer("/metadata/generated_at")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .or(file_time)
        .unwrap_or("")
        .to_string();
    let by_id: BTreeMap<&str, &GuessTest> =
        tests.iter().map(|t| (t.unique_id.as_str(), t)).collect();
    let mut out = Vec::new();
    for r in run_results
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(t) = r
            .get("unique_id")
            .and_then(Value::as_str)
            .and_then(|id| by_id.get(id))
        else {
            continue;
        };
        let passed = match r.get("status").and_then(Value::as_str) {
            Some("pass") => true,
            Some("fail" | "warn") => false,
            _ => continue,
        };
        out.push(GuessResult {
            hypothesis: t.hypothesis.clone(),
            passed,
            failures: r.get("failures").and_then(Value::as_u64).unwrap_or(0),
            test: t.name.clone(),
            generated_at: generated_at.clone(),
            target: target.clone(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(rel: &str, cols: &[&str]) -> Hypothesis {
        Hypothesis::Key {
            relation: RelationId(rel.into()),
            columns: cols.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn result(h: Hypothesis, passed: bool, at: &str) -> GuessResult {
        GuessResult {
            hypothesis: h,
            passed,
            failures: if passed { 0 } else { 3 },
            test: "t".into(),
            generated_at: at.into(),
            target: None,
        }
    }

    #[test]
    fn reads_hypothesis_from_test_meta() {
        let meta = json!({"cardinal": {"guess": {
            "kind": "key", "relation": "source.p.app.customers", "columns": ["Customer_ID"]
        }}});
        assert_eq!(
            Hypothesis::from_meta(Some(&meta)),
            Some(key("source.p.app.customers", &["customer_id"]))
        );
        let bad = json!({"cardinal": {"guess": {"kind": "key", "relation": "x", "columns": []}}});
        assert_eq!(Hypothesis::from_meta(Some(&bad)), None);
        let r = json!({"cardinal": {"guess": {"kind": "references", "relation": "a",
            "columns": ["x"], "to": "b", "to_columns": ["y", "z"]}}});
        assert_eq!(
            Hypothesis::from_meta(Some(&r)),
            None,
            "column counts differ"
        );
        assert_eq!(Hypothesis::from_meta(None), None);
    }

    #[test]
    fn merge_is_idempotent_and_newer_wins() {
        let mut log = GuessLog::default();
        let h = key("a", &["id"]);
        assert!(log.merge(vec![result(h.clone(), false, "2026-11-01T00:00:00Z")]));
        assert!(!log.merge(vec![result(h.clone(), false, "2026-11-01T00:00:00Z")]));
        assert!(
            !log.merge(vec![result(h.clone(), true, "2026-10-01T00:00:00Z")]),
            "an older result never replaces a newer one"
        );
        assert!(log.merge(vec![result(h.clone(), true, "2026-11-04T09:00:00Z")]));
        assert_eq!(log.guesses.len(), 1);
        assert!(log.get(&h).unwrap().passed);
        assert_eq!(log.get(&h).unwrap().date(), "2026-11-04");
        assert!(log.merge(vec![result(
            key("b", &["id"]),
            true,
            "2026-11-04T09:00:00Z"
        )]));
        assert_eq!(log.guesses.len(), 2);
    }

    #[test]
    fn reads_run_results() {
        let tests = vec![
            GuessTest {
                unique_id: "test.p.k".into(),
                name: "k".into(),
                file: "tests/cardinal/k.sql".into(),
                hypothesis: key("a", &["id"]),
            },
            GuessTest {
                unique_id: "test.p.e".into(),
                name: "e".into(),
                file: "tests/cardinal/e.sql".into(),
                hypothesis: key("b", &["id"]),
            },
        ];
        let rr = json!({
            "metadata": {"generated_at": "2026-11-04T10:00:00.123Z"},
            "results": [
                {"unique_id": "test.p.k", "status": "pass", "failures": 0},
                {"unique_id": "test.p.e", "status": "error", "failures": null},
                {"unique_id": "test.p.other", "status": "fail", "failures": 2},
                {"unique_id": "model.p.m", "status": "success"}
            ]
        });
        let r = results_from_run(&rr, &tests, None);
        assert_eq!(r.len(), 1, "errors and unrelated nodes say nothing: {r:?}");
        assert!(r[0].passed);
        assert_eq!(r[0].test, "k");
        let rr = json!({"metadata": {"generated_at": "x"},
            "results": [{"unique_id": "test.p.e", "status": "fail", "failures": 12}]});
        let r = results_from_run(&rr, &tests, None);
        assert_eq!((r[0].passed, r[0].failures), (false, 12));
    }

    fn one_test() -> Vec<GuessTest> {
        vec![GuessTest {
            unique_id: "test.p.k".into(),
            name: "k".into(),
            file: "tests/cardinal/k.sql".into(),
            hypothesis: key("a", &["id"]),
        }]
    }

    #[test]
    fn empty_runs_confirm_nothing() {
        let rr = json!({
            "metadata": {"generated_at": "2026-11-04T10:00:00Z"},
            "args": {"empty": true, "target": "dev"},
            "results": [{"unique_id": "test.p.k", "status": "pass", "failures": 0}]
        });
        assert!(results_from_run(&rr, &one_test(), None).is_empty());
    }

    #[test]
    fn records_target_and_shows_it() {
        let rr = json!({
            "metadata": {"generated_at": "2026-11-04T10:00:00Z"},
            "args": {"empty": false, "target": "prod"},
            "results": [{"unique_id": "test.p.k", "status": "pass", "failures": 0}]
        });
        let r = results_from_run(&rr, &one_test(), None);
        assert_eq!(r[0].target.as_deref(), Some("prod"));
        assert_eq!(
            r[0].passed_note().describe(),
            "guessed by Cardinal; dbt test k passed on target prod on 2026-11-04"
        );
        let mut no_target = r[0].clone();
        no_target.target = None;
        assert_eq!(
            no_target.passed_note().describe(),
            "guessed by Cardinal; dbt test k passed on 2026-11-04"
        );
    }

    #[test]
    fn missing_generated_at_falls_back_to_file_time() {
        let rr = json!({"results": [{"unique_id": "test.p.k", "status": "pass", "failures": 0}]});
        let r = results_from_run(&rr, &one_test(), Some("2026-11-05T08:00:00.000000Z"));
        assert_eq!(r[0].generated_at, "2026-11-05T08:00:00.000000Z");
        let mut log = GuessLog::default();
        log.merge(vec![result(
            key("a", &["id"]),
            false,
            "2026-11-04T09:00:00Z",
        )]);
        assert!(
            log.merge(r),
            "a newer file-time result replaces the older one"
        );
        assert!(log.get(&key("a", &["id"])).unwrap().passed);
    }

    #[test]
    fn timestamps_compare_as_times() {
        // Same instant, different offsets and precision.
        assert!(newer_or_same(
            "2026-11-04T10:00:00+02:00",
            "2026-11-04T08:00:00Z"
        ));
        assert!(newer_or_same(
            "2026-11-04T08:00:00Z",
            "2026-11-04T10:00:00+02:00"
        ));
        // As strings "...T09..." > "...T08:30...+..." but as times it is earlier.
        assert!(!newer_or_same(
            "2026-11-04T09:00:00Z",
            "2026-11-04T08:30:00-01:00"
        ));
        assert!(newer_or_same(
            "2026-11-04T10:00:00.5Z",
            "2026-11-04T10:00:00.123Z"
        ));
        assert!(newer_or_same("2026-11-04T10:00:00Z", ""));
        assert!(!newer_or_same("", "2026-11-04T10:00:00Z"));
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_micros(1_793_786_400_123_456);
        let s = rfc3339(t).unwrap();
        assert_eq!(parse_rfc3339(&s), Some((1_793_786_400, 123_456_000)));
        assert_eq!(s.get(..10), Some("2026-11-04"));
    }

    #[test]
    fn key_test_returns_null_and_repeated_rows() {
        let h = Hypothesis::Key {
            relation: RelationId("source.p.app.events".into()),
            columns: vec!["event_id".into()],
        };
        let g = Guess {
            test_name: "cardinal_key_app_events_event_id".into(),
            hypothesis: h,
            why: vec!["event_id is named like the table's id".into()],
            unlocks: "lets Cardinal check 2 models that depend on it".into(),
        };
        let sql = g.test_sql(&|_| "app.events".into(), &|_| {
            "{{ source('app', 'events') }}".into()
        });
        insta::assert_snapshot!(sql, @r#"
        -- Cardinal's guess: app.events has one row per event_id.
        -- Why: event_id is named like the table's id.
        -- If it holds, it lets Cardinal check 2 models that depend on it.
        -- This test returns every row whose event_id is null or repeated.
        -- Written by `cardinal guess`. Cardinal uses the guess only after dbt passes this test,
        -- and stops guessing it if dbt fails it. Keep the file so dbt checks the guess on every run.
        {{ config(tags=['cardinal'], meta={'cardinal': {'guess': {"kind":"key","relation":"source.p.app.events","columns":["event_id"]}}}) }}

        select *
        from (
            select
                event_id,
                count(*) over (partition by event_id) as cardinal_rows
            from {{ source('app', 'events') }}
        ) rows_by_key
        where event_id is null or cardinal_rows > 1
        "#);
        let meta: serde_json::Value = serde_json::json!({"cardinal": {"guess": serde_json::from_str::<serde_json::Value>(
            &serde_json::to_string(&g.hypothesis).unwrap()).unwrap()}});
        assert_eq!(
            Hypothesis::from_meta(Some(&meta)),
            Some(g.hypothesis.clone())
        );
    }

    #[test]
    fn reference_test_returns_unmatched_rows() {
        let h = Hypothesis::References {
            relation: RelationId("o".into()),
            columns: vec!["customer_id".into()],
            to: RelationId("c".into()),
            to_columns: vec!["id".into()],
        };
        let g = Guess {
            test_name: "t".into(),
            hypothesis: h,
            why: vec![],
            unlocks: "helps".into(),
        };
        let sql = g.test_sql(&|id| id.0.clone(), &|id| {
            format!("{{{{ ref('{}') }}}}", id.0)
        });
        let body = sql.split_once("}}\n").map(|x| x.1).unwrap_or(&sql);
        assert!(sql.contains(
            "-- This test returns every row whose customer_id is not null and has no match in c."
        ));
        assert!(body.contains("from {{ ref('o') }} child\nleft join {{ ref('c') }} parent\n    on child.customer_id = parent.id\nwhere child.customer_id is not null\n    and parent.id is null\n"), "{sql}");
    }

    #[test]
    fn test_names_are_identifiers() {
        let d = |id: &RelationId| id.0.trim_start_matches("source.p.").to_string();
        let h = Hypothesis::Key {
            relation: RelationId("source.p.app.order-lines".into()),
            columns: vec!["order_id".into(), "line".into()],
        };
        let name = h.test_name(&d);
        assert!(
            name.starts_with("cardinal_key_app_order_lines_order_id_line_"),
            "{name}"
        );
        assert_eq!(name, h.test_name(&d), "stable");
        // Hypotheses whose readable names collide still get different names.
        let h2 = Hypothesis::Key {
            relation: RelationId("source.p.app.order_lines".into()),
            columns: vec!["order_id".into(), "line".into()],
        };
        let h3 = Hypothesis::Key {
            relation: RelationId("source.p.app.order-lines".into()),
            columns: vec!["order_id_line".into()],
        };
        assert_eq!(readable_name(&h2, &d), readable_name(&h, &d));
        assert_eq!(readable_name(&h3, &d), readable_name(&h, &d));
        assert_ne!(h2.test_name(&d), name);
        assert_ne!(h3.test_name(&d), name);
        let h = Hypothesis::References {
            relation: RelationId("source.p.app.orders".into()),
            columns: vec!["customer_id".into()],
            to: RelationId("source.p.app.customers".into()),
            to_columns: vec!["id".into()],
        };
        let name = h.test_name(&d);
        assert!(
            name.starts_with("cardinal_ref_app_orders_customer_id_to_app_customers_"),
            "{name}"
        );
        let other_target = Hypothesis::References {
            relation: RelationId("source.p.app.orders".into()),
            columns: vec!["customer_id".into()],
            to: RelationId("source.p.app.customers".into()),
            to_columns: vec!["customer_id".into()],
        };
        assert_ne!(other_target.test_name(&d), name);
    }
}
