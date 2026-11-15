//! Turning a compiler-written depfile into a list of normalized paths.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use crate::{config::vocab::Compiler, depfile::parse_depfile};

/// Normalizes a path from a depfile:
/// * `./` and `x/../` segments are removed lexically,
/// * paths inside `root` become root-relative,
/// * paths outside `root` stay absolute.
pub fn normalize(root: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        match p.strip_prefix(root) {
            Ok(rel) => clean(rel),
            Err(_) => clean(p),
        }
    } else {
        clean(p)
    }
}

fn clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push(".."); // leading `..` must be kept
                }
                // `/..` is `/`
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// What a compiler writes for `-MMD` style header tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepFormat {
    /// A Makefile rule (gcc, clang, clang-cl with `/clang:-MMD`).
    Makefile,
    /// The JSON of MSVC's `/sourceDependencies`.
    MsvcJson,
}

impl DepFormat {
    /// The format the compiler's command line asks for.
    pub fn of(compiler: Compiler) -> DepFormat {
        match compiler {
            Compiler::Msvc => DepFormat::MsvcJson,
            _ => DepFormat::Makefile,
        }
    }

    /// The extension of the depfile after the object's name.
    pub fn extension(self) -> &'static str {
        match self {
            DepFormat::Makefile => "d",
            DepFormat::MsvcJson => "json",
        }
    }
}

/// Every file the compiler listed for one translation unit (the source itself
/// and the headers it included), normalized and de-duplicated, in order.
pub fn read_headers(
    root: &Path,
    depfile: &Path,
    format: DepFormat,
) -> Result<Vec<PathBuf>, String> {
    let bytes = fs::read(depfile).map_err(|e| format!("cannot read {}: {e}", depfile.display()))?;
    let mut out: Vec<PathBuf> = Vec::new();
    let paths = if format == DepFormat::MsvcJson {
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", depfile.display()))?;
        let data = json.get("Data").unwrap_or(&json);
        let mut paths = Vec::new();
        if let Some(source) = data.get("Source").and_then(serde_json::Value::as_str) {
            paths.push(source.to_owned());
        }
        for key in ["Includes", "ImportedHeaderUnits"] {
            if let Some(entries) = data.get(key).and_then(serde_json::Value::as_array) {
                for entry in entries {
                    if let Some(path) = entry.as_str() {
                        paths.push(path.to_owned());
                    } else if let Some(path) = entry
                        .get("Path")
                        .or_else(|| entry.get("Header"))
                        .and_then(serde_json::Value::as_str)
                    {
                        paths.push(path.to_owned());
                    }
                }
            }
        }
        paths
    } else {
        parse_depfile(&bytes)
            .map_err(|e| format!("{}: {e}", depfile.display()))?
            .ins
    };
    for raw in &paths {
        let p = normalize(root, raw);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(root: &str, raw: &str) -> PathBuf {
        normalize(Path::new(root), raw)
    }

    /// An absolute path on every OS: Windows needs a drive letter.
    fn abs(p: &str) -> String {
        if cfg!(windows) {
            format!("C:{p}")
        } else {
            p.to_string()
        }
    }

    #[test]
    fn strips_leading_dot_slash() {
        assert_eq!(n("/proj", "./src/main.cpp"), PathBuf::from("src/main.cpp"));
    }

    #[test]
    fn resolves_dotdot_lexically() {
        assert_eq!(
            n("/proj", "src/../include/a.hpp"),
            PathBuf::from("include/a.hpp")
        );
    }

    #[test]
    fn keeps_leading_dotdot() {
        assert_eq!(n("/proj", "../shared/x.h"), PathBuf::from("../shared/x.h"));
    }

    #[test]
    fn absolute_inside_root_becomes_relative() {
        assert_eq!(
            n(&abs("/proj"), &abs("/proj/include/a.hpp")),
            PathBuf::from("include/a.hpp")
        );
    }

    #[test]
    fn absolute_outside_root_stays_absolute() {
        assert_eq!(
            n(&abs("/proj"), &abs("/usr/include/x.h")),
            PathBuf::from(abs("/usr/include/x.h"))
        );
    }

    #[test]
    fn root_prefix_must_match_whole_components() {
        // /project is not inside /proj
        assert_eq!(
            n(&abs("/proj"), &abs("/project/a.h")),
            PathBuf::from(abs("/project/a.h"))
        );
    }

    #[test]
    fn dotdot_cannot_climb_above_filesystem_root() {
        assert_eq!(
            n(&abs("/proj"), &abs("/../etc/x")),
            PathBuf::from(abs("/etc/x"))
        );
    }

    #[test]
    fn read_headers_normalizes_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let dep = dir.path().join("main.cpp.o.d");
        fs::write(
            &dep,
            "build/o/main.cpp.o: ./src/main.cpp include/hello.hpp \\\n src/../include/hello.hpp\n",
        )
        .unwrap();
        let got = read_headers(Path::new("/proj"), &dep, DepFormat::Makefile).unwrap();
        assert_eq!(
            got,
            vec![
                PathBuf::from("src/main.cpp"),
                PathBuf::from("include/hello.hpp")
            ]
        );
    }

    #[test]
    fn missing_depfile_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.d");
        assert!(read_headers(dir.path(), &missing, DepFormat::Makefile).is_err());
        assert!(read_headers(dir.path(), &missing, DepFormat::MsvcJson).is_err());
    }

    #[test]
    fn format_follows_the_compiler_not_the_msvc_family() {
        assert_eq!(DepFormat::of(Compiler::Msvc), DepFormat::MsvcJson);
        for c in [
            Compiler::ClangCl,
            Compiler::Clang,
            Compiler::Gcc,
            Compiler::AppleClang,
        ] {
            assert_eq!(DepFormat::of(c), DepFormat::Makefile, "{c:?}");
        }
        assert_eq!(DepFormat::Makefile.extension(), "d");
        assert_eq!(DepFormat::MsvcJson.extension(), "json");
    }

    #[test]
    fn each_format_reads_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new("/proj");

        let make = dir.path().join("a.obj.d");
        fs::write(
            &make,
            "a.obj: src/a.c include/a.h
",
        )
        .unwrap();
        let got = read_headers(root, &make, DepFormat::Makefile).unwrap();
        assert_eq!(
            got,
            vec![PathBuf::from("src/a.c"), PathBuf::from("include/a.h")]
        );

        let json = dir.path().join("a.obj.json");
        fs::write(
            &json,
            r#"{"Version":"1.2","Data":{"Source":"src/a.c","Includes":["include/a.h"]}}"#,
        )
        .unwrap();
        let got = read_headers(root, &json, DepFormat::MsvcJson).unwrap();
        assert_eq!(got, read_headers(root, &make, DepFormat::Makefile).unwrap());
        // The wrong format is an error, not a silent guess.
        assert!(read_headers(root, &json, DepFormat::Makefile).is_err());
    }
}
