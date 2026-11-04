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
    P001,
    P002,
    P003,
    E001,
    E002,
    E003,
    E004,
    E005,
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
    Code::P001,
    Code::P002,
    Code::P003,
    Code::E001,
    Code::E002,
    Code::E003,
    Code::E004,
    Code::E005,
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
            Code::P001 => "exactly_once contract isn't proven",
            Code::P002 => "proof relies on something no test checks",
            Code::P003 => "guarantee from main is lost",
            Code::E001 => "missing compiled SQL",
            Code::E002 => "SQL doesn't parse",
            Code::E003 => "unresolved function",
            Code::E004 => "unresolved relation or column",
            Code::E005 => "unsupported SQL",
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
            | Code::P002 => Severity::Warning,
            Code::G004 => Severity::Info,
            _ => Severity::Error,
        }
    }

    pub fn is_tool_error(self) -> bool {
        matches!(
            self,
            Code::E001 | Code::E002 | Code::E003 | Code::E004 | Code::E005
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
                format!("unknown code `{s}` (expected G001 to G009, P001 to P003 or E001 to E005)")
            })
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
