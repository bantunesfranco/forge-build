//! `forge init`: a working project for an empty directory, or a manifest for
//! sources that already exist. Needs no manifest and no compiler.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use walkdir::WalkDir;

use crate::lang::Lang;

const MANIFEST: &str = "project.toml";
const GITIGNORE: &str = ".gitignore";
/// Directories next to the manifest that are taken as include directories.
const INCLUDE_DIRS: [&str; 3] = ["include", "inc", "incs"];

/// What `init` did, for the caller to print.
#[derive(Debug)]
pub struct Created {
    pub name: String,
    /// Files written or changed, relative to the project directory.
    pub files: Vec<String>,
    /// For existing sources: the source globs and include directories found.
    pub detected: Option<Detected>,
}

#[derive(Debug)]
pub struct Detected {
    pub files: usize,
    pub globs: Vec<String>,
    pub include: Vec<String>,
}

/// Creates the project in `dir` (created if missing). `name` is used as given
/// and must be a valid target name; without one it comes from the directory's
/// name. `lang` defaults to what the existing sources use, else C++.
pub fn init(dir: &Path, name: Option<&str>, lang: Option<Lang>) -> Result<Created, String> {
    if dir.join(MANIFEST).exists() {
        return Err(format!("{MANIFEST} already exists in {}", dir.display()));
    }
    if dir.exists() && !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    let name = project_name(dir, name)?;
    let found = scan(dir)?;

    let manifest;
    let mut detected = None;
    let mut starter = None;
    if found.files == 0 {
        let lang = lang.unwrap_or(Lang::Cxx);
        manifest = manifest_text(
            &name,
            &BTreeSet::from([lang]),
            &[glob_for("src", lang)],
            &[],
        );
        starter = Some(lang);
    } else {
        let mut langs = found.langs.clone();
        langs.extend(lang);
        let globs = found.globs();
        let include: Vec<String> = INCLUDE_DIRS
            .iter()
            .filter(|d| dir.join(d).is_dir())
            .map(|d| d.to_string())
            .collect();
        manifest = manifest_text(&name, &langs, &globs, &include);
        detected = Some(Detected {
            files: found.files,
            globs,
            include,
        });
    }

    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut files = Vec::new();
    write_new(&dir.join(MANIFEST), &manifest)?;
    files.push(MANIFEST.to_string());
    if let Some(lang) = starter {
        let (file, text) = hello(&name, lang);
        let path = dir.join(file);
        fs::create_dir_all(path.parent().expect("a file below the directory"))
            .map_err(|e| format!("cannot create {}: {e}", dir.join("src").display()))?;
        write_new(&path, &text)?;
        files.push(file.to_string());
        if ignore_build(dir)? {
            files.push(GITIGNORE.to_string());
        }
    }
    Ok(Created {
        name,
        files,
        detected,
    })
}

/// A valid target name: `[A-Za-z_][A-Za-z0-9_-]*`.
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Makes `raw` a valid target name: other characters become `_` (runs of them
/// one `_`), `_` and `-` at the ends go, and a leading digit gets a `_` in
/// front. `None` when nothing is left.
pub fn sanitize_name(raw: &str) -> Option<String> {
    let mut name = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            name.push(c);
        } else if !name.ends_with('_') {
            name.push('_');
        }
    }
    let name = name.trim_matches(['_', '-']);
    if name.is_empty() {
        return None;
    }
    Some(if name.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{name}")
    } else {
        name.to_string()
    })
}

fn project_name(dir: &Path, explicit: Option<&str>) -> Result<String, String> {
    if let Some(name) = explicit {
        return if is_valid_name(name) {
            Ok(name.to_string())
        } else {
            Err(format!(
                "`{name}` is not a valid name (letters, digits, `_` and `-`, not starting with a digit or `-`)"
            ))
        };
    }
    // `.` and `..` have no name of their own: resolve them first.
    let full = match dir.canonicalize() {
        Ok(p) => p,
        Err(_) => std::path::absolute(dir).map_err(|e| format!("{}: {e}", dir.display()))?,
    };
    let raw = full.file_name().map(|n| n.to_string_lossy().into_owned());
    raw.as_deref().and_then(sanitize_name).ok_or_else(|| {
        format!(
            "cannot make a target name from the directory name {}; pass --name <name>",
            raw.as_deref()
                .map_or_else(|| full.display().to_string(), |r| format!("`{r}`"))
        )
    })
}

/// The C/C++ files below the directory, outside `build/` and hidden directories.
struct Found {
    files: usize,
    langs: BTreeSet<Lang>,
    /// (directory relative to the project, `/`-separated, "" for the project
    /// directory itself; extension) of every source file.
    places: BTreeSet<(String, String)>,
}

fn scan(dir: &Path) -> Result<Found, String> {
    let mut found = Found {
        files: 0,
        langs: BTreeSet::new(),
        places: BTreeSet::new(),
    };
    if !dir.is_dir() {
        return Ok(found);
    }
    let build = dir.join("build");
    let walk = WalkDir::new(dir).into_iter().filter_entry(|e| {
        e.depth() == 0
            || !e.file_type().is_dir()
            || (e.path() != build && !e.file_name().to_string_lossy().starts_with('.'))
    });
    for entry in walk {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let Some(lang) = Lang::from_path(entry.path()).filter(|_| entry.file_type().is_file())
        else {
            continue;
        };
        let rel: PathBuf = entry
            .path()
            .strip_prefix(dir)
            .expect("below the directory")
            .to_path_buf();
        let parent = rel
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let ext = entry
            .path()
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        found.files += 1;
        found.langs.insert(lang);
        found.places.insert((parent, ext));
    }
    Ok(found)
}

impl Found {
    /// One glob per extension and top-most directory: `src/**/*.cpp`, or
    /// `**/*.cpp` when the files are at the top level.
    fn globs(&self) -> Vec<String> {
        let mut by_ext: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (dir, ext) in &self.places {
            by_ext.entry(ext).or_default().push(dir);
        }
        let mut globs: Vec<(String, String)> = Vec::new();
        for (ext, dirs) in by_ext {
            if dirs.contains(&"") {
                globs.push((String::new(), format!("**/*.{ext}")));
                continue;
            }
            for dir in &dirs {
                let covered = dirs
                    .iter()
                    .any(|d| dir.len() > d.len() && dir.starts_with(&format!("{d}/")));
                if !covered {
                    globs.push((dir.to_string(), format!("{dir}/**/*.{ext}")));
                }
            }
        }
        globs.sort();
        globs.into_iter().map(|(_, g)| g).collect()
    }
}

fn glob_for(dir: &str, lang: Lang) -> String {
    match lang {
        Lang::C => format!("{dir}/**/*.c"),
        Lang::Cxx => format!("{dir}/**/*.cpp"),
    }
}

fn manifest_text(
    name: &str,
    langs: &BTreeSet<Lang>,
    sources: &[String],
    include: &[String],
) -> String {
    let list = |items: &[String]| {
        items
            .iter()
            .map(|i| toml_string(i))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut text = String::from(
        "# A Forge project. Every key is described in docs/manifest-spec.md of the Forge repository.\n",
    );
    text.push_str("[workspace]\n");
    text.push_str(&format!("name = {}\n", toml_string(name)));
    text.push_str("version = \"0.1.0\"\n");
    if langs.contains(&Lang::C) {
        text.push_str("c = \"17\"\n");
    }
    if langs.contains(&Lang::Cxx) {
        text.push_str("cxx = \"20\"\n");
    }
    text.push_str("warnings = \"high\"\n\n");
    text.push_str(&format!("[bin.{name}]\n"));
    text.push_str(&format!("sources = [{}]\n", list(sources)));
    if !include.is_empty() {
        text.push_str(&format!("include = [{}]\n", list(include)));
    }
    text
}

/// A TOML basic string.
fn toml_string(s: &str) -> String {
    let mut out = String::from("\"");
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

/// The starter source file (relative path, content).
fn hello(name: &str, lang: Lang) -> (&'static str, String) {
    match lang {
        Lang::Cxx => (
            "src/main.cpp",
            format!(
                "#include <iostream>\n\nint main() {{\n    std::cout << \"Hello from {name}\\n\";\n    return 0;\n}}\n"
            ),
        ),
        Lang::C => (
            "src/main.c",
            format!(
                "#include <stdio.h>\n\nint main(void) {{\n    printf(\"Hello from {name}\\n\");\n    return 0;\n}}\n"
            ),
        ),
    }
}

/// Writes a file that must not exist yet.
fn write_new(path: &Path, text: &str) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    file.write_all(text.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Makes sure `.gitignore` has a `build/` line: creates the file, or appends
/// to it and leaves every other line alone. Returns whether it wrote.
fn ignore_build(dir: &Path) -> Result<bool, String> {
    let path = dir.join(GITIGNORE);
    let existing = match fs::read(&path) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let Some(text) = existing else {
        write_new(&path, "build/\n")?;
        return Ok(true);
    };
    let present = text
        .lines()
        .any(|l| matches!(l.trim(), "build" | "build/" | "/build" | "/build/"));
    if present {
        return Ok(false);
    }
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut add = String::new();
    if !text.is_empty() && !text.ends_with('\n') {
        add.push_str(eol);
    }
    add.push_str("build/");
    add.push_str(eol);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    file.write_all(add.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_sanitized() {
        let s = sanitize_name;
        assert_eq!(s("hello").as_deref(), Some("hello"));
        assert_eq!(s("My Game.v2").as_deref(), Some("My_Game_v2"));
        assert_eq!(s("a  --  b").as_deref(), Some("a_--_b"));
        assert_eq!(s("3d-viewer").as_deref(), Some("_3d-viewer"));
        assert_eq!(s(".hidden-").as_deref(), Some("hidden"));
        assert_eq!(s("caf\u{e9}").as_deref(), Some("caf"));
        assert_eq!(s("---"), None);
        assert_eq!(s("..."), None);
        assert_eq!(s(""), None);
        for raw in ["My Game.v2", "3d", "x y", "_a_", "a-b"] {
            assert!(is_valid_name(&s(raw).unwrap()), "{raw}");
        }
    }

    #[test]
    fn valid_names_follow_the_schema() {
        assert!(is_valid_name("a"));
        assert!(is_valid_name("_a-b_9"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("9a"));
        assert!(!is_valid_name("-a"));
        assert!(!is_valid_name("a b"));
    }

    #[test]
    fn globs_cover_the_top_most_directories() {
        let places = |v: &[(&str, &str)]| Found {
            files: v.len(),
            langs: BTreeSet::new(),
            places: v
                .iter()
                .map(|(d, e)| (d.to_string(), e.to_string()))
                .collect(),
        };
        assert_eq!(
            places(&[("src", "cpp"), ("src/a", "cpp"), ("srcs", "c")]).globs(),
            ["src/**/*.cpp", "srcs/**/*.c"]
        );
        assert_eq!(
            places(&[("", "cc"), ("util", "cc"), ("util", "c")]).globs(),
            ["**/*.cc", "util/**/*.c"]
        );
        assert_eq!(
            places(&[("core", "cpp"), ("ui", "cpp"), ("core2", "cpp")]).globs(),
            ["core/**/*.cpp", "core2/**/*.cpp", "ui/**/*.cpp"]
        );
    }

    #[test]
    fn strings_are_escaped() {
        assert_eq!(toml_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(toml_string("a\nb"), "\"a\\u000Ab\"");
    }
}
