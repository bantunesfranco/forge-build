//! Host detection, `--target` parsing, concrete compiler classification,
//! impossible-combination checks and toolchain-program loading (spec 4.2, 11.1).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{
    config::{
        ConfigError,
        vocab::{Arch, Compiler, Os, Platform},
    },
    executor::{CommandSpec, Executor},
};

/// The operating system and architecture this binary was built for.
pub fn host_os_arch() -> Result<(Os, Arch), ConfigError> {
    let os = if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "linux") {
        Os::Linux
    } else if cfg!(target_os = "macos") {
        Os::Macos
    } else if cfg!(any(
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )) {
        Os::Bsd
    } else {
        return Err(ConfigError::invalid(
            "host",
            "unsupported host operating system",
        ));
    };
    let arch = if cfg!(target_arch = "x86_64") {
        Arch::X86_64
    } else if cfg!(target_arch = "x86") {
        Arch::X86
    } else if cfg!(target_arch = "aarch64") {
        Arch::Arm64
    } else if cfg!(target_arch = "arm") {
        Arch::Arm
    } else {
        return Err(ConfigError::invalid(
            "host",
            "unsupported host architecture",
        ));
    };
    Ok((os, arch))
}

/// Parses `--target <arch>-<os>`.
pub fn parse_target(s: &str) -> Result<(Os, Arch), String> {
    let form =
        || format!("invalid target `{s}`: expected `<arch>-<os>`, for example `arm64-macos`");
    let (arch, os) = s.split_once('-').ok_or_else(form)?;
    let arch = arch
        .parse::<Arch>()
        .map_err(|e| format!("{e} in target `{s}` (expected `<arch>-<os>`)"))?;
    let os = os
        .parse::<Os>()
        .map_err(|e| format!("{e} in target `{s}` (expected `<arch>-<os>`)"))?;
    Ok((os, arch))
}

/// The program name for classification: file name, lowercased, without `.exe`.
fn stem(program: &str) -> String {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let name = name.to_ascii_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

/// `cl` and `clang-cl` are recognised by name alone.
pub fn msvc_like_by_name(program: &str) -> Option<Compiler> {
    match stem(program).as_str() {
        "cl" => Some(Compiler::Msvc),
        "clang-cl" => Some(Compiler::ClangCl),
        _ => None,
    }
}

/// Classifies a compiler from its program name and `--version` output.
pub fn classify_compiler(program: &str, version_output: &str) -> Option<Compiler> {
    if let Some(c) = msvc_like_by_name(program) {
        return Some(c);
    }
    if version_output.contains("Apple clang") {
        return Some(Compiler::AppleClang);
    }
    if version_output.contains("clang version") {
        return Some(Compiler::Clang);
    }
    if version_output.to_ascii_lowercase().contains("gcc")
        || version_output.contains("Free Software Foundation")
    {
        return Some(Compiler::Gcc);
    }
    let name = stem(program);
    if name.contains("clang") {
        Some(Compiler::Clang)
    } else if name.contains("gcc") || name.contains("g++") || name == "cc" || name == "c++" {
        Some(Compiler::Gcc)
    } else {
        None
    }
}

/// Finds the concrete compiler behind `program`. `cl` and `clang-cl` are
/// recognised by name and never run.
pub fn detect_compiler(
    exec: &dyn Executor,
    program: &str,
    cwd: &Path,
) -> Result<Compiler, ConfigError> {
    if let Some(c) = msvc_like_by_name(program) {
        return Ok(c);
    }
    let spec = CommandSpec {
        program: program.to_string(),
        args: vec!["--version".to_string()],
        cwd: cwd.to_path_buf(),
    };
    let outcome = exec
        .run(&spec)
        .map_err(|e| ConfigError::invalid("compiler", format!("cannot run `{program}`: {e}")))?;
    let text = format!("{}\n{}", outcome.stdout, outcome.stderr);
    classify_compiler(program, &text).ok_or_else(|| {
        let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        ConfigError::invalid(
            "compiler",
            format!(
                "cannot tell which compiler `{program}` is (first line of `--version`: `{}`)",
                first.trim()
            ),
        )
    })
}

/// Rejects compiler/os pairs that cannot exist (spec 4.2).
pub fn check_combination(p: &Platform) -> Result<(), String> {
    let needs = match p.compiler {
        Compiler::Msvc | Compiler::ClangCl => Some(Os::Windows),
        Compiler::AppleClang => Some(Os::Macos),
        Compiler::Gcc | Compiler::Clang => None,
    };
    match needs {
        Some(os) if os != p.os => Err(format!(
            "`{}` cannot target `{}` (it requires `{}`)",
            p.compiler.as_str(),
            p.os.as_str(),
            os.as_str()
        )),
        _ => Ok(()),
    }
}

/// The LLVM target triple used for `--target` cross builds.
pub fn llvm_triple(os: Os, arch: Arch) -> String {
    let arch_part = match arch {
        Arch::X86_64 => "x86_64",
        Arch::X86 => "i686",
        Arch::Arm64 if os == Os::Macos => "arm64",
        Arch::Arm64 => "aarch64",
        Arch::Arm => "armv7",
    };
    let os_part = match os {
        Os::Windows => "pc-windows-msvc",
        Os::Linux => "unknown-linux-gnu",
        Os::Macos => "apple-macos",
        Os::Bsd => "unknown-freebsd",
    };
    format!("{arch_part}-{os_part}")
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UserToolchainFile {
    toolchain: UserToolchain,
}

/// The `[toolchain]` table of a toolchain file.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserToolchain {
    pub c: Option<String>,
    pub cxx: Option<String>,
    pub ar: Option<String>,
}

pub fn load_user_toolchain() -> Result<UserToolchain, String> {
    let path = if let Some(path) = std::env::var_os("FORGE_TOOLCHAIN") {
        Some(PathBuf::from(path))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("forge/toolchain.toml"))
    } else if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(path).join("forge/toolchain.toml"))
    } else {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|p| p.join(".config/forge/toolchain.toml"))
    };
    let Some(path) = path else {
        return Ok(UserToolchain::default());
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => toml::from_str::<UserToolchainFile>(&contents)
            .map(|file| file.toolchain)
            .map_err(|e| format!("invalid user toolchain file {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UserToolchain::default()),
        Err(e) => Err(format!(
            "cannot read user toolchain file {}: {e}",
            path.display()
        )),
    }
}

/// The compiler and archiver programs to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolchains {
    pub c: String,
    pub cxx: String,
    pub ar: Option<String>,
}

impl Toolchains {
    /// Neither compiler was named by a toolchain file, `CC`/`CXX` or the user
    /// file: both are the platform's default driver.
    fn default_compilers(&self) -> bool {
        let (c, cxx) = if cfg!(windows) {
            ("cl", "cl")
        } else {
            ("cc", "c++")
        };
        self.c == c && self.cxx == cxx
    }
}

/// gcc and MSVC choose their target by which compiler is run, not by a flag.
/// A cross build that names no compiler (no `--toolchain`, no `CC`/`CXX`) would
/// silently build for the host, so say so.
pub fn cross_compile_warning(
    target: &str,
    compiler: Compiler,
    toolchain_given: bool,
    toolchains: &Toolchains,
) -> Option<String> {
    let by_flag = !matches!(compiler, Compiler::Gcc | Compiler::Msvc);
    if by_flag || toolchain_given || !toolchains.default_compilers() {
        return None;
    }
    Some(format!(
        "--target {target}: {} cannot cross-compile by flag; name a cross compiler with --toolchain",
        compiler.as_str()
    ))
}

/// Precedence per field: explicit file > environment > user file > default.
fn pick_toolchains(
    explicit: Option<UserToolchain>,
    env: &dyn Fn(&str) -> Option<String>,
    user: UserToolchain,
) -> Toolchains {
    let explicit = explicit.unwrap_or_default();
    let default_c = if cfg!(windows) { "cl" } else { "cc" };
    let default_cxx = if cfg!(windows) { "cl" } else { "c++" };
    Toolchains {
        c: explicit
            .c
            .or_else(|| env("CC"))
            .or(user.c)
            .unwrap_or_else(|| default_c.into()),
        cxx: explicit
            .cxx
            .or_else(|| env("CXX"))
            .or(user.cxx)
            .unwrap_or_else(|| default_cxx.into()),
        ar: explicit.ar.or_else(|| env("AR")).or(user.ar),
    }
}

/// Reads an explicit `--toolchain` file.
fn read_toolchain_file(path: &Path) -> Result<UserToolchain, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        ConfigError::invalid(
            "--toolchain",
            format!("cannot read toolchain file {}: {e}", path.display()),
        )
    })?;
    toml::from_str::<UserToolchainFile>(&text)
        .map(|file| file.toolchain)
        .map_err(|e| {
            ConfigError::invalid(
                "--toolchain",
                format!("invalid toolchain file {}: {e}", path.display()),
            )
        })
}

/// Resolves the toolchain programs; `explicit` is the `--toolchain` file.
pub fn load_toolchains(explicit: Option<&Path>) -> Result<Toolchains, ConfigError> {
    let explicit = explicit.map(read_toolchain_file).transpose()?;
    let user = load_user_toolchain().map_err(|m| ConfigError::invalid("toolchain", m))?;
    Ok(pick_toolchains(
        explicit,
        &|k| std::env::var(k).ok().filter(|v| !v.is_empty()),
        user,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::Outcome;
    use std::io;

    fn pf(os: Os, arch: Arch, compiler: Compiler) -> Platform {
        Platform { os, arch, compiler }
    }

    #[test]
    fn classifies_compilers() {
        assert_eq!(classify_compiler("cl", ""), Some(Compiler::Msvc));
        assert_eq!(
            classify_compiler("C:/x/clang-cl.exe", ""),
            Some(Compiler::ClangCl)
        );
        assert_eq!(
            classify_compiler("cc", "Apple clang version 15.0.0"),
            Some(Compiler::AppleClang)
        );
        assert_eq!(
            classify_compiler("clang++", "clang version 17.0.6"),
            Some(Compiler::Clang)
        );
        assert_eq!(
            classify_compiler("g++", "g++ (GCC) 13.2.0"),
            Some(Compiler::Gcc)
        );
        assert_eq!(
            classify_compiler("cc", "cc (Ubuntu 13.2.0) ... Free Software Foundation"),
            Some(Compiler::Gcc)
        );
        assert_eq!(classify_compiler("clang++", ""), Some(Compiler::Clang));
        assert_eq!(
            classify_compiler("aarch64-linux-gnu-gcc", ""),
            Some(Compiler::Gcc)
        );
        assert_eq!(classify_compiler("CL.EXE", ""), Some(Compiler::Msvc));
        assert_eq!(classify_compiler("mystery", "nothing useful"), None);
    }

    #[test]
    fn parses_target() {
        assert_eq!(
            parse_target("arm64-macos").unwrap(),
            (Os::Macos, Arch::Arm64)
        );
        assert!(parse_target("macos-arm64").is_err());
        assert!(parse_target("x86_64").unwrap_err().contains("<arch>-<os>"));
    }

    #[test]
    fn impossible_combinations() {
        let err = check_combination(&pf(Os::Macos, Arch::Arm64, Compiler::Msvc)).unwrap_err();
        assert!(err.contains("msvc") && err.contains("macos"), "{err}");
        assert!(check_combination(&pf(Os::Linux, Arch::X86_64, Compiler::ClangCl)).is_err());
        assert!(check_combination(&pf(Os::Linux, Arch::X86_64, Compiler::AppleClang)).is_err());
        assert!(check_combination(&pf(Os::Linux, Arch::Arm64, Compiler::Clang)).is_ok());
        assert!(check_combination(&pf(Os::Windows, Arch::X86_64, Compiler::Msvc)).is_ok());
        assert!(check_combination(&pf(Os::Macos, Arch::Arm64, Compiler::AppleClang)).is_ok());
    }

    #[test]
    fn gcc_and_msvc_cross_builds_without_a_toolchain_warn() {
        let defaults = pick_toolchains(None, &|_| None, UserToolchain::default());
        let named = pick_toolchains(None, &|_| Some("x-gcc".into()), UserToolchain::default());
        let warn = |compiler, given, t: &Toolchains| {
            cross_compile_warning("arm64-linux", compiler, given, t)
        };
        let w = warn(Compiler::Gcc, false, &defaults).unwrap();
        assert_eq!(
            w,
            "--target arm64-linux: gcc cannot cross-compile by flag; name a cross compiler with --toolchain"
        );
        assert!(
            warn(Compiler::Msvc, false, &defaults)
                .unwrap()
                .contains("msvc")
        );
        assert!(warn(Compiler::Gcc, true, &defaults).is_none());
        assert!(warn(Compiler::Gcc, false, &named).is_none());
        for flagged in [Compiler::Clang, Compiler::AppleClang, Compiler::ClangCl] {
            assert!(warn(flagged, false, &defaults).is_none(), "{flagged:?}");
        }
    }

    #[test]
    fn triples() {
        assert_eq!(llvm_triple(Os::Macos, Arch::Arm64), "arm64-apple-macos");
        assert_eq!(
            llvm_triple(Os::Linux, Arch::X86_64),
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            llvm_triple(Os::Linux, Arch::Arm64),
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(llvm_triple(Os::Windows, Arch::X86), "i686-pc-windows-msvc");
    }

    #[test]
    fn host_is_detected() {
        assert!(host_os_arch().is_ok());
    }

    fn user(c: Option<&str>, cxx: Option<&str>, ar: Option<&str>) -> UserToolchain {
        UserToolchain {
            c: c.map(Into::into),
            cxx: cxx.map(Into::into),
            ar: ar.map(Into::into),
        }
    }

    #[test]
    fn toolchain_file_overrides_env() {
        let env = |k: &str| match k {
            "CC" => Some("env-cc".to_string()),
            "CXX" => Some("env-cxx".to_string()),
            "AR" => Some("env-ar".to_string()),
            _ => None,
        };
        let t = pick_toolchains(
            Some(user(Some("file-cc"), None, None)),
            &env,
            user(Some("user-cc"), Some("user-cxx"), Some("user-ar")),
        );
        assert_eq!(t.c, "file-cc");
        assert_eq!(t.cxx, "env-cxx");
        assert_eq!(t.ar.as_deref(), Some("env-ar"));
    }

    #[test]
    fn env_overrides_user_file_and_defaults_apply() {
        let env = |k: &str| (k == "CC").then(|| "env-cc".to_string());
        let t = pick_toolchains(None, &env, user(Some("user-cc"), Some("user-cxx"), None));
        assert_eq!(t.c, "env-cc");
        assert_eq!(t.cxx, "user-cxx");
        assert_eq!(t.ar, None);
        let t = pick_toolchains(None, &|_| None, UserToolchain::default());
        assert_eq!(t.c, if cfg!(windows) { "cl" } else { "cc" });
        assert_eq!(t.cxx, if cfg!(windows) { "cl" } else { "c++" });
    }

    #[test]
    fn explicit_toolchain_file_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("toolchain.toml");
        std::fs::write(&path, "[toolchain]\nc = \"file-cc\"\nar = \"llvm-ar\"\n").unwrap();
        let t = read_toolchain_file(&path).unwrap();
        assert_eq!(t.c.as_deref(), Some("file-cc"));
        assert_eq!(t.cxx, None);
        assert_eq!(t.ar.as_deref(), Some("llvm-ar"));
    }

    #[test]
    fn missing_toolchain_file_names_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.toml");
        let err = read_toolchain_file(&missing).unwrap_err().to_string();
        assert!(
            err.contains("--toolchain") && err.contains("nope.toml"),
            "{err}"
        );
    }

    #[test]
    fn toolchain_file_rejects_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "[toolchain]\nlinker = \"x\"\n").unwrap();
        let err = read_toolchain_file(&path).unwrap_err().to_string();
        assert!(err.contains("bad.toml") && err.contains("linker"), "{err}");
    }

    struct Fake(io::Result<(i32, &'static str, &'static str)>);
    impl Executor for Fake {
        fn run(&self, _: &CommandSpec) -> io::Result<Outcome> {
            match &self.0 {
                Ok((code, out, err)) => Ok(Outcome {
                    code: Some(*code),
                    stdout: out.to_string(),
                    stderr: err.to_string(),
                }),
                Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
            }
        }
    }

    #[test]
    fn detects_from_version_output() {
        let cwd = Path::new(".");
        let ok = Fake(Ok((0, "", "clang version 17.0.6\n")));
        assert_eq!(detect_compiler(&ok, "tool", cwd).unwrap(), Compiler::Clang);
        // msvc-like names never run anything.
        let broken = Fake(Err(io::Error::from(io::ErrorKind::NotFound)));
        assert_eq!(detect_compiler(&broken, "cl", cwd).unwrap(), Compiler::Msvc);
        let err = detect_compiler(&broken, "nope", cwd)
            .unwrap_err()
            .to_string();
        assert!(err.contains("nope"), "{err}");
        let odd = Fake(Ok((0, "weird banner\nsecond", "")));
        let err = detect_compiler(&odd, "mystery", cwd)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("mystery") && err.contains("weird banner"),
            "{err}"
        );
    }
}
