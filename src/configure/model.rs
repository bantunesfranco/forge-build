//! The per-unit configuration model (spec 4.1, 8, 10) and its propagation.
//!
//! `configure()` (in `mod.rs`) turns a manifest into one [`Unit`] per lib, bin
//! and dependency. Everything here is typed and platform-resolved; later
//! stages (flags, graph, explain) read these types and never the manifest.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use crate::config::{
    Diagnostics,
    key::Visibility,
    schema::DepSource,
    types::{Define, DepKind, Opt, Subsystem, Warnings},
    vocab::Platform,
};
use crate::configure::{
    lookup::Alias, merge::Merged, options::ResolvedOption, system_libs::SystemLib,
};

/// What the command line asked for.
#[derive(Debug, Clone, Default)]
pub struct Invocation {
    pub profile: Option<String>,
    /// `--opt name=value` pairs.
    pub options: Vec<(String, String)>,
    /// `--target <arch>-<os>`.
    pub target: Option<String>,
    /// `--toolchain <file>`.
    pub toolchain: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeDir {
    /// Relative to the project root when inside it, absolute otherwise.
    pub path: PathBuf,
    /// Compiled as a system include (`-isystem`, `-external:I`).
    pub system: bool,
}

/// The settings with visibility behind them: either a unit's private side or
/// its public (exported) side.
#[derive(Debug, Default, Clone)]
pub struct Settings {
    pub defines: Vec<Define>,
    pub include: Vec<IncludeDir>,
    pub flags: Vec<String>,
    pub link_flags: Vec<String>,
    pub system: Vec<SystemLib>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileScalars {
    pub opt: Opt,
    pub debug_info: bool,
    pub asserts: bool,
    pub lto: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    /// A static library with sources.
    Lib,
    /// A `lib` with `include` and no `sources`: headers only, no archive.
    InterfaceLib,
    Bin,
    Dep(DepKind),
}

/// What only a dependency has.
#[derive(Debug, Clone)]
pub struct DepDetails {
    pub source: DepSource,
    /// `path` joined to the manifest directory, or for `git` the checkout
    /// directory `build/deps/<name>/source` (it need not exist yet: it is
    /// fetched when the dependency is prepared). `None` for `system`.
    pub source_dir: Option<PathBuf>,
    /// The `root` key: a subdirectory of `source_dir` that is the base of
    /// `files`, `include` and `lib_dir`.
    pub base_dir_rel: Option<PathBuf>,
    pub alias: Option<Alias>,
    pub find: Option<String>,
    pub system_headers: bool,
    /// `expose.target` names.
    pub expose: Vec<String>,
    /// CMake cache variables: `true` is `ON`, `false` is `OFF`.
    pub cmake_options: BTreeMap<String, String>,
    /// The `files` patterns as written (relative to the base directory).
    pub files: Vec<String>,
    pub libs: Vec<String>,
    /// Expanded (lookups and alias applied) but not joined to a directory.
    pub lib_dir: Option<String>,
}

#[derive(Debug)]
pub struct Unit {
    pub name: String,
    pub kind: UnitKind,
    pub private: Settings,
    pub public: Settings,
    /// Dependency names with the visibility they were listed with.
    pub deps: Vec<(String, Visibility)>,
    /// The directory `include` and `sources` are relative to. Empty for a
    /// `system` dependency.
    pub base_dir: PathBuf,
    /// Relative to the project root, `/`-separated. Empty for dependencies
    /// (their files are resolved when they are prepared).
    pub sources: Vec<PathBuf>,
    pub warnings: Option<Warnings>,
    pub warnings_as_errors: bool,
    /// The resolved `opt` of a lib or bin (target over profile over workspace).
    /// `None` for a dependency (it follows the profile) and for an interface
    /// lib (its headers follow the consumer).
    pub opt: Option<Opt>,
    /// Output name; the unit name unless `output` is set.
    pub output: String,
    pub subsystem: Option<Subsystem>,
    pub dep: Option<DepDetails>,
    /// The merged keys with provenance, for `explain`.
    pub merged: Merged,
}

#[derive(Debug)]
pub struct Configured {
    pub platform: Platform,
    /// The LLVM triple of the target platform, set only when `--target` names
    /// a platform other than the host's.
    pub cross_triple: Option<String>,
    pub profile: String,
    pub profile_scalars: ProfileScalars,
    /// The profile's `defines` and `flags` (both visibilities); these apply to
    /// dependency compiles (spec 8.1).
    pub profile_settings: Settings,
    pub options: BTreeMap<String, ResolvedOption>,
    pub c_std: Option<String>,
    pub cxx_std: Option<String>,
    /// Libs and bins by name, then dependencies by name.
    pub units: Vec<Unit>,
    /// Every unit once, consumers before the units they depend on.
    pub link_order: Vec<usize>,
    pub diagnostics: Diagnostics,
}

/// What one unit sees: its own settings plus what its dependencies export.
#[derive(Debug, Default)]
pub struct Effective {
    /// `defines`, `include` and `flags` for compiling this unit: its own
    /// private and public side, then the public interface of every dependency
    /// (and of that dependency's public dependencies). `link_flags` and
    /// `system` are left empty; see `link_system` and `link_flags` below.
    pub compile: Settings,
    /// Static libraries to link (`Lib` and `sources` dependency units), through
    /// every dependency edge whatever its visibility, consumers first.
    pub link_units: Vec<usize>,
    /// System libraries of this unit and everything it reaches.
    pub link_system: Vec<SystemLib>,
    /// Raw link flags of this unit and everything it reaches.
    pub link_flags: Vec<String>,
}

/// Compares dotted version numbers part by part; missing parts count as 0.
pub fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let a: Vec<u64> = left.split('.').map(|v| v.parse().unwrap_or(0)).collect();
    let b: Vec<u64> = right.split('.').map(|v| v.parse().unwrap_or(0)).collect();
    for i in 0..a.len().max(b.len()) {
        match a
            .get(i)
            .copied()
            .unwrap_or(0)
            .cmp(&b.get(i).copied().unwrap_or(0))
        {
            std::cmp::Ordering::Equal => continue,
            order => return order,
        }
    }
    std::cmp::Ordering::Equal
}

// ---------------------------------------------------------------- propagation

fn name_index(c: &Configured) -> HashMap<&str, usize> {
    c.units
        .iter()
        .enumerate()
        .map(|(i, u)| (u.name.as_str(), i))
        .collect()
}

/// Dependency edges of `unit` as indices. Names were validated by `configure`.
fn edges(c: &Configured, index: &HashMap<&str, usize>, unit: usize) -> Vec<(usize, Visibility)> {
    c.units[unit]
        .deps
        .iter()
        .filter_map(|(n, v)| index.get(n.as_str()).map(|i| (*i, *v)))
        .collect()
}

fn push_unique<T: PartialEq + Clone>(into: &mut Vec<T>, items: &[T]) {
    for item in items {
        if !into.contains(item) {
            into.push(item.clone());
        }
    }
}

/// Appends the compile side of `from` to `into`, first occurrence winning.
fn append_compile(into: &mut Settings, from: &Settings) {
    push_unique(&mut into.defines, &from.defines);
    for inc in &from.include {
        if !into.include.iter().any(|i| i.path == inc.path) {
            into.include.push(inc.clone());
        }
    }
    push_unique(&mut into.flags, &from.flags);
}

/// Adds the public interface of `unit` and of its public dependencies, transitively.
fn export_into(
    c: &Configured,
    index: &HashMap<&str, usize>,
    unit: usize,
    seen: &mut HashSet<usize>,
    into: &mut Settings,
) {
    if !seen.insert(unit) {
        return;
    }
    append_compile(into, &c.units[unit].public);
    for (dep, vis) in edges(c, index, unit) {
        if vis == Visibility::Public {
            export_into(c, index, dep, seen, into);
        }
    }
}

/// Every unit reachable from `unit` through any dependency edge (not `unit`
/// itself), in `link_order`.
pub fn reachable(c: &Configured, unit: usize) -> Vec<usize> {
    let index = name_index(c);
    let mut seen = HashSet::new();
    let mut stack = vec![unit];
    while let Some(u) = stack.pop() {
        for (dep, _) in edges(c, &index, u) {
            if seen.insert(dep) {
                stack.push(dep);
            }
        }
    }
    seen.remove(&unit);
    c.link_order
        .iter()
        .copied()
        .filter(|u| seen.contains(u))
        .collect()
}

pub fn effective(c: &Configured, unit: usize) -> Effective {
    let index = name_index(c);
    let own = &c.units[unit];

    let mut compile = Settings::default();
    append_compile(&mut compile, &own.private);
    append_compile(&mut compile, &own.public);
    let mut seen = HashSet::new();
    for (dep, _) in edges(c, &index, unit) {
        export_into(c, &index, dep, &mut seen, &mut compile);
    }

    let reach = reachable(c, unit);
    let link_units = reach
        .iter()
        .copied()
        .filter(|u| {
            matches!(
                c.units[*u].kind,
                UnitKind::Lib | UnitKind::Dep(DepKind::Sources)
            )
        })
        .collect();

    let mut link_system = Vec::new();
    let mut link_flags = Vec::new();
    for u in std::iter::once(unit).chain(reach) {
        let unit = &c.units[u];
        for side in [&unit.private, &unit.public] {
            push_unique(&mut link_system, &side.system);
            push_unique(&mut link_flags, &side.link_flags);
        }
    }

    Effective {
        compile,
        link_units,
        link_system,
        link_flags,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ConfigError, Level,
        flat::parse_flat,
        schema::parse_manifest,
        vocab::{Arch, Compiler, Os},
    };
    use crate::configure::configure;
    use Visibility::{Private, Public};

    const LINUX: Platform = Platform {
        os: Os::Linux,
        arch: Arch::X86_64,
        compiler: Compiler::Gcc,
    };
    const WINDOWS: Platform = Platform {
        os: Os::Windows,
        arch: Arch::X86_64,
        compiler: Compiler::Msvc,
    };
    const MACOS: Platform = Platform {
        os: Os::Macos,
        arch: Arch::Arm64,
        compiler: Compiler::AppleClang,
    };

    /// Writes `files` (empty; a trailing `/` makes a directory) into a
    /// tempdir, parses `src`, and configures.
    fn run(
        src: &str,
        files: &[&str],
        platform: Platform,
        inv: &Invocation,
    ) -> Result<Configured, ConfigError> {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        for f in files {
            let p = root.join(f);
            if f.ends_with('/') {
                std::fs::create_dir_all(&p).unwrap();
            } else {
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, "").unwrap();
            }
        }
        let m = parse_manifest(parse_flat(src).unwrap()).unwrap();
        configure(&m, &root, inv, platform, &BTreeMap::new())
    }

    fn cfg(src: &str, files: &[&str], platform: Platform) -> Configured {
        run(src, files, platform, &Invocation::default()).unwrap()
    }

    fn err(src: &str, files: &[&str], platform: Platform) -> String {
        run(src, files, platform, &Invocation::default())
            .unwrap_err()
            .to_string()
    }

    fn idx(c: &Configured, name: &str) -> usize {
        c.units
            .iter()
            .position(|u| u.name == name)
            .unwrap_or_else(|| panic!("no unit `{name}`"))
    }

    fn unit<'a>(c: &'a Configured, name: &str) -> &'a Unit {
        &c.units[idx(c, name)]
    }

    fn has_define(s: &Settings, name: &str) -> bool {
        s.defines.iter().any(|d| d.name == name)
    }

    fn warnings(c: &Configured) -> Vec<&str> {
        c.diagnostics
            .items
            .iter()
            .filter(|(l, _)| *l == Level::Warning)
            .map(|(_, m)| m.as_str())
            .collect()
    }

    fn lib(name: &str) -> SystemLib {
        SystemLib::Lib(name.into())
    }

    const SPEC: &str = include_str!("../../tests/fixtures/spec_example.toml");
    const SPEC_FILES: &[&str] = &[
        "engine/srcs/a.cpp",
        "server/srcs/n.cpp",
        "srcs/main.cpp",
        "engine/vendor/glad/src/glad.c",
        "engine/vendor/glad/include/",
        "vendor/foo/",
        "vendor/bar/",
    ];

    #[test]
    fn spec_example_configures_on_three_platforms() {
        for (platform, os_define, system_libs) in [
            (WINDOWS, "NOMINMAX", vec![lib("opengl32"), lib("ws2_32")]),
            (
                LINUX,
                "GLM_FORCE_AVX2",
                vec![lib("GL"), lib("pthread"), lib("dl")],
            ),
            (
                MACOS,
                "GL_SILENCE_DEPRECATION",
                vec![SystemLib::Framework("OpenGL".into())],
            ),
        ] {
            let c = cfg(SPEC, SPEC_FILES, platform);
            let name = platform.triplet();
            // 3 targets and 7 dependencies, each once.
            assert_eq!(c.units.len(), 10, "{name}");
            assert_eq!(c.link_order.len(), 10);
            assert!(c.diagnostics.items.is_empty(), "{:?}", c.diagnostics);
            assert_eq!(c.c_std.as_deref(), Some("11"));
            assert_eq!(c.cxx_std.as_deref(), Some("20"));

            let main = idx(&c, "ft_vox");
            let e = effective(&c, main);
            assert!(
                has_define(&e.compile, os_define),
                "{name}: {:?}",
                e.compile.defines
            );
            for l in &system_libs {
                assert!(e.link_system.contains(l), "{name}: {l:?}");
            }
            assert!(unit(&c, "ft_vox").warnings_as_errors);
            // Consumers come first: the bin, then engine, then glad.
            let pos = |n: &str| c.link_order.iter().position(|i| *i == idx(&c, n)).unwrap();
            assert!(pos("ft_vox") < pos("engine") && pos("engine") < pos("glad"));
            // glm's SIMD settings travel with it to every consumer.
            let glm_public = &unit(&c, "glm").public;
            if platform.arch == Arch::X86_64 {
                assert!(has_define(glm_public, "GLM_FORCE_AVX2"));
            } else {
                assert!(glm_public.defines.is_empty());
            }
        }
    }

    #[test]
    fn spec_example_dependency_details() {
        let c = cfg(SPEC, SPEC_FILES, LINUX);
        let glfw = unit(&c, "glfw").dep.as_ref().unwrap();
        // A git dependency has its checkout directory before anything is fetched.
        assert!(
            glfw.source_dir
                .as_ref()
                .unwrap()
                .ends_with("build/deps/glfw/source")
        );
        assert_eq!(glfw.cmake_options["GLFW_BUILD_X11"], "ON");
        assert_eq!(glfw.cmake_options["GLFW_INSTALL"], "OFF");
        assert_eq!(glfw.expose, ["glfw"]);
        let foo = unit(&c, "foo").dep.as_ref().unwrap();
        assert_eq!(foo.lib_dir.as_deref(), Some("lib/x64-linux/debug"));
        assert_eq!(foo.libs, ["foo"]);
        assert!(foo.source_dir.is_some());
        let glad = unit(&c, "glad");
        assert_eq!(glad.dep.as_ref().unwrap().files, ["src/glad.c"]);
        assert!(glad.sources.is_empty());
        assert_eq!(
            glad.public.include,
            [IncludeDir {
                path: PathBuf::from("engine/vendor/glad/include"),
                system: true
            }]
        );
        // A target's own includes are not system includes.
        assert_eq!(
            unit(&c, "engine").public.include,
            [IncludeDir {
                path: PathBuf::from("engine/incs"),
                system: false
            }]
        );
        let on_mac = cfg(SPEC, SPEC_FILES, MACOS);
        let foo = unit(&on_mac, "foo").dep.as_ref().unwrap();
        assert_eq!(foo.lib_dir.as_deref(), Some("lib/aarch64-osx/debug"));
    }

    #[test]
    fn root_prefixes_globs_with_forward_slashes() {
        let c = cfg(
            "[lib.engine]\nroot = \"engine\"\nsources = [\"srcs/**/*.cpp\"]\n",
            &[
                "engine/srcs/a.cpp",
                "engine/srcs/sub/b.cpp",
                "engine/srcs/c.txt",
            ],
            WINDOWS,
        );
        assert_eq!(
            unit(&c, "engine").sources,
            [
                PathBuf::from("engine/srcs/a.cpp"),
                PathBuf::from("engine/srcs/sub/b.cpp")
            ]
        );
        assert_eq!(unit(&c, "engine").kind, UnitKind::Lib);
    }

    const STRING_OPTION: &str = "[option.s]\ntype = \"string\"\ndefault = \"-O2 -g\"\n";

    #[test]
    fn flags_checked_after_lookup_expansion() {
        let src =
            format!("{STRING_OPTION}[lib.a]\nsources = [\"a.c\"]\nflags = [\"${{option.s}}\"]\n");
        let e = err(&src, &["a.c"], LINUX);
        assert!(e.contains("whitespace") && e.contains("lib.a"), "{e}");
        assert!(e.contains("flags"), "{e}");
    }

    #[test]
    fn werror_lookup_becomes_bool() {
        let c = cfg(SPEC, SPEC_FILES, LINUX);
        assert!(unit(&c, "ft_vox").warnings_as_errors);
        // A wrong type names the key and the table.
        let src = format!(
            "{STRING_OPTION}[lib.a]\nsources = [\"a.c\"]\nwarnings_as_errors = \"${{option.s}}\"\n"
        );
        let e = err(&src, &["a.c"], LINUX);
        assert!(
            e.contains("lib.a") && e.contains("warnings_as_errors") && e.contains("true or false"),
            "{e}"
        );
        // Enums and defines are typed the same way.
        let e = err(
            "[lib.a]\nsources = [\"a.c\"]\nopt = \"fast\"\n",
            &["a.c"],
            LINUX,
        );
        assert!(e.contains("lib.a") && e.contains("opt"), "{e}");
        let e = err(
            "[lib.a]\nsources = [\"a.c\"]\ndefines = [\"1BAD\"]\n",
            &["a.c"],
            LINUX,
        );
        assert!(e.contains("lib.a") && e.contains("defines"), "{e}");
    }

    #[test]
    fn literal_system_under_os_condition() {
        let src =
            "[lib.a]\nsources = [\"a.c\"]\nsystem.windows = [\"winmm\"]\nsystem = [\"threads\"]\n";
        let c = cfg(src, &["a.c"], WINDOWS);
        assert_eq!(unit(&c, "a").private.system, [lib("winmm")]);
        let c = cfg(src, &["a.c"], LINUX);
        assert_eq!(unit(&c, "a").private.system, [lib("pthread")]);
    }

    #[test]
    fn unknown_logical_system_name_is_an_error() {
        let e = err(
            "[lib.a]\nsources = [\"a.c\"]\nsystem.public = [\"openGL\"]\n",
            &["a.c"],
            LINUX,
        );
        assert!(
            e.contains("unknown system library `openGL`")
                && e.contains("opengl")
                && e.contains("lib.a"),
            "{e}"
        );
    }

    #[test]
    fn logical_name_in_deps_hints_system() {
        let e = err(
            "[lib.a]\nsources = [\"a.c\"]\ndeps = [\"threads\"]\n",
            &["a.c"],
            LINUX,
        );
        assert!(
            e.contains("`threads` is a system library") && e.contains("system"),
            "{e}"
        );
    }

    #[test]
    fn unknown_dep_and_cycle() {
        let e = err(
            "[lib.a]\nsources = [\"a.c\"]\ndeps = [\"nope\"]\n",
            &["a.c"],
            LINUX,
        );
        assert!(e.contains("lib.a") && e.contains("`nope`"), "{e}");

        let e = err(
            "[lib.a]\nsources = [\"a.c\"]\ndeps = [\"b\"]\n[lib.b]\nsources = [\"a.c\"]\ndeps.public = [\"c\"]\n[lib.c]\nsources = [\"a.c\"]\ndeps = [\"a\"]\n",
            &["a.c"],
            LINUX,
        );
        assert!(e.contains("a -> b -> c -> a"), "{e}");

        let e = err(
            "[lib.a]\nsources = [\"a.c\"]\ndeps = [\"a\"]\n",
            &["a.c"],
            LINUX,
        );
        assert!(e.contains("a -> a"), "{e}");

        let e = err(
            "[lib.a]\nsources = [\"a.c\"]\ndeps = [\"m\"]\n[bin.m]\nsources = [\"a.c\"]\n",
            &["a.c"],
            LINUX,
        );
        assert!(e.contains("cannot depend on an executable `m`"), "{e}");
    }

    #[test]
    fn remove_can_drop_a_value_a_feature_adds() {
        let src = "[workspace]
features = [\"f\"]
[feature.f]
defines = [\"A\", \"B\"]
[bin.app]
sources = [\"a.c\"]
remove.defines = [\"A\"]
";
        let c = cfg(src, &["a.c"], LINUX);
        assert!(!has_define(&unit(&c, "app").private, "A"));
        assert!(has_define(&unit(&c, "app").private, "B"));
    }

    #[test]
    fn profile_remove_drops_a_workspace_value() {
        let src = "[workspace]
defines = [\"LOG\", \"KEEP\"]
[profile.release]
remove.defines = [\"LOG\"]
[bin.app]
sources = [\"a.c\"]
[dependency.d]
path = \"d\"
kind = \"header-only\"
";
        let inv = Invocation {
            profile: Some("release".into()),
            ..Invocation::default()
        };
        let c = run(src, &["a.c", "d/"], LINUX, &inv).unwrap();
        let d = &unit(&c, "app").private;
        assert!(!has_define(d, "LOG") && has_define(d, "KEEP"));
        // Dependencies never see workspace values, so the removal has nothing to do there.
        assert!(!has_define(&c.profile_settings, "LOG"));
    }

    #[test]
    fn target_root_applies_to_its_own_paths_only() {
        let src = "[workspace]
include.public = [\"include\"]
sources = [\"common/*.cpp\"]
[lib.engine]
root = \"engine\"
sources = [\"srcs/*.cpp\"]
include.public = [\"inc\"]
";
        let c = cfg(
            src,
            &["engine/srcs/a.cpp", "common/w.cpp", "engine/common/no.cpp"],
            LINUX,
        );
        let e = unit(&c, "engine");
        let includes: Vec<_> = e.public.include.iter().map(|i| i.path.clone()).collect();
        assert_eq!(
            includes,
            [PathBuf::from("include"), PathBuf::from("engine/inc")]
        );
        assert_eq!(
            e.sources,
            [
                PathBuf::from("common/w.cpp"),
                PathBuf::from("engine/srcs/a.cpp")
            ]
        );
    }

    #[test]
    fn feature_and_profile_paths_follow_the_manifest_dir_but_a_dependencys_own_base() {
        let src = "[workspace]
features = [\"f\"]
[feature.f]
include.public = [\"fi\"]
[profile.debug]
include = [\"pi\"]
[lib.engine]
root = \"engine\"
sources = [\"a.cpp\"]
[dependency.d]
path = \"vendor/d\"
kind = \"header-only\"
root = \"sub\"
features = [\"f\"]
include.public = [\"di\"]
";
        let c = cfg(src, &["engine/a.cpp", "vendor/d/sub/"], LINUX);
        let paths =
            |s: &Settings| -> Vec<PathBuf> { s.include.iter().map(|i| i.path.clone()).collect() };
        let e = unit(&c, "engine");
        assert_eq!(paths(&e.public), [PathBuf::from("fi")]);
        assert_eq!(paths(&e.private), [PathBuf::from("pi")]);
        // The workspace's `features` list does not reach the dependency, which lists its own.
        let d = unit(&c, "d");
        assert_eq!(
            paths(&d.public),
            [
                PathBuf::from("vendor/d/sub/di"),
                PathBuf::from("vendor/d/sub/fi")
            ]
        );
    }

    #[test]
    fn inherited_include_does_not_make_a_lib_header_only() {
        let src = "[workspace]
include = [\"inc\"]
[lib.hdr]
defines = [\"X\"]
";
        let e = err(src, &[], LINUX);
        assert!(
            e.contains("lib.hdr") && e.contains("needs `sources`"),
            "{e}"
        );
    }

    #[test]
    fn include_under_any_condition_makes_a_lib_header_only() {
        let src = "[lib.hdr]
include.public.windows = [\"inc\"]
";
        let c = cfg(src, &[], LINUX);
        assert_eq!(unit(&c, "hdr").kind, UnitKind::InterfaceLib);
    }

    #[test]
    fn output_must_be_a_single_file_name() {
        for bad in ["", "a/b", "a\\\\b", "..", ".", "../../x", "/abs"] {
            let src = format!(
                "[bin.m]
sources = [\"a.c\"]
output = \"{bad}\"
"
            );
            let e = err(&src, &["a.c"], LINUX);
            assert!(e.contains("bin.m") && e.contains("`output`"), "{bad}: {e}");
        }
        let c = cfg(
            "[bin.m]
sources = [\"a.c\"]
output = \"my.tool\"
",
            &["a.c"],
            LINUX,
        );
        assert_eq!(unit(&c, "m").output, "my.tool");
    }

    #[test]
    fn duplicate_outputs_of_the_same_kind_are_rejected() {
        let src = "[bin.a]
sources = [\"a.c\"]
output = \"tool\"
[bin.b]
sources = [\"a.c\"]
output = \"tool\"
";
        let e = err(src, &["a.c"], LINUX);
        assert!(
            e.contains("tool") && e.contains("bin.a") && e.contains("bin.b"),
            "{e}"
        );
        let src = "[lib.a]
sources = [\"a.c\"]
output = \"x\"
[lib.b]
sources = [\"a.c\"]
output = \"x\"
";
        let e = err(src, &["a.c"], LINUX);
        assert!(e.contains("lib.a") && e.contains("lib.b"), "{e}");
        // A lib and a bin land in different directories; an interface lib has no file.
        let src = "[lib.a]
sources = [\"a.c\"]
output = \"x\"
[bin.b]
sources = [\"a.c\"]
output = \"x\"
[lib.h]
include = [\"i\"]
output = \"x\"
";
        cfg(src, &["a.c"], LINUX);
        // `tool` and `Tool.exe` are the same file on Windows only.
        let src = "[bin.a]
sources = [\"a.c\"]
output = \"tool\"
[bin.b]
sources = [\"a.c\"]
output = \"Tool.exe\"
";
        cfg(src, &["a.c"], LINUX);
        let e = err(src, &["a.c"], WINDOWS);
        assert!(e.contains("bin.a") && e.contains("bin.b"), "{e}");
    }

    #[test]
    fn lib_without_sources_or_include_is_error() {
        let e = err("[lib.x]\ndefines = [\"A\"]\n", &[], LINUX);
        assert!(e.contains("lib.x") && e.contains("needs `sources`"), "{e}");
    }

    #[test]
    fn lib_with_only_include_is_interface() {
        let c = cfg(
            "[lib.h]\ninclude.public = [\"inc\"]\nopt = \"max\"\n[bin.m]\nsources = [\"m.c\"]\ndeps = [\"h\"]\n",
            &["m.c"],
            LINUX,
        );
        assert!(matches!(unit(&c, "h").kind, UnitKind::InterfaceLib));
        assert!(unit(&c, "h").sources.is_empty());
        // Its headers follow the consumer's opt.
        assert_eq!(unit(&c, "h").opt, None);
        let e = effective(&c, idx(&c, "m"));
        assert_eq!(e.compile.include[0].path, PathBuf::from("inc"));
        assert!(e.link_units.is_empty());
    }

    #[test]
    fn bin_without_sources_is_error() {
        let e = err("[bin.m]\ndefines = [\"A\"]\n", &[], LINUX);
        assert!(e.contains("bin.m") && e.contains("sources"), "{e}");
    }

    #[test]
    fn sources_matching_nothing_is_error() {
        let e = err(
            "[lib.a]\nsources = [\"srcs/**/*.cpp\"]\n",
            &["srcs/readme.txt"],
            LINUX,
        );
        assert!(e.contains("lib.a") && e.contains("srcs/**/*.cpp"), "{e}");
    }

    const CHAIN: &str = "
[lib.a]
sources = [\"a.c\"]
defines.public = [\"A\"]
defines = [\"a_priv\"]
system = [\"math\"]
link_flags = [\"-lfoo\"]
include.public = [\"a_inc\"]

[lib.b]
sources = [\"a.c\"]
deps.public = [\"a\"]
defines.public = [\"B\"]
system.public = [\"threads\"]

[bin.c]
sources = [\"a.c\"]
deps = [\"b\"]
";

    #[test]
    fn public_propagates_transitively_private_does_not() {
        let c = cfg(CHAIN, &["a.c"], LINUX);
        let on_c = effective(&c, idx(&c, "c"));
        assert!(has_define(&on_c.compile, "A"));
        assert!(has_define(&on_c.compile, "B"));
        assert!(!has_define(&on_c.compile, "a_priv"));
        assert_eq!(on_c.compile.include[0].path, PathBuf::from("a_inc"));
        // b sees a's public side and its own; a's private side stays in a.
        let on_b = effective(&c, idx(&c, "b"));
        assert!(has_define(&on_b.compile, "A") && !has_define(&on_b.compile, "a_priv"));
        let on_a = effective(&c, idx(&c, "a"));
        assert!(has_define(&on_a.compile, "a_priv") && !has_define(&on_a.compile, "B"));

        // A private edge hides the interface from the next consumer.
        let src = CHAIN.replace("deps.public = [\"a\"]", "deps = [\"a\"]");
        let c = cfg(&src, &["a.c"], LINUX);
        assert!(has_define(&effective(&c, idx(&c, "b")).compile, "A"));
        assert!(!has_define(&effective(&c, idx(&c, "c")).compile, "A"));
        assert!(has_define(&effective(&c, idx(&c, "c")).compile, "B"));
    }

    #[test]
    fn link_closure_ignores_visibility() {
        let src = CHAIN.replace("deps.public = [\"a\"]", "deps = [\"a\"]");
        let c = cfg(&src, &["a.c"], LINUX);
        let e = effective(&c, idx(&c, "c"));
        assert_eq!(e.link_units, [idx(&c, "b"), idx(&c, "a")]);
        // Link-side keys reach the bin whatever their visibility.
        assert_eq!(e.link_system, [lib("pthread"), lib("m")]);
        assert_eq!(e.link_flags, ["-lfoo"]);
    }

    #[test]
    fn link_units_skip_interface_header_only_and_prebuilt() {
        let src = "
[lib.h]
include.public = [\"inc\"]
system = [\"dl\"]
[dependency.hdr]
path = \"hdr\"
kind = \"header-only\"
[dependency.pre]
path = \"pre\"
kind = \"prebuilt\"
libs = [\"pre\"]
system = [\"math\"]
[dependency.src]
path = \"src\"
kind = \"sources\"
files = [\"a.c\"]
[bin.m]
sources = [\"m.c\"]
deps = [\"h\", \"hdr\", \"pre\", \"src\"]
";
        let c = cfg(src, &["m.c", "hdr/", "pre/", "src/"], LINUX);
        let m = idx(&c, "m");
        let e = effective(&c, m);
        assert_eq!(e.link_units, [idx(&c, "src")]);
        // Interface and prebuilt units still contribute their system libraries.
        assert_eq!(e.link_system.len(), 2);
        assert!(e.link_system.contains(&lib("dl")) && e.link_system.contains(&lib("m")));
        assert_eq!(reachable(&c, m).len(), 4);
    }

    #[test]
    fn workspace_public_define_on_every_target() {
        let src = "
[workspace]
defines.public.windows = [\"NOMINMAX\"]
[lib.a]
sources = [\"a.c\"]
[bin.m]
sources = [\"a.c\"]
[dependency.d]
path = \"d\"
kind = \"header-only\"
";
        let c = cfg(src, &["a.c", "d/"], WINDOWS);
        assert!(has_define(&unit(&c, "a").public, "NOMINMAX"));
        assert!(has_define(&unit(&c, "m").public, "NOMINMAX"));
        // Not the dependency: workspace values never apply there.
        assert!(!has_define(&unit(&c, "d").public, "NOMINMAX"));
        assert!(!has_define(&unit(&c, "d").private, "NOMINMAX"));
        let c = cfg(src, &["a.c", "d/"], LINUX);
        assert!(!has_define(&unit(&c, "a").public, "NOMINMAX"));
    }

    #[test]
    fn dependency_ignores_workspace_and_profile_warnings() {
        let src = "
[workspace]
warnings = \"high\"
warnings_as_errors = true
defines = [\"WS\"]
[profile.debug]
warnings = \"all\"
warnings_as_errors = true
defines = [\"PD\"]
flags = [\"-fno-omit-frame-pointer\"]
[lib.a]
sources = [\"a.c\"]
[dependency.d]
path = \"d\"
kind = \"sources\"
files = [\"x.c\"]
";
        let c = cfg(src, &["a.c", "d/"], LINUX);
        let a = unit(&c, "a");
        assert_eq!(a.warnings, Some(Warnings::All));
        assert!(a.warnings_as_errors);
        assert!(has_define(&a.private, "WS") && has_define(&a.private, "PD"));
        let d = unit(&c, "d");
        assert_eq!(d.warnings, None);
        assert!(!d.warnings_as_errors);
        assert!(d.private.defines.is_empty() && d.public.defines.is_empty());
        // The profile's defines and flags reach dependency builds via the profile settings.
        assert!(has_define(&c.profile_settings, "PD"));
        assert!(!has_define(&c.profile_settings, "WS"));
        assert_eq!(c.profile_settings.flags, ["-fno-omit-frame-pointer"]);
    }

    #[test]
    fn abi_flag_private_on_lib_warns() {
        let src = "[lib.a]\nsources = [\"a.c\"]\nflags = [\"-mavx2\"]\ndefines = [\"_GLIBCXX_USE_CXX11_ABI=0\"]\n";
        let c = cfg(src, &["a.c"], LINUX);
        let w = warnings(&c);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains("-mavx2") && w[0].contains("lib.a"), "{w:?}");
        assert!(
            w.iter().any(|m| m.contains("_GLIBCXX_USE_CXX11_ABI")),
            "{w:?}"
        );

        // Public is fine, and so is a bin or a non-ABI flag.
        let src = "[lib.a]\nsources = [\"a.c\"]\nflags.public = [\"-mavx2\"]\nflags = [\"-fPIC\"]\n[bin.m]\nsources = [\"a.c\"]\nflags = [\"-mavx2\"]\n";
        assert!(warnings(&cfg(src, &["a.c"], LINUX)).is_empty());
    }

    #[test]
    fn unused_alias_warns() {
        let used = "[dependency.f]\npath = \"f\"\nkind = \"prebuilt\"\nlib_dir = \"lib/${arch}\"\nalias = { arch = { x86_64 = \"x64\" } }\n";
        assert!(warnings(&cfg(used, &["f/"], LINUX)).is_empty());
        let unused = "[dependency.f]\npath = \"f\"\nkind = \"prebuilt\"\nlib_dir = \"lib\"\nalias = { arch = { x86_64 = \"x64\" } }\n";
        let c = cfg(unused, &["f/"], LINUX);
        let w = warnings(&c);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].contains("alias") && w[0].contains("dependency.f"),
            "{w:?}"
        );
    }

    #[test]
    fn dependency_diagnostics() {
        let src = "
[feature.x]
defines = [\"PRIV\"]
defines.public = [\"PUB\"]
[dependency.h]
path = \"h\"
kind = \"header-only\"
options = { A = true }
features = [\"x\"]
[dependency.s]
path = \"s\"
kind = \"sources\"
files = [\"a.c\"]
features = [\"x\"]
";
        let c = cfg(src, &["h/", "s/"], LINUX);
        let w = warnings(&c);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(
            w.iter()
                .any(|m| m.contains("dependency.h") && m.contains("options"))
        );
        assert!(w.iter().any(|m| m.contains("dependency.h")
            && m.contains("feature.x")
            && m.contains("private")));
        // A `sources` dependency applies its private part itself; public ones travel on.
        assert!(has_define(&unit(&c, "s").private, "PRIV"));
        assert!(has_define(&unit(&c, "h").public, "PUB"));
    }

    #[test]
    fn feature_provenance_uses_the_full_origin() {
        let c = cfg(SPEC, SPEC_FILES, LINUX);
        let glm = unit(&c, "glm");
        let define = &glm.merged.list("defines", Public)[0];
        assert_eq!(define.origin, "feature.simd.avx2");
        assert_eq!(
            glm.merged.list("flags", Public)[0].raw_key,
            "flags.public.gcc_like"
        );
        let e = err(
            "[lib.a]\nsources = [\"a.c\"]\nfeatures = [\"missing\"]\n",
            &["a.c"],
            LINUX,
        );
        assert!(e.contains("lib.a") && e.contains("`missing`"), "{e}");
    }

    #[test]
    fn builtin_profile_overlay() {
        let src = "[profile.debug]\nopt = \"speed\"\n[lib.a]\nsources = [\"a.c\"]\n";
        let c = cfg(src, &["a.c"], LINUX);
        assert_eq!(
            c.profile_scalars,
            ProfileScalars {
                opt: Opt::Speed,
                debug_info: true,
                asserts: true,
                lto: false
            }
        );
        assert_eq!(unit(&c, "a").opt, Some(Opt::Speed));
        // Without an overlay the built-in values apply, and a target's own opt wins.
        let src = "[lib.a]\nsources = [\"a.c\"]\nopt = \"size\"\n[lib.b]\nsources = [\"a.c\"]\n";
        let c = cfg(src, &["a.c"], LINUX);
        assert_eq!(c.profile_scalars.opt, Opt::None);
        assert_eq!(unit(&c, "a").opt, Some(Opt::Size));
        assert_eq!(unit(&c, "b").opt, Some(Opt::None));
    }

    #[test]
    fn selected_profile_and_asserts() {
        let src = "[lib.a]\nsources = [\"a.c\"]\n";
        let inv = Invocation {
            profile: Some("release".into()),
            ..Invocation::default()
        };
        let c = run(src, &["a.c"], LINUX, &inv).unwrap();
        assert_eq!(c.profile, "release");
        assert_eq!(
            c.profile_scalars,
            ProfileScalars {
                opt: Opt::Max,
                debug_info: false,
                asserts: false,
                lto: true
            }
        );
        // `asserts = false` is `NDEBUG`, for targets and dependency builds.
        assert!(has_define(&unit(&c, "a").private, "NDEBUG"));
        assert!(has_define(&c.profile_settings, "NDEBUG"));
        let n = unit(&c, "a").merged.list("defines", Private);
        assert_eq!(n[0].origin, "profile.release");
        // Debug keeps asserts, so there is no NDEBUG.
        let c = cfg(src, &["a.c"], LINUX);
        assert!(!has_define(&unit(&c, "a").private, "NDEBUG"));
    }

    #[test]
    fn unknown_profile_lists_defined() {
        let src = "[profile.fast]\nopt = \"max\"\n[lib.a]\nsources = [\"a.c\"]\n";
        let inv = Invocation {
            profile: Some("nope".into()),
            ..Invocation::default()
        };
        let e = run(src, &["a.c"], LINUX, &inv).unwrap_err().to_string();
        for name in ["nope", "debug", "release", "relwithdebinfo", "fast"] {
            assert!(e.contains(name), "{e}");
        }
        // A manifest-only profile is usable.
        let inv = Invocation {
            profile: Some("fast".into()),
            ..Invocation::default()
        };
        let c = run(src, &["a.c"], LINUX, &inv).unwrap();
        assert_eq!(c.profile_scalars.opt, Opt::Max);
    }

    #[test]
    fn path_dependency_must_exist_and_git_has_a_checkout_dir() {
        let e = err(
            "[dependency.d]\npath = \"missing\"\nkind = \"header-only\"\n",
            &[],
            LINUX,
        );
        assert!(e.contains("dependency.d") && e.contains("missing"), "{e}");

        let src = "[dependency.g]\ngit = \"https://example.com/g\"\nrev = \"1\"\nkind = \"sources\"\nfiles = [\"src/*.c\"]\nroot = \"sub\"\ninclude.public = [\"inc\"]\n";
        let c = cfg(src, &[], LINUX);
        let g = unit(&c, "g");
        let d = g.dep.as_ref().unwrap();
        // Nothing is on disk yet, but the directories are already known.
        assert!(
            d.source_dir
                .as_ref()
                .unwrap()
                .ends_with("build/deps/g/source")
        );
        assert_eq!(d.base_dir_rel, Some(PathBuf::from("sub")));
        assert_eq!(d.files, ["src/*.c"]);
        assert!(g.sources.is_empty());
        assert!(g.base_dir.ends_with("build/deps/g/source/sub"));
        assert_eq!(
            g.public.include[0].path,
            PathBuf::from("build/deps/g/source/sub/inc")
        );
    }

    #[test]
    fn dependency_paths_are_relative_to_source_dir_and_root_key() {
        let src = "[dependency.d]\npath = \"vendor/d\"\nkind = \"sources\"\nroot = \"sub\"\nfiles = [\"a.c\"]\ninclude.public = [\".\"]\nsystem_headers = false\n";
        let c = cfg(src, &["vendor/d/sub/a.c"], LINUX);
        let d = unit(&c, "d");
        assert_eq!(d.public.include[0].path, PathBuf::from("vendor/d/sub"));
        assert!(!d.public.include[0].system);
        assert_eq!(
            d.dep.as_ref().unwrap().base_dir_rel,
            Some(PathBuf::from("sub"))
        );
    }

    #[test]
    fn cmake_options_convert_and_reject_other_types() {
        let src = "[dependency.c]\npath = \"c\"\nkind = \"cmake\"\noptions = { A = true, B = \"x\", C = 3 }\noptions.linux = { B = \"y\", D = false }\n";
        let c = cfg(src, &["c/"], LINUX);
        let o = &unit(&c, "c").dep.as_ref().unwrap().cmake_options;
        assert_eq!(o["A"], "ON");
        assert_eq!(o["B"], "y");
        assert_eq!(o["C"], "3");
        assert_eq!(o["D"], "OFF");
        let c = cfg(src, &["c/"], WINDOWS);
        assert!(
            !unit(&c, "c")
                .dep
                .as_ref()
                .unwrap()
                .cmake_options
                .contains_key("D")
        );
        let bad = "[dependency.c]\npath = \"c\"\nkind = \"cmake\"\noptions = { A = 1.5 }\n";
        let e = err(bad, &["c/"], LINUX);
        assert!(e.contains("dependency.c") && e.contains("`A`"), "{e}");
    }

    #[test]
    fn requires_tool_and_standards() {
        let ok = format!(
            "[workspace]\nrequires = {{ tool = \">={}\" }}\n",
            env!("CARGO_PKG_VERSION")
        );
        cfg(&ok, &[], LINUX);
        let e = err(
            "[workspace]\nrequires = { tool = \">=99.0\" }\n",
            &[],
            LINUX,
        );
        assert!(e.contains("requires.tool") && e.contains(">=99.0"), "{e}");
        let e = err("[workspace]\nrequires = { tool = \"^1\" }\n", &[], LINUX);
        assert!(e.contains(">="), "{e}");
        let e = err("[workspace]\nc = \"12\"\n", &[], LINUX);
        assert!(
            e.contains("workspace.c") && e.contains("89, 99, 11, 17, 23"),
            "{e}"
        );
        let e = err("[workspace]\ncxx = \"98\"\n", &[], LINUX);
        assert!(
            e.contains("workspace.cxx") && e.contains("11, 14, 17, 20, 23"),
            "{e}"
        );
    }

    #[test]
    fn output_and_subsystem() {
        let src = "[bin.m]\nsources = [\"a.c\"]\noutput = \"game\"\nsubsystem = \"windows\"\n[lib.a]\nsources = [\"a.c\"]\n";
        let c = cfg(src, &["a.c"], WINDOWS);
        assert_eq!(unit(&c, "m").output, "game");
        assert_eq!(unit(&c, "m").subsystem, Some(Subsystem::Windows));
        assert_eq!(unit(&c, "a").output, "a");
    }

    #[test]
    fn compare_versions_orders_numerically() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions("0.1.0", "0.1"), Equal);
        assert_eq!(compare_versions("0.10", "0.9"), Greater);
        assert_eq!(compare_versions("1.0", "1.0.1"), Less);
    }
}
