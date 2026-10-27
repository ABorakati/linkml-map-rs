//! `slugify()`: port of upstream `eval_utils._slugify`, which wraps
//! python-slugify's `slugify(s, separator=separator, lowercase=True)`.
//!
//! python-slugify 9.x (upstream bumped 8.0.4 -> 9.1.3 in 27ca24d) keeps the
//! 8.0.4 output byte for byte as the default `algorithm="legacy"`: 9.x froze
//! that pipeline in `slugify/_legacy.py` ("DO NOT MODIFY") and put its new
//! behaviour behind opt-in arguments `slugify()` here never passes. The port
//! therefore follows the legacy pipeline, whose step order is kept below.
//!
//! PARITY: python-slugify transliterates with `text-unidecode` (Unidecode when
//! installed; the upstream lock has only `text-unidecode`). The `deunicode` crate
//! stands in for it. Both descend from Perl's Text::Unidecode, so Latin, Greek
//! and Cyrillic text agree in practice, but a rare character may transliterate
//! differently.

use unicode_normalization::UnicodeNormalization;

const DEFAULT_SEPARATOR: char = '-';

/// `eval_utils._slugify(s, separator)`: `None` when no identifier-shaped
/// content remains (the `or None`), and a leading digit gets `separator`
/// prefixed so the result is a valid identifier.
pub(super) fn slugify(s: &str, separator: &str) -> Option<String> {
    let slug = slugify_lib(s, separator);
    if slug.is_empty() {
        return None;
    }
    if slug.starts_with(|c: char| c.is_ascii_digit()) {
        return Some(format!("{separator}{slug}"));
    }
    Some(slug)
}

/// `slugify.slugify(text, separator=separator, lowercase=True)` with every other
/// argument at its default (entities, decimal and hexadecimal references decoded,
/// no stopwords, replacements or length limit, ASCII output).
fn slugify_lib(text: &str, separator: &str) -> String {
    // QUOTE_PATTERN `[']+` -> "-" (pre-process).
    let text = replace_quote_runs(text, "-");
    // unicodedata.normalize("NFKD") then unidecode.
    let text: String = text.nfkd().collect();
    let text = deunicode::deunicode(&text);
    let text = decode_entities(&text);
    // Re-normalise, lowercase, drop the quotes decoding may have produced.
    let text: String = text.nfkd().collect();
    let text = text.to_lowercase();
    let text = replace_quote_runs(&text, "");
    // NUMBERS_PATTERN `(?<=\d),(?=\d)` -> "".
    let text = remove_digit_commas(&text);
    // DISALLOWED_CHARS_PATTERN `[^-a-zA-Z0-9]+` -> "-", then DUPLICATE_DASH_PATTERN
    // `-{2,}` -> "-" and strip("-").
    let mut slug = String::with_capacity(text.len());
    let mut pending_dash = false;
    for c in text.chars() {
        if c == DEFAULT_SEPARATOR || !c.is_ascii_alphanumeric() {
            pending_dash = true;
        } else {
            if pending_dash && !slug.is_empty() {
                slug.push(DEFAULT_SEPARATOR);
            }
            pending_dash = false;
            slug.push(c);
        }
    }
    if separator == "-" {
        slug
    } else {
        slug.replace(DEFAULT_SEPARATOR, separator)
    }
}

/// Replace each run of `'` with `with`.
fn replace_quote_runs(text: &str, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_run = false;
    for c in text.chars() {
        if c == '\'' {
            if !in_run {
                out.push_str(with);
            }
            in_run = true;
        } else {
            in_run = false;
            out.push(c);
        }
    }
    out
}

/// Remove a comma that sits between two digits.
fn remove_digit_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars
        .iter()
        .enumerate()
        .filter(|&(i, &c)| {
            !(c == ','
                && i > 0
                && chars[i - 1].is_ascii_digit()
                && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        })
        .map(|(_, &c)| c)
        .collect()
}

/// `CHAR_ENTITY_PATTERN`, `DECIMAL_PATTERN` then `HEX_PATTERN` substitutions.
///
/// PARITY: each numeric kind is all-or-nothing, as in legacy python-slugify:
/// `chr()` raising on one reference (above U+10FFFF) aborts the whole `re.sub`
/// and leaves the text unchanged (`except (ValueError, OverflowError): pass`).
fn decode_entities(text: &str) -> String {
    let text = substitute_references(text, Reference::Named);
    let text = substitute_references(&text, Reference::Decimal);
    substitute_references(&text, Reference::Hex)
}

#[derive(Clone, Copy)]
enum Reference {
    Named,
    Decimal,
    Hex,
}

fn substitute_references(text: &str, kind: Reference) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        match match_reference(tail, kind) {
            Some((len, Some(decoded))) => {
                out.extend(decoded);
                rest = &tail[len..];
            }
            // An unrepresentable code point: Python's `chr()` raised, so the
            // whole pass is abandoned.
            Some((_, None)) => return text.to_string(),
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Match one reference at the start of `tail` (which begins with `&`). Returns its
/// length and the decoded char; the char is `None` when `chr()` would raise. A lone
/// surrogate is valid in Python but cannot be held in a Rust `String`; it is
/// dropped, which the later filtering would do to it anyway.
fn match_reference(tail: &str, kind: Reference) -> Option<(usize, Option<Option<char>>)> {
    let semi = tail.find(';')?;
    let body = &tail[1..semi];
    let len = semi + 1;
    let code = match kind {
        Reference::Named => {
            let (_, cp) = HTML_ENTITIES.iter().find(|(name, _)| *name == body)?;
            *cp
        }
        Reference::Decimal => {
            let digits = body.strip_prefix('#')?;
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            digits.parse::<u32>().unwrap_or(u32::MAX)
        }
        Reference::Hex => {
            let digits = body.strip_prefix("#x")?;
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            u32::from_str_radix(digits, 16).unwrap_or(u32::MAX)
        }
    };
    if code > 0x10FFFF {
        return Some((len, None));
    }
    Some((len, Some(char::from_u32(code))))
}

/// `html.entities.name2codepoint` (the HTML 4 entities).
const HTML_ENTITIES: &[(&str, u32)] = &[
    ("AElig", 0x00C6),
    ("Aacute", 0x00C1),
    ("Acirc", 0x00C2),
    ("Agrave", 0x00C0),
    ("Alpha", 0x0391),
    ("Aring", 0x00C5),
    ("Atilde", 0x00C3),
    ("Auml", 0x00C4),
    ("Beta", 0x0392),
    ("Ccedil", 0x00C7),
    ("Chi", 0x03A7),
    ("Dagger", 0x2021),
    ("Delta", 0x0394),
    ("ETH", 0x00D0),
    ("Eacute", 0x00C9),
    ("Ecirc", 0x00CA),
    ("Egrave", 0x00C8),
    ("Epsilon", 0x0395),
    ("Eta", 0x0397),
    ("Euml", 0x00CB),
    ("Gamma", 0x0393),
    ("Iacute", 0x00CD),
    ("Icirc", 0x00CE),
    ("Igrave", 0x00CC),
    ("Iota", 0x0399),
    ("Iuml", 0x00CF),
    ("Kappa", 0x039A),
    ("Lambda", 0x039B),
    ("Mu", 0x039C),
    ("Ntilde", 0x00D1),
    ("Nu", 0x039D),
    ("OElig", 0x0152),
    ("Oacute", 0x00D3),
    ("Ocirc", 0x00D4),
    ("Ograve", 0x00D2),
    ("Omega", 0x03A9),
    ("Omicron", 0x039F),
    ("Oslash", 0x00D8),
    ("Otilde", 0x00D5),
    ("Ouml", 0x00D6),
    ("Phi", 0x03A6),
    ("Pi", 0x03A0),
    ("Prime", 0x2033),
    ("Psi", 0x03A8),
    ("Rho", 0x03A1),
    ("Scaron", 0x0160),
    ("Sigma", 0x03A3),
    ("THORN", 0x00DE),
    ("Tau", 0x03A4),
    ("Theta", 0x0398),
    ("Uacute", 0x00DA),
    ("Ucirc", 0x00DB),
    ("Ugrave", 0x00D9),
    ("Upsilon", 0x03A5),
    ("Uuml", 0x00DC),
    ("Xi", 0x039E),
    ("Yacute", 0x00DD),
    ("Yuml", 0x0178),
    ("Zeta", 0x0396),
    ("aacute", 0x00E1),
    ("acirc", 0x00E2),
    ("acute", 0x00B4),
    ("aelig", 0x00E6),
    ("agrave", 0x00E0),
    ("alefsym", 0x2135),
    ("alpha", 0x03B1),
    ("amp", 0x0026),
    ("and", 0x2227),
    ("ang", 0x2220),
    ("aring", 0x00E5),
    ("asymp", 0x2248),
    ("atilde", 0x00E3),
    ("auml", 0x00E4),
    ("bdquo", 0x201E),
    ("beta", 0x03B2),
    ("brvbar", 0x00A6),
    ("bull", 0x2022),
    ("cap", 0x2229),
    ("ccedil", 0x00E7),
    ("cedil", 0x00B8),
    ("cent", 0x00A2),
    ("chi", 0x03C7),
    ("circ", 0x02C6),
    ("clubs", 0x2663),
    ("cong", 0x2245),
    ("copy", 0x00A9),
    ("crarr", 0x21B5),
    ("cup", 0x222A),
    ("curren", 0x00A4),
    ("dArr", 0x21D3),
    ("dagger", 0x2020),
    ("darr", 0x2193),
    ("deg", 0x00B0),
    ("delta", 0x03B4),
    ("diams", 0x2666),
    ("divide", 0x00F7),
    ("eacute", 0x00E9),
    ("ecirc", 0x00EA),
    ("egrave", 0x00E8),
    ("empty", 0x2205),
    ("emsp", 0x2003),
    ("ensp", 0x2002),
    ("epsilon", 0x03B5),
    ("equiv", 0x2261),
    ("eta", 0x03B7),
    ("eth", 0x00F0),
    ("euml", 0x00EB),
    ("euro", 0x20AC),
    ("exist", 0x2203),
    ("fnof", 0x0192),
    ("forall", 0x2200),
    ("frac12", 0x00BD),
    ("frac14", 0x00BC),
    ("frac34", 0x00BE),
    ("frasl", 0x2044),
    ("gamma", 0x03B3),
    ("ge", 0x2265),
    ("gt", 0x003E),
    ("hArr", 0x21D4),
    ("harr", 0x2194),
    ("hearts", 0x2665),
    ("hellip", 0x2026),
    ("iacute", 0x00ED),
    ("icirc", 0x00EE),
    ("iexcl", 0x00A1),
    ("igrave", 0x00EC),
    ("image", 0x2111),
    ("infin", 0x221E),
    ("int", 0x222B),
    ("iota", 0x03B9),
    ("iquest", 0x00BF),
    ("isin", 0x2208),
    ("iuml", 0x00EF),
    ("kappa", 0x03BA),
    ("lArr", 0x21D0),
    ("lambda", 0x03BB),
    ("lang", 0x2329),
    ("laquo", 0x00AB),
    ("larr", 0x2190),
    ("lceil", 0x2308),
    ("ldquo", 0x201C),
    ("le", 0x2264),
    ("lfloor", 0x230A),
    ("lowast", 0x2217),
    ("loz", 0x25CA),
    ("lrm", 0x200E),
    ("lsaquo", 0x2039),
    ("lsquo", 0x2018),
    ("lt", 0x003C),
    ("macr", 0x00AF),
    ("mdash", 0x2014),
    ("micro", 0x00B5),
    ("middot", 0x00B7),
    ("minus", 0x2212),
    ("mu", 0x03BC),
    ("nabla", 0x2207),
    ("nbsp", 0x00A0),
    ("ndash", 0x2013),
    ("ne", 0x2260),
    ("ni", 0x220B),
    ("not", 0x00AC),
    ("notin", 0x2209),
    ("nsub", 0x2284),
    ("ntilde", 0x00F1),
    ("nu", 0x03BD),
    ("oacute", 0x00F3),
    ("ocirc", 0x00F4),
    ("oelig", 0x0153),
    ("ograve", 0x00F2),
    ("oline", 0x203E),
    ("omega", 0x03C9),
    ("omicron", 0x03BF),
    ("oplus", 0x2295),
    ("or", 0x2228),
    ("ordf", 0x00AA),
    ("ordm", 0x00BA),
    ("oslash", 0x00F8),
    ("otilde", 0x00F5),
    ("otimes", 0x2297),
    ("ouml", 0x00F6),
    ("para", 0x00B6),
    ("part", 0x2202),
    ("permil", 0x2030),
    ("perp", 0x22A5),
    ("phi", 0x03C6),
    ("pi", 0x03C0),
    ("piv", 0x03D6),
    ("plusmn", 0x00B1),
    ("pound", 0x00A3),
    ("prime", 0x2032),
    ("prod", 0x220F),
    ("prop", 0x221D),
    ("psi", 0x03C8),
    ("quot", 0x0022),
    ("rArr", 0x21D2),
    ("radic", 0x221A),
    ("rang", 0x232A),
    ("raquo", 0x00BB),
    ("rarr", 0x2192),
    ("rceil", 0x2309),
    ("rdquo", 0x201D),
    ("real", 0x211C),
    ("reg", 0x00AE),
    ("rfloor", 0x230B),
    ("rho", 0x03C1),
    ("rlm", 0x200F),
    ("rsaquo", 0x203A),
    ("rsquo", 0x2019),
    ("sbquo", 0x201A),
    ("scaron", 0x0161),
    ("sdot", 0x22C5),
    ("sect", 0x00A7),
    ("shy", 0x00AD),
    ("sigma", 0x03C3),
    ("sigmaf", 0x03C2),
    ("sim", 0x223C),
    ("spades", 0x2660),
    ("sub", 0x2282),
    ("sube", 0x2286),
    ("sum", 0x2211),
    ("sup", 0x2283),
    ("sup1", 0x00B9),
    ("sup2", 0x00B2),
    ("sup3", 0x00B3),
    ("supe", 0x2287),
    ("szlig", 0x00DF),
    ("tau", 0x03C4),
    ("there4", 0x2234),
    ("theta", 0x03B8),
    ("thetasym", 0x03D1),
    ("thinsp", 0x2009),
    ("thorn", 0x00FE),
    ("tilde", 0x02DC),
    ("times", 0x00D7),
    ("trade", 0x2122),
    ("uArr", 0x21D1),
    ("uacute", 0x00FA),
    ("uarr", 0x2191),
    ("ucirc", 0x00FB),
    ("ugrave", 0x00F9),
    ("uml", 0x00A8),
    ("upsih", 0x03D2),
    ("upsilon", 0x03C5),
    ("uuml", 0x00FC),
    ("weierp", 0x2118),
    ("xi", 0x03BE),
    ("yacute", 0x00FD),
    ("yen", 0x00A5),
    ("yuml", 0x00FF),
    ("zeta", 0x03B6),
    ("zwj", 0x200D),
    ("zwnj", 0x200C),
];

#[cfg(test)]
mod tests {
    use super::slugify;

    fn s(text: &str) -> Option<String> {
        slugify(text, "_")
    }

    // PARITY: upstream `_slugify` doctests (eval_utils.py).
    #[test]
    fn upstream_doctests() {
        assert_eq!(s("Hello, World!").as_deref(), Some("hello_world"));
        assert_eq!(s("Schöne Grüße").as_deref(), Some("schone_grusse"));
        assert_eq!(
            slugify("café-au-lait", "-").as_deref(),
            Some("cafe-au-lait")
        );
        assert_eq!(s("123 hits").as_deref(), Some("_123_hits"));
        assert_eq!(s("!!!"), None);
        assert_eq!(s(""), None);
    }

    // Behaviours of python-slugify's frozen legacy pipeline, identical in 8.0.4
    // and 9.1.3 (expected values worked out from slugify/_legacy.py).
    #[test]
    fn legacy_pipeline_details() {
        // Quotes: a run becomes "-" before folding, then any left are removed.
        assert_eq!(s("it's").as_deref(), Some("it_s"));
        assert_eq!(s("don&#39;t").as_deref(), Some("dont"));
        // Entities are decoded: `&amp;` is "&", then a separator.
        assert_eq!(s("A &amp; B").as_deref(), Some("a_b"));
        assert_eq!(s("&lt;b&gt;bold&lt;/b&gt;").as_deref(), Some("b_bold_b"));
        assert_eq!(s("caf&eacute;").as_deref(), Some("cafe"));
        assert_eq!(s("&#65;&#x42;c").as_deref(), Some("abc"));
        // A reference past U+10FFFF abandons that numeric kind entirely.
        assert_eq!(s("a &#1114112; &#66;").as_deref(), Some("a_1114112_66"));
        // Commas between digits vanish.
        assert_eq!(s("1,234,567 items").as_deref(), Some("_1234567_items"));
        assert_eq!(s("a,b").as_deref(), Some("a_b"));
        // Runs of separators collapse and the ends are stripped.
        assert_eq!(s("  --Foo___Bar  Baz--  ").as_deref(), Some("foo_bar_baz"));
        // Digit-leading results take the separator prefix.
        assert_eq!(slugify("2024 report", "-").as_deref(), Some("-2024-report"));
        // Non-Latin scripts transliterate rather than vanish.
        assert_eq!(s("Привет мир").as_deref(), Some("privet_mir"));
    }
}
