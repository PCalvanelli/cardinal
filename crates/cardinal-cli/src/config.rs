//! The cardinal.toml `cardinal init` writes: a starter config and `[[stub]]` blocks.

#[cfg(test)]
use cardinal_engine::Config;

/// A `[[stub]]` `cardinal init` writes for a function the models call that Cardinal
/// doesn't know, with the models that call it.
#[derive(Clone)]
pub struct NewStub {
    pub name: String,
    pub models: Vec<String>,
}

/// A TOML basic string.
fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// The `[[stub]]` blocks for `stubs`, ready to paste into cardinal.toml.
pub fn stub_blocks(stubs: &[NewStub]) -> String {
    let mut out = String::new();
    for s in stubs {
        let mut used: Vec<&str> = s.models.iter().take(3).map(String::as_str).collect();
        if s.models.len() > 3 {
            used.push("...");
        }
        out.push_str(&format!(
            "[[stub]]\nname = {}\nreturns = \"arg0\"   # TODO: the return type (a warehouse type); \"arg0\" is the first argument's type\n# kind = \"aggregate\"   # if it is an aggregate\n# Called by {}\n\n",
            toml_str(&s.name),
            used.join(", ")
        ));
    }
    out
}

/// A starter cardinal.toml. `dialect_note` says where the dialect came from, as
/// comment lines.
pub fn template(dialect: &str, dialect_note: &str, stubs: &[NewStub]) -> String {
    let stubs = if stubs.is_empty() {
        String::new()
    } else {
        format!(
            "# `cardinal init` found these functions in your models and wrote a stub for each.\n# Check each return type.\n{}",
            stub_blocks(stubs)
        )
    };
    format!(
        r#"# cardinal.toml
{dialect_note}
dialect = {dialect}

# Layers `cardinal check` checks: "code", "graph", "node", "operator". Default: all.
# layers = ["code", "graph", "node", "operator"]

# Severity per code: "error", "warning", "info" or "off".
[severity]
# G003 = "error"

# Severity for files under a path; the longest matching path wins.
# [[override]]
# path = "models/legacy/"
# G001 = "warning"

# Warehouse functions DataFusion does not know. Stubs are never run; only the
# return type matters.
# [[stub]]
# name = "convert_timezone"
# returns = "timestamp"      # a warehouse type, or "arg0" for the first argument's type
# deterministic = true
# kind = "aggregate"         # for aggregates; add idempotent = true if duplicates cannot change it

{stubs}# Today's findings, recorded by `cardinal init` or `cardinal check --update-baseline`.
# In CI, `cardinal check --baseline .cardinal/baseline.json` fails only on new findings.
# Uncomment to apply the baseline to every run, which hides the findings it holds.
# [baseline]
# path = ".cardinal/baseline.json"

# Where `cardinal guess` writes the dbt tests that check its guesses.
# [guess]
# path = "tests/cardinal"
"#,
        dialect = toml_str(dialect),
    )
}

#[cfg(test)]
mod layer_tests {
    use super::*;

    #[test]
    fn template_parses_with_layers_uncommented() {
        let t = template("duckdb", "# note", &[]).replace("# layers = ", "layers = ");
        let c: Config = toml::from_str(&t).unwrap();
        assert_eq!(c.layers, vec!["code", "graph", "node", "operator"]);
    }

    #[test]
    fn template_stubs_parse() {
        let stubs = [
            NewStub {
                name: "my_udf".into(),
                models: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            },
            NewStub {
                name: "odd\"name".into(),
                models: vec!["a".into()],
            },
        ];
        let t = template("snowflake", "# Detected.", &stubs);
        let c: Config = toml::from_str(&t).unwrap();
        assert_eq!(c.dialect.as_deref(), Some("snowflake"));
        let names: Vec<&str> = c.stub.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["my_udf", "odd\"name"]);
        assert!(c.stub.iter().all(|s| s.returns == "arg0"));
        assert!(c.baseline.path.is_none());
        assert!(t.contains("# Called by a, b, c, ..."), "{t}");
    }
}
