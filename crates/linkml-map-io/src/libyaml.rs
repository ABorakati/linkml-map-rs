//! A minimal safe wrapper around `unsafe-libyaml`'s event parser.
//!
//! PyYAML parses YAML with a Python scanner and parser that its optional C
//! extension (`yaml.CSafeLoader`) replaces with libyaml; the two accept the same
//! language. `unsafe-libyaml` is libyaml itself (a mechanical Rust translation,
//! already in the dependency tree through `serde_yaml_ng`), so it is the closest
//! available match for the *structure* PyYAML sees: which characters make a
//! plain scalar, how flow collections nest, how block scalars fold. What each
//! scalar *means* is decided afterwards by `linkml_map_core::pyyaml`.
//!
//! This is the only `unsafe` in the crate. The unsafe surface is the three calls
//! below (`yaml_parser_initialize` / `yaml_parser_parse` / `yaml_parser_delete`)
//! plus reading the fields of an event that the parser has just produced and
//! that is deleted straight after.

#![allow(unsafe_code)]

use std::ffi::CStr;
use std::marker::PhantomData;
use std::mem::MaybeUninit;

use unsafe_libyaml as sys;

/// A parser event (libyaml's `yaml_event_t`, copied out).
#[derive(Debug)]
pub(crate) enum Event {
    StreamStart,
    StreamEnd,
    DocumentStart,
    DocumentEnd,
    Alias {
        anchor: String,
    },
    Scalar {
        anchor: Option<String>,
        /// The resolved tag (`!!int` is `tag:yaml.org,2002:int`), or `!`.
        tag: Option<String>,
        value: String,
        /// libyaml's `plain_implicit`: the implicit resolvers apply (a plain
        /// untagged scalar, or one tagged with the non-specific `!`).
        plain_implicit: bool,
        /// Where the scalar's text lies in the input, as character offsets
        /// (libyaml's `start_mark.index..end_mark.index`).
        span: (usize, usize),
    },
    SequenceStart {
        anchor: Option<String>,
        tag: Option<String>,
    },
    SequenceEnd,
    MappingStart {
        anchor: Option<String>,
        tag: Option<String>,
    },
    MappingEnd,
}

/// An event stream over a borrowed UTF-8 string.
pub(crate) struct Parser<'input> {
    /// Boxed: the parser keeps pointers into itself, so it must not move.
    parser: Box<MaybeUninit<sys::yaml_parser_t>>,
    finished: bool,
    _input: PhantomData<&'input str>,
}

impl<'input> Parser<'input> {
    pub(crate) fn new(input: &'input str) -> Self {
        let mut parser = Box::new(MaybeUninit::<sys::yaml_parser_t>::uninit());
        // SAFETY: `parser` points to writable memory of the right size.
        // `yaml_parser_initialize` fully initialises it (or fails on
        // allocation, which aborts below). The input pointer stays valid for
        // `'input`, which the returned parser borrows; it is only read while
        // parsing, and `Drop` deletes the parser before the borrow ends.
        unsafe {
            if sys::yaml_parser_initialize(parser.as_mut_ptr()).fail {
                panic!("libyaml: out of memory initialising the parser");
            }
            sys::yaml_parser_set_encoding(parser.as_mut_ptr(), sys::YAML_UTF8_ENCODING);
            sys::yaml_parser_set_input_string(
                parser.as_mut_ptr(),
                input.as_ptr(),
                input.len() as u64,
            );
        }
        Parser {
            parser,
            finished: false,
            _input: PhantomData,
        }
    }

    /// The next event; `Ok(None)` after `StreamEnd`.
    pub(crate) fn next_event(&mut self) -> Result<Option<Event>, String> {
        if self.finished {
            return Ok(None);
        }
        let mut event = MaybeUninit::<sys::yaml_event_t>::uninit();
        // SAFETY: the parser is initialised (see `new`); `event` is written by
        // a successful `yaml_parser_parse`, read once by `convert`, and then
        // released with `yaml_event_delete`, which is exactly libyaml's
        // contract. After a failure the event is uninitialised and untouched.
        unsafe {
            let parser = self.parser.as_mut_ptr();
            if sys::yaml_parser_parse(parser, event.as_mut_ptr()).fail {
                self.finished = true;
                return Err(describe_error(parser));
            }
            let converted = convert(&*event.as_ptr());
            sys::yaml_event_delete(event.as_mut_ptr());
            if matches!(converted, Event::StreamEnd) {
                self.finished = true;
            }
            Ok(Some(converted))
        }
    }
}

impl Drop for Parser<'_> {
    fn drop(&mut self) {
        // SAFETY: initialised in `new`, deleted exactly once.
        unsafe { sys::yaml_parser_delete(self.parser.as_mut_ptr()) }
    }
}

/// `problem`, `problem_mark` and `context` of a failed parse, as PyYAML-style
/// text.
///
/// # Safety
/// `parser` must be an initialised parser whose last call failed.
unsafe fn describe_error(parser: *mut sys::yaml_parser_t) -> String {
    // SAFETY: per the contract, `problem` and `context` are null or valid
    // NUL-terminated strings owned by libyaml's static data.
    unsafe {
        let parser: &sys::yaml_parser_t = &*parser;
        let text = |p: *const std::ffi::c_char| {
            (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
        };
        let problem = text(parser.problem).unwrap_or_else(|| "unknown error".to_owned());
        let mark = parser.problem_mark;
        match text(parser.context) {
            Some(context) => format!(
                "{context}: {problem} at line {}, column {}",
                mark.line + 1,
                mark.column + 1
            ),
            None => format!(
                "{problem} at line {}, column {}",
                mark.line + 1,
                mark.column + 1
            ),
        }
    }
}

/// An optional NUL-terminated string (anchor or tag).
///
/// # Safety
/// `p` is null or a valid NUL-terminated string.
unsafe fn optional(p: *const u8) -> Option<String> {
    // SAFETY: per the contract.
    (!p.is_null()).then(|| {
        unsafe { CStr::from_ptr(p.cast()) }
            .to_string_lossy()
            .into_owned()
    })
}

/// Copy a live event into owned Rust data.
///
/// # Safety
/// `event` must have been produced by a successful `yaml_parser_parse` and not
/// yet deleted.
unsafe fn convert(event: &sys::yaml_event_t) -> Event {
    // SAFETY: the union member read matches `event.type_`.
    unsafe {
        match event.type_ {
            sys::YAML_STREAM_START_EVENT => Event::StreamStart,
            sys::YAML_STREAM_END_EVENT => Event::StreamEnd,
            sys::YAML_DOCUMENT_START_EVENT => Event::DocumentStart,
            sys::YAML_DOCUMENT_END_EVENT => Event::DocumentEnd,
            sys::YAML_ALIAS_EVENT => Event::Alias {
                anchor: optional(event.data.alias.anchor).unwrap_or_default(),
            },
            sys::YAML_SCALAR_EVENT => {
                let scalar = &event.data.scalar;
                let bytes = if scalar.length == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(scalar.value, scalar.length as usize)
                };
                Event::Scalar {
                    anchor: optional(scalar.anchor),
                    tag: optional(scalar.tag),
                    value: String::from_utf8_lossy(bytes).into_owned(),
                    plain_implicit: scalar.plain_implicit,
                    span: (
                        event.start_mark.index as usize,
                        event.end_mark.index as usize,
                    ),
                }
            }
            sys::YAML_SEQUENCE_START_EVENT => Event::SequenceStart {
                anchor: optional(event.data.sequence_start.anchor),
                tag: optional(event.data.sequence_start.tag),
            },
            sys::YAML_SEQUENCE_END_EVENT => Event::SequenceEnd,
            sys::YAML_MAPPING_START_EVENT => Event::MappingStart {
                anchor: optional(event.data.mapping_start.anchor),
                tag: optional(event.data.mapping_start.tag),
            },
            sys::YAML_MAPPING_END_EVENT => Event::MappingEnd,
            // `YAML_NO_EVENT` is never returned by a successful parse.
            _ => Event::StreamEnd,
        }
    }
}
