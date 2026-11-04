use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Code {
    G001,
    G002,
    G003,
    G004,
    G005,
    G006,
    G007,
    G008,
    G009,
    G010,
    G011,
    P001,
    P002,
    P003,
    D001,
    D002,
    E001,
    E002,
    E003,
    E004,
    E005,
    E006,
}

pub const ALL_CODES: &[Code] = &[
    Code::G001,
    Code::G002,
    Code::G003,
    Code::G004,
    Code::G005,
    Code::G006,
    Code::G007,
    Code::G008,
    Code::G009,
    Code::G010,
    Code::G011,
    Code::P001,
    Code::P002,
    Code::P003,
    Code::D001,
    Code::D002,
    Code::E001,
    Code::E002,
    Code::E003,
    Code::E004,
    Code::E005,
    Code::E006,
];

impl Code {
    pub fn title(self) -> &'static str {
        match self {
            Code::G001 => "a join repeats values that are then added up",
            Code::G002 => "declared key isn't guaranteed by the SQL",
            Code::G003 => "many-to-many join",
            Code::G004 => "key is unknown",
            Code::G005 => "suppression matches no finding",
            Code::G006 => "incremental unique_key isn't unique",
            Code::G007 => "window ordering can tie",
            Code::G008 => "adds up a number that can't be added",
            Code::G009 => "adds amounts in different units",
            Code::G010 => "set operation matches columns by position, not name",
            Code::G011 => "filter drops rows with nulls",
            Code::P001 => "exactly_once contract isn't proven",
            Code::P002 => "proof relies on something no test checks",
            Code::P003 => "guarantee from main is lost",
            Code::D001 => "declaration doesn't match the model",
            Code::D002 => "declaration names the wrong relation",
            Code::E001 => "missing compiled SQL",
            Code::E002 => "SQL doesn't parse",
            Code::E003 => "unresolved function",
            Code::E004 => "unresolved relation",
            Code::E005 => "unsupported SQL",
            Code::E006 => "unresolved column",
        }
    }

    pub fn default_severity(self) -> Severity {
        match self {
            Code::G001 => Severity::Error,
            Code::G002
            | Code::G003
            | Code::G005
            | Code::G006
            | Code::G007
            | Code::G008
            | Code::G010
            | Code::G011
            | Code::P002
            | Code::D001
            | Code::D002 => Severity::Warning,
            Code::G004 => Severity::Info,
            _ => Severity::Error,
        }
    }

    /// The invariant layer the code checks. Each layer depends on the ones before it:
    /// a model that fails one is not checked by the layers after it.
    pub fn layer(self) -> Layer {
        match self {
            Code::E001 | Code::E002 | Code::E003 | Code::G005 => Layer::Code,
            Code::E004 | Code::D002 => Layer::Graph,
            Code::E006 | Code::D001 | Code::G004 => Layer::Node,
            Code::G001
            | Code::G002
            | Code::G003
            | Code::G006
            | Code::G007
            | Code::G008
            | Code::G009
            | Code::G010
            | Code::G011
            | Code::P001
            | Code::P002
            | Code::P003
            | Code::E005 => Layer::Operator,
        }
    }

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
            .ok_or_else(|| {
                format!("unknown code `{s}` (expected G001 to G011, P001 to P003, D001, D002 or E001 to E006)")
            })
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
