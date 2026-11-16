//! Flat-key TOML reader.
//!
//! The manifest grammar lets `defines` sit next to `defines.public`, which is
//! not valid TOML. We lex the file with `toml_parser`, rewrite every dotted key
//! that appears in a table body into one quoted key (`"defines.public"`), and
//! parse the rewritten text with the `toml` crate. Table headers and inline
//! tables keep their normal TOML meaning.

use toml_parser::decoder::Encoding;
use toml_parser::parser::{Event, EventKind};
use toml_parser::{ParseError, Raw, Source, Span};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct FlatError {
    pub line: usize,
    pub message: String,
}

/// Parses `src` as TOML in which dotted keys inside table bodies are kept as
/// single flat keys: `defines.public = [..]` yields the key `"defines.public"`
/// in the enclosing table, next to a plain `defines`.
pub fn parse_flat(src: &str) -> Result<toml::Table, FlatError> {
    let source = Source::new(src);
    let tokens = source.lex().into_vec();
    let mut events: Vec<Event> = Vec::new();
    let mut errors: Vec<ParseError> = Vec::new();
    toml_parser::parser::parse_document(&tokens, &mut |e: Event| events.push(e), &mut errors);
    if let Some(err) = errors.first() {
        let at = err
            .unexpected()
            .or_else(|| err.context())
            .map_or(src.len(), |s| s.start());
        return Err(FlatError {
            line: line_of(src, at),
            message: err.description().to_owned(),
        });
    }

    let replacements = collect_replacements(source, &events)?;
    let mut rewritten = src.to_owned();
    for (span, key) in replacements.iter().rev() {
        rewritten.replace_range(span.start()..span.end(), key);
    }

    toml::from_str::<toml::Table>(&rewritten).map_err(|e| FlatError {
        line: e.span().map_or(1, |s| line_of(&rewritten, s.start)),
        message: e.message().to_owned(),
    })
}

/// One-based line of the byte offset `at`. Rewriting never adds or removes
/// newlines, so this is valid for the original and the rewritten text.
fn line_of(src: &str, at: usize) -> usize {
    let end = at.min(src.len());
    src.as_bytes()[..end]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

/// Finds every dotted key of a table body (outside headers, inline tables and
/// arrays) and the quoted single key that replaces it.
fn collect_replacements(
    source: Source<'_>,
    events: &[Event],
) -> Result<Vec<(Span, String)>, FlatError> {
    let mut out = Vec::new();
    let mut in_header = false;
    let mut inline_depth = 0usize;
    let mut array_depth = 0usize;
    let mut run: Vec<(Span, Option<Encoding>)> = Vec::new();

    for ev in events {
        match ev.kind() {
            EventKind::StdTableOpen | EventKind::ArrayTableOpen => in_header = true,
            EventKind::StdTableClose | EventKind::ArrayTableClose => in_header = false,
            EventKind::InlineTableOpen => inline_depth += 1,
            EventKind::InlineTableClose => inline_depth = inline_depth.saturating_sub(1),
            EventKind::ArrayOpen => array_depth += 1,
            EventKind::ArrayClose => array_depth = array_depth.saturating_sub(1),
            EventKind::SimpleKey if !in_header && inline_depth == 0 && array_depth == 0 => {
                run.push((ev.span(), ev.encoding()));
            }
            EventKind::KeyValSep if !in_header && inline_depth == 0 && array_depth == 0 => {
                if run.len() > 1 {
                    out.push(flatten(source, &run)?);
                }
                run.clear();
            }
            _ => {}
        }
    }
    Ok(out)
}

fn flatten(
    source: Source<'_>,
    segments: &[(Span, Option<Encoding>)],
) -> Result<(Span, String), FlatError> {
    let first = segments[0].0;
    let last = segments[segments.len() - 1].0;
    let mut parts = Vec::with_capacity(segments.len());
    for &(span, encoding) in segments {
        let line = line_of(source.input(), span.start());
        let text = source.get(span).ok_or_else(|| FlatError {
            line,
            message: "invalid key".into(),
        })?;
        let raw = Raw::new_unchecked(text.as_str(), encoding, span);
        let mut decoded = String::new();
        let mut key_errors: Vec<ParseError> = Vec::new();
        raw.decode_key(&mut decoded, &mut key_errors);
        if let Some(err) = key_errors.first() {
            return Err(FlatError {
                line,
                message: err.description().to_owned(),
            });
        }
        if decoded.contains('.') {
            return Err(FlatError {
                line,
                message: format!("key segment `{decoded}` must not contain `.`"),
            });
        }
        parts.push(decoded);
    }
    let whole = Span::new_unchecked(first.start(), last.end());
    Ok((whole, quote(&parts.join("."))))
}

/// A TOML basic string holding `s`.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_keys_coexist() {
        let t = parse_flat("defines = [\"A\"]\ndefines.public = [\"B\"]\ndefines.windows = [\"C\"]\ndefines.public.macos = [\"D\"]\n").unwrap();
        assert_eq!(
            t.keys().cloned().collect::<Vec<_>>(),
            [
                "defines",
                "defines.public",
                "defines.public.macos",
                "defines.windows"
            ]
        );
        assert_eq!(
            t["defines.public.macos"].as_array().unwrap()[0].as_str(),
            Some("D")
        );
    }

    #[test]
    fn headers_stay_nested() {
        let t = parse_flat("[feature.simd.avx2]\ndefines.public = [\"X\"]\n").unwrap();
        assert!(t["feature"]["simd"]["avx2"].get("defines.public").is_some());
    }

    #[test]
    fn inline_tables_untouched() {
        let t = parse_flat("alias = { arch = { x86_64 = \"x64\" } }\n").unwrap();
        assert_eq!(t["alias"]["arch"]["x86_64"].as_str(), Some("x64"));
    }

    #[test]
    fn scalar_and_suffixed_scalar_coexist() {
        let t = parse_flat("default = \"avx2\"\ndefault.arm64 = \"neon\"\noptions = { A = false,\n  B = true }\noptions.linux = { C = true }\n").unwrap();
        assert_eq!(t["default.arm64"].as_str(), Some("neon"));
        assert!(t["options.linux"].is_table());
    }

    #[test]
    fn strings_and_comments_with_dots_untouched() {
        let t = parse_flat("a = \"x.y\" # c.d\n").unwrap();
        assert_eq!(t["a"].as_str(), Some("x.y"));
    }

    #[test]
    fn duplicate_flat_key_is_error() {
        assert!(parse_flat("a.b = 1\na.b = 2\n").is_err());
    }

    #[test]
    fn quoted_segment_with_dot_is_error() {
        assert!(
            parse_flat("\"a.b\".c = 1\n")
                .unwrap_err()
                .message
                .contains("`.`")
        );
    }

    #[test]
    fn error_line_is_preserved() {
        assert_eq!(parse_flat("a = 1\nb.c = 2\nd = [\n").unwrap_err().line, 3);
    }

    #[test]
    fn spec_example_parses() {
        parse_flat(include_str!("../../tests/fixtures/spec_example.toml")).unwrap();
    }

    #[test]
    fn quoted_segments_are_decoded_and_requoted() {
        let t = parse_flat(
            "a.\"b c\".\"q\\\"\" = 1
",
        )
        .unwrap();
        assert!(t.contains_key("a.b c.q\""));
    }

    #[test]
    fn dotted_keys_inside_arrays_of_tables_are_left_alone() {
        let t = parse_flat(
            "x = [ { a.b = 1 } ]
",
        )
        .unwrap();
        assert_eq!(t["x"][0]["a"]["b"].as_integer(), Some(1));
    }
}
