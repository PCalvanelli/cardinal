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
    assert!(!out.contains("G006]: incremental unique_key (order_id) is not a key of the model's rows\n --> models/marts/inc_tested_key.sql"), "{out}");
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
