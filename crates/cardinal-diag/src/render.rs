//! Renderers. Text follows rustc: the finding, where it is, where it came from, the
//! evidence, and the fix. JSON and SARIF carry the same fields.

use crate::code::ALL_CODES;
use crate::{Diagnostic, Label, Severity, SourceFiles};
use serde_json::{Value, json};
use std::fmt::Write;

pub const JSON_SCHEMA_VERSION: &str = "1.0.0";

#[derive(Clone, Copy)]
pub struct Style {
    pub color: bool,
}

impl Style {
    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    fn sev(&self, sev: Severity, s: &str) -> String {
        match sev {
            Severity::Error => self.paint("1;31", s),
            Severity::Warning => self.paint("1;33", s),
            Severity::Info => self.paint("1;36", s),
        }
    }
    fn blue(&self, s: &str) -> String {
        self.paint("1;34", s)
    }
    fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
}

fn line_of(text: &str, line: u32) -> &str {
    text.lines()
        .nth(line.saturating_sub(1) as usize)
        .unwrap_or("")
}

fn label_block(
    out: &mut String,
    label: &Label,
    files: &dyn SourceFiles,
    gutter: usize,
    primary: bool,
    sev: Severity,
    st: Style,
) {
    let pad = " ".repeat(gutter);
    let arrow = if primary { "-->" } else { ":::" };
    match label.span {
        Some(sp) => {
            let _ = writeln!(
                out,
                "{}{} {}:{}:{}",
                " ".repeat(gutter),
                st.blue(arrow),
                label.file,
                sp.start.line,
                sp.start.col
            );
            let text = files.text(&label.file).unwrap_or("");
            let src = line_of(text, sp.start.line);
            let _ = writeln!(out, "{pad} {}", st.blue("|"));
            let num = sp.start.line.to_string();
            let _ = writeln!(out, "{num:>gutter$} {} {}", st.blue("|"), src);
            let start = (sp.start.col as usize).saturating_sub(1);
            let src_len = src.chars().count();
            let end = if sp.end.line == sp.start.line {
                (sp.end.col as usize).saturating_sub(1)
            } else {
                src_len
            };
            let width = end.saturating_sub(start).max(1);
            let mark = if primary { "^" } else { "-" };
            let marks = mark.repeat(width);
            let marks = if primary {
                st.sev(sev, &marks)
            } else {
                st.blue(&marks)
            };
            let msg = if primary {
                st.sev(sev, &label.message)
            } else {
                st.blue(&label.message)
            };
            let _ = writeln!(
                out,
                "{pad} {} {}{} {}",
                st.blue("|"),
                " ".repeat(start),
                marks,
                msg
            );
        }
        None => {
            let _ = writeln!(
                out,
                "{}{} {}",
                " ".repeat(gutter),
                st.blue(arrow),
                label.file
            );
            if !label.message.is_empty() {
                let _ = writeln!(out, "{pad} {}", st.blue("|"));
                let _ = writeln!(out, "{pad} {} {}", st.blue("="), label.message);
            }
        }
    }
}

pub fn text(d: &Diagnostic, files: &dyn SourceFiles, st: Style) -> String {
    let mut out = String::new();
    let max_line = std::iter::once(&d.primary)
        .chain(d.secondary.iter())
        .filter_map(|l| l.span.map(|s| s.start.line))
        .max()
        .unwrap_or(1);
    let gutter = max_line.to_string().len();
    let pad = " ".repeat(gutter);
    let _ = writeln!(
        out,
        "{}{}",
        st.sev(d.severity, &format!("{}[{}]", d.severity, d.code)),
        st.bold(&format!(": {}", d.message))
    );
    label_block(&mut out, &d.primary, files, gutter, true, d.severity, st);
    for s in &d.secondary {
        let _ = writeln!(out, "{pad} {}", st.blue("|"));
        label_block(&mut out, s, files, gutter, false, d.severity, st);
    }
    let _ = writeln!(out, "{pad} {}", st.blue("|"));
    for n in &d.notes {
        let _ = writeln!(
            out,
            "{pad} {} {}",
            st.blue("="),
            format_args!("{} {n}", st.bold("note:"))
        );
    }
    if let Some(f) = &d.fix {
        let _ = writeln!(
            out,
            "{pad} {} {} {}",
            st.blue("="),
            st.bold("help:"),
            f.message
        );
    }
    let _ = writeln!(
        out,
        "{pad} {} {} cardinal explain {}",
        st.blue("="),
        st.bold("see:"),
        d.code
    );
    out
}

fn label_json(l: &Label) -> Value {
    json!({
        "file": l.file,
        "line": l.span.map(|s| s.start.line),
        "column": l.span.map(|s| s.start.col),
        "end_line": l.span.map(|s| s.end.line),
        "end_column": l.span.map(|s| s.end.col),
        "message": l.message,
    })
}

pub fn finding_json(d: &Diagnostic) -> Value {
    json!({
        "code": d.code.to_string(),
        "layer": d.code.layer().to_string(),
        "severity": d.severity.to_string(),
        "model": d.relation,
        "message": d.message,
        "file": d.primary.file,
        "line": d.primary.span.map(|s| s.start.line),
        "column": d.primary.span.map(|s| s.start.col),
        "end_line": d.primary.span.map(|s| s.end.line),
        "end_column": d.primary.span.map(|s| s.end.col),
        "label": d.primary.message,
        "related": d.secondary.iter().map(label_json).collect::<Vec<_>>(),
        "notes": d.notes,
        "fix": d.fix.as_ref().map(|f| json!({"message": f.message, "replacement": f.replacement})),
        "evidence": d.evidence,
        "fingerprint": d.fingerprint,
    })
}

pub fn json(findings: &[Diagnostic], summary: Value) -> String {
    let v = json!({
        "schema_version": JSON_SCHEMA_VERSION,
        "cardinal_version": env!("CARGO_PKG_VERSION"),
        "findings": findings.iter().map(finding_json).collect::<Vec<_>>(),
        "summary": summary,
    });
    serde_json::to_string_pretty(&v).unwrap() + "\n"
}

fn region(l: &Label) -> Option<Value> {
    l.span.map(|s| {
        json!({"startLine": s.start.line, "startColumn": s.start.col, "endLine": s.end.line, "endColumn": s.end.col})
    })
}

fn location(l: &Label) -> Value {
    let mut phys = json!({"artifactLocation": {"uri": l.file, "uriBaseId": "%SRCROOT%"}});
    if let Some(r) = region(l) {
        phys["region"] = r;
    }
    json!({"physicalLocation": phys, "message": {"text": l.message}})
}

/// SARIF 2.1.0, as GitHub code scanning reads it.
pub fn sarif(findings: &[Diagnostic]) -> String {
    let rules: Vec<Value> = ALL_CODES
        .iter()
        .map(|c| {
            let tags = vec![format!("layer:{}", c.layer())];
            json!({
                "id": c.to_string(),
                "name": c.title(),
                "shortDescription": {"text": c.title()},
                "fullDescription": {"text": format!("{}. Run `cardinal explain {c}` for details.", c.title())},
                "defaultConfiguration": {"level": sarif_level(c.default_severity())},
                "properties": {"tags": tags},
            })
        })
        .collect();
    let results: Vec<Value> = findings
        .iter()
        .map(|d| {
            let mut text = d.message.clone();
            if !d.primary.message.is_empty() {
                text.push_str(": ");
                text.push_str(&d.primary.message);
            }
            for n in &d.notes {
                text.push_str("\nnote: ");
                text.push_str(n);
            }
            if let Some(f) = &d.fix {
                text.push_str("\nhelp: ");
                text.push_str(&f.message);
            }
            json!({
                "ruleId": d.code.to_string(),
                "ruleIndex": ALL_CODES.iter().position(|c| *c == d.code),
                "level": sarif_level(d.severity),
                "message": {"text": text},
                "locations": [location(&d.primary)],
                "relatedLocations": d.secondary.iter().enumerate().map(|(i, l)| {
                    let mut v = location(l);
                    v["id"] = json!(i);
                    v
                }).collect::<Vec<_>>(),
                "partialFingerprints": {"cardinalFingerprint/v1": format!("{}:{}:{}", d.code, d.relation, d.fingerprint)},
                "properties": {"model": d.relation, "layer": d.code.layer().to_string(), "evidence": d.evidence},
            })
        })
        .collect();
    let v = json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {"driver": {
                "name": "cardinal",
                "version": env!("CARGO_PKG_VERSION"),
                "informationUri": "https://github.com/PCalvanelli/cardinal",
                "rules": rules,
            }},
            "results": results,
        }],
    });
    serde_json::to_string_pretty(&v).unwrap() + "\n"
}

fn sarif_level(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "note",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Code, Fix};
    use cardinal_sql::{LineCol, SqlSpan};
    use std::collections::HashMap;

    #[test]
    fn renders_rustc_style() {
        let mut files = HashMap::new();
        files.insert(
            "models/marts/daily_revenue.sql".to_string(),
            "select\n  d,\n  x,\n  y,\n  sum(line_amount) + sum(shipping_fee) as revenue\n"
                .to_string(),
        );
        files.insert(
            "models/intermediate/order_lines.sql".to_string(),
            "\n".repeat(10) + "join shop.order_items i using (order_id)\n",
        );
        let sp = |l, a, b| {
            Some(SqlSpan {
                start: LineCol { line: l, col: a },
                end: LineCol { line: l, col: b },
            })
        };
        let d = Diagnostic {
            code: Code::G001,
            severity: Severity::Error,
            relation: "model.shop.daily_revenue".into(),
            message: "aggregation fans out".into(),
            primary: Label {
                file: "models/marts/daily_revenue.sql".into(),
                span: sp(5, 22, 39),
                message: "shipping_fee is determined by (order_id)".into(),
            },
            secondary: vec![Label {
                file: "models/intermediate/order_lines.sql".into(),
                span: sp(11, 1, 41),
                message: "one-to-many: rows repeat per line item".into(),
            }],
            notes: vec![
                "order_lines has grain (order_id, line_id), from test unique_order_lines_combo"
                    .into(),
            ],
            fix: Some(Fix {
                message: "sum shipping_fee per order before the join, or read it from shop.orders"
                    .into(),
                replacement: None,
            }),
            evidence: vec![],
            fingerprint: String::new(),
        };
        let got = text(&d, &files, Style { color: false });
        let want = "\
error[G001]: aggregation fans out
  --> models/marts/daily_revenue.sql:5:22
   |
 5 |   sum(line_amount) + sum(shipping_fee) as revenue
   |                      ^^^^^^^^^^^^^^^^^ shipping_fee is determined by (order_id)
   |
  ::: models/intermediate/order_lines.sql:11:1
   |
11 | join shop.order_items i using (order_id)
   | ---------------------------------------- one-to-many: rows repeat per line item
   |
   = note: order_lines has grain (order_id, line_id), from test unique_order_lines_combo
   = help: sum shipping_fee per order before the join, or read it from shop.orders
   = see: cardinal explain G001
";
        assert_eq!(got, want);
    }
}
