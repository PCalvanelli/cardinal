//! Maps positions in a model's compiled SQL back to its Jinja source file by matching
//! line text. A raw line's `{{ ... }}` expressions match any text in the compiled line;
//! when no unique match exists the caller reports the compiled file instead.

use derive_sql::{LineCol, SqlSpan};

#[derive(Debug, Clone)]
enum Seg {
    Lit(String),
    Hole,
}

struct RawLine {
    segs: Vec<Seg>,
    /// Byte offset in the raw line where each segment starts.
    starts: Vec<usize>,
    /// Leading whitespace removed before matching.
    indent: usize,
    literal_len: usize,
}

pub struct SourceMap {
    /// For each compiled line (index = line - 1): the raw line and per-segment info.
    lines: Vec<Option<LineMatch>>,
}

#[derive(Clone, Debug)]
struct LineMatch {
    raw_line: u32,
    /// (compiled offset, raw offset, is_hole) at the start of each segment, trimmed coords.
    anchors: Vec<(usize, usize, bool)>,
    compiled_indent: usize,
    raw_indent: usize,
}

fn parse_raw_line(line: &str) -> Option<RawLine> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    let t = trimmed.trim_end();
    if t.is_empty() {
        return None;
    }
    let mut segs = Vec::new();
    let mut starts = Vec::new();
    let mut rest = t;
    let mut pos = 0;
    let mut literal_len = 0;
    while !rest.is_empty() {
        let next = ["{{", "{%", "{#"]
            .iter()
            .filter_map(|o| rest.find(o).map(|i| (i, *o)))
            .min();
        match next {
            None => {
                starts.push(pos);
                literal_len += rest.trim().len();
                segs.push(Seg::Lit(rest.to_string()));
                break;
            }
            Some((i, open)) => {
                if i > 0 {
                    starts.push(pos);
                    literal_len += rest[..i].trim().len();
                    segs.push(Seg::Lit(rest[..i].to_string()));
                }
                let close = match open {
                    "{{" => "}}",
                    "{%" => "%}",
                    _ => "#}",
                };
                let Some(j) = rest[i..].find(close) else {
                    // A tag that spans lines: not matchable.
                    return None;
                };
                let end = i + j + 2;
                if open == "{{" {
                    starts.push(pos + i);
                    segs.push(Seg::Hole);
                } else if open == "{%" {
                    // Block tags render to nothing or to whole other lines.
                    return None;
                }
                pos += end;
                rest = &rest[end..];
            }
        }
    }
    if segs.is_empty() {
        return None;
    }
    Some(RawLine {
        segs,
        starts,
        indent,
        literal_len,
    })
}

/// Match segments against text; returns the text offset where each segment starts.
fn match_segs(segs: &[Seg], text: &str) -> Option<Vec<usize>> {
    fn go(segs: &[Seg], text: &str, at: usize, out: &mut Vec<usize>) -> bool {
        let Some((first, rest)) = segs.split_first() else {
            return at == text.len();
        };
        match first {
            Seg::Lit(l) => {
                if text[at..].starts_with(l.as_str()) {
                    out.push(at);
                    if go(rest, text, at + l.len(), out) {
                        return true;
                    }
                    out.pop();
                }
                false
            }
            Seg::Hole => {
                out.push(at);
                if rest.is_empty() {
                    return true;
                }
                let mut k = at + 1;
                while k <= text.len() {
                    if text.is_char_boundary(k) && go(rest, text, k, out) {
                        return true;
                    }
                    k += 1;
                }
                out.pop();
                false
            }
        }
    }
    let mut out = Vec::new();
    go(segs, text, 0, &mut out).then_some(out)
}

impl SourceMap {
    pub fn new(raw: &str, compiled: &str) -> Self {
        let raw_lines: Vec<Option<RawLine>> = raw.lines().map(parse_raw_line).collect();
        let mut lines = Vec::new();
        let mut last_raw: i64 = -1;
        for c in compiled.lines() {
            let ct = c.trim_start();
            let cindent = c.len() - ct.len();
            let ct = ct.trim_end();
            if ct.is_empty() {
                lines.push(None);
                continue;
            }
            let mut cands: Vec<(usize, Vec<usize>)> = Vec::new();
            for (ri, rl) in raw_lines.iter().enumerate() {
                let Some(rl) = rl else { continue };
                // Lines that are all Jinja match anything; require some literal text.
                if rl.literal_len < 2 && rl.segs.iter().any(|s| matches!(s, Seg::Hole)) {
                    continue;
                }
                if let Some(offs) = match_segs(&rl.segs, ct) {
                    cands.push((ri, offs));
                }
            }
            let pick = cands
                .iter()
                .filter(|(ri, _)| *ri as i64 > last_raw)
                .min_by_key(|(ri, _)| *ri)
                .or_else(|| (cands.len() == 1).then(|| &cands[0]))
                .cloned();
            match pick {
                Some((ri, offs)) => {
                    last_raw = ri as i64;
                    let rl = raw_lines[ri].as_ref().unwrap();
                    let anchors = offs
                        .iter()
                        .zip(rl.starts.iter())
                        .zip(rl.segs.iter())
                        .map(|((c, r), s)| (*c, *r, matches!(s, Seg::Hole)))
                        .collect();
                    lines.push(Some(LineMatch {
                        raw_line: ri as u32 + 1,
                        anchors,
                        compiled_indent: cindent,
                        raw_indent: rl.indent,
                    }));
                }
                None => lines.push(None),
            }
        }
        SourceMap { lines }
    }

    fn map_point(&self, lc: LineCol, compiled_line: &str) -> Option<LineCol> {
        let m = self
            .lines
            .get((lc.line as usize).checked_sub(1)?)?
            .as_ref()?;
        let byte: usize = compiled_line
            .chars()
            .take((lc.col as usize).saturating_sub(1))
            .map(char::len_utf8)
            .sum();
        let off = byte.saturating_sub(m.compiled_indent);
        let (c0, r0, hole) = m
            .anchors
            .iter()
            .rev()
            .find(|(c, _, _)| *c <= off)
            .copied()
            .unwrap_or((0, 0, false));
        let raw_off = if hole { r0 } else { r0 + (off - c0) };
        Some(LineCol {
            line: m.raw_line,
            col: (m.raw_indent + raw_off) as u32 + 1,
        })
    }

    /// Map a span in compiled SQL to the raw source, if its start line maps.
    pub fn map_span(&self, span: SqlSpan, compiled: &str, raw: &str) -> Option<SqlSpan> {
        let cl: Vec<&str> = compiled.lines().collect();
        let start_line = cl.get(span.start.line as usize - 1)?;
        let start = self.map_point(span.start, start_line)?;
        let end = if span.end.line == span.start.line {
            self.map_point(span.end, start_line)
        } else {
            None
        };
        let raw_line_len = raw
            .lines()
            .nth(start.line as usize - 1)
            .map(|l| l.chars().count())
            .unwrap_or(0) as u32;
        let end = match end {
            Some(e) if e.line == start.line && e.col > start.col => e,
            _ => LineCol {
                line: start.line,
                col: raw_line_len + 1,
            },
        };
        Some(SqlSpan { start, end })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_through_jinja() {
        let raw = "select\n    o.order_id,\n    sum(shipping_fee) as fees\nfrom {{ ref('orders') }} o\njoin {{ source('shop', 'order_items') }} i using (order_id)\n{% if is_incremental() %}\nwhere 1=1\n{% endif %}\ngroup by 1";
        let compiled = "select\n    o.order_id,\n    sum(shipping_fee) as fees\nfrom \"db\".\"main\".\"orders\" o\njoin \"db\".\"shop\".\"order_items\" i using (order_id)\n\ngroup by 1";
        let m = SourceMap::new(raw, compiled);
        // sum(shipping_fee) on compiled line 3, col 5..22
        let s = SqlSpan {
            start: LineCol { line: 3, col: 5 },
            end: LineCol { line: 3, col: 22 },
        };
        let r = m.map_span(s, compiled, raw).unwrap();
        assert_eq!((r.start.line, r.start.col, r.end.col), (3, 5, 22));
        // the join line maps to raw line 5; "using" keeps its column relative to the literal tail
        let j = SqlSpan {
            start: LineCol { line: 5, col: 1 },
            end: LineCol { line: 5, col: 50 },
        };
        let r = m.map_span(j, compiled, raw).unwrap();
        assert_eq!(r.start, LineCol { line: 5, col: 1 });
        assert_eq!(
            m.map_span(
                SqlSpan {
                    start: LineCol { line: 7, col: 1 },
                    end: LineCol { line: 7, col: 3 }
                },
                compiled,
                raw
            )
            .unwrap()
            .start
            .line,
            9
        );
    }
}
