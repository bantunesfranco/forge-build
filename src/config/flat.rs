//! Flat-key TOML reader.
//!
//! The manifest grammar lets `defines` sit next to `defines.public`, which is
//! not valid TOML. We lex the file with `toml_parser`, rewrite every dotted key
//! that appears in a table body into one quoted key (`"defines.public"`), and
//! parse the rewritten text with the `toml` crate. Table headers and inline
//! tables keep their normal TOML meaning.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml_parser::decoder::Encoding;
use toml_parser::parser::{Event, EventKind};
use toml_parser::{ParseError, Raw, Source, Span};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct FlatError {
    pub line: usize,
    pub message: String,
}

/// Where the tables and keys of a manifest are written: the 1-based line of
/// every table header (`lib.engine`, `feature.simd.avx2`, ...) and of every key
/// in a table body, by the flat name the key has after the rewrite
/// (`defines.public.windows`). A multi-line value is located at its key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Locations {
    file: PathBuf,
    tables: BTreeMap<String, usize>,
    keys: BTreeMap<(String, String), usize>,
}

impl Locations {
    /// The file these lines are in, as it is shown in messages.
    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn with_file(mut self, file: PathBuf) -> Self {
        self.file = file;
        self
    }

    /// The line of the header of `table`. A table that only exists because a
    /// deeper header names it (`[feature.simd.avx2]` makes `feature.simd`) is
    /// located at its first child.
    pub fn table(&self, table: &str) -> Option<usize> {
        if let Some(&line) = self.tables.get(table) {
            return Some(line);
        }
        let prefix = format!("{table}.");
        self.tables
            .iter()
            .filter(|(path, _)| path.starts_with(&prefix))
            .map(|(_, &line)| line)
            .min()
    }

    /// The line of `key` in `table`.
    pub fn key(&self, table: &str, key: &str) -> Option<usize> {
        self.keys.get(&(table.to_owned(), key.to_owned())).copied()
    }

    /// The line of `key` in `table`, or of the table when the key is unknown or not given.
    pub fn line(&self, table: &str, key: Option<&str>) -> Option<usize> {
        key.and_then(|k| self.key(table, k))
            .or_else(|| self.table(table))
    }
}

/// Parses `src` as TOML in which dotted keys inside table bodies are kept as
/// single flat keys: `defines.public = [..]` yields the key `"defines.public"`
/// in the enclosing table, next to a plain `defines`.
pub fn parse_flat(src: &str) -> Result<toml::Table, FlatError> {
    parse_located(src).map(|(table, _)| table)
}

/// Like [`parse_flat`], and says on which line each table and key is.
pub fn parse_located(src: &str) -> Result<(toml::Table, Locations), FlatError> {
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

    let (replacements, locations) = scan(source, &events)?;
    let mut rewritten = src.to_owned();
    for (span, key) in replacements.iter().rev() {
        rewritten.replace_range(span.start()..span.end(), key);
    }

    let table = toml::from_str::<toml::Table>(&rewritten).map_err(|e| FlatError {
        line: e.span().map_or(1, |s| line_of(&rewritten, s.start)),
        message: e.message().to_owned(),
    })?;
    Ok((table, locations))
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
/// arrays) and the quoted single key that replaces it, and records where each
/// table header and each key is.
fn scan(
    source: Source<'_>,
    events: &[Event],
) -> Result<(Vec<(Span, String)>, Locations), FlatError> {
    let mut out = Vec::new();
    let mut locations = Locations::default();
    let mut in_header = false;
    let mut inline_depth = 0usize;
    let mut array_depth = 0usize;
    let mut table = String::new();
    let mut header: Vec<(Span, Option<Encoding>)> = Vec::new();
    let mut run: Vec<(Span, Option<Encoding>)> = Vec::new();

    for ev in events {
        match ev.kind() {
            EventKind::StdTableOpen | EventKind::ArrayTableOpen => {
                in_header = true;
                header.clear();
            }
            EventKind::StdTableClose | EventKind::ArrayTableClose => {
                in_header = false;
                table = decode_run(source, &header)?.join(".");
                if let Some(&(first, _)) = header.first() {
                    let line = line_of(source.input(), first.start());
                    locations.tables.entry(table.clone()).or_insert(line);
                }
            }
            EventKind::SimpleKey if in_header => header.push((ev.span(), ev.encoding())),
            EventKind::InlineTableOpen => inline_depth += 1,
            EventKind::InlineTableClose => inline_depth = inline_depth.saturating_sub(1),
            EventKind::ArrayOpen => array_depth += 1,
            EventKind::ArrayClose => array_depth = array_depth.saturating_sub(1),
            EventKind::SimpleKey if inline_depth == 0 && array_depth == 0 => {
                run.push((ev.span(), ev.encoding()));
            }
            EventKind::KeyValSep if !in_header && inline_depth == 0 && array_depth == 0 => {
                if run.len() > 1 {
                    let parts = decode_run(source, &run)?;
                    if let Some(bad) = parts.iter().find(|p| p.contains('.')) {
                        return Err(FlatError {
                            line: line_of(source.input(), run[0].0.start()),
                            message: format!("key segment `{bad}` must not contain `.`"),
                        });
                    }
                    let whole = Span::new_unchecked(run[0].0.start(), run[run.len() - 1].0.end());
                    out.push((whole, quote(&parts.join("."))));
                }
                // A key that does not decode is reported by the TOML parser.
                if let Ok(parts) = decode_run(source, &run) {
                    let line = line_of(source.input(), run[0].0.start());
                    locations
                        .keys
                        .entry((table.clone(), parts.join(".")))
                        .or_insert(line);
                }
                run.clear();
            }
            _ => {}
        }
    }
    Ok((out, locations))
}

/// The decoded text of each key segment.
fn decode_run(
    source: Source<'_>,
    segments: &[(Span, Option<Encoding>)],
) -> Result<Vec<String>, FlatError> {
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
        parts.push(decoded);
    }
    Ok(parts)
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

    fn located(src: &str) -> Locations {
        parse_located(src).unwrap().1
    }

    #[test]
    fn tables_and_keys_have_lines() {
        let l = located(concat!(
            "[workspace]\n",
            "name = \"a\"\n",
            "\n",
            "[lib.engine]\n",
            "sources = [\"a.c\"]\n",
            "defines.public.windows = [\"X\"]\n",
            "\n",
            "[feature.simd.avx2]\n",
            "flags = []\n",
        ));
        assert_eq!(l.table("workspace"), Some(1));
        assert_eq!(l.table("lib.engine"), Some(4));
        assert_eq!(l.table("feature.simd.avx2"), Some(8));
        assert_eq!(l.key("workspace", "name"), Some(2));
        assert_eq!(l.key("lib.engine", "sources"), Some(5));
        assert_eq!(l.key("lib.engine", "defines.public.windows"), Some(6));
        assert_eq!(l.key("feature.simd.avx2", "flags"), Some(9));
        assert_eq!(l.key("lib.engine", "missing"), None);
        assert_eq!(l.table("lib.other"), None);
    }

    #[test]
    fn an_implicit_table_is_located_at_its_first_child() {
        let l = located("\n[feature.simd.sse]\n\n[feature.simd.avx2]\n");
        assert_eq!(l.table("feature.simd"), Some(2));
        assert_eq!(l.table("feature"), Some(2));
    }

    #[test]
    fn line_prefers_the_key_and_falls_back_to_the_table() {
        let l = located("[lib.a]\nsources = []\n");
        assert_eq!(l.line("lib.a", Some("sources")), Some(2));
        assert_eq!(l.line("lib.a", Some("nope")), Some(1));
        assert_eq!(l.line("lib.a", None), Some(1));
        assert_eq!(l.line("lib.b", Some("sources")), None);
    }

    #[test]
    fn multi_line_values_are_located_at_their_key() {
        let l = located(concat!(
            "[lib.a]\n",
            "sources = [\n",
            "  \"a.c\",\n",
            "  \"b.c\",\n",
            "]\n",
            "alias = { arch = {\n",
            "  x86_64 = \"x64\" } }\n",
            "defines.public = [\"X\"]\n",
            "flags = [ # a, b = 1\n",
            "  \"-x\",\n",
            "]\n",
            "last = 1\n",
        ));
        assert_eq!(l.key("lib.a", "sources"), Some(2));
        assert_eq!(l.key("lib.a", "alias"), Some(6));
        assert_eq!(l.key("lib.a", "defines.public"), Some(8));
        assert_eq!(l.key("lib.a", "flags"), Some(9));
        assert_eq!(l.key("lib.a", "last"), Some(12));
        // Keys inside an inline table are not keys of the table.
        assert_eq!(l.key("lib.a", "arch"), None);
        assert_eq!(l.key("lib.a", "x86_64"), None);
    }

    #[test]
    fn comments_and_blank_lines_count() {
        let l = located("# one\n\n# three\n[lib.a] # four\n# five\nsources = [] # six\n");
        assert_eq!(l.table("lib.a"), Some(4));
        assert_eq!(l.key("lib.a", "sources"), Some(6));
    }

    #[test]
    fn crlf_files_have_the_same_lines() {
        let l = located(concat!(
            "[lib.a]\r\n",
            "sources = [\r\n",
            "  \"a.c\",\r\n",
            "]\r\n",
            "[lib.b]\r\n",
            "deps.public = []\r\n",
        ));
        assert_eq!(l.table("lib.a"), Some(1));
        assert_eq!(l.key("lib.a", "sources"), Some(2));
        assert_eq!(l.table("lib.b"), Some(5));
        assert_eq!(l.key("lib.b", "deps.public"), Some(6));
    }

    #[test]
    fn quoted_and_spaced_keys_are_located_by_their_decoded_name() {
        let l = located("[ lib . \"a\" ]\n\"defines\" . \"public\" = []\n");
        assert_eq!(l.table("lib.a"), Some(1));
        assert_eq!(l.key("lib.a", "defines.public"), Some(2));
    }
}
