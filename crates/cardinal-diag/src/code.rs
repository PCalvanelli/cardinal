use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Every code, with its layer, default severity and title, in one table. The order is
/// the order findings sort in.
macro_rules! codes {
    ($($code:ident: $layer:ident, $severity:ident, $title:literal;)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum Code {
            $($code,)*
        }

        pub const ALL_CODES: &[Code] = &[$(Code::$code,)*];

        impl Code {
            pub fn title(self) -> &'static str {
                match self {
                    $(Code::$code => $title,)*
                }
            }

            pub fn default_severity(self) -> Severity {
                match self {
                    $(Code::$code => Severity::$severity,)*
                }
            }

            /// The invariant layer the code checks. Each layer depends on the ones before
            /// it: a model that fails one is not checked by the layers after it.
            pub fn layer(self) -> Layer {
                match self {
                    $(Code::$code => Layer::$layer,)*
                }
            }
        }
    };
}

codes! {
    G001: Operator, Error, "a join repeats values that are then added up";
    G002: Operator, Warning, "declared key isn't guaranteed by the SQL";
    G003: Operator, Warning, "many-to-many join";
    G004: Node, Info, "key is unknown";
    G005: Code, Warning, "suppression matches no finding";
    G006: Operator, Warning, "unique_key isn't unique";
    G007: Operator, Warning, "window ordering can tie";
    G008: Operator, Warning, "adds up a number that can't be added";
    G009: Operator, Error, "adds amounts in different units";
    G010: Operator, Warning, "set operation matches columns by position, not name";
    G011: Operator, Warning, "filter drops rows with nulls";
    G012: Operator, Warning, "UNION merges rows that can repeat";
    G013: Operator, Warning, "count(*) counts unmatched rows of an outer join";
    G014: Operator, Warning, "deleted rows kept: soft-delete flag dropped without a filter";
    P001: Operator, Error, "exactly_once contract isn't proven";
    P002: Operator, Warning, "proof relies on something no test checks";
    P003: Operator, Error, "guarantee from main is lost";
    D001: Node, Warning, "declaration doesn't match the model";
    D002: Graph, Warning, "declaration names the wrong relation";
    E001: Code, Error, "missing compiled SQL";
    E002: Code, Error, "SQL doesn't parse";
    E003: Code, Error, "unresolved function";
    E004: Graph, Error, "unresolved relation";
    E005: Operator, Error, "unsupported SQL";
    E006: Node, Error, "unresolved column";
}

impl Code {
    pub fn is_tool_error(self) -> bool {
        matches!(
            self,
            Code::E001 | Code::E002 | Code::E003 | Code::E004 | Code::E005 | Code::E006
        )
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl FromStr for Code {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        ALL_CODES
            .iter()
            .find(|c| c.to_string().eq_ignore_ascii_case(s.trim()))
            .copied()
            .ok_or_else(|| format!("unknown code `{s}` (expected {})", expected_codes()))
    }
}

/// Every code, in prose: runs of three or more as `G001 to G014`, the rest one by one.
fn expected_codes() -> String {
    // A code's letter and number: G014 is ('G', 14).
    let parts = |c: Code| {
        let s = c.to_string();
        (s.as_bytes()[0], s[1..].parse::<u32>().unwrap_or(0))
    };
    let mut runs: Vec<Vec<Code>> = Vec::new();
    for &c in ALL_CODES {
        let (letter, n) = parts(c);
        match runs.last_mut() {
            Some(run)
                if run
                    .last()
                    .is_some_and(|&p| parts(p) == (letter, n.wrapping_sub(1))) =>
            {
                run.push(c)
            }
            _ => runs.push(vec![c]),
        }
    }
    let mut items: Vec<String> = Vec::new();
    for run in runs {
        if run.len() >= 3 {
            items.push(format!("{} to {}", run[0], run[run.len() - 1]));
        } else {
            items.extend(run.iter().map(Code::to_string));
        }
    }
    let last = items.pop().unwrap_or_default();
    if items.is_empty() {
        last
    } else {
        format!("{} or {last}", items.join(", "))
    }
}

/// The four invariant layers of a project's structure, in dependency order. Like the
/// stages of a compiler, each one checks what the one before it makes available:
///
/// - code (parsing): each model's file: its compiled SQL exists and parses.
/// - graph (name resolution): each ref() and declared relation resolves.
/// - node (declarations): each model's declared shape: columns, types, grain.
/// - operator (type checking): each join, filter, aggregate, window and set operation,
///   checked against the shapes (keys are the types) of what it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    Code,
    Graph,
    Node,
    Operator,
}

pub const ALL_LAYERS: &[Layer] = &[Layer::Code, Layer::Graph, Layer::Node, Layer::Operator];

impl Layer {
    pub fn describe(self) -> &'static str {
        match self {
            Layer::Code => "each model's file: its compiled SQL exists and parses",
            Layer::Graph => "the edges between models: every ref() and declared relation resolves",
            Layer::Node => "each model's declared shape: columns, types and grain",
            Layer::Operator => {
                "each join, filter, aggregate, window and set operation, checked against the keys of what it reads"
            }
        }
    }

    /// Whether checking this layer needs the SQL analyzed. Graph and node checks read
    /// only the manifest and catalog.
    pub fn needs_sql(self) -> bool {
        matches!(self, Layer::Code | Layer::Operator)
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Layer::Code => "code",
            Layer::Graph => "graph",
            Layer::Node => "node",
            Layer::Operator => "operator",
        })
    }
}

impl FromStr for Layer {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "code" | "file" => Ok(Layer::Code),
            "graph" | "dag" => Ok(Layer::Graph),
            "node" | "model" => Ok(Layer::Node),
            "operator" | "op" => Ok(Layer::Operator),
            other => Err(format!(
                "unknown layer `{other}` (expected code, graph, node or operator)"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        })
    }
}

impl FromStr for Severity {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "info" | "note" => Ok(Severity::Info),
            "warning" | "warn" => Ok(Severity::Warning),
            "error" | "deny" => Ok(Severity::Error),
            other => Err(format!(
                "unknown severity `{other}` (expected error, warning, info or off)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_code_lists_every_code() {
        assert_eq!(
            "nope".parse::<Code>().unwrap_err(),
            "unknown code `nope` (expected G001 to G014, P001 to P003, D001, D002 or E001 to E006)"
        );
        for &c in ALL_CODES {
            assert_eq!(c.to_string().to_lowercase().parse::<Code>(), Ok(c));
        }
    }
}
