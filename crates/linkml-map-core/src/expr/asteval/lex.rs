//! A tokenizer following CPython 3.13's (`Parser/tokenizer.c`) closely enough
//! that the same texts are accepted and rejected: indentation (including the
//! tab/space consistency check), implicit line joining inside brackets,
//! backslash continuations, numeric literal forms and string prefixes.
//!
//! `asteval` runs `ast.parse` on the text and swallows every failure, so the
//! error values here carry only a message.

use unicode_normalization::UnicodeNormalization;

pub(crate) type PResult<T> = Result<T, String>;

/// CPython's `MAXINDENT`: the 100th indentation level is rejected.
const MAX_INDENT: usize = 100;
/// CPython's `MAXLEVEL`: bracket nesting.
const MAX_LEVEL: usize = 200;

/// What a numeric token holds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Num {
    Int(i64),
    /// Beyond `i64`.
    Big(crate::pynum::BigInt),
    Float(f64),
    Imag(f64),
}

/// A string literal token, undecoded.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StrTok {
    pub raw: bool,
    pub bytes: bool,
    pub fstring: bool,
    /// The text between the quotes.
    pub body: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    Name(String),
    Number(Num),
    Str(StrTok),
    Op(&'static str),
    Newline,
    Indent,
    Dedent,
    End,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token {
    pub tok: Tok,
    pub line: u32,
    /// UTF-8 byte offset in the line.
    pub col: u32,
    pub end_line: u32,
    /// Byte offsets in the source.
    pub start: usize,
    pub end: usize,
}

const OPS3: [&str; 5] = ["**=", "//=", ">>=", "<<=", "..."];
const OPS2: [&str; 19] = [
    "==", "!=", "<=", ">=", "->", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "@=", ":=", "**",
    "//", "<<", ">>",
];
const OPS1: &str = "+-*/%@&|^~<>()[]{},:.;=";

/// `str.isidentifier`-style start character.
pub(crate) fn is_id_start(c: char) -> bool {
    c == '_' || c.is_alphabetic() || (c as u32 > 0x7f && is_other_id_start(c))
}

fn is_other_id_start(c: char) -> bool {
    matches!(
        c as u32,
        0x2118 | 0x212E | 0x309B | 0x309C | 0x1885 | 0x1886
    )
}

/// `XID_Continue` approximated: letters, digits, `_`, and the common combining
/// mark and connector-punctuation blocks.
pub(crate) fn is_id_continue(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    c.is_alphanumeric()
        || matches!(c as u32,
            0x00B7 | 0x0300..=0x036F | 0x0483..=0x0487 | 0x0591..=0x05BD | 0x05BF | 0x05C1..=0x05C2
            | 0x05C4..=0x05C5 | 0x05C7 | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC
            | 0x06DF..=0x06E4 | 0x06E7..=0x06E8 | 0x06EA..=0x06ED | 0x0900..=0x0903 | 0x093A..=0x094F
            | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF
            | 0x203F..=0x2040 | 0x2054 | 0x20D0..=0x20DC | 0x20E1 | 0x20E5..=0x20F0 | 0x302A..=0x302F
            | 0x3099..=0x309A | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F | 0xFE33..=0xFE34 | 0xFE4D..=0xFE4F
            | 0xFF3F | 0xE0100..=0xE01EF)
}

struct Lexer {
    chars: Vec<char>,
    /// Byte offset of each char (plus the end).
    offs: Vec<usize>,
    i: usize,
    line: u32,
    line_start: usize,
    out: Vec<Token>,
    indents: Vec<(usize, usize)>,
    parens: Vec<char>,
}

/// Python's universal-newline translation (`\r\n` and `\r` become `\n`).
pub(crate) fn translate_newlines(src: &str) -> String {
    if !src.contains('\r') {
        return src.to_string();
    }
    src.replace("\r\n", "\n").replace('\r', "\n")
}

/// Tokenize a (newline-translated) source text.
pub(crate) fn tokenize(src: &str) -> PResult<Vec<Token>> {
    if src.contains('\0') {
        return Err("source code string cannot contain null bytes".into());
    }
    let mut chars = Vec::with_capacity(src.len());
    let mut offs = Vec::with_capacity(src.len() + 1);
    for (o, c) in src.char_indices() {
        chars.push(c);
        offs.push(o);
    }
    offs.push(src.len());
    let mut lx = Lexer {
        chars,
        offs,
        i: 0,
        line: 1,
        line_start: 0,
        out: Vec::new(),
        indents: vec![(0, 0)],
        parens: Vec::new(),
    };
    lx.run()?;
    Ok(lx.out)
}

impl Lexer {
    fn peek(&self, k: usize) -> Option<char> {
        self.chars.get(self.i + k).copied()
    }

    fn off(&self) -> usize {
        self.offs[self.i.min(self.chars.len())]
    }

    fn push(&mut self, tok: Tok, start_i: usize, start_line: u32, start_line_off: usize) {
        let start = self.offs[start_i];
        let end = self.off();
        self.out.push(Token {
            tok,
            line: start_line,
            col: (start - start_line_off) as u32,
            end_line: self.line,
            start,
            end,
        });
    }

    fn bump_newline(&mut self) {
        self.i += 1;
        self.line += 1;
        self.line_start = self.off();
    }

    fn run(&mut self) -> PResult<()> {
        let mut at_line_start = true;
        let mut line_has_tokens = false;
        loop {
            if at_line_start && self.parens.is_empty() {
                at_line_start = false;
                // Indentation of a new logical line.
                let (mut col, mut altcol) = (0usize, 0usize);
                loop {
                    match self.peek(0) {
                        Some(' ') => {
                            col += 1;
                            altcol += 1;
                        }
                        Some('\t') => {
                            col = (col / 8 + 1) * 8;
                            altcol += 1;
                        }
                        Some('\x0c') => {
                            col = 0;
                            altcol = 0;
                        }
                        _ => break,
                    }
                    self.i += 1;
                }
                match self.peek(0) {
                    None => break,
                    Some('#') => {
                        while !matches!(self.peek(0), None | Some('\n')) {
                            self.i += 1;
                        }
                        if self.peek(0).is_some() {
                            self.bump_newline();
                        }
                        at_line_start = true;
                        continue;
                    }
                    Some('\n') => {
                        self.bump_newline();
                        at_line_start = true;
                        continue;
                    }
                    Some('\\') if self.peek(1) == Some('\n') => {
                        // A line starting with a continuation backslash: the
                        // indentation is that of the first such backslash.
                        self.i += 1;
                        self.bump_newline();
                        at_line_start = true;
                        continue;
                    }
                    _ => {}
                }
                let (top, alttop) = *self.indents.last().expect("indent stack");
                let (start_i, line, lo) = (self.i, self.line, self.line_start);
                if col == top {
                    if altcol != alttop {
                        return Err("inconsistent use of tabs and spaces in indentation".into());
                    }
                } else if col > top {
                    if altcol <= alttop {
                        return Err("inconsistent use of tabs and spaces in indentation".into());
                    }
                    if self.indents.len() >= MAX_INDENT {
                        return Err("too many levels of indentation".into());
                    }
                    self.indents.push((col, altcol));
                    self.push(Tok::Indent, start_i, line, lo);
                } else {
                    while self.indents.len() > 1 && col < self.indents.last().expect("stack").0 {
                        self.indents.pop();
                        self.push(Tok::Dedent, start_i, line, lo);
                    }
                    let (t, at) = *self.indents.last().expect("indent stack");
                    if col != t {
                        return Err("unindent does not match any outer indentation level".into());
                    }
                    if altcol != at {
                        return Err("inconsistent use of tabs and spaces in indentation".into());
                    }
                }
            }
            // Skip inline whitespace.
            while matches!(self.peek(0), Some(' ' | '\t' | '\x0c')) {
                self.i += 1;
            }
            let Some(c) = self.peek(0) else { break };
            let (start_i, line, lo) = (self.i, self.line, self.line_start);
            match c {
                '#' => {
                    while !matches!(self.peek(0), None | Some('\n')) {
                        self.i += 1;
                    }
                }
                '\n' => {
                    if self.parens.is_empty() {
                        if line_has_tokens {
                            self.i += 1;
                            self.push(Tok::Newline, start_i, line, lo);
                            self.line += 1;
                            self.line_start = self.off();
                            line_has_tokens = false;
                        } else {
                            self.bump_newline();
                        }
                        at_line_start = true;
                    } else {
                        self.bump_newline();
                    }
                }
                '\\' => match self.peek(1) {
                    Some('\n') => {
                        self.i += 1;
                        self.bump_newline();
                        if self.peek(0).is_none() {
                            return Err("unexpected EOF while parsing".into());
                        }
                    }
                    None => return Err("unexpected EOF while parsing".into()),
                    Some(_) => {
                        return Err("unexpected character after line continuation character".into());
                    }
                },
                c if c.is_ascii_digit()
                    || (c == '.' && self.peek(1).is_some_and(|d| d.is_ascii_digit())) =>
                {
                    let n = self.number()?;
                    self.push(Tok::Number(n), start_i, line, lo);
                    line_has_tokens = true;
                }
                '"' | '\'' => {
                    let t = self.string(false, false, false)?;
                    self.push(Tok::Str(t), start_i, line, lo);
                    line_has_tokens = true;
                }
                c if is_id_start(c) => {
                    let mut end = self.i + 1;
                    while end < self.chars.len() && is_id_continue(self.chars[end]) {
                        end += 1;
                    }
                    // A string prefix?
                    let word: String = self.chars[self.i..end].iter().collect();
                    let quote_next = matches!(self.chars.get(end), Some('"' | '\''));
                    if quote_next && word.len() <= 2 {
                        let lw = word.to_ascii_lowercase();
                        let valid = matches!(
                            lw.as_str(),
                            "r" | "u" | "b" | "f" | "br" | "rb" | "fr" | "rf"
                        );
                        if valid {
                            self.i = end;
                            let t =
                                self.string(lw.contains('r'), lw.contains('b'), lw.contains('f'))?;
                            self.push(Tok::Str(t), start_i, line, lo);
                            line_has_tokens = true;
                            continue;
                        }
                    }
                    self.i = end;
                    let name = if word.is_ascii() {
                        word
                    } else {
                        word.nfkc().collect()
                    };
                    self.push(Tok::Name(name), start_i, line, lo);
                    line_has_tokens = true;
                }
                _ => {
                    let op = self.operator()?;
                    match op {
                        "(" | "[" | "{" => {
                            if self.parens.len() >= MAX_LEVEL {
                                return Err("too many nested parentheses".into());
                            }
                            self.parens.push(op.chars().next().expect("op"));
                        }
                        ")" | "]" | "}" => {
                            let Some(open) = self.parens.pop() else {
                                return Err(format!("unmatched '{op}'"));
                            };
                            let want = match open {
                                '(' => ')',
                                '[' => ']',
                                _ => '}',
                            };
                            if !op.starts_with(want) {
                                return Err(format!(
                                    "closing parenthesis '{op}' does not match opening parenthesis '{open}'"
                                ));
                            }
                        }
                        _ => {}
                    }
                    self.push(Tok::Op(op), start_i, line, lo);
                    line_has_tokens = true;
                }
            }
        }
        // End of input.
        if let Some(open) = self.parens.last() {
            return Err(format!("'{open}' was never closed"));
        }
        let (i, line, lo) = (self.chars.len(), self.line, self.line_start);
        if line_has_tokens {
            self.push(Tok::Newline, i, line, lo);
        }
        while self.indents.len() > 1 {
            self.indents.pop();
            self.push(Tok::Dedent, i, line, lo);
        }
        self.push(Tok::End, i, line, lo);
        Ok(())
    }

    fn operator(&mut self) -> PResult<&'static str> {
        let rest: String = self.chars[self.i..(self.i + 3).min(self.chars.len())]
            .iter()
            .collect();
        for op in OPS3 {
            if rest.starts_with(op) {
                self.i += 3;
                return Ok(op);
            }
        }
        for op in OPS2 {
            if rest.starts_with(op) {
                self.i += 2;
                return Ok(op);
            }
        }
        let c = self.chars[self.i];
        if let Some(pos) = OPS1.find(c) {
            self.i += 1;
            return Ok(&OPS1[pos..pos + 1]);
        }
        if c.is_control() || c.is_whitespace() {
            Err(format!(
                "invalid non-printable character U+{:04X}",
                c as u32
            ))
        } else {
            Err(format!("invalid character '{c}'"))
        }
    }

    // --- numbers ----------------------------------------------------------

    fn digits(&mut self, radix: u32, what: &str) -> PResult<String> {
        // digit ( '_'? digit )*, the first digit already required by the caller.
        let mut s = String::new();
        loop {
            match self.peek(0) {
                Some(c) if c.is_digit(radix) => {
                    s.push(c);
                    self.i += 1;
                }
                Some('_') => match self.peek(1) {
                    Some(d) if d.is_digit(radix) => self.i += 1,
                    _ => return Err(format!("invalid {what} literal")),
                },
                _ => break,
            }
        }
        Ok(s)
    }

    fn verify_end_of_number(&self, kind: &str) -> PResult<()> {
        let Some(c) = self.peek(0) else { return Ok(()) };
        let rest: String = self.chars[self.i..(self.i + 4).min(self.chars.len())]
            .iter()
            .collect();
        let ok = match c {
            'a' => rest.starts_with("and"),
            'e' => rest.starts_with("else"),
            'f' => rest.starts_with("for"),
            'i' => rest.starts_with("in") || rest.starts_with("if") || rest.starts_with("is"),
            'n' => rest.starts_with("not"),
            'o' => rest.starts_with("or"),
            _ => false,
        };
        if ok {
            return Ok(());
        }
        if is_id_start(c) || c.is_ascii_digit() {
            return Err(format!("invalid {kind} literal"));
        }
        Ok(())
    }

    fn number(&mut self) -> PResult<Num> {
        let c = self.peek(0).expect("digit");
        if c == '0' {
            let radix = match self.peek(1) {
                Some('x' | 'X') => Some((16, "hexadecimal")),
                Some('o' | 'O') => Some((8, "octal")),
                Some('b' | 'B') => Some((2, "binary")),
                _ => None,
            };
            if let Some((radix, what)) = radix {
                self.i += 2;
                if self.peek(0) == Some('_') {
                    self.i += 1;
                }
                let first = self.peek(0);
                if !first.is_some_and(|d| d.is_digit(radix)) {
                    if let Some(d) = first.filter(|d| d.is_ascii_digit()) {
                        return Err(format!("invalid digit '{d}' in {what} literal"));
                    }
                    return Err(format!("invalid {what} literal"));
                }
                let ds = self.digits(radix, what)?;
                if let Some(d) = self.peek(0).filter(|d| d.is_ascii_digit()) {
                    return Err(format!("invalid digit '{d}' in {what} literal"));
                }
                self.verify_end_of_number(what)?;
                return Ok(match i64::from_str_radix(&ds, radix) {
                    Ok(v) => Num::Int(v),
                    Err(_) => Num::Big(
                        crate::pynum::parse_digits(&ds, radix).map_err(|e| e.to_string())?,
                    ),
                });
            }
        }
        // Decimal: integer part (maybe empty before '.'), fraction, exponent.
        let mut text = String::new();
        let mut nonzero_lead = false;
        if c != '.' {
            if c == '0' {
                // Zeros (and underscores between them), then maybe more digits.
                let mut zeros = String::new();
                loop {
                    match self.peek(0) {
                        Some('0') => {
                            zeros.push('0');
                            self.i += 1;
                        }
                        Some('_') if self.peek(1).is_some_and(|d| d.is_ascii_digit()) => {
                            self.i += 1
                        }
                        _ => break,
                    }
                }
                text.push_str(&zeros);
                if self.peek(0).is_some_and(|d| d.is_ascii_digit()) {
                    nonzero_lead = true;
                    text.push_str(&self.digits(10, "decimal")?);
                }
            } else {
                text.push_str(&self.digits(10, "decimal")?);
            }
        }
        let mut is_float = false;
        if self.peek(0) == Some('.') {
            is_float = true;
            text.push('.');
            self.i += 1;
            if self.peek(0).is_some_and(|d| d.is_ascii_digit()) {
                text.push_str(&self.digits(10, "decimal")?);
            }
        }
        if matches!(self.peek(0), Some('e' | 'E')) {
            let sign = matches!(self.peek(1), Some('+' | '-'));
            let digit_at = if sign { 2 } else { 1 };
            if self.peek(digit_at).is_some_and(|d| d.is_ascii_digit()) {
                is_float = true;
                text.push('e');
                self.i += 1;
                if sign {
                    text.push(self.peek(0).expect("sign"));
                    self.i += 1;
                }
                text.push_str(&self.digits(10, "decimal")?);
            }
        }
        if matches!(self.peek(0), Some('j' | 'J')) {
            self.i += 1;
            self.verify_end_of_number("imaginary")?;
            let v: f64 = text.parse().unwrap_or(0.0);
            return Ok(Num::Imag(v));
        }
        if nonzero_lead && !is_float {
            return Err(
                "leading zeros in decimal integer literals are not permitted; use an 0o prefix for octal integers"
                    .into(),
            );
        }
        self.verify_end_of_number("decimal")?;
        if is_float {
            let t = if text.starts_with('.') {
                format!("0{text}")
            } else {
                text
            };
            let t = t.replace(".e", ".0e");
            let t = if t.ends_with('.') { format!("{t}0") } else { t };
            return Ok(Num::Float(t.parse::<f64>().map_err(|e| e.to_string())?));
        }
        Ok(match text.parse::<i64>() {
            Ok(v) => Num::Int(v),
            Err(_) => Num::Big(crate::pynum::parse_digits(&text, 10).map_err(|e| {
                format!("{e} - Consider hexadecimal for huge integer literals to avoid decimal conversion limits.")
            })?),
        })
    }

    // --- strings ----------------------------------------------------------

    /// Scan a string literal starting at the opening quote; `self.i` ends after
    /// the closing quote.
    fn string(&mut self, raw: bool, bytes: bool, fstring: bool) -> PResult<StrTok> {
        let q = self.peek(0).expect("quote");
        let triple = self.peek(1) == Some(q) && self.peek(2) == Some(q);
        self.i += if triple { 3 } else { 1 };
        let body_start = self.i;
        let end = scan_string_body(&self.chars, self.i, q, triple, fstring, raw)?;
        let body: String = self.chars[body_start..end].iter().collect();
        for c in body.chars().filter(|&c| c == '\n') {
            let _ = c;
            self.line += 1;
        }
        // Reposition after the closing quote(s) and fix the line start.
        self.i = end + if triple { 3 } else { 1 };
        if let Some(last_nl) = self.chars[body_start..end].iter().rposition(|&c| c == '\n') {
            self.line_start = self.offs[body_start + last_nl + 1];
        }
        Ok(StrTok {
            raw,
            bytes,
            fstring,
            body,
        })
    }
}

/// Find the index of the closing quote of a string body starting at `i`.
fn scan_string_body(
    chars: &[char],
    mut i: usize,
    q: char,
    triple: bool,
    fstring: bool,
    raw: bool,
) -> PResult<usize> {
    let n = chars.len();
    while i < n {
        let c = chars[i];
        if c == '\\' {
            if i + 1 >= n {
                break;
            }
            // In a raw f-string `\{` does not escape the brace, but the quote still is.
            if fstring && !raw && chars.get(i + 1) == Some(&'N') && chars.get(i + 2) == Some(&'{') {
                // `\N{NAME}`: the braces belong to the escape.
                i += 3;
                while i < n && chars[i] != '}' {
                    i += 1;
                }
                i += 1;
                continue;
            }
            i += 2;
            continue;
        }
        if c == q {
            if !triple {
                return Ok(i);
            }
            if chars.get(i + 1) == Some(&q) && chars.get(i + 2) == Some(&q) {
                return Ok(i);
            }
            i += 1;
            continue;
        }
        if c == '\n' && !triple {
            return Err("unterminated string literal".into());
        }
        if fstring && c == '{' {
            if chars.get(i + 1) == Some(&'{') {
                i += 2;
                continue;
            }
            i = scan_field(chars, i + 1, triple, raw)?;
            continue;
        }
        i += 1;
    }
    Err(if triple {
        "unterminated triple-quoted string literal".into()
    } else {
        "unterminated string literal".into()
    })
}

/// Scan an f-string replacement field starting after its `{`; returns the
/// index just past the closing `}`.
fn scan_field(chars: &[char], mut i: usize, triple: bool, raw: bool) -> PResult<usize> {
    let n = chars.len();
    let mut stack: Vec<char> = Vec::new();
    while i < n {
        let c = chars[i];
        match c {
            '\'' | '"' => {
                // A nested string literal (any quote, possibly the same as the
                // enclosing one, since 3.12).
                let nq = c;
                let ntriple = chars.get(i + 1) == Some(&nq) && chars.get(i + 2) == Some(&nq);
                // Is it an f-string? Look back at the prefix letters.
                let mut j = i;
                let mut pfx = String::new();
                while j > 0 && chars[j - 1].is_ascii_alphabetic() && pfx.len() < 2 {
                    j -= 1;
                    pfx.push(chars[j]);
                }
                let pl = pfx.to_ascii_lowercase();
                let nf = pl.contains('f');
                let nraw = pl.contains('r');
                let start = i + if ntriple { 3 } else { 1 };
                let e = scan_string_body(chars, start, nq, ntriple, nf, nraw)?;
                i = e + if ntriple { 3 } else { 1 };
            }
            '(' | '[' | '{' => {
                stack.push(c);
                i += 1;
            }
            ')' | ']' => {
                stack.pop();
                i += 1;
            }
            '}' => {
                if stack.is_empty() {
                    return Ok(i + 1);
                }
                stack.pop();
                i += 1;
            }
            '#' => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
            }
            '\\' => {
                if stack.is_empty() && !raw {
                    // A backslash in the expression part.
                    return Err("f-string expression part cannot include a backslash".into());
                }
                i += 2;
            }
            '!' if stack.is_empty() && chars.get(i + 1) != Some(&'=') => {
                // Conversion `!r` then `:spec` or `}`.
                i += 1;
                while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
            }
            ':' if stack.is_empty() => {
                // Format spec: literal text with nested fields until the `}`.
                i += 1;
                while i < n {
                    match chars[i] {
                        '{' => {
                            i = scan_field(chars, i + 1, triple, raw)?;
                        }
                        '}' => return Ok(i + 1),
                        '\n' if !triple => return Err("unterminated string literal".into()),
                        _ => i += 1,
                    }
                }
            }
            _ => i += 1,
        }
    }
    Err("f-string: expecting '}'".into())
}
