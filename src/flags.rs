//! Maps the abstract per-unit settings (spec 6.4, 8, 8.1, 10.4) to concrete
//! compiler, linker and archiver arguments.
//!
//! The argument order is fixed in one place so command hashes stay stable.

use std::path::PathBuf;

use crate::config::{
    ConfigError,
    types::{Define, DepKind, Opt, Subsystem, Warnings},
    vocab::{Compiler, Os},
};
use crate::configure::{
    model::{Configured, ProfileScalars, UnitKind, effective},
    system_libs::SystemLib,
};
use crate::executor::path_arg;
use crate::lang::Lang;

fn msvc_like(compiler: Compiler) -> bool {
    matches!(compiler, Compiler::Msvc | Compiler::ClangCl)
}

/// The language standard as a compiler flag. Never a GNU-extension dialect.
pub fn std_flag(compiler: Compiler, lang: Lang, std: &str) -> Result<String, String> {
    let known: &[&str] = match lang {
        Lang::C => &["89", "99", "11", "17", "23"],
        Lang::Cxx => &["11", "14", "17", "20", "23"],
    };
    if !known.contains(&std) {
        return Err(format!(
            "unsupported {} standard `{std}`; expected one of: {}",
            lang.name(),
            known.join(", ")
        ));
    }
    if !msvc_like(compiler) {
        return Ok(match lang {
            Lang::C => format!("-std=c{std}"),
            Lang::Cxx => format!("-std=c++{std}"),
        });
    }
    match (lang, std) {
        (Lang::C, "11" | "17") => Ok(format!("/std:c{std}")),
        (Lang::C, "23") => Ok("/std:clatest".to_string()),
        (Lang::C, _) => Err(format!(
            "msvc's minimum is c11, so C standard `{std}` is not available"
        )),
        (Lang::Cxx, "23") => Ok("/std:c++latest".to_string()),
        (Lang::Cxx, "14" | "17" | "20") => Ok(format!("/std:c++{std}")),
        (Lang::Cxx, _) => Err(format!(
            "msvc's minimum is c++14, so C++ standard `{std}` is not available"
        )),
    }
}

/// Spec 6.4 table. `None` (unset) emits nothing.
fn warning_args(msvc: bool, warnings: Option<Warnings>) -> &'static [&'static str] {
    match (warnings, msvc) {
        (None, _) => &[],
        (Some(Warnings::Off), true) => &["/W0"],
        (Some(Warnings::Off), false) => &["-w"],
        (Some(Warnings::Default), true) => &["/W3"],
        (Some(Warnings::Default), false) => &[],
        (Some(Warnings::High), true) => &["/W4"],
        (Some(Warnings::High), false) => &["-Wall", "-Wextra"],
        (Some(Warnings::All), true) => &["/Wall"],
        (Some(Warnings::All), false) => &["-Wall", "-Wextra", "-Wpedantic"],
    }
}

/// Spec 8 `opt` table.
fn opt_args(msvc: bool, opt: Opt) -> &'static [&'static str] {
    match (opt, msvc) {
        (Opt::None, true) => &["/Od"],
        (Opt::None, false) => &["-O0"],
        (Opt::Size, true) => &["/O1"],
        (Opt::Size, false) => &["-Os"],
        (Opt::Speed, true) => &["/O2"],
        (Opt::Speed, false) => &["-O2"],
        (Opt::Max, true) => &["/O2", "/Ob3"],
        (Opt::Max, false) => &["-O3"],
    }
}

fn debug_arg(msvc: bool) -> &'static str {
    if msvc { "/Zi" } else { "-g" }
}

fn lto_args(compiler: Compiler) -> &'static [&'static str] {
    match compiler {
        Compiler::Gcc => &["-flto", "-ffat-lto-objects"],
        Compiler::Clang | Compiler::AppleClang | Compiler::ClangCl => &["-flto=thin"],
        Compiler::Msvc => &["/GL"],
    }
}

/// The CMake build type that matches a profile (also selects the MSVC runtime).
pub fn cmake_build_type(profile: &str, s: &ProfileScalars) -> &'static str {
    match profile {
        "debug" => "Debug",
        "release" => "Release",
        "relwithdebinfo" => "RelWithDebInfo",
        _ if s.opt == Opt::None => "Debug",
        _ if s.debug_info => "RelWithDebInfo",
        _ => "Release",
    }
}

fn runtime_arg(c: &Configured) -> &'static str {
    if cmake_build_type(&c.profile, &c.profile_scalars) == "Debug" {
        "/MDd"
    } else {
        "/MD"
    }
}

fn define_arg(msvc: bool, d: &Define) -> String {
    let prefix = if msvc { "/D" } else { "-D" };
    match &d.value {
        None => format!("{prefix}{}", d.name),
        Some(v) => format!("{prefix}{}={v}", d.name),
    }
}

fn owned(args: &[&str]) -> impl Iterator<Item = String> {
    args.iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .into_iter()
}

fn push_unique<T: PartialEq>(into: &mut Vec<T>, items: impl IntoIterator<Item = T>) {
    for item in items {
        if !into.contains(&item) {
            into.push(item);
        }
    }
}

/// The profile's defines for a dependency build; `asserts = false` always
/// means exactly one `NDEBUG`.
fn profile_defines(c: &Configured) -> Vec<Define> {
    let mut defines = c.profile_settings.defines.clone();
    if !c.profile_scalars.asserts && !defines.iter().any(|d| d.name == "NDEBUG") {
        defines.push(Define {
            name: "NDEBUG".to_string(),
            value: None,
        });
    }
    defines
}

/// `--target=<triple>` for the compilers that choose their target by flag
/// (clang, Apple clang, clang-cl), when the build is for a platform other than
/// the host's. gcc and MSVC get their cross compilers from the toolchain file.
pub fn target_arg(c: &Configured) -> Option<String> {
    match c.platform.compiler {
        Compiler::Clang | Compiler::AppleClang | Compiler::ClangCl => {
            c.cross_triple.as_ref().map(|t| format!("--target={t}"))
        }
        _ => None,
    }
}

/// Compile arguments for a lib, a bin, or a `sources` dependency, in the one
/// fixed order that keeps command hashes stable.
pub fn compile_args(c: &Configured, unit: usize, lang: Lang) -> Result<Vec<String>, ConfigError> {
    let u = &c.units[unit];
    let own = match u.kind {
        UnitKind::Lib | UnitKind::Bin => true,
        UnitKind::Dep(DepKind::Sources) => false,
        UnitKind::InterfaceLib | UnitKind::Dep(_) => {
            return Err(ConfigError::invalid(
                &u.name,
                "internal error: this unit has no compile command (only libs, bins and `sources` dependencies are compiled)",
            ));
        }
    };
    let compiler = c.platform.compiler;
    let msvc = msvc_like(compiler);
    let eff = effective(c, unit);
    let scalars = &c.profile_scalars;
    let mut args: Vec<String> = Vec::new();
    args.extend(target_arg(c));

    let (key, std) = match lang {
        Lang::C => ("c", c.c_std.as_deref()),
        Lang::Cxx => ("cxx", c.cxx_std.as_deref()),
    };
    let std = std.ok_or_else(|| {
        ConfigError::invalid(
            key,
            format!(
                "no {} standard is set; add `{key} = \"<standard>\"` to [workspace]",
                lang.name()
            ),
        )
    })?;
    args.push(std_flag(compiler, lang, std).map_err(|e| ConfigError::invalid(key, e))?);
    if msvc && lang == Lang::Cxx {
        args.push("/permissive-".to_string());
        args.push("/EHsc".to_string());
    }
    if msvc {
        args.push(runtime_arg(c).to_string());
    }
    if own {
        args.extend(owned(warning_args(msvc, u.warnings)));
        if u.warnings_as_errors {
            args.push(if msvc { "/WX" } else { "-Werror" }.to_string());
        }
    }
    let opt = if own {
        u.opt.unwrap_or(scalars.opt)
    } else {
        scalars.opt
    };
    args.extend(owned(opt_args(msvc, opt)));
    if scalars.debug_info {
        args.push(debug_arg(msvc).to_string());
    }
    if scalars.lto {
        args.extend(owned(lto_args(compiler)));
    }

    let mut defines: Vec<Define> = if own { Vec::new() } else { profile_defines(c) };
    push_unique(&mut defines, eff.compile.defines.iter().cloned());
    args.extend(defines.iter().map(|d| define_arg(msvc, d)));

    let (system, plain): (Vec<_>, Vec<_>) = eff.compile.include.iter().partition(|i| i.system);
    for inc in plain {
        let dir = path_arg(&inc.path);
        args.push(if msvc {
            format!("/I{dir}")
        } else {
            format!("-I{dir}")
        });
    }
    for (n, inc) in system.into_iter().enumerate() {
        let dir = path_arg(&inc.path);
        if msvc {
            if n == 0 {
                args.push("/external:W0".to_string());
            }
            args.push(format!("/external:I{dir}"));
        } else {
            args.push("-isystem".to_string());
            args.push(dir);
        }
    }

    let mut flags: Vec<String> = if own {
        Vec::new()
    } else {
        c.profile_settings.flags.clone()
    };
    push_unique(&mut flags, eff.compile.flags.iter().cloned());
    args.extend(flags);
    Ok(args)
}

/// What follows the object files on the link line. For MSVC every argument is
/// a linker option (the caller puts `/link` in front of them).
pub fn link_args(
    c: &Configured,
    unit: usize,
    artifact: &dyn Fn(usize) -> PathBuf,
    external_libs: &[PathBuf],
) -> Vec<String> {
    let compiler = c.platform.compiler;
    let msvc = msvc_like(compiler);
    let eff = effective(c, unit);
    let u = &c.units[unit];
    let mut args: Vec<String> = Vec::new();
    // clang-cl's link line is all linker options (after `/link`), where the
    // triple does not belong: `build::command_for` puts it before `/link`.
    if !msvc {
        args.extend(target_arg(c));
    }

    for lib in &eff.link_units {
        args.push(path_arg(&artifact(*lib)));
    }
    for lib in external_libs {
        args.push(path_arg(lib));
    }
    for lib in &eff.link_system {
        match lib {
            SystemLib::Lib(n) if msvc => {
                if n.to_ascii_lowercase().ends_with(".lib") {
                    args.push(n.clone());
                } else {
                    args.push(format!("{n}.lib"));
                }
            }
            SystemLib::Lib(n) => args.push(format!("-l{n}")),
            // Frameworks exist only on macOS, which is never msvc-like.
            SystemLib::Framework(_) if msvc => {}
            SystemLib::Framework(f) => {
                args.push("-framework".to_string());
                args.push(f.clone());
            }
        }
    }
    args.extend(eff.link_flags.iter().cloned());
    if c.profile_scalars.lto {
        if msvc {
            // clang-cl links bitcode with lld, which needs no flag.
            if compiler == Compiler::Msvc {
                args.push("/LTCG".to_string());
            }
        } else {
            args.extend(owned(lto_args(compiler)));
            // clang's LTO on Windows cannot use the MSVC linker.
            if compiler == Compiler::Clang && c.platform.os == Os::Windows {
                args.push("-fuse-ld=lld".to_string());
            }
        }
    }
    if msvc && c.profile_scalars.debug_info {
        args.push("/DEBUG".to_string());
    }
    if c.platform.os == Os::Windows
        && u.kind == UnitKind::Bin
        && u.subsystem == Some(Subsystem::Windows)
    {
        if msvc {
            args.push("/SUBSYSTEM:WINDOWS".to_string());
            args.push("/ENTRY:mainCRTStartup".to_string());
        } else {
            args.push("-mwindows".to_string());
        }
    }
    args
}

/// The static-library archiver. An explicit override always wins.
pub fn archiver(
    compiler: Compiler,
    os: Os,
    lto: bool,
    ar_override: Option<&str>,
) -> Result<String, String> {
    if let Some(ar) = ar_override {
        return Ok(ar.to_string());
    }
    Ok(if compiler == Compiler::ClangCl && lto {
        // lib.exe cannot read the bitcode objects of thin LTO.
        "llvm-lib"
    } else if msvc_like(compiler) {
        "lib.exe"
    } else if compiler == Compiler::Clang && os == Os::Windows {
        // Windows has no `ar`.
        "llvm-ar"
    } else if compiler == Compiler::Clang && os != Os::Macos && lto {
        // Thin-LTO bitcode objects need an archiver that understands them.
        "llvm-ar"
    } else {
        "ar"
    }
    .to_string())
}

/// Profile-derived arguments for a dependency that is built by its own build
/// system (`cmake`): opt, debug info, lto, runtime, profile defines and flags.
/// No standard and no warnings (spec 8.1, 10.4).
pub fn profile_args(c: &Configured, _lang: Lang) -> Vec<String> {
    let compiler = c.platform.compiler;
    let msvc = msvc_like(compiler);
    let scalars = &c.profile_scalars;
    let mut args: Vec<String> = Vec::new();
    args.extend(target_arg(c));
    args.extend(owned(opt_args(msvc, scalars.opt)));
    if scalars.debug_info {
        args.push(debug_arg(msvc).to_string());
    }
    if scalars.lto {
        args.extend(owned(lto_args(compiler)));
    }
    if msvc {
        args.push(runtime_arg(c).to_string());
    }
    let mut defines = Vec::new();
    push_unique(&mut defines, profile_defines(c));
    args.extend(defines.iter().map(|d| define_arg(msvc, d)));
    let mut flags = Vec::new();
    push_unique(&mut flags, c.profile_settings.flags.iter().cloned());
    args.extend(flags);
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        flat::parse_flat,
        schema::parse_manifest,
        vocab::{Arch, Platform},
    };
    use crate::configure::{configure, model::Invocation};
    use std::collections::BTreeMap;

    fn plat(os: Os, compiler: Compiler) -> Platform {
        Platform {
            os,
            arch: Arch::X86_64,
            compiler,
        }
    }

    fn linux(compiler: Compiler) -> Platform {
        plat(Os::Linux, compiler)
    }

    fn windows(compiler: Compiler) -> Platform {
        plat(Os::Windows, compiler)
    }

    /// Writes `files` (empty; a trailing `/` makes a directory) into a tempdir,
    /// parses `src`, and configures.
    fn build(src: &str, files: &[&str], platform: Platform, profile: Option<&str>) -> Configured {
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
        let inv = Invocation {
            profile: profile.map(str::to_string),
            ..Invocation::default()
        };
        let m = parse_manifest(parse_flat(src).unwrap()).unwrap();
        configure(&m, &root, &inv, platform, &BTreeMap::new()).unwrap()
    }

    fn idx(c: &Configured, name: &str) -> usize {
        c.units.iter().position(|u| u.name == name).unwrap()
    }

    fn compile(c: &Configured, name: &str, lang: Lang) -> Vec<String> {
        compile_args(c, idx(c, name), lang).unwrap()
    }

    fn pos(args: &[String], what: &str) -> usize {
        args.iter()
            .position(|a| a == what)
            .unwrap_or_else(|| panic!("`{what}` not in {args:?}"))
    }

    fn count(args: &[String], what: &str) -> usize {
        args.iter().filter(|a| *a == what).count()
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// One lib with the given extra keys, and a `c` standard of 11, `cxx` of 20.
    fn warn_src(extra: &str) -> String {
        format!("[workspace]\nc = \"11\"\ncxx = \"20\"\n[lib.a]\nsources = [\"a.c\"]\n{extra}")
    }

    fn warn_args(compiler: Compiler, os: Os, extra: &str) -> Vec<String> {
        let c = build(&warn_src(extra), &["a.c"], plat(os, compiler), None);
        compile(&c, "a", Lang::Cxx)
    }

    #[test]
    fn warnings_table() {
        let w = |compiler, os, level: &str| {
            let a = warn_args(compiler, os, &format!("warnings = \"{level}\"\n"));
            a.into_iter()
                .filter(|x| x.starts_with("/W") || x.starts_with("-W") || x == "-w")
                .collect::<Vec<_>>()
        };
        let m = |l: &str| w(Compiler::Msvc, Os::Windows, l);
        let g = |l: &str| w(Compiler::Gcc, Os::Linux, l);
        assert_eq!(m("high"), ["/W4"]);
        assert_eq!(g("high"), ["-Wall", "-Wextra"]);
        assert_eq!(m("all"), ["/Wall"]);
        assert_eq!(
            w(Compiler::Clang, Os::Linux, "all"),
            ["-Wall", "-Wextra", "-Wpedantic"]
        );
        assert_eq!(m("default"), ["/W3"]);
        assert!(g("default").is_empty());
        assert_eq!(m("off"), ["/W0"]);
        assert_eq!(g("off"), ["-w"]);
        // Unset warnings emit nothing at all.
        let a = warn_args(Compiler::Msvc, Os::Windows, "");
        assert!(
            !a.iter().any(|x| x.starts_with("/W") && x != "/WX"),
            "{a:?}"
        );
        // warnings_as_errors
        let a = warn_args(Compiler::Msvc, Os::Windows, "warnings_as_errors = true\n");
        assert!(a.contains(&"/WX".to_string()));
        let a = warn_args(Compiler::Gcc, Os::Linux, "warnings_as_errors = true\n");
        assert!(a.contains(&"-Werror".to_string()));
    }

    #[test]
    fn opt_table() {
        let opt = |compiler, os, v: &str| {
            let a = warn_args(compiler, os, &format!("opt = \"{v}\"\n"));
            a.into_iter()
                .filter(|x| x.starts_with("-O") || x.starts_with("/O"))
                .collect::<Vec<_>>()
        };
        for (v, msvc, gnu) in [
            ("none", vec!["/Od"], "-O0"),
            ("size", vec!["/O1"], "-Os"),
            ("speed", vec!["/O2"], "-O2"),
            ("max", vec!["/O2", "/Ob3"], "-O3"),
        ] {
            assert_eq!(opt(Compiler::Msvc, Os::Windows, v), msvc, "{v}");
            assert_eq!(opt(Compiler::Gcc, Os::Linux, v), [gnu], "{v}");
            assert_eq!(opt(Compiler::Clang, Os::Linux, v), [gnu], "{v}");
        }
    }

    #[test]
    fn conformance_no_gnu_extensions() {
        assert_eq!(
            std_flag(Compiler::Gcc, Lang::Cxx, "20").unwrap(),
            "-std=c++20"
        );
        assert_eq!(
            std_flag(Compiler::Clang, Lang::C, "89").unwrap(),
            "-std=c89"
        );
        assert_eq!(std_flag(Compiler::Gcc, Lang::C, "23").unwrap(), "-std=c23");
        assert_eq!(
            std_flag(Compiler::Msvc, Lang::Cxx, "17").unwrap(),
            "/std:c++17"
        );
        assert_eq!(
            std_flag(Compiler::ClangCl, Lang::Cxx, "23").unwrap(),
            "/std:c++latest"
        );
        assert_eq!(std_flag(Compiler::Msvc, Lang::C, "11").unwrap(), "/std:c11");
        assert_eq!(std_flag(Compiler::Msvc, Lang::C, "17").unwrap(), "/std:c17");
        assert_eq!(
            std_flag(Compiler::Msvc, Lang::C, "23").unwrap(),
            "/std:clatest"
        );
        let e = std_flag(Compiler::Msvc, Lang::Cxx, "11").unwrap_err();
        assert!(e.contains("minimum is c++14"), "{e}");
        assert!(std_flag(Compiler::Msvc, Lang::C, "89").is_err());
        assert!(std_flag(Compiler::Msvc, Lang::C, "99").is_err());
        assert!(std_flag(Compiler::Gcc, Lang::C, "20").is_err());

        // MSVC C++ also gets /permissive- and /EHsc; the std flag comes first.
        let a = warn_args(Compiler::Msvc, Os::Windows, "");
        assert_eq!(&a[..3], ["/std:c++20", "/permissive-", "/EHsc"]);
        let c = build(&warn_src(""), &["a.c"], windows(Compiler::Msvc), None);
        let a = compile(&c, "a", Lang::C);
        assert_eq!(a[0], "/std:c11");
        assert!(!a.contains(&"/permissive-".to_string()) && !a.contains(&"/EHsc".to_string()));
    }

    #[test]
    fn missing_standard_names_the_workspace_key() {
        let src = "[lib.a]\nsources = [\"a.c\"]\n";
        let c = build(src, &["a.c"], linux(Compiler::Gcc), None);
        let e = compile_args(&c, idx(&c, "a"), Lang::Cxx)
            .unwrap_err()
            .to_string();
        assert!(e.contains("cxx"), "{e}");
        let e = compile_args(&c, idx(&c, "a"), Lang::C)
            .unwrap_err()
            .to_string();
        assert!(e.contains('c') && e.contains("workspace"), "{e}");
    }

    const DEP_SRC: &str = "
[workspace]
c = \"11\"
cxx = \"20\"
warnings = \"high\"
warnings_as_errors = true
[lib.a]
sources = [\"a.c\"]
deps = [\"d\"]
[dependency.d]
path = \"d\"
kind = \"sources\"
files = [\"x.c\"]
include.public = [\"inc\"]
defines = [\"DEP_PRIVATE\"]
";

    #[test]
    fn dependency_gets_no_warning_flags_even_with_werror_target() {
        for (platform, bad) in [
            (linux(Compiler::Gcc), ["-Wall", "-Wextra", "-Werror", "-w"]),
            (windows(Compiler::Msvc), ["/W4", "/WX", "/W0", "/W3"]),
        ] {
            let c = build(DEP_SRC, &["a.c", "d/x.c", "d/inc/"], platform, None);
            let d = compile(&c, "d", Lang::C);
            for b in bad {
                assert!(!d.contains(&b.to_string()), "{b} in {d:?}");
            }
            // The target itself still gets them.
            let a = compile(&c, "a", Lang::C);
            assert!(a.contains(&bad[0].to_string()) || a.contains(&bad[2].to_string()));
        }
        // The dependency follows the profile for opt/debug and its own defines.
        let c = build(
            DEP_SRC,
            &["a.c", "d/x.c", "d/inc/"],
            linux(Compiler::Gcc),
            None,
        );
        let d = compile(&c, "d", Lang::C);
        assert!(d.contains(&"-O0".to_string()) && d.contains(&"-g".to_string()));
        assert!(d.contains(&"-DDEP_PRIVATE".to_string()));
    }

    #[test]
    fn calling_for_non_compilable_units_is_an_error() {
        let src = "
[workspace]
c = \"11\"
[lib.h]
include.public = [\"inc\"]
[dependency.hdr]
path = \"hdr\"
kind = \"header-only\"
[bin.m]
sources = [\"m.c\"]
deps = [\"h\", \"hdr\"]
";
        let c = build(src, &["m.c", "hdr/", "inc/"], linux(Compiler::Gcc), None);
        assert!(compile_args(&c, idx(&c, "h"), Lang::C).is_err());
        assert!(compile_args(&c, idx(&c, "hdr"), Lang::C).is_err());
        assert!(compile_args(&c, idx(&c, "m"), Lang::C).is_ok());
    }

    #[test]
    fn system_includes_for_dependencies() {
        let files = ["a.c", "d/x.c", "d/inc/", "d/inc2/", "own/"];
        let src = "
[workspace]
c = \"11\"
[lib.a]
sources = [\"a.c\"]
include = [\"own\"]
deps = [\"d\"]
[dependency.d]
path = \"d\"
kind = \"sources\"
files = [\"x.c\"]
include.public = [\"inc\", \"inc2\"]
";
        let c = build(src, &files, linux(Compiler::Gcc), None);
        let a = compile(&c, "a", Lang::C);
        let own = pos(&a, "-Iown");
        let sys = pos(&a, "-isystem");
        assert!(own < sys, "{a:?}");
        assert_eq!(a[sys + 1], "d/inc");
        assert_eq!(a[sys + 2], "-isystem");
        assert_eq!(a[sys + 3], "d/inc2");

        let c = build(src, &files, windows(Compiler::Msvc), None);
        let a = compile(&c, "a", Lang::C);
        assert_eq!(count(&a, "/external:W0"), 1, "{a:?}");
        let first = pos(&a, "/external:Id/inc");
        assert_eq!(a[first - 1], "/external:W0");
        assert_eq!(a[first + 1], "/external:Id/inc2");
        assert!(pos(&a, "/Iown") < first);

        // No system include, no /external:W0.
        let c = build(&warn_src(""), &["a.c"], windows(Compiler::Msvc), None);
        assert_eq!(count(&compile(&c, "a", Lang::C), "/external:W0"), 0);
    }

    #[test]
    fn header_only_follows_consumer_opt() {
        let src = "
[workspace]
c = \"11\"
[lib.h]
include.public = [\"inc\"]
opt = \"none\"
[bin.m]
sources = [\"m.c\"]
deps = [\"h\"]
opt = \"max\"
";
        let c = build(src, &["m.c", "inc/"], linux(Compiler::Gcc), None);
        let a = compile(&c, "m", Lang::C);
        assert!(a.contains(&"-O3".to_string()), "{a:?}");
        assert!(!a.contains(&"-O0".to_string()));
        assert!(a.contains(&"-Iinc".to_string()));
    }

    #[test]
    fn asserts_false_defines_ndebug_once() {
        let src = "
[workspace]
c = \"11\"
[lib.a]
sources = [\"a.c\"]
defines = [\"NDEBUG\"]
deps = [\"d\"]
[dependency.d]
path = \"d\"
kind = \"sources\"
files = [\"x.c\"]
[profile.release]
defines = [\"NDEBUG\"]
";
        let files = ["a.c", "d/x.c"];
        for platform in [linux(Compiler::Gcc), windows(Compiler::Msvc)] {
            let c = build(src, &files, platform, Some("release"));
            let flag = if platform.compiler == Compiler::Msvc {
                "/DNDEBUG"
            } else {
                "-DNDEBUG"
            };
            assert_eq!(count(&compile(&c, "a", Lang::C), flag), 1);
            assert_eq!(count(&compile(&c, "d", Lang::C), flag), 1);
            assert_eq!(count(&profile_args(&c, Lang::C), flag), 1);
        }
        // Debug has asserts on: no NDEBUG anywhere.
        let c = build(
            src.replace("defines = [\"NDEBUG\"]", "").as_str(),
            &files,
            linux(Compiler::Gcc),
            None,
        );
        assert_eq!(count(&compile(&c, "d", Lang::C), "-DNDEBUG"), 0);
        assert_eq!(count(&profile_args(&c, Lang::C), "-DNDEBUG"), 0);
    }

    #[test]
    fn archiver_selection_for_lto() {
        assert_eq!(
            archiver(Compiler::Clang, Os::Linux, true, None).unwrap(),
            "llvm-ar"
        );
        assert_eq!(
            archiver(Compiler::Gcc, Os::Linux, true, None).unwrap(),
            "ar"
        );
        assert_eq!(
            archiver(Compiler::AppleClang, Os::Macos, true, None).unwrap(),
            "ar"
        );
        assert_eq!(
            archiver(Compiler::Clang, Os::Macos, true, None).unwrap(),
            "ar"
        );
        assert_eq!(
            archiver(Compiler::Clang, Os::Linux, false, None).unwrap(),
            "ar"
        );
        assert_eq!(
            archiver(Compiler::Msvc, Os::Windows, true, None).unwrap(),
            "lib.exe"
        );
        assert_eq!(
            archiver(Compiler::ClangCl, Os::Windows, false, None).unwrap(),
            "lib.exe"
        );
        assert_eq!(
            archiver(Compiler::ClangCl, Os::Windows, true, None).unwrap(),
            "llvm-lib"
        );
        for lto in [false, true] {
            assert_eq!(
                archiver(Compiler::Clang, Os::Windows, lto, None).unwrap(),
                "llvm-ar"
            );
            assert_eq!(
                archiver(Compiler::Gcc, Os::Windows, lto, None).unwrap(),
                "ar"
            );
        }
        assert_eq!(
            archiver(Compiler::Clang, Os::Windows, true, Some("my-ar")).unwrap(),
            "my-ar"
        );
        assert_eq!(
            archiver(Compiler::ClangCl, Os::Windows, true, Some("my-lib")).unwrap(),
            "my-lib"
        );
        assert_eq!(
            archiver(Compiler::Clang, Os::Linux, true, Some("my-ar")).unwrap(),
            "my-ar"
        );
        assert_eq!(
            archiver(Compiler::Msvc, Os::Windows, false, Some("my-lib")).unwrap(),
            "my-lib"
        );
    }

    #[test]
    fn lto_and_debug_flags() {
        let src = "[workspace]\nc = \"11\"\n[lib.a]\nsources = [\"a.c\"]\n";
        for (compiler, os, lto) in [
            (Compiler::Gcc, Os::Linux, vec!["-flto", "-ffat-lto-objects"]),
            (Compiler::Clang, Os::Linux, vec!["-flto=thin"]),
            (Compiler::AppleClang, Os::Macos, vec!["-flto=thin"]),
            (Compiler::Msvc, Os::Windows, vec!["/GL"]),
            (Compiler::ClangCl, Os::Windows, vec!["-flto=thin"]),
        ] {
            let c = build(src, &["a.c"], plat(os, compiler), Some("release"));
            let a = compile(&c, "a", Lang::C);
            for f in &lto {
                assert!(a.contains(&f.to_string()), "{compiler:?}: {a:?}");
            }
            let c = build(src, &["a.c"], plat(os, compiler), Some("debug"));
            let a = compile(&c, "a", Lang::C);
            assert!(!a.iter().any(|x| x.contains("lto") || x == "/GL"));
        }
        let c = build(src, &["a.c"], linux(Compiler::Gcc), None);
        assert!(compile(&c, "a", Lang::C).contains(&"-g".to_string()));
        let c = build(src, &["a.c"], windows(Compiler::Msvc), None);
        assert!(compile(&c, "a", Lang::C).contains(&"/Zi".to_string()));
        let c = build(src, &["a.c"], windows(Compiler::Msvc), Some("release"));
        assert!(!compile(&c, "a", Lang::C).contains(&"/Zi".to_string()));
    }

    #[test]
    fn msvc_runtime_follows_build_type() {
        let src = DEP_SRC;
        let files = ["a.c", "d/x.c", "d/inc/"];
        let c = build(src, &files, windows(Compiler::Msvc), None);
        for n in ["a", "d"] {
            let a = compile(&c, n, Lang::C);
            assert!(a.contains(&"/MDd".to_string()) && !a.contains(&"/MD".to_string()));
        }
        assert!(profile_args(&c, Lang::C).contains(&"/MDd".to_string()));
        let c = build(src, &files, windows(Compiler::Msvc), Some("release"));
        for n in ["a", "d"] {
            let a = compile(&c, n, Lang::C);
            assert!(a.contains(&"/MD".to_string()) && !a.contains(&"/MDd".to_string()));
        }
        // Not on gnu.
        let c = build(src, &files, linux(Compiler::Gcc), None);
        assert!(
            !compile(&c, "a", Lang::C)
                .iter()
                .any(|a| a.starts_with("/M"))
        );
    }

    #[test]
    fn cmake_build_type_rules() {
        let s = |opt, debug_info| ProfileScalars {
            opt,
            debug_info,
            asserts: true,
            lto: false,
        };
        use crate::config::types::Opt;
        assert_eq!(cmake_build_type("debug", &s(Opt::Max, false)), "Debug");
        assert_eq!(cmake_build_type("release", &s(Opt::None, true)), "Release");
        assert_eq!(
            cmake_build_type("relwithdebinfo", &s(Opt::None, false)),
            "RelWithDebInfo"
        );
        assert_eq!(cmake_build_type("fast", &s(Opt::None, false)), "Debug");
        assert_eq!(
            cmake_build_type("fast", &s(Opt::Max, true)),
            "RelWithDebInfo"
        );
        assert_eq!(cmake_build_type("fast", &s(Opt::Size, false)), "Release");
    }

    #[test]
    fn profile_args_for_cmake_deps() {
        let src = "
[workspace]
c = \"11\"
warnings = \"high\"
[profile.release]
flags = [\"-fno-plt\"]
defines = [\"P=1\"]
[lib.a]
sources = [\"a.c\"]
";
        let c = build(src, &["a.c"], linux(Compiler::Gcc), Some("release"));
        let a = profile_args(&c, Lang::C);
        assert_eq!(
            a,
            strs(&[
                "-O3",
                "-flto",
                "-ffat-lto-objects",
                "-DP=1",
                "-DNDEBUG",
                "-fno-plt"
            ])
        );
        let c = build(src, &["a.c"], windows(Compiler::Msvc), None);
        let a = profile_args(&c, Lang::Cxx);
        assert_eq!(a, strs(&["/Od", "/Zi", "/MDd"]));
        // No std, no warnings, ever.
        assert!(!a.iter().any(|x| x.contains("std") || x.starts_with("/W")));
    }

    /// bin `m` -> lib `a` -> lib `b`, plus system libs and link flags.
    fn link_fixture(platform: Platform, bin_extra: &str) -> Configured {
        let src = format!(
            "
[workspace]
c = \"11\"
[lib.b]
sources = [\"b.c\"]
system = [\"math\"]
[lib.a]
sources = [\"a.c\"]
deps = [\"b\"]
link_flags = [\"LFLAG\"]
[bin.m]
sources = [\"m.c\"]
deps = [\"a\"]
system = [\"opengl\"]
{bin_extra}"
        );
        build(&src, &["a.c", "b.c", "m.c"], platform, None)
    }

    fn artifact_of(c: &Configured) -> impl Fn(usize) -> PathBuf + '_ {
        move |u| PathBuf::from(format!("out/{}.a", c.units[u].name))
    }

    #[test]
    fn link_order_consumer_first() {
        let c = link_fixture(linux(Compiler::Gcc), "");
        let ext = [PathBuf::from("ext/libz.a")];
        let args = link_args(&c, idx(&c, "m"), &artifact_of(&c), &ext);
        assert_eq!(
            args,
            strs(&["out/a.a", "out/b.a", "ext/libz.a", "-lGL", "-lm", "LFLAG"])
        );
    }

    #[test]
    fn link_order_msvc_is_all_linker_options() {
        let c = link_fixture(windows(Compiler::Msvc), "");
        let args = link_args(&c, idx(&c, "m"), &artifact_of(&c), &[]);
        assert_eq!(
            args,
            strs(&["out/a.a", "out/b.a", "opengl32.lib", "LFLAG", "/DEBUG"])
        );
    }

    #[test]
    fn clang_lto_on_windows_uses_lld() {
        let src = "[workspace]\nc = \"11\"\n[bin.m]\nsources = [\"m.c\"]\n";
        let link = |platform, profile| {
            let c = build(src, &["m.c"], platform, Some(profile));
            link_args(&c, idx(&c, "m"), &artifact_of(&c), &[])
        };
        let lld = "-fuse-ld=lld".to_string();
        assert_eq!(
            link(windows(Compiler::Clang), "release"),
            strs(&["-flto=thin", "-fuse-ld=lld"])
        );
        // not on linux, not without lto, not for other compilers
        assert!(!link(linux(Compiler::Clang), "release").contains(&lld));
        assert!(!link(windows(Compiler::Clang), "debug").contains(&lld));
        assert!(!link(windows(Compiler::Gcc), "release").contains(&lld));
    }

    #[test]
    fn clang_cl_lto_link_has_no_ltcg() {
        let src = "[workspace]
c = \"11\"
[bin.m]
sources = [\"m.c\"]
";
        let link = |compiler| {
            let c = build(src, &["m.c"], windows(compiler), Some("release"));
            link_args(&c, idx(&c, "m"), &artifact_of(&c), &[])
        };
        assert_eq!(link(Compiler::ClangCl), Vec::<String>::new());
        assert_eq!(link(Compiler::Msvc), strs(&["/LTCG"]));
    }

    #[test]
    fn link_lto_debug_and_frameworks() {
        let c2 = build(
            "[workspace]\nc = \"11\"\n[bin.m]\nsources = [\"m.c\"]\n",
            &["m.c"],
            linux(Compiler::Clang),
            Some("release"),
        );
        let a = link_args(&c2, idx(&c2, "m"), &artifact_of(&c2), &[]);
        assert_eq!(a, strs(&["-flto=thin"]));
        let c3 = build(
            "[workspace]\nc = \"11\"\n[bin.m]\nsources = [\"m.c\"]\n",
            &["m.c"],
            windows(Compiler::Msvc),
            Some("release"),
        );
        assert_eq!(
            link_args(&c3, idx(&c3, "m"), &artifact_of(&c3), &[]),
            strs(&["/LTCG"])
        );
        // relwithdebinfo: both
        let c4 = build(
            "[workspace]\nc = \"11\"\n[bin.m]\nsources = [\"m.c\"]\n",
            &["m.c"],
            windows(Compiler::Msvc),
            Some("relwithdebinfo"),
        );
        assert_eq!(
            link_args(&c4, idx(&c4, "m"), &artifact_of(&c4), &[]),
            strs(&["/DEBUG"])
        );
        // macOS window system library is a framework: two args.
        let c5 = build(
            "[workspace]\nc = \"11\"\n[bin.m]\nsources = [\"m.c\"]\nsystem = [\"opengl\"]\n",
            &["m.c"],
            plat(Os::Macos, Compiler::AppleClang),
            None,
        );
        assert_eq!(
            link_args(&c5, idx(&c5, "m"), &artifact_of(&c5), &[]),
            strs(&["-framework", "OpenGL"])
        );
    }

    #[test]
    fn cross_triple_only_for_the_clang_family() {
        let src = "[workspace]\nc = \"11\"\n[bin.m]\nsources = [\"m.c\"]\n";
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        std::fs::write(root.join("m.c"), "").unwrap();
        let m = parse_manifest(parse_flat(src).unwrap()).unwrap();
        let cross = |platform: Platform| {
            let mut c = configure(
                &m,
                &root,
                &Invocation::default(),
                platform,
                &BTreeMap::new(),
            )
            .unwrap();
            c.cross_triple = Some("TRIPLE".to_string());
            c
        };
        let want = "--target=TRIPLE".to_string();
        for (platform, expect) in [
            (linux(Compiler::Clang), true),
            (windows(Compiler::ClangCl), true),
            (linux(Compiler::Gcc), false),
            (plat(Os::Macos, Compiler::AppleClang), true),
            (windows(Compiler::Msvc), false),
        ] {
            let c = cross(platform);
            let m = idx(&c, "m");
            let compile = compile_args(&c, m, Lang::C).unwrap();
            let link = link_args(&c, m, &artifact_of(&c), &[]);
            let profile = profile_args(&c, Lang::C);
            assert_eq!(compile.contains(&want), expect, "{platform:?}: {compile:?}");
            assert_eq!(profile.contains(&want), expect, "{platform:?}: {profile:?}");
            // clang-cl's link line is linker options: the triple goes before `/link`.
            let in_link = expect && platform.compiler != Compiler::ClangCl;
            assert_eq!(link.contains(&want), in_link, "{platform:?}: {link:?}");
            assert_eq!(target_arg(&c).is_some(), expect);
        }
        // Not cross-compiling: nothing, even for clang.
        let mut c = cross(linux(Compiler::Clang));
        c.cross_triple = None;
        assert!(target_arg(&c).is_none());
    }

    #[test]
    fn subsystem_only_on_windows() {
        let extra = "subsystem = \"windows\"\n";
        let c = link_fixture(windows(Compiler::Msvc), extra);
        let a = link_args(&c, idx(&c, "m"), &artifact_of(&c), &[]);
        assert_eq!(
            &a[a.len() - 2..],
            ["/SUBSYSTEM:WINDOWS", "/ENTRY:mainCRTStartup"]
        );
        let c = link_fixture(windows(Compiler::Gcc), extra);
        let a = link_args(&c, idx(&c, "m"), &artifact_of(&c), &[]);
        assert_eq!(a.last().unwrap(), "-mwindows");
        let c = link_fixture(windows(Compiler::ClangCl), extra);
        let a = link_args(&c, idx(&c, "m"), &artifact_of(&c), &[]);
        assert!(a.contains(&"/SUBSYSTEM:WINDOWS".to_string()));
        // Ignored elsewhere.
        for p in [linux(Compiler::Gcc), plat(Os::Macos, Compiler::AppleClang)] {
            let c = link_fixture(p, extra);
            let a = link_args(&c, idx(&c, "m"), &artifact_of(&c), &[]);
            assert!(
                !a.iter()
                    .any(|x| x.contains("indows") || x.contains("ENTRY")),
                "{a:?}"
            );
        }
        // Console subsystem and libs never get one.
        let c = link_fixture(windows(Compiler::Msvc), "subsystem = \"console\"\n");
        let a = link_args(&c, idx(&c, "m"), &artifact_of(&c), &[]);
        assert!(!a.iter().any(|x| x.starts_with("/SUBSYSTEM")));
    }
}
