//! Inline suppressions: `-- derive: allow(G001) reason: fee is allocated per line upstream`.
//! A SQL comment survives compilation (Jinja `{# #}` comments do not). It applies to its
//! own line, or to the next line when it stands alone.

use crate::Code;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suppression {
    pub file: String,
    /// Line of the comment.
    pub line: u32,
    /// Line the suppression applies to.
    pub target_line: u32,
    pub codes: Vec<Code>,
    pub reason: Option<String>,
    /// 1-based columns of the comment on its line, end exclusive.
    pub col_start: u32,
    pub col_end: u32,
}

pub fn parse(file: &str, text: &str) -> Vec<Suppression> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let Some(at) = line.find("--") else { continue };
        let comment = line[at + 2..].trim();
        let Some(rest) = comment.strip_prefix("derive:") else {
            continue;
        };
        let rest = rest.trim();
        let Some(args) = rest.strip_prefix("allow(") else {
            continue;
        };
        let Some(close) = args.find(')') else {
            continue;
        };
        let codes: Vec<Code> = args[..close]
            .split(',')
            .filter_map(|c| c.trim().parse().ok())
            .collect();
        if codes.is_empty() {
            continue;
        }
        let after = args[close + 1..].trim();
        let reason = after
            .strip_prefix("reason:")
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty());
        let alone = line[..at].trim().is_empty();
        let n = i as u32 + 1;
        out.push(Suppression {
            file: file.to_string(),
            line: n,
            target_line: if alone { n + 1 } else { n },
            codes,
            reason,
            col_start: line[..at].chars().count() as u32 + 1,
            col_end: line.trim_end().chars().count() as u32 + 1,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_and_standalone() {
        let t = "select\n  sum(fee) as s  -- derive: allow(G001) reason: allocated per line\n  -- derive: allow(G003, G001)\n  , x\n";
        let s = parse("m.sql", t);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].target_line, 2);
        assert_eq!(s[0].reason.as_deref(), Some("allocated per line"));
        assert_eq!(s[1].target_line, 4);
        assert_eq!(s[1].codes, vec![Code::G003, Code::G001]);
        assert!(s[1].reason.is_none());
    }
}
