//! Source positions. sqlparser gives DataFusion columns their spans (1-based line and
//! column); everything else is located by expanding from those spans in the SQL text,
//! read with the one lexer in `lex`.

use crate::lex::{Kind, Tokens};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LineCol {
    pub line: u32,
    pub col: u32,
}

/// A range in one SQL text: 1-based line and column, end exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SqlSpan {
    pub start: LineCol,
    pub end: LineCol,
}

impl SqlSpan {
    pub fn from_df(s: &datafusion_common::Span) -> Option<SqlSpan> {
        if s.start.line == 0 {
            return None;
        }
        Some(SqlSpan {
            start: LineCol {
                line: s.start.line as u32,
                col: s.start.column as u32,
            },
            end: LineCol {
                line: s.end.line as u32,
                col: s.end.column as u32,
            },
        })
    }
}

/// A SQL text with a line index, for converting between spans and byte offsets.
pub struct SourceText<'a> {
    pub text: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> SourceText<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        SourceText { text, line_starts }
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Text of a 1-based line, without its newline.
    pub fn line(&self, line: u32) -> &'a str {
        let i = (line as usize).saturating_sub(1);
        let Some(&start) = self.line_starts.get(i) else {
            return "";
        };
        let end = self
            .line_starts
            .get(i + 1)
            .map(|e| e - 1)
            .unwrap_or(self.text.len());
        self.text[start..end.max(start)].trim_end_matches('\r')
    }

    pub fn offset(&self, lc: LineCol) -> usize {
        let i = (lc.line as usize).saturating_sub(1);
        let start = *self.line_starts.get(i).unwrap_or(&self.text.len());
        let line = self.line(lc.line);
        let col_bytes: usize = line
            .chars()
            .take((lc.col as usize).saturating_sub(1))
            .map(char::len_utf8)
            .sum();
        (start + col_bytes).min(self.text.len())
    }

    pub fn position(&self, offset: usize) -> LineCol {
        let i = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let start = self.line_starts[i];
        let col = self.text[start..offset.min(self.text.len())]
            .chars()
            .count() as u32
            + 1;
        LineCol {
            line: i as u32 + 1,
            col,
        }
    }

    pub fn span(&self, start: usize, end: usize) -> SqlSpan {
        SqlSpan {
            start: self.position(start),
            end: self.position(end),
        }
    }

    pub fn slice(&self, span: SqlSpan) -> &'a str {
        let (a, b) = (self.offset(span.start), self.offset(span.end));
        &self.text[a.min(b)..b.max(a)]
    }

    /// The call `name(...)` that encloses `inner`, e.g. `sum(shipping_fee)` around the
    /// column `shipping_fee`.
    pub fn enclosing_call(&self, inner: SqlSpan, name: &str) -> Option<SqlSpan> {
        let t = Tokens::new(self.text);
        let mut i = t.at(self.offset(inner.start));
        let mut depth = 0usize;
        while i > 0 {
            i -= 1;
            if t.is_punct(i, ')') {
                depth += 1;
            } else if t.is_punct(i, '(') {
                if depth > 0 {
                    depth -= 1;
                } else if let Some(w) = t.prev(i)
                    && t.is_word(w, name)
                {
                    let close = t.closing(i)?;
                    return Some(self.span(t.get(w).start, t.get(close).end));
                }
            }
        }
        None
    }

    /// `span` extended back over the call just before it, if there is one: from
    /// `over (...)` to `lag(status, 1) over (...)`.
    pub fn with_call_before(&self, span: SqlSpan) -> SqlSpan {
        let t = Tokens::new(self.text);
        let at = t.at(self.offset(span.start));
        let Some(open) = t
            .prev(at)
            .filter(|&c| t.is_punct(c, ')'))
            .and_then(|c| t.opening(c))
        else {
            return span;
        };
        let start = t
            .prev(open)
            .filter(|&w| t.get(w).kind == Kind::Word)
            .unwrap_or(open);
        self.span(t.get(start).start, self.offset(span.end))
    }

    /// The JOIN clause that precedes `at`, e.g. the `join ... using (order_id)` holding a
    /// join key. Spans from the JOIN keyword (with any LEFT/INNER/... before it) to the end
    /// of that line.
    pub fn join_clause_before(&self, at: SqlSpan) -> Option<SqlSpan> {
        let limit = self.offset(at.start);
        let t = Tokens::new(self.text);
        let join = (0..t.at(limit)).rev().find(|&i| t.is_word(i, "join"))?;
        let i = t.get(join).start;
        let line_start = self.text[..i].rfind('\n').map(|n| n + 1).unwrap_or(0);
        let prefix = self.text[line_start..i].trim().to_ascii_lowercase();
        let start = if prefix.split_whitespace().all(|w| {
            matches!(
                w,
                "left"
                    | "right"
                    | "full"
                    | "inner"
                    | "outer"
                    | "cross"
                    | "natural"
                    | "any"
                    | "asof"
            )
        }) {
            line_start
                + (self.text[line_start..i].len() - self.text[line_start..i].trim_start().len())
        } else {
            i
        };
        let line_end = self.text[i..]
            .find('\n')
            .map(|n| i + n)
            .unwrap_or(self.text.len());
        let end = start + self.text[start..line_end].trim_end().len();
        Some(self.span(start, end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_call_and_join() {
        let sql = "select\n  sum(line_amount) + sum(shipping_fee) as revenue\nfrom a\nleft join b i using (order_id)";
        let t = SourceText::new(sql);
        let col = SqlSpan {
            start: LineCol { line: 2, col: 26 },
            end: LineCol { line: 2, col: 38 },
        };
        assert_eq!(t.slice(col), "shipping_fee");
        let call = t.enclosing_call(col, "sum").unwrap();
        assert_eq!(t.slice(call), "sum(shipping_fee)");
        let key = SqlSpan {
            start: LineCol { line: 4, col: 25 },
            end: LineCol { line: 4, col: 33 },
        };
        let j = t.join_clause_before(key).unwrap();
        assert_eq!(t.slice(j), "left join b i using (order_id)");
    }

    fn span_of(t: &SourceText<'_>, needle: &str) -> SqlSpan {
        let at = t.text.find(needle).unwrap();
        t.span(at, at + needle.len())
    }

    #[test]
    fn quotes_and_comments_do_not_count() {
        let sql = "select sum(case when s = ')' then fee end) as f -- left join x\nfrom a\n/* join */ join b using (id)";
        let t = SourceText::new(sql);
        let call = t.enclosing_call(span_of(&t, "fee"), "sum").unwrap();
        assert_eq!(t.slice(call), "sum(case when s = ')' then fee end)");
        let j = t.join_clause_before(span_of(&t, "id)")).unwrap();
        assert_eq!(t.slice(j), "join b using (id)");
    }

    #[test]
    fn extends_over_the_call_before() {
        let sql = "select lag(status, 1) over (partition by id) as prev, (x) over (y) from t";
        let t = SourceText::new(sql);
        let over = t.enclosing_call(span_of(&t, "id)"), "over").unwrap();
        assert_eq!(
            t.slice(t.with_call_before(over)),
            "lag(status, 1) over (partition by id)"
        );
        let over = t.enclosing_call(span_of(&t, "y)"), "over").unwrap();
        assert_eq!(t.slice(t.with_call_before(over)), "(x) over (y)");
        let bare = span_of(&t, "status");
        assert_eq!(t.with_call_before(bare), bare);
    }
}
