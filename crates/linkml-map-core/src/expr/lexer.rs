//! Tokenizer for the restricted expression language.
//!
//! Produces a flat token stream consumed by the Pratt parser in
//! [`crate::expr::parser`]. The grammar is the Python expression grammar
//! CPython 3.13's tokenizer accepts: the lexer follows `Parser/tokenizer.c`
//! closely (numeric literal forms, string prefixes, escapes, f-strings per PEP
//! 701) so that an expression that parses upstream lexes here, and one that does
//! not is refused, even when the offending part would never be evaluated.
//!
//! The `{` `}` braces are plain tokens: `{x}` (the variable-reference syntax),
//! dict and set displays are told apart by the parser.
//!
//! The source is scanned as bytes (every character that matters to the
//! tokenizer is ASCII) and slices of it are copied only when a token needs its
//! own text.

use std::borrow::Cow;

use super::error::{ExprError, ExprResult, UNREPRESENTABLE_PREFIX};
use super::pymethods::{is_identifier_continue, is_identifier_start};
use crate::pynum::{self, BigInt};

/// A lexical token.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Literals
    Int(i64),
    Float(f64),
    /// An integer literal beyond `i64`.
    BigInt(Box<BigInt>),
    /// An imaginary literal (`1j`): the value of the imaginary part.
    Imag(f64),
    Str(String),
    /// A `b'...'` literal.
    Bytes(Vec<u8>),
    /// An f-string literal: its literal text and replacement fields.
    FStr(Vec<FStrPart>),

    // Names / keywords
    Name(String),
    True,
    False,
    None,
    And,
    Or,
    Not,
    In,
    Is,
    Lambda,

    // Punctuation / grouping
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Dot,
    Ellipsis, // ...
    Colon,    // :  (dict literals, slices; `if`/`else` are keywords lexed as names)
    ColonEq,  // :=
    Assign,   // =  (keyword arguments `f(k=v)`)

    // Operators
    Plus,
    Minus,
    Star,
    Slash,       // /
    DoubleSlash, // //
    Percent,     // %
    DoubleStar,  // **  (power)
    Caret,       // ^   (bitwise xor, per doctests)
    Amp,         // &
    Pipe,        // |
    Tilde,       // ~
    LShift,      // <<
    RShift,      // >>
    At,          // @   (matrix multiplication: lexed, refused only when evaluated)

    // Comparisons
    Eq,    // ==
    NotEq, // !=
    Lt,    // <
    LtE,   // <=
    Gt,    // >
    GtE,   // >=

    /// The end of a logical line: a newline outside brackets that follows a
    /// token (blank and comment-only lines produce none). The parser accepts it
    /// only at the end of the input, as `ast.parse(.., mode="eval")` does.
    Newline,

    Eof,
}

/// One piece of an f-string: literal text (escapes already decoded) or a
/// replacement field.
#[derive(Debug, Clone, PartialEq)]
pub enum FStrPart {
    Lit(String),
    Field(Box<FStrField>),
}

/// A replacement field `{expr[=][!conv][:spec]}` of an f-string.
#[derive(Debug, Clone, PartialEq)]
pub struct FStrField {
    /// The expression's tokens, terminated by [`Token::Eof`].
    pub tokens: Vec<Token>,
    /// The `!s`, `!r` or `!a` conversion.
    pub conversion: Option<char>,
    /// The format spec, itself an f-string body (it may hold nested fields).
    pub spec: Option<Vec<FStrPart>>,
    /// For the self-documenting form `{expr=}`: the source text from the start
    /// of the field up to and including the `=` and the whitespace after it.
    pub debug: Option<String>,
}

/// How many brackets (`(`, `[`, `{`) may be open at once. CPython's tokenizer
/// (`MAXLEVEL` in `tokenizer.c`) raises `SyntaxError: too many nested
/// parentheses` for a 201st; stopping here also bounds the parser's recursion.
pub const MAX_BRACKET_LEVEL: usize = 200;

/// How deeply f-string fields may nest (CPython's `MAXFSTRINGLEVEL`).
const MAX_FSTRING_LEVEL: usize = 150;

/// Tokenize `src` into a token stream terminated by [`Token::Eof`].
pub fn lex(src: &str) -> ExprResult<Vec<Token>> {
    // CPython refuses a NUL anywhere in the source ("source code string cannot
    // contain null bytes").
    if src.as_bytes().contains(&0) {
        return Err(ExprError::Lex(
            "source code string cannot contain null bytes".into(),
        ));
    }
    let mut lexer = Lexer {
        src,
        pos: 0,
        level: 0,
        fstring_level: 0,
    };
    let mut tokens = Vec::with_capacity(src.len() / 3 + 2);
    lexer.tokens(&mut tokens, false)?;
    tokens.push(Token::Eof);
    Ok(tokens)
}

fn err(message: impl Into<String>) -> ExprError {
    ExprError::Lex(message.into())
}

/// What a string prefix (`r`, `b`, `f`, `rb`, `fr`, ...) asks for.
#[derive(Clone, Copy, Default)]
struct Prefix {
    raw: bool,
    bytes: bool,
    fstring: bool,
}

/// The string prefix at the start of `b`, with its length, when one is followed
/// by a quote. `u` stands alone; `r` combines with `b` or `f`.
fn string_prefix(b: &[u8]) -> Option<(Prefix, usize)> {
    let quote_at = |n: usize| matches!(b.get(n), Some(b'\'' | b'"'));
    let lower = |n: usize| b.get(n).map(u8::to_ascii_lowercase);
    match lower(0)? {
        b'r' => {
            if quote_at(1) {
                return Some((
                    Prefix {
                        raw: true,
                        ..Prefix::default()
                    },
                    1,
                ));
            }
            let kind = match lower(1)? {
                b'b' => Prefix {
                    raw: true,
                    bytes: true,
                    fstring: false,
                },
                b'f' => Prefix {
                    raw: true,
                    bytes: false,
                    fstring: true,
                },
                _ => return None,
            };
            quote_at(2).then_some((kind, 2))
        }
        b'b' => {
            if quote_at(1) {
                return Some((
                    Prefix {
                        bytes: true,
                        ..Prefix::default()
                    },
                    1,
                ));
            }
            (lower(1) == Some(b'r') && quote_at(2)).then_some((
                Prefix {
                    raw: true,
                    bytes: true,
                    fstring: false,
                },
                2,
            ))
        }
        b'f' => {
            if quote_at(1) {
                return Some((
                    Prefix {
                        fstring: true,
                        ..Prefix::default()
                    },
                    1,
                ));
            }
            (lower(1) == Some(b'r') && quote_at(2)).then_some((
                Prefix {
                    raw: true,
                    bytes: false,
                    fstring: true,
                },
                2,
            ))
        }
        b'u' => quote_at(1).then_some((Prefix::default(), 1)),
        _ => None,
    }
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    /// Open brackets, counting the braces of f-string replacement fields.
    level: usize,
    /// Replacement fields being lexed (nested f-strings).
    fstring_level: usize,
}

impl Lexer<'_> {
    fn byte(&self, offset: usize) -> Option<u8> {
        self.src.as_bytes().get(self.pos + offset).copied()
    }

    fn next_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    /// Skip blanks, comments and backslash-newline continuations; true when a
    /// newline outside any bracket was crossed (a logical line ended).
    fn skip_blanks(&mut self) -> ExprResult<bool> {
        let mut newline = false;
        loop {
            match self.byte(0) {
                Some(b'\n' | b'\r') => {
                    newline |= self.level == 0;
                    self.pos += 1;
                }
                Some(b' ' | b'\t' | b'\x0c') => self.pos += 1,
                Some(b'#') => {
                    while !matches!(self.byte(0), None | Some(b'\n' | b'\r')) {
                        self.pos += 1;
                    }
                }
                Some(b'\\') => match (self.byte(1), self.byte(2)) {
                    (Some(b'\n'), _) => self.pos += 2,
                    (Some(b'\r'), Some(b'\n')) => self.pos += 3,
                    (Some(b'\r'), _) => self.pos += 2,
                    _ => {
                        return Err(err(
                            "unexpected character after line continuation character",
                        ));
                    }
                },
                _ => return Ok(newline),
            }
        }
    }

    /// Lex tokens into `out`: to the end of the source, or (inside an f-string
    /// replacement field) up to, not including, the `}`, `:`, `!` or `=` that
    /// ends the expression at the field's own bracket level.
    fn tokens(&mut self, out: &mut Vec<Token>, in_field: bool) -> ExprResult<()> {
        let base = self.level;
        loop {
            if self.skip_blanks()? && !matches!(out.last(), None | Some(Token::Newline)) {
                out.push(Token::Newline);
            }
            let Some(c) = self.next_char() else {
                if in_field {
                    return Err(err("f-string: expecting '}'"));
                }
                return Ok(());
            };
            if in_field && self.level == base {
                let next = self.byte(1);
                match c {
                    '}' | ':' => return Ok(()),
                    '!' | '=' if next != Some(b'=') => return Ok(()),
                    _ => {}
                }
            }
            let token = self.token(c)?;
            out.push(token);
        }
    }

    /// The token starting with `c` at the current position.
    fn token(&mut self, c: char) -> ExprResult<Token> {
        if c.is_ascii_digit() || (c == '.' && self.byte(1).is_some_and(|b| b.is_ascii_digit())) {
            return self.number();
        }
        if c == '\'' || c == '"' {
            return self.string(Prefix::default(), 0);
        }
        if c.is_ascii_alphabetic()
            && let Some((prefix, len)) = string_prefix(&self.src.as_bytes()[self.pos..])
        {
            return self.string(prefix, len);
        }
        if is_identifier_start(c) {
            return Ok(self.word());
        }
        self.operator(c)
    }

    fn word(&mut self) -> Token {
        let start = self.pos;
        for ch in self.src[start..].chars() {
            if is_identifier_continue(ch) {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
        match &self.src[start..self.pos] {
            "True" => Token::True,
            "False" => Token::False,
            "None" => Token::None,
            "and" => Token::And,
            "or" => Token::Or,
            "not" => Token::Not,
            "in" => Token::In,
            "is" => Token::Is,
            "lambda" => Token::Lambda,
            word => Token::Name(word.to_owned()),
        }
    }

    fn operator(&mut self, c: char) -> ExprResult<Token> {
        let next = self.byte(1);
        let two = match (c, next) {
            ('*', Some(b'*')) => Some(Token::DoubleStar),
            ('/', Some(b'/')) => Some(Token::DoubleSlash),
            ('<', Some(b'<')) => Some(Token::LShift),
            ('>', Some(b'>')) => Some(Token::RShift),
            ('=', Some(b'=')) => Some(Token::Eq),
            ('!', Some(b'=')) => Some(Token::NotEq),
            ('<', Some(b'=')) => Some(Token::LtE),
            ('>', Some(b'=')) => Some(Token::GtE),
            (':', Some(b'=')) => Some(Token::ColonEq),
            _ => None,
        };
        if let Some(token) = two {
            self.pos += 2;
            return Ok(token);
        }
        if c == '.' && next == Some(b'.') && self.byte(2) == Some(b'.') {
            self.pos += 3;
            return Ok(Token::Ellipsis);
        }
        let token = match c {
            '+' => Token::Plus,
            '-' => Token::Minus,
            '*' => Token::Star,
            '/' => Token::Slash,
            '%' => Token::Percent,
            '^' => Token::Caret,
            '&' => Token::Amp,
            '|' => Token::Pipe,
            '~' => Token::Tilde,
            '<' => Token::Lt,
            '>' => Token::Gt,
            '@' => Token::At,
            '(' | '[' | '{' => {
                self.open_bracket()?;
                match c {
                    '(' => Token::LParen,
                    '[' => Token::LBracket,
                    _ => Token::LBrace,
                }
            }
            ')' | ']' | '}' => {
                self.level = self.level.saturating_sub(1);
                match c {
                    ')' => Token::RParen,
                    ']' => Token::RBracket,
                    _ => Token::RBrace,
                }
            }
            ',' => Token::Comma,
            ':' => Token::Colon,
            '=' => Token::Assign,
            '.' => Token::Dot,
            other => return Err(err(format!("unexpected character: {other:?}"))),
        };
        self.pos += c.len_utf8();
        Ok(token)
    }

    fn open_bracket(&mut self) -> ExprResult<()> {
        if self.level >= MAX_BRACKET_LEVEL {
            return Err(err("too many nested parentheses"));
        }
        self.level += 1;
        Ok(())
    }

    // === numbers ==========================================================

    fn number(&mut self) -> ExprResult<Token> {
        let (token, end) = scan_number(self.src, self.pos)?;
        self.pos = end;
        Ok(token)
    }

    // === strings ==========================================================

    /// A string literal whose prefix (`prefix_len` bytes) starts at the current
    /// position.
    fn string(&mut self, prefix: Prefix, prefix_len: usize) -> ExprResult<Token> {
        self.pos += prefix_len;
        let quote = self.byte(0).expect("a quote follows the prefix");
        let triple = self.byte(1) == Some(quote) && self.byte(2) == Some(quote);
        self.pos += if triple { 3 } else { 1 };
        if prefix.fstring {
            let parts = self.fstring_body(quote, triple, prefix.raw, false)?;
            return Ok(Token::FStr(parts));
        }
        if prefix.bytes {
            return self.bytes_body(quote, triple, prefix.raw).map(Token::Bytes);
        }
        let mut out = String::new();
        loop {
            match self.literal_run(quote, triple, &mut out, false)? {
                Stop::Quote => return Ok(Token::Str(out)),
                Stop::Backslash if prefix.raw => self.raw_backslash(&mut out, false),
                Stop::Backslash => self.escape(&mut out, false)?,
                Stop::Brace(_) => unreachable!("braces are only special in f-strings"),
            }
        }
    }

    /// Copy literal text into `out` up to the next quote, backslash (or, for an
    /// f-string, brace), which is consumed when it is the closing quote and left
    /// in place otherwise. Newlines are normalised as CPython does (`\r\n` and
    /// `\r` become `\n`) and a raw newline ends a single-quoted string in error.
    fn literal_run(
        &mut self,
        quote: u8,
        triple: bool,
        out: &mut String,
        fstring: bool,
    ) -> ExprResult<Stop> {
        let bytes = self.src.as_bytes();
        loop {
            let run_start = self.pos;
            while let Some(&b) = bytes.get(self.pos) {
                if b == quote
                    || b == b'\\'
                    || b == b'\n'
                    || b == b'\r'
                    || (fstring && (b == b'{' || b == b'}'))
                {
                    break;
                }
                self.pos += 1;
            }
            out.push_str(&self.src[run_start..self.pos]);
            let Some(&b) = bytes.get(self.pos) else {
                return Err(err("unterminated string literal"));
            };
            match b {
                b'\\' => return Ok(Stop::Backslash),
                b'{' | b'}' => return Ok(Stop::Brace(b)),
                b'\n' | b'\r' => {
                    if !triple {
                        return Err(err("unterminated string literal"));
                    }
                    self.pos += if b == b'\r' && bytes.get(self.pos + 1) == Some(&b'\n') {
                        2
                    } else {
                        1
                    };
                    out.push('\n');
                }
                _ => {
                    // The quote character.
                    if !triple {
                        self.pos += 1;
                        return Ok(Stop::Quote);
                    }
                    if bytes.get(self.pos + 1) == Some(&quote)
                        && bytes.get(self.pos + 2) == Some(&quote)
                    {
                        self.pos += 3;
                        return Ok(Stop::Quote);
                    }
                    out.push(quote as char);
                    self.pos += 1;
                }
            }
        }
    }

    /// A backslash in a raw string: kept, together with the character after it
    /// (so `\"` does not end the string). In an f-string a following brace is
    /// left to open or close a field.
    fn raw_backslash(&mut self, out: &mut String, fstring: bool) {
        out.push('\\');
        self.pos += 1;
        if let Some(ch) = self.next_char() {
            if fstring && (ch == '{' || ch == '}') {
                return;
            }
            if ch == '\r' {
                // Newline normalisation applies inside raw strings too.
                out.push('\n');
                self.pos += if self.byte(1) == Some(b'\n') { 2 } else { 1 };
            } else {
                out.push(ch);
                self.pos += ch.len_utf8();
            }
        }
    }

    /// The escape sequence after the backslash at the current position. In an
    /// f-string an unknown escape before a brace keeps the backslash and leaves
    /// the brace to open or close a field.
    fn escape(&mut self, out: &mut String, fstring: bool) -> ExprResult<()> {
        self.pos += 1; // the backslash
        let Some(c) = self.next_char() else {
            return Err(err("unterminated string literal"));
        };
        if fstring && (c == '{' || c == '}') {
            out.push('\\');
            return Ok(());
        }
        self.pos += c.len_utf8();
        match c {
            '\n' => {}
            '\r' => {
                if self.byte(0) == Some(b'\n') {
                    self.pos += 1;
                }
            }
            '\\' | '\'' | '"' => out.push(c),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\x0b'),
            '0'..='7' => {
                let mut value = c as u32 - '0' as u32;
                for _ in 0..2 {
                    match self.byte(0) {
                        Some(d @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(d - b'0');
                            self.pos += 1;
                        }
                        _ => break,
                    }
                }
                out.push(char::from_u32(value).expect("at most 0o777"));
            }
            'x' => out.push(self.hex_escape(2, "\\xXX")?),
            'u' => out.push(self.hex_escape(4, "\\uXXXX")?),
            'U' => out.push(self.hex_escape(8, "\\UXXXXXXXX")?),
            'N' => out.push(self.named_escape()?),
            other => {
                // An unknown escape keeps its backslash.
                out.push('\\');
                out.push(other);
            }
        }
        Ok(())
    }

    fn hex_escape(&mut self, digits: usize, shape: &str) -> ExprResult<char> {
        let mut value = 0u32;
        for _ in 0..digits {
            let digit = self
                .byte(0)
                .and_then(|b| (b as char).to_digit(16))
                .ok_or_else(|| {
                    err(format!(
                        "(unicode error) 'unicodeescape' codec can't decode bytes: truncated {shape} escape"
                    ))
                })?;
            value = value.wrapping_mul(16).wrapping_add(digit);
            self.pos += 1;
        }
        char::from_u32(value).ok_or_else(|| {
            if (0xD800..=0xDFFF).contains(&value) {
                err(format!("{UNREPRESENTABLE_PREFIX}string with a lone surrogate"))
            } else {
                err("(unicode error) 'unicodeescape' codec can't decode bytes: illegal Unicode character")
            }
        })
    }

    fn named_escape(&mut self) -> ExprResult<char> {
        let unknown = || {
            err(
                "(unicode error) 'unicodeescape' codec can't decode bytes: unknown Unicode character name",
            )
        };
        if self.byte(0) != Some(b'{') {
            return Err(err(
                "(unicode error) 'unicodeescape' codec can't decode bytes: malformed \\N character escape",
            ));
        }
        let rest = &self.src[self.pos + 1..];
        let end = rest.find('}').ok_or_else(|| {
            err("(unicode error) 'unicodeescape' codec can't decode bytes: malformed \\N character escape")
        })?;
        let name = &rest[..end];
        let found = unicode_names2::character(name).ok_or_else(unknown)?;
        self.pos += end + 2;
        Ok(found)
    }

    /// The body of a bytes literal (the opening quote consumed).
    fn bytes_body(&mut self, quote: u8, triple: bool, raw: bool) -> ExprResult<Vec<u8>> {
        let bytes = self.src.as_bytes();
        let mut out = Vec::new();
        loop {
            let Some(&b) = bytes.get(self.pos) else {
                return Err(err("unterminated string literal"));
            };
            if !b.is_ascii() {
                return Err(err("bytes can only contain ASCII literal characters"));
            }
            match b {
                b'\\' => {
                    self.pos += 1;
                    let Some(&c) = bytes.get(self.pos) else {
                        return Err(err("unterminated string literal"));
                    };
                    if raw {
                        out.push(b'\\');
                        continue;
                    }
                    self.pos += 1;
                    match c {
                        b'\n' => {}
                        b'\r' => {
                            if bytes.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\\' | b'\'' | b'"' => out.push(c),
                        b'a' => out.push(0x07),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'v' => out.push(0x0b),
                        b'0'..=b'7' => {
                            let mut value = u32::from(c - b'0');
                            for _ in 0..2 {
                                match bytes.get(self.pos) {
                                    Some(&d @ b'0'..=b'7') => {
                                        value = value * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((value & 0xff) as u8);
                        }
                        b'x' => {
                            let digits = (0..2).map(|k| {
                                bytes
                                    .get(self.pos + k)
                                    .and_then(|&d| (d as char).to_digit(16))
                            });
                            let mut value = 0u32;
                            for d in digits {
                                value = value * 16
                                    + d.ok_or_else(|| err("(value error) invalid \\x escape"))?;
                            }
                            self.pos += 2;
                            out.push(value as u8);
                        }
                        other => {
                            out.push(b'\\');
                            out.push(other);
                        }
                    }
                }
                b'\n' | b'\r' => {
                    if !triple {
                        return Err(err("unterminated string literal"));
                    }
                    self.pos += if b == b'\r' && bytes.get(self.pos + 1) == Some(&b'\n') {
                        2
                    } else {
                        1
                    };
                    out.push(b'\n');
                }
                _ if b == quote => {
                    if !triple {
                        self.pos += 1;
                        return Ok(out);
                    }
                    if bytes.get(self.pos + 1) == Some(&quote)
                        && bytes.get(self.pos + 2) == Some(&quote)
                    {
                        self.pos += 3;
                        return Ok(out);
                    }
                    out.push(b);
                    self.pos += 1;
                }
                _ => {
                    out.push(b);
                    self.pos += 1;
                }
            }
        }
    }

    // === f-strings ========================================================

    /// The body of an f-string (the opening quote consumed): literal text and
    /// replacement fields, up to the closing quote. With `in_spec` it is a
    /// format spec instead, which ends at the `}` closing its field.
    fn fstring_body(
        &mut self,
        quote: u8,
        triple: bool,
        raw: bool,
        in_spec: bool,
    ) -> ExprResult<Vec<FStrPart>> {
        let mut parts = Vec::new();
        let mut text = String::new();
        loop {
            // In a spec the quote is not special until the field it belongs to
            // is closed, but a quote character still ends the string.
            let stop = self.literal_run(quote, triple, &mut text, true)?;
            match stop {
                Stop::Quote => {
                    if in_spec {
                        return Err(err("f-string: expecting '}'"));
                    }
                    break;
                }
                Stop::Backslash if raw => self.raw_backslash(&mut text, true),
                Stop::Backslash => self.escape(&mut text, true)?,
                Stop::Brace(b'{') => {
                    if !in_spec && self.byte(1) == Some(b'{') {
                        text.push('{');
                        self.pos += 2;
                        continue;
                    }
                    self.pos += 1;
                    if !text.is_empty() {
                        parts.push(FStrPart::Lit(std::mem::take(&mut text)));
                    }
                    let field = self.fstring_field(quote, triple, raw)?;
                    parts.push(FStrPart::Field(Box::new(field)));
                }
                Stop::Brace(_) => {
                    if in_spec {
                        break;
                    }
                    if self.byte(1) == Some(b'}') {
                        text.push('}');
                        self.pos += 2;
                    } else {
                        return Err(err("f-string: single '}' is not allowed"));
                    }
                }
            }
        }
        if !text.is_empty() || parts.is_empty() {
            parts.push(FStrPart::Lit(text));
        }
        Ok(parts)
    }

    /// A replacement field; the opening `{` is consumed.
    fn fstring_field(&mut self, quote: u8, triple: bool, raw: bool) -> ExprResult<FStrField> {
        if self.fstring_level >= MAX_FSTRING_LEVEL {
            return Err(err("f-string: expressions nested too deeply"));
        }
        self.fstring_level += 1;
        let field_start = self.pos;
        self.open_bracket()?;
        let mut tokens = Vec::new();
        let result = self.fstring_field_parts(quote, triple, raw, field_start, &mut tokens);
        self.level -= 1;
        self.fstring_level -= 1;
        let (conversion, spec, debug) = result?;
        Ok(FStrField {
            tokens,
            conversion,
            spec,
            debug,
        })
    }

    fn fstring_field_parts(
        &mut self,
        quote: u8,
        triple: bool,
        raw: bool,
        field_start: usize,
        tokens: &mut Vec<Token>,
    ) -> ExprResult<FieldSuffix> {
        self.tokens(tokens, true)?;
        if tokens.is_empty() {
            return Err(err("f-string: valid expression required before '}'"));
        }
        tokens.push(Token::Eof);
        let mut debug = None;
        if self.byte(0) == Some(b'=') {
            self.pos += 1;
            while matches!(self.byte(0), Some(b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')) {
                self.pos += 1;
            }
            debug = Some(self.src[field_start..self.pos].to_owned());
        }
        let mut conversion = None;
        if self.byte(0) == Some(b'!') {
            self.pos += 1;
            match self.byte(0) {
                Some(c @ (b's' | b'r' | b'a')) => {
                    conversion = Some(c as char);
                    self.pos += 1;
                }
                _ => return Err(err("f-string: invalid conversion character")),
            }
        }
        let mut spec = None;
        if self.byte(0) == Some(b':') {
            self.pos += 1;
            spec = Some(self.fstring_body(quote, triple, raw, true)?);
        }
        if self.byte(0) != Some(b'}') {
            return Err(err("f-string: expecting '}'"));
        }
        self.pos += 1;
        Ok((conversion, spec, debug))
    }
}

/// What follows a replacement field's expression: conversion, format spec and
/// the text of a `=` debug marker.
type FieldSuffix = (Option<char>, Option<Vec<FStrPart>>, Option<String>);

/// What ended a run of literal text.
enum Stop {
    /// The closing quote (consumed).
    Quote,
    /// A backslash (not consumed).
    Backslash,
    /// A `{` or `}` of an f-string (not consumed).
    Brace(u8),
}

// === numeric literals =====================================================

/// Scan the numeric literal starting at `start`, following CPython's
/// `tok_get`: returns its token and the offset just past it.
fn scan_number(src: &str, start: usize) -> ExprResult<(Token, usize)> {
    let b = src.as_bytes();
    let at = |i: usize| b.get(i).copied();
    let is_digit = |i: usize| at(i).is_some_and(|c| c.is_ascii_digit());
    let mut i = start;
    let mut is_float = false;

    if b[i] == b'0' && matches!(at(i + 1), Some(b'x' | b'X' | b'o' | b'O' | b'b' | b'B')) {
        let (radix, name) = match at(i + 1).map(|c| c.to_ascii_lowercase()) {
            Some(b'x') => (16, "hexadecimal"),
            Some(b'o') => (8, "octal"),
            _ => (2, "binary"),
        };
        i += 2;
        let digits_start = i;
        loop {
            if at(i) == Some(b'_') {
                i += 1;
            }
            if !at(i).is_some_and(|c| (c as char).is_digit(radix)) {
                return Err(match at(i) {
                    Some(c) if radix != 16 && c.is_ascii_digit() => {
                        err(format!("invalid digit '{}' in {name} literal", c as char))
                    }
                    _ => err(format!("invalid {name} literal")),
                });
            }
            while at(i).is_some_and(|c| (c as char).is_digit(radix)) {
                i += 1;
            }
            if at(i) != Some(b'_') {
                break;
            }
        }
        if radix != 16
            && let Some(c) = at(i).filter(u8::is_ascii_digit)
        {
            return Err(err(format!(
                "invalid digit '{}' in {name} literal",
                c as char
            )));
        }
        verify_end_of_number(b, i, name)?;
        return Ok((radix_token(&src[digits_start..i], radix)?, i));
    }

    if b[i] == b'.' {
        // `.5`: a fraction with no integer part.
        i += 1;
        i = decimal_tail(b, i + 1)?;
        is_float = true;
    } else if b[i] == b'0' {
        // Zeros, which may begin an integer, a float or an imaginary literal.
        i += 1;
        loop {
            if at(i) == Some(b'_') {
                i += 1;
                if !is_digit(i) {
                    return Err(err("invalid decimal literal"));
                }
            }
            if at(i) != Some(b'0') {
                break;
            }
            i += 1;
        }
        let mut nonzero = false;
        if is_digit(i) {
            nonzero = true;
            i = decimal_tail(b, i + 1)?;
        }
        if at(i) == Some(b'.') {
            i += 1;
            is_float = true;
            if is_digit(i) {
                i = decimal_tail(b, i + 1)?;
            }
        } else if !matches!(at(i), Some(b'e' | b'E' | b'j' | b'J')) {
            if nonzero {
                return Err(err(
                    "leading zeros in decimal integer literals are not permitted; use an 0o prefix for octal integers",
                ));
            }
            verify_end_of_number(b, i, "decimal")?;
            return Ok((decimal_int_token(&src[start..i])?, i));
        }
    } else {
        i = decimal_tail(b, i + 1)?;
        if at(i) == Some(b'.') {
            i += 1;
            is_float = true;
            if is_digit(i) {
                i = decimal_tail(b, i + 1)?;
            }
        }
    }

    // Exponent.
    if matches!(at(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(at(j), Some(b'+' | b'-')) {
            j += 1;
            if !is_digit(j) {
                return Err(err("invalid decimal literal"));
            }
        } else if !is_digit(j) {
            // `1else`: the `e` begins a keyword, not an exponent.
            verify_end_of_number(b, i, "decimal")?;
            return Ok((number_token(&src[start..i], is_float)?, i));
        }
        i = decimal_tail(b, j + 1)?;
        is_float = true;
    }

    if matches!(at(i), Some(b'j' | b'J')) {
        let digits = &src[start..i];
        i += 1;
        verify_end_of_number(b, i, "imaginary")?;
        return Ok((Token::Imag(parse_float(digits)?), i));
    }
    verify_end_of_number(b, i, "decimal")?;
    Ok((number_token(&src[start..i], is_float)?, i))
}

/// Digits and underscores after the first digit, which has been consumed (an
/// underscore must be followed by a digit). Returns the offset past them.
fn decimal_tail(b: &[u8], mut i: usize) -> ExprResult<usize> {
    loop {
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if b.get(i) != Some(&b'_') {
            return Ok(i);
        }
        i += 1;
        if !b.get(i).is_some_and(u8::is_ascii_digit) {
            return Err(err("invalid decimal literal"));
        }
    }
}

/// CPython's `verify_end_of_number`: a literal may be followed directly by
/// `and`, `else`, `for`, `if`, `in`, `is`, `not` or `or` (with a deprecation
/// warning), but not by any other identifier character.
fn verify_end_of_number(b: &[u8], i: usize, kind: &str) -> ExprResult<()> {
    let rest = &b[i.min(b.len())..];
    let keyword = match rest.first() {
        Some(b'a') => rest.starts_with(b"and"),
        Some(b'e') => rest.starts_with(b"else"),
        Some(b'f') => rest.starts_with(b"for"),
        Some(b'i') => matches!(rest.get(1), Some(b'f' | b'n' | b's')),
        Some(b'o') => rest.starts_with(b"or"),
        Some(b'n') => rest.starts_with(b"not"),
        _ => false,
    };
    if !keyword
        && rest
            .first()
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
    {
        return Err(err(format!("invalid {kind} literal")));
    }
    Ok(())
}

fn without_underscores(text: &str) -> Cow<'_, str> {
    if text.contains('_') {
        Cow::Owned(text.replace('_', ""))
    } else {
        Cow::Borrowed(text)
    }
}

fn parse_float(text: &str) -> ExprResult<f64> {
    let digits = without_underscores(text);
    digits
        .parse::<f64>()
        .map_err(|_| err(format!("invalid float literal: {digits}")))
}

/// A decimal integer literal: [`Token::Int`] while it fits `i64`, else
/// [`Token::BigInt`]. CPython refuses more than 4300 digits (a `SyntaxError`).
fn decimal_int_token(text: &str) -> ExprResult<Token> {
    let digits = without_underscores(text);
    match digits.parse::<i64>() {
        Ok(v) => Ok(Token::Int(v)),
        Err(_) => pynum::parse_digits(&digits, 10)
            .map(|n| Token::BigInt(Box::new(n)))
            .map_err(|e| err(format!("{e} - Consider hexadecimal for huge integer literals to avoid decimal conversion limits."))),
    }
}

fn radix_token(text: &str, radix: u32) -> ExprResult<Token> {
    let digits = without_underscores(text);
    match i64::from_str_radix(&digits, radix) {
        Ok(v) => Ok(Token::Int(v)),
        Err(_) => pynum::parse_digits(&digits, radix)
            .map(|n| Token::BigInt(Box::new(n)))
            .map_err(|e| err(e.to_string())),
    }
}

fn number_token(text: &str, is_float: bool) -> ExprResult<Token> {
    if is_float {
        Ok(Token::Float(parse_float(text).unwrap_or(f64::NAN)))
    } else {
        decimal_int_token(text)
    }
}

#[cfg(test)]
mod tests {
    //! Cases checked against CPython 3.13's tokenizer/`ast.parse`.

    use super::*;

    fn one(src: &str) -> Token {
        let mut tokens = lex(src).unwrap_or_else(|e| panic!("{src:?}: {e}"));
        assert_eq!(tokens.pop(), Some(Token::Eof), "{src:?}");
        assert_eq!(tokens.len(), 1, "{src:?}: {tokens:?}");
        tokens.pop().unwrap()
    }

    fn string(src: &str) -> String {
        match one(src) {
            Token::Str(s) => s,
            other => panic!("{src:?}: {other:?}"),
        }
    }

    #[test]
    fn numeric_literal_forms() {
        assert_eq!(one("0x1f"), Token::Int(31));
        assert_eq!(one("0XFF"), Token::Int(255));
        assert_eq!(one("0o17"), Token::Int(15));
        assert_eq!(one("0b101"), Token::Int(5));
        assert_eq!(one("0x_ff"), Token::Int(255));
        assert_eq!(one("1_000"), Token::Int(1000));
        assert_eq!(one("0_0"), Token::Int(0));
        assert_eq!(one("00"), Token::Int(0));
        assert_eq!(one("1e3"), Token::Float(1000.0));
        assert_eq!(one("1.5e3"), Token::Float(1500.0));
        assert_eq!(one(".5"), Token::Float(0.5));
        assert_eq!(one("5."), Token::Float(5.0));
        assert_eq!(one("1_0.5"), Token::Float(10.5));
        assert_eq!(one("1e1_0"), Token::Float(1e10));
        assert_eq!(one("1j"), Token::Imag(1.0));
        assert_eq!(one("1.5J"), Token::Imag(1.5));
        assert_eq!(one("1e3j"), Token::Imag(1000.0));
    }

    #[test]
    fn malformed_numeric_literals_are_refused() {
        for bad in [
            "1__0", "1_", "0x", "0b2", "0o8", "01", "1e", "1e+", "0x1g", "1abc", "1.real", "0b",
            "0_",
        ] {
            assert!(lex(bad).is_err(), "{bad:?} lexed");
        }
    }

    #[test]
    fn a_number_may_run_into_a_keyword() {
        // CPython 3.13 accepts `1if 1else 2` (with a warning).
        let tokens = lex("1if 1else 2").unwrap();
        assert_eq!(tokens[0], Token::Int(1));
        assert_eq!(tokens[1], Token::Name("if".into()));
        assert_eq!(tokens[3], Token::Name("else".into()));
        assert!(lex("0x1for 1").is_ok());
        assert!(lex("1or 2").is_ok());
        assert!(lex("1in [1]").is_ok());
    }

    #[test]
    fn a_float_then_an_attribute() {
        assert_eq!(
            lex("1.5.real").unwrap(),
            vec![
                Token::Float(1.5),
                Token::Dot,
                Token::Name("real".into()),
                Token::Eof
            ]
        );
        assert_eq!(
            lex("1..real").unwrap(),
            vec![
                Token::Float(1.0),
                Token::Dot,
                Token::Name("real".into()),
                Token::Eof
            ]
        );
        assert_eq!(
            lex("0.0._p").unwrap(),
            vec![
                Token::Float(0.0),
                Token::Dot,
                Token::Name("_p".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn string_prefixes_and_quotes() {
        assert_eq!(string(r#""""abc""""#), "abc");
        assert_eq!(string("'''a'b'''"), "a'b");
        assert_eq!(string("'''a\nb'''"), "a\nb");
        assert_eq!(string("'''a\r\nb'''"), "a\nb");
        assert_eq!(string(r"r'a\nb'"), r"a\nb");
        assert_eq!(string(r"R'\''"), r"\'");
        assert_eq!(string("u'x'"), "x");
        assert!(matches!(one(r"b'x'"), Token::Bytes(b) if b == b"x"));
        assert!(matches!(one(r"Rb'\n'"), Token::Bytes(b) if b == b"\\n"));
        // `ur` is not a prefix: a name, then a string (a parse error later).
        assert_eq!(lex("ur'x'").unwrap().len(), 3);
        assert!(lex("'a\nb'").is_err());
        assert!(lex("'abc").is_err());
        assert!(lex("'''abc").is_err());
        assert!(lex("b'é'").is_err());
    }

    #[test]
    fn string_escapes() {
        assert_eq!(string(r"'\x41é\U0001F600'"), "A\u{e9}\u{1F600}");
        assert_eq!(string(r"'\N{BULLET}\N{bullet}'"), "\u{2022}\u{2022}");
        assert_eq!(string(r"'\101\1\777'"), "A\u{1}\u{1ff}");
        assert_eq!(string(r"'\a\b\f\v\0'"), "\x07\x08\x0c\x0b\0");
        assert_eq!(string(r"'\q\ \8'"), r"\q\ \8");
        assert_eq!(string("'a\\\nb'"), "ab");
        assert_eq!(string("'a\\\r\nb'"), "ab");
        assert_eq!(string(r#"'\'\"\\'"#), "'\"\\");
        for bad in [
            r"'\x4'",
            r"'\u00e'",
            r"'\U00110000'",
            r"'\ud800'",
            r"'\N{NOPE}'",
            r"'\N'",
        ] {
            assert!(lex(bad).is_err(), "{bad:?} lexed");
        }
    }

    #[test]
    fn fstring_fields() {
        let Token::FStr(parts) = one(r#"f"a{x!r:>{w}}b{{c}}""#) else {
            panic!("not an f-string")
        };
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], FStrPart::Lit("a".into()));
        let FStrPart::Field(field) = &parts[1] else {
            panic!("not a field")
        };
        assert_eq!(field.conversion, Some('r'));
        assert_eq!(field.tokens, vec![Token::Name("x".into()), Token::Eof]);
        assert!(matches!(&field.spec, Some(spec) if spec.len() == 2));
        assert_eq!(parts[2], FStrPart::Lit("b{c}".into()));
    }

    #[test]
    fn fstring_debug_and_nested_quotes() {
        let Token::FStr(parts) = one(r#"f"{x = }""#) else {
            panic!("not an f-string")
        };
        let FStrPart::Field(field) = &parts[0] else {
            panic!("not a field")
        };
        assert_eq!(field.debug.as_deref(), Some("x = "));
        // PEP 701: the same quote may nest inside a field.
        assert!(matches!(one(r#"f"{"a"}""#), Token::FStr(_)));
        assert!(matches!(one(r#"f"{a!=b}{a==b}{a<=b}""#), Token::FStr(_)));
    }

    #[test]
    fn malformed_fstrings_are_refused() {
        for bad in [
            r#"f"{""#,
            r#"f"}""#,
            r#"f"{}""#,
            r#"f"{a!x}""#,
            r#"f"{a"#,
            r#"f"{a:"#,
        ] {
            assert!(lex(bad).is_err(), "{bad:?} lexed");
        }
    }

    #[test]
    fn operators_and_whitespace() {
        assert_eq!(
            lex("a @ b := ... # c").unwrap(),
            vec![
                Token::Name("a".into()),
                Token::At,
                Token::Name("b".into()),
                Token::ColonEq,
                Token::Ellipsis,
                Token::Eof
            ]
        );
        assert!(lex("a\u{a0}b").is_err());
        assert!(lex("a\0").is_err());
        assert!(lex("a \\ b").is_err());
        assert_eq!(lex("a \\\n b").unwrap().len(), 3);
    }
}
