//! Resolving a profile into the arguments of a build.
//!
//! Layers, later ones winning:
//!   base `[build.flags]`  ->  inherited profiles (depth-first, in listed order)  ->  the profile itself
//!
//! Within a layer a `ListEdit` is applied as `set`, then `remove`, then `add`.
//! Defines are identified by macro name, so `VERSION=4` replaces `VERSION=3`.

use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

use crate::{
    config::{
        ConfigError, Manifest,
        manifest::BuildProfile,
        types::{Define, OptLevel},
    },
    executor::path_arg,
    lang::{Driver, Lang, Toolchain},
    project::Project,
};

/// Everything needed to build one profile.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub c: Option<Toolchain>,
    pub cpp: Option<Toolchain>,
    /// Flags for every compile, after `-std=`: warnings, optimization, debug
    /// info, defines, include dirs, then the profile's raw `compile_flags`.
    pub common_args: Vec<String>,
    /// Placed after the objects: raw `link_flags`, then `-L`, then `-l`.
    pub link_args: Vec<String>,
}

impl Resolved {
    /// The toolchain for a language that appears in the project.
    pub fn toolchain(&self, lang: Lang) -> &Toolchain {
        match lang {
            Lang::C => self.c.as_ref(),
            Lang::Cxx => self.cpp.as_ref(),
        }
        .expect("a task's language always has a toolchain: it comes from the sources")
    }

    /// `-std=...` followed by the common flags.
    pub fn compile_args(&self, lang: Lang) -> Vec<String> {
        let toolchain = self.toolchain(lang);
        let msvc = toolchain.driver() == Driver::Msvc;
        let mut args = vec![standard_flag(&toolchain.standard, msvc)];
        if msvc && lang == Lang::Cxx {
            args.push("/EHsc".into());
        }
        args.extend(self.common_args.iter().cloned());
        args
    }
}

/// The result of layering, before it is turned into arguments.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Layered {
    pub opt_level: Option<OptLevel>,
    pub debug_info: bool,
    pub defines: Vec<Define>,
    pub compile_flags: Vec<String>,
    pub link_flags: Vec<String>,
}

pub enum Origin {
    System { found_by: String },  // "pkg-config:zlib"
    Vendored { prefix: PathBuf }, // vendor/<name>/<profile>/install
}

pub struct ResolvedDep {
    pub name: String,
    pub version: Option<String>,
    pub origin: Origin,
    pub include_dirs: Vec<PathBuf>,
    pub lib_dirs: Vec<PathBuf>,
    pub libs: Vec<String>,
    pub defines: Vec<Define>,
    pub cflags: Vec<String>,
    pub ldflags: Vec<String>,
}

/// Applies the base layer and the chain of `profile` to produce its settings.
pub fn layer(manifest: &Manifest, profile: &str) -> Result<Layered, ConfigError> {
    let mut chain = Vec::new();
    collect(
        profile,
        &manifest.profiles,
        &mut Vec::new(),
        &mut HashSet::new(),
        &mut chain,
    )?;

    let mut out = Layered {
        defines: manifest.build.flags.defines.clone(),
        ..Layered::default()
    };
    for p in chain {
        if let Some(o) = p.opt_level {
            out.opt_level = Some(o);
        }
        if let Some(d) = p.debug_info {
            out.debug_info = d;
        }
        p.defines.apply(&mut out.defines, |a, b| a.name == b.name);
        p.compile_flags.apply(&mut out.compile_flags, |a, b| a == b);
        p.link_flags.apply(&mut out.link_flags, |a, b| a == b);
    }
    Ok(out)
}

/// Parents first (in listed order), then the profile. A profile reached twice
/// (diamond inheritance) is applied once.
fn collect<'a>(
    name: &'a str,
    profiles: &'a BTreeMap<String, BuildProfile>,
    stack: &mut Vec<&'a str>,
    done: &mut HashSet<&'a str>,
    out: &mut Vec<&'a BuildProfile>,
) -> Result<(), ConfigError> {
    if stack.contains(&name) {
        return Err(ConfigError::invalid(
            "--profile",
            format!("inheritance cycle: {} -> {name}", stack.join(" -> ")),
        ));
    }
    if !done.insert(name) {
        return Ok(());
    }
    let profile = profiles.get(name).ok_or_else(|| {
        let valid: Vec<&str> = profiles.keys().map(String::as_str).collect();
        ConfigError::invalid(
            "--profile",
            format!("unknown profile `{name}` (defined: {})", valid.join(", ")),
        )
    })?;
    stack.push(name);
    for parent in &profile.inherits {
        collect(parent, profiles, stack, done, out)?;
    }
    stack.pop();
    out.push(profile);
    Ok(())
}

/// A path as it should appear in an argument: relative to the root when inside it.
fn arg_path(root: &Path, p: &Path) -> String {
    let rel = p.strip_prefix(root).unwrap_or(p);
    if rel.as_os_str().is_empty() {
        ".".into()
    } else {
        path_arg(rel)
    }
}

pub fn resolve(project: &Project, profile: &str) -> Result<Resolved, ConfigError> {
    let layered = layer(&project.manifest, profile)?;
    let flags = &project.manifest.build.flags;
    let msvc = project.uses_msvc();

    let mut common = Vec::new();
    if msvc {
        // Match CMake's default MSVC runtime selection for dependency builds:
        // Debug uses the debug DLL runtime, other profiles use the release DLL.
        common.push(if profile == "debug" { "/MDd" } else { "/MD" }.to_owned());
    }
    if flags.warnings == Some(true) {
        if msvc {
            common.push("/W4".to_string());
        } else {
            common.push("-Wall".to_string());
            common.push("-Wextra".to_string());
        }
    }
    if flags.pedantic == Some(true) {
        common.push(if msvc { "/permissive-" } else { "-Wpedantic" }.to_string());
    }
    if flags.error_on_warning == Some(true) {
        common.push(if msvc { "/WX" } else { "-Werror" }.to_string());
    }
    if let Some(o) = layered.opt_level {
        common.push(if msvc { msvc_opt_flag(o) } else { o.flag() }.to_string());
        if msvc && o == OptLevel::Ofast {
            common.push("/fp:fast".into());
        }
    }
    if layered.debug_info {
        common.push(if msvc { "/Zi" } else { "-g" }.to_string());
    }
    common.extend(layered.defines.iter().map(|d| {
        if msvc {
            format!(
                "/D{}{}",
                d.name,
                d.value.as_ref().map_or(String::new(), |v| format!("={v}"))
            )
        } else {
            d.to_string()
        }
    }));
    for dir in &project.include_dirs {
        common.push(format!(
            "{}{}",
            if msvc { "/I" } else { "-I" },
            arg_path(&project.root, dir)
        ));
    }
    common.extend(layered.compile_flags);

    let mut link = layered.link_flags;
    if msvc && layered.debug_info {
        link.push("/DEBUG".into());
    }
    for dir in &project.lib_dirs {
        link.push(if msvc {
            format!("/LIBPATH:{}", arg_path(&project.root, dir))
        } else {
            format!("-L{}", arg_path(&project.root, dir))
        });
    }
    for lib in &flags.libs {
        if msvc {
            link.push(
                if lib.contains('/')
                    || lib.contains('\\')
                    || lib.to_ascii_lowercase().ends_with(".lib")
                {
                    lib.clone()
                } else {
                    format!("{lib}.lib")
                },
            );
        } else {
            link.push(format!("-l{lib}"));
        }
    }

    Ok(Resolved {
        c: project.c.clone(),
        cpp: project.cpp.clone(),
        common_args: common,
        link_args: link,
    })
}

fn standard_flag(standard: &str, msvc: bool) -> String {
    if !msvc {
        return format!("-std={standard}");
    }
    let standard = standard
        .strip_prefix("gnu++")
        .map(|version| format!("c++{version}"))
        .or_else(|| {
            standard
                .strip_prefix("gnu")
                .map(|version| format!("c{version}"))
        })
        .unwrap_or_else(|| standard.to_string());
    format!("/std:{standard}")
}

fn msvc_opt_flag(level: OptLevel) -> &'static str {
    match level {
        OptLevel::O0 | OptLevel::Og => "/Od",
        OptLevel::O1 | OptLevel::Os | OptLevel::Oz => "/O1",
        OptLevel::O2 | OptLevel::O3 => "/O2",
        OptLevel::Ofast => "/O2",
    }
}

// ------------------------------------------------------------ validation

/// Raw flags are passed to the compiler verbatim, one argument per entry, so
/// they are checked when the manifest loads: no empty or whitespace-containing
/// entries (there is no shell to split them), and nothing Forge controls itself.
fn check_flag(field: &str, flag: &str) -> Result<(), ConfigError> {
    if flag.is_empty() {
        return Err(ConfigError::invalid(field, "empty entry"));
    }
    if flag.chars().any(char::is_whitespace) {
        return Err(ConfigError::invalid(
            field,
            format!(
                "`{flag}` contains whitespace; every entry is exactly one argument, so split it into separate entries"
            ),
        ));
    }
    if flag.starts_with("-M") || flag.starts_with("-o") || matches!(flag, "-c" | "-E" | "-S") {
        return Err(ConfigError::invalid(
            field,
            format!(
                "`{flag}` is controlled by Forge (output, dependency files, compile-only) and cannot be set here"
            ),
        ));
    }
    Ok(())
}

pub fn validate_flags(manifest: &Manifest) -> Result<(), ConfigError> {
    let flags = &manifest.build.flags;

    let mut seen = HashSet::new();
    for d in &flags.defines {
        if !seen.insert(&d.name) {
            return Err(ConfigError::invalid(
                "build.flags.defines",
                format!("`{}` is defined more than once", d.name),
            ));
        }
    }
    for lib in &flags.libs {
        if lib.is_empty() || lib.chars().any(char::is_whitespace) {
            return Err(ConfigError::invalid(
                "build.flags.libs",
                format!("`{lib}` is not a library name"),
            ));
        }
    }

    for (name, p) in &manifest.profiles {
        for f in p.compile_flags.entries() {
            check_flag(&format!("profile.{name}.compile_flags"), f)?;
        }
        for f in p.link_flags.entries() {
            check_flag(&format!("profile.{name}.link_flags"), f)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(profiles: &str) -> Manifest {
        let text = format!(
            r#"
[project]
name = "t"
version = "0.1.0"

[build]
compiler = "clang++"
standard = "c++20"
source_dirs = ["src"]
output = "app"
default_profile = "debug"

[build.flags]
defines = ["USE_FOO", "VERSION=3"]

{profiles}
"#
        );
        toml::from_str(&text).unwrap()
    }

    fn shown(l: &Layered) -> Vec<String> {
        l.defines.iter().map(|d| d.to_string()).collect()
    }

    const PROFILES: &str = r#"
[profile.debug]
opt_level = 0
debug_info = true

[profile.release]
opt_level = 2
defines = { add = ["NDEBUG", "VERSION=4"], remove = ["USE_FOO"] }

[profile.debug_release]
inherits = ["debug", "release"]

[profile.asan]
inherits = ["debug"]
compile_flags = { add = ["-fsanitize=address", "-fno-omit-frame-pointer"] }
link_flags = { add = ["-fsanitize=address"] }
"#;

    #[test]
    fn base_only_profile_keeps_base_defines() {
        let l = layer(&manifest(PROFILES), "debug").unwrap();
        assert_eq!(l.opt_level, Some(OptLevel::O0));
        assert!(l.debug_info);
        assert_eq!(shown(&l), ["-DUSE_FOO", "-DVERSION=3"]);
    }

    #[test]
    fn profile_replaces_removes_and_adds_defines() {
        let l = layer(&manifest(PROFILES), "release").unwrap();
        assert_eq!(l.opt_level, Some(OptLevel::O2));
        assert!(!l.debug_info);
        assert_eq!(shown(&l), ["-DNDEBUG", "-DVERSION=4"]); // one VERSION, USE_FOO gone
    }

    #[test]
    fn later_parent_wins_and_profile_comes_last() {
        let l = layer(&manifest(PROFILES), "debug_release").unwrap();
        assert_eq!(l.opt_level, Some(OptLevel::O2)); // release overrides debug
        assert!(l.debug_info); // release never set it, debug did
        assert_eq!(shown(&l), ["-DNDEBUG", "-DVERSION=4"]);
    }

    #[test]
    fn inherited_flags_accumulate() {
        let l = layer(&manifest(PROFILES), "asan").unwrap();
        assert_eq!(
            l.compile_flags,
            ["-fsanitize=address", "-fno-omit-frame-pointer"]
        );
        assert_eq!(l.link_flags, ["-fsanitize=address"]);
        assert_eq!(l.opt_level, Some(OptLevel::O0));
    }

    #[test]
    fn child_can_remove_an_inherited_flag_and_add_another() {
        let m = manifest(
            r#"
[profile.debug]
compile_flags = ["-O2", "-Wshadow"]
[profile.tuned]
inherits = ["debug"]
compile_flags = { remove = ["-O2"], add = ["-O3"] }
"#,
        );
        assert_eq!(
            layer(&m, "tuned").unwrap().compile_flags,
            ["-Wshadow", "-O3"]
        );
    }

    #[test]
    fn set_discards_inherited_entries() {
        let m = manifest(
            r#"
[profile.debug]
defines = ["A", "B"]
[profile.minimal]
inherits = ["debug"]
defines = { set = ["ONLY"] }
"#,
        );
        assert_eq!(shown(&layer(&m, "minimal").unwrap()), ["-DONLY"]);
    }

    #[test]
    fn diamond_parent_is_applied_once() {
        let m = manifest(
            r#"
[profile.base]
compile_flags = ["-Wbase"]
[profile.a]
inherits = ["base"]
[profile.b]
inherits = ["base"]
[profile.top]
inherits = ["a", "b"]
"#,
        );
        assert_eq!(layer(&m, "top").unwrap().compile_flags, ["-Wbase"]);
    }

    #[test]
    fn unknown_profile_lists_the_valid_ones() {
        let e = layer(&manifest(PROFILES), "nope").unwrap_err().to_string();
        assert!(e.contains("nope") && e.contains("release"), "{e}");
    }

    #[test]
    fn cycles_are_reported_not_looped() {
        let m = manifest("[profile.a]\ninherits = [\"b\"]\n[profile.b]\ninherits = [\"a\"]\n");
        assert!(layer(&m, "a").unwrap_err().to_string().contains("cycle"));
    }

    fn flag_error(profiles: &str) -> String {
        validate_flags(&manifest(profiles)).unwrap_err().to_string()
    }

    #[test]
    fn rejects_bad_raw_flags() {
        let e = flag_error("[profile.x]\ncompile_flags = [\"-O2 -g\"]");
        assert!(
            e.contains("profile.x.compile_flags") && e.contains("whitespace"),
            "{e}"
        );
        assert!(flag_error("[profile.x]\ncompile_flags = [\"\"]").contains("empty"));
        for bad in ["-c", "-o", "-ofoo", "-MMD", "-MF", "-MT", "-E", "-S"] {
            let e = flag_error(&format!("[profile.x]\nlink_flags = [\"{bad}\"]"));
            assert!(e.contains("controlled by Forge"), "{bad}: {e}");
        }
    }

    #[test]
    fn flags_inside_edit_tables_are_checked_too() {
        let e = flag_error("[profile.x]\ncompile_flags = { remove = [\"-MMD\"] }");
        assert!(e.contains("controlled by Forge"), "{e}");
    }

    #[test]
    fn accepts_ordinary_flags() {
        let m = manifest(
            "[profile.x]\ncompile_flags = [\"-fsanitize=address\", \"-Wno-unused\", \"-march=native\"]",
        );
        assert!(validate_flags(&m).is_ok());
    }

    #[test]
    fn rejects_duplicate_base_defines() {
        let text = r#"
[project]
name = "t"
version = "0.1.0"
[build]
compiler = "g++"
standard = "c++20"
source_dirs = ["src"]
output = "app"
default_profile = "debug"
[build.flags]
defines = ["A=1", "A=2"]
"#;
        let m: Manifest = toml::from_str(text).unwrap();
        assert!(
            validate_flags(&m)
                .unwrap_err()
                .to_string()
                .contains("more than once")
        );
    }
}
