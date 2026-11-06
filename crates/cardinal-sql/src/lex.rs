//! One lexer for the code that reads SQL text directly rather than the plan: the spans on
//! labels, and the heuristics behind `cardinal guess`. It never fails, and reads every
//! dialect the same way, so it knows only what they share:
//!
//! - words: a letter or `_`, then letters, digits, `_` and `$`;
//! - `"…"` and `` `…` `` quoted names and `'…'` strings, where a doubled quote stays inside;
//! - `--` comments to the end of the line, and `/* … */` comments;
//! - numbers, whitespace, and any other character as punctuation of its own.
//!
//! Backslash escapes, `#` comments and `$$` strings belong to some dialects only and are
//! not recognised. An unterminated string or comment runs to the end of the text.
//!
//! Suppression comments are not read with it: they are found in the raw Jinja template,
//! which is not SQL (see `cardinal_diag::suppress`).

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Word,
    /// A `"quoted"` or `` `quoted` `` name.
    Quoted,
    String,
    Number,
    Comment,
    Space,
    Punct,
}

/// A token: its kind and byte range in the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: Kind,
    pub start: usize,
    pub end: usize,
}

/// The tokens of one SQL text, in order, covering all of it.
pub struct Tokens<'a> {
    pub sql: &'a str,
    list: Vec<Token>,
}

impl<'a> Tokens<'a> {
    pub fn new(sql: &'a str) -> Self {
        Tokens {
            sql,
            list: lex(sql),
        }
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn get(&self, i: usize) -> Token {
        self.list[i]
    }

    pub fn text(&self, i: usize) -> &'a str {
        let t = self.list[i];
        &self.sql[t.start..t.end]
    }

    /// Where the name a word or quoted name stands for is: the word, or the text between
    /// the quotes (a doubled quote in it is left doubled).
    pub fn name_range(&self, i: usize) -> Option<Range<usize>> {
        let t = self.list[i];
        match t.kind {
            Kind::Word => Some(t.start..t.end),
            Kind::Quoted => {
                let q = self.sql.as_bytes()[t.start];
                let closed = t.end - t.start >= 2 && self.sql.as_bytes()[t.end - 1] == q;
                Some(t.start + 1..if closed { t.end - 1 } else { t.end })
            }
            _ => None,
        }
    }

    /// The name a word or quoted name stands for.
    pub fn name(&self, i: usize) -> Option<&'a str> {
        self.name_range(i).map(|r| &self.sql[r])
    }

    /// Whether token `i` is the word `word`, ignoring ASCII case.
    pub fn is_word(&self, i: usize, word: &str) -> bool {
        self.list[i].kind == Kind::Word && self.text(i).eq_ignore_ascii_case(word)
    }

    pub fn is_punct(&self, i: usize, c: char) -> bool {
        self.list[i].kind == Kind::Punct && self.text(i).starts_with(c)
    }

    /// Every occurrence of the word `word`, ignoring ASCII case. Words in comments,
    /// strings and quoted names do not count.
    pub fn words<'w>(&'w self, word: &'w str) -> impl Iterator<Item = usize> + 'w {
        (0..self.len()).filter(move |&i| self.is_word(i, word))
    }

    /// Every word or quoted name that stands for `name`, ignoring ASCII case.
    pub fn names<'w>(&'w self, name: &'w str) -> impl Iterator<Item = usize> + 'w {
        (0..self.len()).filter(move |&i| self.name(i).is_some_and(|n| n.eq_ignore_ascii_case(name)))
    }

    /// The next token after `i` that is neither whitespace nor a comment.
    pub fn next(&self, i: usize) -> Option<usize> {
        (i + 1..self.len()).find(|&k| !self.is_trivia(k))
    }

    /// The last token before `i` that is neither whitespace nor a comment.
    pub fn prev(&self, i: usize) -> Option<usize> {
        (0..i).rev().find(|&k| !self.is_trivia(k))
    }

    fn is_trivia(&self, i: usize) -> bool {
        matches!(self.list[i].kind, Kind::Space | Kind::Comment)
    }

    /// The `)` that closes the `(` at `open`.
    pub fn closing(&self, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for k in open..self.len() {
            if self.is_punct(k, '(') {
                depth += 1;
            } else if self.is_punct(k, ')') {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(k);
                }
            }
        }
        None
    }

    /// The `(` that the `)` at `close` closes.
    pub fn opening(&self, close: usize) -> Option<usize> {
        let mut depth = 0usize;
        for k in (0..=close).rev() {
            if self.is_punct(k, ')') {
                depth += 1;
            } else if self.is_punct(k, '(') {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(k);
                }
            }
        }
        None
    }

    /// The token holding byte `offset`, or `len()` at the end of the text.
    pub fn at(&self, offset: usize) -> usize {
        self.list.partition_point(|t| t.end <= offset)
    }
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn lex(sql: &str) -> Vec<Token> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(c) = sql[i..].chars().next() {
        let start = i;
        let kind = match c {
            '-' if b.get(i + 1) == Some(&b'-') => {
                i = sql[i..].find('\n').map_or(sql.len(), |n| i + n);
                Kind::Comment
            }
            '/' if b.get(i + 1) == Some(&b'*') => {
                i = sql[i + 2..].find("*/").map_or(sql.len(), |n| i + 2 + n + 2);
                Kind::Comment
            }
            '\'' => {
                i = quoted_end(b, i);
                Kind::String
            }
            '"' | '`' => {
                i = quoted_end(b, i);
                Kind::Quoted
            }
            c if c.is_whitespace() => {
                i = run(sql, i, char::is_whitespace);
                Kind::Space
            }
            c if c.is_alphabetic() || c == '_' => {
                i = run(sql, i, word_char);
                Kind::Word
            }
            c if c.is_ascii_digit() => {
                i = run(sql, i, |c| c.is_alphanumeric() || c == '_' || c == '.');
                Kind::Number
            }
            c => {
                i += c.len_utf8();
                Kind::Punct
            }
        };
        out.push(Token {
            kind,
            start,
            end: i,
        });
    }
    out
}

/// The end of the quoted text opening at `open`; a doubled quote does not close it.
fn quoted_end(b: &[u8], open: usize) -> usize {
    let q = b[open];
    let mut i = open + 1;
    loop {
        match b[i..].iter().position(|&c| c == q) {
            None => return b.len(),
            Some(n) if b.get(i + n + 1) == Some(&q) => i += n + 2,
            Some(n) => return i + n + 1,
        }
    }
}

fn run(sql: &str, from: usize, keep: impl Fn(char) -> bool) -> usize {
    sql[from..]
        .char_indices()
        .find(|&(_, c)| !keep(c))
        .map_or(sql.len(), |(n, _)| from + n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(sql: &str) -> Vec<(Kind, &str)> {
        let t = Tokens::new(sql);
        (0..t.len())
            .filter(|&i| t.get(i).kind != Kind::Space)
            .map(|i| (t.get(i).kind, t.text(i)))
            .collect()
    }

    #[test]
    fn tokens_cover_the_text() {
        let sql = "select a$1, \"b\"\"c\", 'it''s' -- note\n/* x */ from café where n = 1.5";
        let t = Tokens::new(sql);
        let joined: std::string::String = (0..t.len()).map(|i| t.text(i)).collect();
        assert_eq!(joined, sql);
        use Kind::*;
        assert_eq!(
            kinds(sql),
            vec![
                (Word, "select"),
                (Word, "a$1"),
                (Punct, ","),
                (Quoted, "\"b\"\"c\""),
                (Punct, ","),
                (String, "'it''s'"),
                (Comment, "-- note"),
                (Comment, "/* x */"),
                (Word, "from"),
                (Word, "café"),
                (Word, "where"),
                (Word, "n"),
                (Punct, "="),
                (Number, "1.5"),
            ]
        );
    }

    #[test]
    fn unterminated_runs_to_the_end() {
        use Kind::*;
        assert_eq!(kinds("a 'b c"), vec![(Word, "a"), (String, "'b c")]);
        assert_eq!(kinds("a /* b"), vec![(Word, "a"), (Comment, "/* b")]);
        assert_eq!(kinds("a \"b"), vec![(Word, "a"), (Quoted, "\"b")]);
        let t = Tokens::new("a \"b");
        assert_eq!(t.name(2), Some("b"));
    }

    #[test]
    fn words_skip_comments_strings_and_quotes() {
        let sql = "select 'union' as \"union\" -- union\n/* union */ union all select 1";
        let t = Tokens::new(sql);
        let at: Vec<usize> = t.words("UNION").map(|i| t.get(i).start).collect();
        assert_eq!(at, vec![sql.rfind("union all").unwrap()]);
        assert_eq!(t.names("union").count(), 2);
    }

    #[test]
    fn parens_skip_quoted_text() {
        let sql = "f(a, ')', g(\"(\"), b) + 1";
        let t = Tokens::new(sql);
        let open = 1;
        let close = t.closing(open).unwrap();
        assert_eq!(&sql[..t.get(close).end], "f(a, ')', g(\"(\"), b)");
        assert_eq!(t.opening(close), Some(open));
        assert_eq!(t.prev(open), Some(0));
        assert_eq!(t.at(sql.find("b)").unwrap()), t.prev(close).unwrap());
    }
}
