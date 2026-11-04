//! Golden tests: small dbt projects under fixtures/ with their compiled manifest.json
//! and catalog.json checked in, so tests need neither dbt nor a warehouse. insta
//! snapshots the text, JSON and SARIF output. `cargo xtask fixtures` regenerates them.

use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn cardinal(dir: &PathBuf, args: &[&str]) -> (String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_cardinal"))
        .args(args)
        .arg("--project-dir")
        .arg(dir)
        .env("CARDINAL_ALLOW_STALE", "1")
        .env("NO_COLOR", "1")
        .env_remove("DBT_STATE")
        .output()
        .unwrap();
    let mut s = String::from_utf8(out.stdout).unwrap();
    // Timings vary run to run.
    let re_time = |s: &str| -> String {
        s.lines()
            .map(|l| {
                if let (Some(a), Some(b)) = (l.find(" in "), l.find("s:"))
                    && l.starts_with("Analyzed")
                    && b > a
                {
                    return format!("{} in <time>{}", &l[..a], &l[b + 1..]);
                }
                l.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    if args.contains(&"text") || !args.contains(&"--format") {
        s = re_time(&s);
    }
    (s, out.status.code().unwrap_or(-1))
}

fn json_without_timing(s: &str) -> serde_json::Value {
    let mut v: serde_json::Value = serde_json::from_str(s).unwrap();
    if let Some(summary) = v.get_mut("summary") {
        summary["elapsed_ms"] = serde_json::json!(0);
        summary["cache_hits"] = serde_json::json!(0);
    }
    if let Some(d) = v.pointer_mut("/cardinal_version") {
        *d = serde_json::json!("<version>");
    }
    if let Some(d) = v.pointer_mut("/runs/0/tool/driver/version") {
        *d = serde_json::json!("<version>");
    }
    v
}

#[test]
fn shop_text() {
    let (out, code) = cardinal(
        &fixture("shop"),
        &["check", "--no-cache", "--min-severity", "info"],
    );
    assert_eq!(code, 1, "G001 is an error, so the exit code is 1");
    insta::assert_snapshot!(out);
}

#[test]
fn shop_json() {
    let (out, _) = cardinal(
        &fixture("shop"),
        &[
            "check",
            "--no-cache",
            "--min-severity",
            "info",
            "--format",
            "json",
        ],
    );
    insta::assert_json_snapshot!(json_without_timing(&out));
}

#[test]
fn shop_sarif() {
    let (out, _) = cardinal(
        &fixture("shop"),
        &["check", "--no-cache", "--format", "sarif"],
    );
    insta::assert_json_snapshot!(json_without_timing(&out));
}

#[test]
fn shop_describe() {
    let (out, code) = cardinal(&fixture("shop"), &["describe", "order_lines"]);
    assert_eq!(code, 0);
    insta::assert_snapshot!(out);
}

#[test]
fn shop_coverage() {
    let (out, _) = cardinal(&fixture("shop"), &["coverage"]);
    insta::assert_snapshot!(out);
}

#[test]
fn select_limits_findings() {
    let (out, _) = cardinal(
        &fixture("shop"),
        &["check", "--no-cache", "--select", "customer_promos"],
    );
    assert!(out.contains("G003"), "{out}");
    assert!(!out.contains("G001"), "{out}");
}

#[test]
fn allow_turns_code_off() {
    let (out, code) = cardinal(
        &fixture("shop"),
        &["check", "--no-cache", "--allow", "G001"],
    );
    assert!(!out.contains("error[G001]"), "{out}");
    assert_eq!(code, 0);
}

#[test]
fn baseline_hides_known_findings() {
    let tmp = std::env::temp_dir().join(format!("cardinal-baseline-{}.json", std::process::id()));
    let b = tmp.to_str().unwrap();
    let (_, code) = cardinal(
        &fixture("shop"),
        &["check", "--no-cache", "--update-baseline", "--baseline", b],
    );
    assert_eq!(code, 0);
    let (out, code) = cardinal(&fixture("shop"), &["check", "--no-cache", "--baseline", b]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("No findings"), "{out}");
    let _ = std::fs::remove_file(tmp);
}

#[test]
fn missing_artifacts_exit_2() {
    let dir = std::env::temp_dir().join(format!("cardinal-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("dbt_project.yml"), "name: x\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_cardinal"))
        .args(["check", "--project-dir"])
        .arg(&dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("run: dbt compile"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn measures_text() {
    let (out, code) = cardinal(&fixture("measures"), &["check", "--no-cache"]);
    assert_eq!(code, 1, "G009 is an error, so the exit code is 1");
    insta::assert_snapshot!(out);
}

#[test]
fn measures_only_on_bad_models() {
    let (out, _) = cardinal(&fixture("measures"), &["check", "--no-cache"]);
    for clean in [
        "customer_aov.sql:",
        "daily_balances.sql:",
        "monthly_balances.sql:",
        "daily_revenue_by_currency.sql:",
        "daily_revenue_usd.sql:",
        "account_balances.sql:",
        "order_conversions.sql:",
        "region_aov.sql:6",
        "revenue_to_date_total.sql:4",
    ] {
        assert!(
            !out.lines()
                .any(|l| l.contains(" --> ") && l.contains(clean)),
            "{clean} should have no findings:\n{out}"
        );
    }
    for bad in [
        "region_aov.sql:4",
        "region_aov.sql:5",
        "monthly_completion.sql:3",
        "revenue_to_date_total.sql:3",
        "account_totals.sql:1",
        "daily_revenue.sql:1",
        "balance_total.sql:1",
    ] {
        assert!(out.contains(bad), "{bad} should have a finding:\n{out}");
    }
}

#[test]
fn measures_describe() {
    let (out, code) = cardinal(&fixture("measures"), &["describe", "customer_aov"]);
    assert_eq!(code, 0);
    assert!(out.contains("avg_order_value  average — don't add up; in units of currency"));
    let (out, _) = cardinal(&fixture("measures"), &["describe", "running_revenue"]);
    assert!(
        out.contains(
            "revenue_to_date  running total over order_date — don't add across order_date values"
        ),
        "{out}"
    );
    let (out, _) = cardinal(&fixture("measures"), &["describe", "metrics.balances"]);
    assert!(
        out.contains("balance  balance over snapshot_date — don't add across snapshot_date values"),
        "{out}"
    );
}

#[test]
fn explain_measure_codes() {
    for code in ["G008", "G009"] {
        let (out, status) = cardinal(&fixture("measures"), &["explain", code]);
        assert_eq!(status, 0);
        assert!(out.starts_with(&format!("# {code}:")), "{out}");
    }
}

#[test]
fn ledger_text() {
    let (out, code) = cardinal(&fixture("ledger"), &["check", "--no-cache"]);
    assert_eq!(
        code, 1,
        "a unique_key naming a missing column is an error: {out}"
    );
    insta::assert_snapshot!(out);
}

#[test]
fn ledger_no_false_positives() {
    let (out, _) = cardinal(&fixture("ledger"), &["check", "--no-cache"]);
    for clean in [
        "inc_orders.sql",
        "inc_payments_composite.sql",
        "inc_append.sql",
        "inc_expression_key.sql",
        "latest_payment_fixed.sql",
    ] {
        assert!(
            !out.contains(clean),
            "{clean} should have no findings:\n{out}"
        );
    }
    assert!(!out.contains("G006]: incremental unique_key (order_id) isn't unique: the SQL can return several rows per (order_id)\n --> models/marts/inc_tested_key.sql"), "{out}");
    assert!(
        !out.contains("first_value(o.status)"),
        "first_status is determined by order_id:\n{out}"
    );
    assert!(
        !out.contains("fixed_rn"),
        "payment_id breaks the ties:\n{out}"
    );
}

#[test]
fn ledger_severity_flags_apply_to_new_codes() {
    let (out, code) = cardinal(
        &fixture("ledger"),
        &["check", "--no-cache", "--allow", "G006", "--deny", "G007"],
    );
    assert!(!out.contains("G006"), "{out}");
    assert!(out.contains("error[G007]"), "{out}");
    assert_eq!(code, 1);
}

#[test]
fn explain_new_codes() {
    for code in ["G006", "G007"] {
        let (out, status) = cardinal(&fixture("ledger"), &["explain", code]);
        assert_eq!(status, 0);
        assert!(out.starts_with(&format!("# {code}:")), "{out}");
    }
}

#[test]
fn contracts_text() {
    let (out, code) = cardinal(
        &fixture("contracts"),
        &["check", "--no-cache", "--min-severity", "info"],
    );
    assert_eq!(code, 1, "unproven contracts are errors: {out}");
    insta::assert_snapshot!(out);
}

#[test]
fn contracts_proven_have_no_p001() {
    let (out, _) = cardinal(&fixture("contracts"), &["check", "--no-cache"]);
    // The file each P001 points at.
    let lines: Vec<&str> = out.lines().collect();
    let p001_files: Vec<&str> = lines
        .windows(2)
        .filter(|w| w[0].starts_with("error[P001]"))
        .map(|w| w[1].trim_start_matches(" --> "))
        .collect();
    // The third contract names a relation that doesn't exist: D002, not P001.
    assert_eq!(p001_files.len(), 2, "{out}");
    assert_eq!(out.matches("error[D002]").count(), 1, "{out}");
    for proven in [
        "stg_orders",
        "orders_enriched",
        "order_lines",
        "daily_revenue",
    ] {
        assert!(
            !p001_files
                .iter()
                .any(|f| f.contains(&format!("/{proven}.sql"))),
            "{proven} is proven:\n{out}"
        );
    }
    // order_lines and daily_revenue are proven but rest on unchecked assumptions.
    assert_eq!(out.matches("warning[P002]").count(), 2, "{out}");
    assert!(
        out.contains("Contracts: 4 of 7 exactly_once contracts proven."),
        "{out}"
    );
}

#[test]
fn contracts_json_summary() {
    let (out, _) = cardinal(
        &fixture("contracts"),
        &["check", "--no-cache", "--format", "json"],
    );
    let v = json_without_timing(&out);
    assert_eq!(v["summary"]["contracts_proven"], 4, "{v}");
    assert_eq!(v["summary"]["contracts_not_proven"], 3, "{v}");
}

#[test]
fn contracts_describe() {
    let (out, code) = cardinal(&fixture("contracts"), &["describe", "orders_enriched"]);
    assert_eq!(code, 0);
    insta::assert_snapshot!(out);
}

#[test]
fn contracts_describe_aggregate() {
    let (out, code) = cardinal(&fixture("contracts"), &["describe", "daily_revenue"]);
    assert_eq!(code, 0);
    insta::assert_snapshot!(out);
}

#[test]
fn contracts_assumptions() {
    let (out, code) = cardinal(&fixture("contracts"), &["assumptions"]);
    assert_eq!(code, 0);
    insta::assert_snapshot!(out);
}

#[test]
fn contracts_emit_tests() {
    let (out, code) = cardinal(&fixture("contracts"), &["assumptions", "--emit-tests"]);
    assert_eq!(code, 0);
    insta::assert_snapshot!(out);
}

#[test]
fn contracts_assumptions_json() {
    let (out, code) = cardinal(&fixture("contracts"), &["assumptions", "--format", "json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let all = v["assumptions"].as_array().unwrap();
    assert_eq!(all.len(), 8, "{v}");
    let unchecked = all.iter().filter(|a| a["checked"] == false).count();
    assert_eq!(unchecked, 4, "{v}");
}

#[test]
fn explain_contract_codes() {
    for code in ["P001", "P002"] {
        let (out, status) = cardinal(&fixture("contracts"), &["explain", code]);
        assert_eq!(status, 0);
        assert!(out.starts_with(&format!("# {code}:")), "{out}");
    }
}

#[test]
fn filtered_unique_test_declares_no_key() {
    // payments.payment_id has a unique test with a `where` filter: not a key.
    let (out, _) = cardinal(&fixture("contracts"), &["describe", "shop.payments"]);
    assert!(out.contains("key     unknown"), "{out}");
}

#[test]
fn guarantees_text() {
    let (out, code) = cardinal(
        &fixture("guarantees"),
        &["check", "--no-cache", "--state", "state"],
    );
    assert_eq!(code, 1, "lost guarantees are errors: {out}");
    insta::assert_snapshot!(out);
}

#[test]
fn guarantees_need_state() {
    // Without --state nothing is compared: only the many-to-many join, a warning.
    let (out, code) = cardinal(&fixture("guarantees"), &["check", "--no-cache"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("P003"), "{out}");
    assert!(out.contains("warning[G003]"), "{out}");
    assert!(!out.contains("Compared with main"), "{out}");
}

#[test]
fn guarantees_json_summary() {
    let (out, _) = cardinal(
        &fixture("guarantees"),
        &[
            "check",
            "--no-cache",
            "--state",
            "state",
            "--format",
            "json",
        ],
    );
    let v = json_without_timing(&out);
    let c = &v["summary"]["comparison"];
    assert_eq!(c["state"], "state", "{v}");
    assert_eq!(c["guarantees_lost"], 7, "{v}");
    assert_eq!(c["guarantees_gained"], 1, "{v}");
    assert_eq!(c["guarantees_kept"], 17, "{v}");
    assert_eq!(c["findings_on_main"], 1, "{v}");
    assert_eq!(v["summary"]["findings_by_code"]["P003"], 3, "{v}");
    assert_eq!(v["summary"]["suppressed"], 1, "{v}");
}

#[test]
fn guarantees_state_from_env_and_sarif() {
    let out = Command::new(env!("CARGO_BIN_EXE_cardinal"))
        .args(["check", "--no-cache", "--format", "sarif", "--project-dir"])
        .arg(fixture("guarantees"))
        .env("CARDINAL_ALLOW_STALE", "1")
        .env("DBT_STATE", "state")
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = v["runs"][0]["results"].as_array().unwrap();
    let p003 = results.iter().filter(|r| r["ruleId"] == "P003").count();
    assert_eq!(p003, 3, "{v}");
    assert!(
        v["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "P003"),
        "{v}"
    );
}

#[test]
fn guarantees_with_baseline() {
    // --update-baseline ignores --state: the baseline records this branch's own findings,
    // never P003, so a later run with --state still reports the lost guarantees.
    let tmp = std::env::temp_dir().join(format!(
        "cardinal-state-baseline-{}.json",
        std::process::id()
    ));
    let b = tmp.to_str().unwrap();
    let dir = fixture("guarantees");
    let out = Command::new(env!("CARGO_BIN_EXE_cardinal"))
        .args([
            "check",
            "--no-cache",
            "--state",
            "state",
            "--update-baseline",
            "--baseline",
            b,
            "--project-dir",
        ])
        .arg(&dir)
        .env("CARDINAL_ALLOW_STALE", "1")
        .env_remove("DBT_STATE")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        err.matches("--state is ignored with --update-baseline")
            .count(),
        1,
        "{err}"
    );
    let recorded = std::fs::read_to_string(&tmp).unwrap();
    assert!(!recorded.contains("P003"), "{recorded}");
    let (out, _) = cardinal(
        &dir,
        &["check", "--no-cache", "--state", "state", "--baseline", b],
    );
    assert!(out.contains("P003"), "{out}");
    let _ = std::fs::remove_file(tmp);
}

#[test]
fn dbt_state_without_manifest_is_ignored() {
    // DBT_STATE is ambient: one that names no manifest.json never breaks a run.
    for state in ["no-such-dir", "models"] {
        let out = Command::new(env!("CARGO_BIN_EXE_cardinal"))
            .args(["check", "--no-cache", "--project-dir"])
            .arg(fixture("guarantees"))
            .env("CARDINAL_ALLOW_STALE", "1")
            .env("NO_COLOR", "1")
            .env("DBT_STATE", state)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_ne!(out.status.code(), Some(2), "{state}: {text}");
        assert!(!text.contains("P003"), "{state}: {text}");
    }
}

#[test]
fn missing_state_exits_2() {
    let (_, code) = cardinal(
        &fixture("guarantees"),
        &["check", "--no-cache", "--state", "no-such-dir"],
    );
    assert_eq!(code, 2);
}

#[test]
fn explain_p003() {
    let (out, status) = cardinal(&fixture("guarantees"), &["explain", "P003"]);
    assert_eq!(status, 0);
    assert!(
        out.starts_with("# P003: guarantee from main is lost"),
        "{out}"
    );
}

/// A copy of a fixture in a fresh temp directory, without its fact cache, so a test can
/// change it. `skip` names paths (relative to the fixture) to leave out.
fn fixture_copy(name: &str, tag: &str, skip: &[&str]) -> PathBuf {
    fn copy(from: &std::path::Path, to: &std::path::Path, rel: &str, skip: &[&str]) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let p = e.unwrap().path();
            let n = p.file_name().unwrap().to_string_lossy().to_string();
            let r = if rel.is_empty() {
                n.clone()
            } else {
                format!("{rel}/{n}")
            };
            if skip.contains(&r.as_str()) || r == ".cardinal/cache" {
                continue;
            }
            if p.is_dir() {
                copy(&p, &to.join(&n), &r, skip);
            } else {
                std::fs::copy(&p, to.join(&n)).unwrap();
            }
        }
    }
    let dir = std::env::temp_dir().join(format!("cardinal-{name}-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy(&fixture(name), &dir, "", skip);
    dir
}

fn files_in(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn guesses_guess_fresh() {
    // Before dbt has run anything: the fixture as `cargo xtask fixtures` built it, minus
    // the guess tests, their results and the record of them.
    let dir = fixture_copy(
        "guesses",
        "fresh",
        &["tests", ".cardinal", "target/run_results.json"],
    );
    let (out, code) = cardinal(&dir, &["guess"]);
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!(out);
    let tests = dir.join("tests/cardinal");
    assert_eq!(
        files_in(&tests),
        vec![
            "cardinal_key_app_customers_customer_id_4997a671.sql",
            "cardinal_key_app_events_event_id_daa465c3.sql",
            "cardinal_ref_app_orders_customer_id_to_app_customers_9be4ebc6.sql",
        ]
    );
    // The files are the ones dbt ran to build the fixture.
    for f in files_in(&tests) {
        assert_eq!(
            std::fs::read_to_string(tests.join(&f)).unwrap(),
            std::fs::read_to_string(fixture("guesses").join("tests/cardinal").join(&f)).unwrap(),
            "{f}"
        );
    }
    // Running it again changes nothing.
    let (again, _) = cardinal(&dir, &["guess"]);
    assert!(
        again.contains("Wrote 3 tests to tests/cardinal/ (3 unchanged)."),
        "{again}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn guesses_guess_after_dbt_test() {
    // dbt ran the guesses: one key failed. Its test is removed and never proposed again.
    let dir = fixture_copy("guesses", "rerun", &[".cardinal"]);
    let (out, code) = cardinal(&dir, &["guess"]);
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!(out);
    assert_eq!(
        files_in(&dir.join("tests/cardinal")),
        vec![
            "cardinal_key_app_customers_customer_id_4997a671.sql",
            "cardinal_ref_app_orders_customer_id_to_app_customers_9be4ebc6.sql",
        ]
    );
    let log = std::fs::read_to_string(dir.join(".cardinal/guesses.json")).unwrap();
    assert_eq!(
        log,
        std::fs::read_to_string(fixture("guesses").join(".cardinal/guesses.json")).unwrap()
    );
    let (again, _) = cardinal(&dir, &["guess"]);
    assert!(!again.contains("removed"), "{again}");
    assert!(again.contains("Nothing new to guess."), "{again}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn guesses_text() {
    let (out, code) = cardinal(
        &fixture("guesses"),
        &["check", "--no-cache", "--min-severity", "info"],
    );
    assert_eq!(code, 0);
    insta::assert_snapshot!(out);
}

#[test]
fn guesses_coverage() {
    let (out, _) = cardinal(&fixture("guesses"), &["coverage"]);
    insta::assert_snapshot!(out);
}

#[test]
fn guesses_assumptions() {
    let (out, _) = cardinal(&fixture("guesses"), &["assumptions"]);
    insta::assert_snapshot!(out);
}

#[test]
fn guesses_describe() {
    let (out, _) = cardinal(&fixture("guesses"), &["describe", "orders_with_customer"]);
    assert!(out.contains("app.orders       exactly once"), "{out}");
    assert!(
        out.contains("checked by test cardinal_ref_app_orders_customer_id_to_app_customers_9be4ebc6 (Cardinal's guess, passed on "),
        "{out}"
    );
}

#[test]
fn guesses_without_results_are_unused() {
    // The guess tests are in the project but dbt has not run them: nothing is used.
    let dir = fixture_copy(
        "guesses",
        "pending",
        &[".cardinal", "target/run_results.json"],
    );
    let (out, _) = cardinal(&dir, &["check", "--no-cache"]);
    assert!(out.contains("1 with a known key"), "{out}");
    assert!(
        out.contains("Next: run dbt test --select tag:cardinal to check Cardinal's guessed keys for app.customers, app.events."),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn guesses_checked_once_when_test_is_gone() {
    // dbt passed the guesses, then their tests left the project: the guesses are still
    // used, but only as checked once, which is not checked.
    let dir = fixture_copy("guesses", "once", &["tests"]);
    let manifest = dir.join("target/manifest.json");
    let mut m: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    m["nodes"]
        .as_object_mut()
        .unwrap()
        .retain(|k, _| !k.starts_with("test.guesses.cardinal_"));
    std::fs::write(&manifest, serde_json::to_string(&m).unwrap()).unwrap();
    let (out, _) = cardinal(&dir, &["assumptions"]);
    assert!(
        out.contains("ONCE       key (customer_id) of app.customers\n           guessed by Cardinal; dbt test cardinal_key_app_customers_customer_id_4997a671 passed on "),
        "{out}"
    );
    assert!(
        out.contains("but that test is no longer in the project"),
        "{out}"
    );
    let (json, _) = cardinal(&dir, &["assumptions", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let once = v["assumptions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["checked_once"] == true && a["checked"] == false)
        .count();
    assert_eq!(once, 3, "{json}");
    let (check, _) = cardinal(&dir, &["check", "--no-cache"]);
    assert!(check.contains("3 with a known key"), "{check}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn measures_g008_evidence_names_what_the_grouping_misses() {
    let (out, _) = cardinal(
        &fixture("measures"),
        &["check", "--no-cache", "--format", "json"],
    );
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let evidence = |file: &str| -> Vec<String> {
        v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["code"] == "G008" && d.to_string().contains(file))
            .flat_map(|d| d["evidence"].as_array().unwrap().clone())
            .map(|e| e.as_str().unwrap().to_string())
            .collect()
    };
    let stock = evidence("account_totals.sql");
    assert!(
        stock.contains(&"the GROUP BY does not keep snapshot_date to one value per group".into()),
        "{stock:?}"
    );
    assert!(
        !stock.iter().any(|e| e.contains("one row of the input")),
        "{stock:?}"
    );
    let unknown = evidence("balance_total.sql");
    assert!(
        unknown
            .iter()
            .any(|e| e.contains("snapshot_date is not a column of the sum's input")),
        "{unknown:?}"
    );
    let avg = evidence("region_aov.sql");
    assert!(
        avg.iter()
            .any(|e| e.contains("does not keep each group to one row of the input")),
        "{avg:?}"
    );
}

#[test]
fn measure_declarations_invalidate_the_cache() {
    let dir = fixture_copy("measures", "measure-cache", &[]);
    let (out, _) = cardinal(&dir, &["describe", "customer_aov"]);
    assert!(out.contains("avg_order_value  average"), "{out}");

    // Declare the column additive in YAML: the SQL is unchanged, so only the
    // declaration can tell the cache the summary is out of date.
    let path = dir.join("target/manifest.json");
    let mut m: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    m["nodes"]["model.measures.customer_aov"]["columns"] = serde_json::json!({
        "avg_order_value": {
            "name": "avg_order_value",
            "meta": {"cardinal": {"measure": "additive"}}
        }
    });
    std::fs::write(&path, serde_json::to_string(&m).unwrap()).unwrap();

    let (out, _) = cardinal(&dir, &["describe", "customer_aov"]);
    assert!(
        !out.contains("avg_order_value  average"),
        "a cached summary hid the new declaration:\n{out}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

// ---- Invariant layers ------------------------------------------------------------

#[test]
fn layer_operator_reports_only_operator_findings() {
    let (out, code) = cardinal(
        &fixture("shop"),
        &["check", "--no-cache", "--layer", "operator"],
    );
    assert_eq!(code, 1, "G001 is an error:\n{out}");
    for op in ["G001", "G002", "G003", "G008"] {
        assert!(out.contains(&format!("[{op}]")), "{op}:\n{out}");
    }
    // G004 is node; G005 (a stale suppression comment) is code.
    assert!(!out.contains("[G004]") && !out.contains("[G005]"), "{out}");
    assert!(out.contains("Findings in operator only:"), "{out}");
}

#[test]
fn stale_suppression_needs_its_codes_layer() {
    // allow(G001) can only be judged stale when the operator layer is checked too.
    let (out, _) = cardinal(
        &fixture("shop"),
        &["check", "--no-cache", "--layer", "code"],
    );
    assert!(!out.contains("[G005]"), "{out}");
    assert!(!out.contains("[G001]") && !out.contains("[G003]"), "{out}");
    let (out, _) = cardinal(
        &fixture("shop"),
        &["check", "--no-cache", "--layer", "code,operator"],
    );
    assert!(out.contains("warning[G005]"), "{out}");
}

#[test]
fn a_failing_layer_hides_later_layers_of_that_model() {
    // stg_customers's SQL no longer parses (code layer): its node and operator
    // findings would rest on nothing, so only E002 is reported for it.
    let dir = bad_declarations("blocked");
    edit_manifest(&dir, |m| {
        m["nodes"]["model.contracts.stg_customers"]["compiled_code"] = "select from from".into();
    });
    let (out, _) = cardinal(&dir, &["check", "--no-cache"]);
    assert!(out.contains("error[E002]"), "{out}");
    assert!(
        !out.contains("stg_orders, which stg_customers doesn't read"),
        "D002 is after the code layer:\n{out}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn layer_list_and_json_summary() {
    let (out, _) = cardinal(
        &fixture("contracts"),
        &[
            "check",
            "--no-cache",
            "--layer",
            "graph,node",
            "--format",
            "json",
        ],
    );
    let v = json_without_timing(&out);
    assert_eq!(
        v["summary"]["layers"],
        serde_json::json!(["graph", "node"]),
        "{v}"
    );
    for f in v["findings"].as_array().unwrap() {
        assert!(f["layer"] == "graph" || f["layer"] == "node", "{f}");
    }
    assert_eq!(v["summary"]["findings_by_layer"]["operator"], 0, "{v}");
    assert_eq!(v["summary"]["sql_analyzed"], false, "{v}");
}

#[test]
fn unknown_layer_exits_2() {
    let (_, code) = cardinal(&fixture("shop"), &["check", "--layer", "dag,tables"]);
    assert_eq!(code, 2);
}

// ---- Declarations (D001, D002) and --manifest-only -------------------------------

fn edit_manifest(dir: &std::path::Path, f: impl FnOnce(&mut serde_json::Value)) {
    let p = dir.join("target/manifest.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    f(&mut v);
    std::fs::write(&p, serde_json::to_string(&v).unwrap()).unwrap();
}

/// The contracts fixture with one declaration of each kind gone wrong.
fn bad_declarations(tag: &str) -> PathBuf {
    let dir = fixture_copy("contracts", tag, &["target/cardinal_shapes.json"]);
    edit_manifest(&dir, |m| {
        let n = &mut m["nodes"]["model.contracts.daily_revenue"];
        // The model outputs order_date and revenue.
        n["meta"]["cardinal"]["grain"] = serde_json::json!(["day"]);
        n["columns"]["revenue"] = serde_json::json!({
            "name": "revenue",
            "meta": {"cardinal": {"unit": "currency"}}
        });
        // stg_customers reads only shop.customers.
        m["nodes"]["model.contracts.stg_customers"]["meta"] =
            serde_json::json!({"cardinal": {"exactly_once": "stg_orders"}});
        m["sources"]["source.contracts.shop.orders"]["columns"]["customer_id"]["meta"] =
            serde_json::json!({"cardinal": {"references": "nowhere"}});
    });
    dir
}

fn finding_lines(out: &str, prefix: &str) -> Vec<String> {
    let mut v: Vec<String> = out
        .lines()
        .filter(|l| l.contains(prefix))
        .map(|l| l.to_string())
        .collect();
    v.sort();
    v
}

#[test]
fn declarations_that_dont_match() {
    let dir = bad_declarations("decl");
    let (out, code) = cardinal(&dir, &["check", "--no-cache"]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(
        finding_lines(&out, "[D00"),
        vec![
            "error[D002]: exactly_once names `orders_archive`, which is not a model, seed, snapshot or source",
            "error[D002]: exactly_once names stg_orders, which stg_customers doesn't read",
            "warning[D001]: meta.cardinal.unit on daily_revenue is ignored",
            "warning[D002]: meta.cardinal.references on shop.orders is ignored",
        ],
        "{out}"
    );
    // The grain on a column the SQL doesn't output is G002's, which reads the SQL.
    assert!(
        out.contains("warning[G002]: declared key (day) names columns the model doesn't output"),
        "{out}"
    );
    // The non-upstream contract is D002, not also P001.
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        !lines
            .windows(2)
            .any(|w| w[0].starts_with("error[P001]") && w[1].contains("stg_customers")),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn graph_and_node_check_declarations_without_sql() {
    let dir = bad_declarations("no-sql");
    // As dbt parse writes it: no compiled SQL.
    edit_manifest(&dir, |m| {
        for n in m["nodes"].as_object_mut().unwrap().values_mut() {
            n.as_object_mut().unwrap().remove("compiled_code");
        }
    });
    let (out, code) = cardinal(&dir, &["check", "--no-cache", "--layer", "graph,node"]);
    assert_eq!(code, 1, "the D002 contracts are errors:\n{out}");
    // Without SQL, D001 also checks the grain against the catalog's columns.
    assert_eq!(
        finding_lines(&out, "[D001]"),
        vec![
            "warning[D001]: meta.cardinal.grain on daily_revenue names day, which it doesn't have",
            "warning[D001]: meta.cardinal.unit on daily_revenue is ignored",
        ],
        "{out}"
    );
    assert_eq!(finding_lines(&out, "[D002]").len(), 3, "{out}");
    for absent in ["[E001]", "[P001]", "[P002]", "[G004]"] {
        assert!(!out.contains(absent), "{absent}:\n{out}");
    }
    assert!(out.contains("(no SQL read)"), "{out}");
    assert!(
        !out.contains("Contracts:"),
        "nothing is proven without SQL:\n{out}"
    );
    assert!(
        !dir.join("target/cardinal_shapes.json").exists(),
        "no shapes without analysis"
    );
    let _ = std::fs::remove_dir_all(dir);
}

// ---- The shapes file -------------------------------------------------------------

fn run_with_log(dir: &std::path::Path, args: &[&str]) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_cardinal"))
        .args(args)
        .arg("--project-dir")
        .arg(dir)
        .env("CARDINAL_ALLOW_STALE", "1")
        .env("NO_COLOR", "1")
        .env("CARDINAL_LOG", "cardinal_cli=debug")
        .env_remove("DBT_STATE")
        .output()
        .unwrap();
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn check_writes_shapes() {
    let dir = fixture_copy("contracts", "shapes", &["target/cardinal_shapes.json"]);
    let (out, _) = cardinal(&dir, &["check", "--no-cache"]);
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("target/cardinal_shapes.json")).expect(&out),
    )
    .unwrap();
    assert_eq!(v["schema"], "cardinal.shapes/v1");
    let m: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("target/manifest.json")).unwrap())
            .unwrap();
    assert_eq!(v["manifest"], m["metadata"]["invocation_id"]);
    let shape = &v["relations"]["model.contracts.stg_orders"];
    assert_eq!(shape["planned"], true, "{shape}");
    assert!(
        !shape["summary"]["keys"].as_array().unwrap().is_empty(),
        "{shape}"
    );
    // --layer runs see only part of the findings, so they don't write shapes.
    std::fs::remove_file(dir.join("target/cardinal_shapes.json")).unwrap();
    cardinal(&dir, &["check", "--no-cache", "--layer", "code"]);
    assert!(!dir.join("target/cardinal_shapes.json").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn state_reads_mains_shapes() {
    // Main's own `cardinal check`, run on main's artifacts, writes the shapes.
    let main = fixture_copy(
        "guarantees",
        "shapes-main",
        &["target/cardinal_shapes.json"],
    );
    for f in ["manifest.json", "catalog.json"] {
        std::fs::copy(main.join("state").join(f), main.join("target").join(f)).unwrap();
    }
    cardinal(&main, &["check", "--no-cache"]);
    let shapes = main.join("target/cardinal_shapes.json");
    assert!(shapes.is_file());

    let branch = fixture_copy(
        "guarantees",
        "shapes-branch",
        &["target/cardinal_shapes.json"],
    );
    let args = ["check", "--no-cache", "--state", "state"];
    let (analyzed, log) = run_with_log(&branch, &args);
    assert!(
        log.contains("analyzing main's artifacts: no cardinal_shapes.json"),
        "{log}"
    );

    std::fs::copy(&shapes, branch.join("state/cardinal_shapes.json")).unwrap();
    let (from_file, log) = run_with_log(&branch, &args);
    assert!(log.contains("--state: using"), "{log}");
    let strip = |s: &str| {
        s.lines()
            .filter(|l| !l.starts_with("Analyzed"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(strip(&from_file), strip(&analyzed));

    // Shapes from another manifest are never used.
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&shapes).unwrap()).unwrap();
    v["manifest"] = "another-run".into();
    std::fs::write(branch.join("state/cardinal_shapes.json"), v.to_string()).unwrap();
    let (again, log) = run_with_log(&branch, &args);
    assert!(log.contains("from a different manifest.json"), "{log}");
    assert_eq!(strip(&again), strip(&analyzed));
    let _ = std::fs::remove_dir_all(main);
    let _ = std::fs::remove_dir_all(branch);
}

// ---- Operator rules through the whole pipeline -----------------------------------

fn set_sql(m: &mut serde_json::Value, model: &str, raw: &str, compiled: &str) {
    let n = &mut m["nodes"][format!("model.contracts.{model}")];
    n["raw_code"] = raw.into();
    n["compiled_code"] = compiled.into();
}

#[test]
fn set_op_and_null_filter_rules_end_to_end() {
    let dir = fixture_copy("contracts", "operators", &["target/cardinal_shapes.json"]);
    edit_manifest(&dir, |m| {
        set_sql(
            m,
            "orders_enriched",
            "select o.order_id, o.status, c.name as customer_name\nfrom {{ ref('stg_orders') }} o\nleft join {{ ref('stg_customers') }} c on o.customer_id = c.customer_id\nwhere c.name <> 'test'\n",
            "select o.order_id, o.status, c.name as customer_name\nfrom \"warehouse\".\"main\".\"stg_orders\" o\nleft join \"warehouse\".\"main\".\"stg_customers\" c on o.customer_id = c.customer_id\nwhere c.name <> 'test'\n",
        );
        set_sql(
            m,
            "completed_orders",
            "select order_id, customer_id from {{ ref('stg_orders') }}\nunion all\nselect customer_id, order_id from {{ ref('stg_orders') }}\n",
            "select order_id, customer_id from \"warehouse\".\"main\".\"stg_orders\"\nunion all\nselect customer_id, order_id from \"warehouse\".\"main\".\"stg_orders\"\n",
        );
    });
    let (out, code) = cardinal(&dir, &["check", "--no-cache", "--layer", "operator"]);
    assert_eq!(code, 1, "{out}");
    let lines: Vec<&str> = out.lines().collect();
    let at = |prefix: &str| -> Vec<String> {
        lines
            .windows(2)
            .filter(|w| w[0].starts_with(prefix))
            .map(|w| w[1].trim_start_matches(" --> ").to_string())
            .collect()
    };
    assert_eq!(
        at(
            "warning[G011]: WHERE on stg_customers's columns turns the left join into an inner join"
        ),
        vec!["models/marts/orders_enriched.sql:4:7"],
        "{out}"
    );
    assert_eq!(
        at("error[G010]"),
        vec![
            "models/marts/completed_orders.sql:3:8",
            "models/marts/completed_orders.sql:3:21"
        ],
        "{out}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn operators_text() {
    let (out, code) = cardinal(&fixture("operators"), &["check", "--no-cache"]);
    assert_eq!(code, 0, "the new codes are warnings: {out}");
    insta::assert_snapshot!(out);
}

/// Each new operator code fires on exactly the models built to trip it, and the left
/// join from a dimension proves its exactly_once contract (null-exempt keys).
#[test]
fn operators_fire_where_expected() {
    let (out, _) = cardinal(&fixture("operators"), &["check", "--no-cache"]);
    let lines: Vec<&str> = out.lines().collect();
    let at = |code: &str| -> Vec<String> {
        let mut v: Vec<String> = lines
            .windows(2)
            .filter(|w| w[0].starts_with(&format!("warning[{code}]")))
            .map(|w| {
                let loc = w[1].trim_start_matches(" --> ");
                loc.split(':').next().unwrap_or(loc).to_string()
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        at("G006"),
        vec!["snapshots/customers_snapshot.sql"],
        "{out}"
    );
    assert_eq!(
        at("G012"),
        vec!["models/marts/money_movements.sql"],
        "{out}"
    );
    assert_eq!(
        at("G013"),
        vec!["models/marts/customer_order_counts.sql"],
        "{out}"
    );
    assert_eq!(
        at("G014"),
        vec![
            "models/marts/order_totals_by_customer.sql",
            "models/staging/stg_orders_all.sql"
        ],
        "{out}"
    );
    assert!(
        out.contains("Contracts: 1 of 1 exactly_once contract proven."),
        "{out}"
    );
}

#[test]
fn explain_operator_catalog_codes() {
    for code in ["G012", "G013", "G014"] {
        let (out, status) = cardinal(&fixture("operators"), &["explain", code]);
        assert_eq!(status, 0);
        assert!(out.starts_with(&format!("# {code}:")), "{out}");
    }
}
