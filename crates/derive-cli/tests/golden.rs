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

fn derive(dir: &PathBuf, args: &[&str]) -> (String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_derive"))
        .args(args)
        .arg("--project-dir")
        .arg(dir)
        .env("DERIVE_ALLOW_STALE", "1")
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
    if let Some(d) = v.pointer_mut("/derive_version") {
        *d = serde_json::json!("<version>");
    }
    if let Some(d) = v.pointer_mut("/runs/0/tool/driver/version") {
        *d = serde_json::json!("<version>");
    }
    v
}

#[test]
fn shop_text() {
    let (out, code) = derive(
        &fixture("shop"),
        &["check", "--no-cache", "--min-severity", "info"],
    );
    assert_eq!(code, 1, "G001 is an error, so the exit code is 1");
    insta::assert_snapshot!(out);
}

#[test]
fn shop_json() {
    let (out, _) = derive(
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
    let (out, _) = derive(
        &fixture("shop"),
        &["check", "--no-cache", "--format", "sarif"],
    );
    insta::assert_json_snapshot!(json_without_timing(&out));
}

#[test]
fn shop_describe() {
    let (out, code) = derive(&fixture("shop"), &["describe", "order_lines"]);
    assert_eq!(code, 0);
    insta::assert_snapshot!(out);
}

#[test]
fn shop_coverage() {
    let (out, _) = derive(&fixture("shop"), &["coverage"]);
    insta::assert_snapshot!(out);
}

#[test]
fn select_limits_findings() {
    let (out, _) = derive(
        &fixture("shop"),
        &["check", "--no-cache", "--select", "customer_promos"],
    );
    assert!(out.contains("G003"), "{out}");
    assert!(!out.contains("G001"), "{out}");
}

#[test]
fn allow_turns_code_off() {
    let (out, code) = derive(
        &fixture("shop"),
        &["check", "--no-cache", "--allow", "G001"],
    );
    assert!(!out.contains("error[G001]"), "{out}");
    assert_eq!(code, 0);
}

#[test]
fn baseline_hides_known_findings() {
    let tmp = std::env::temp_dir().join(format!("derive-baseline-{}.json", std::process::id()));
    let b = tmp.to_str().unwrap();
    let (_, code) = derive(
        &fixture("shop"),
        &["check", "--no-cache", "--update-baseline", "--baseline", b],
    );
    assert_eq!(code, 0);
    let (out, code) = derive(&fixture("shop"), &["check", "--no-cache", "--baseline", b]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("No findings"), "{out}");
    let _ = std::fs::remove_file(tmp);
}

#[test]
fn missing_artifacts_exit_2() {
    let dir = std::env::temp_dir().join(format!("derive-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("dbt_project.yml"), "name: x\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_derive"))
        .args(["check", "--project-dir"])
        .arg(&dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("run: dbt compile"));
    let _ = std::fs::remove_dir_all(dir);
}
