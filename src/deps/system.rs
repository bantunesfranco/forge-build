//! `system` dependencies: libraries the host already has, found with
//! `pkg-config` or, failing that, `cmake --find-package` (spec 10.1, 10.3).

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::{
    config::{ConfigError, Diagnostics, types::Define, vocab::Compiler},
    configure::{model::IncludeDir, system_libs::SystemLib},
    deps::Prepared,
    executor::path_arg,
    project::Project,
};

/// What a finder reports, split by kind.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Found {
    pub include: Vec<PathBuf>,
    pub defines: Vec<Define>,
    pub cflags: Vec<String>,
    pub lib_dirs: Vec<PathBuf>,
    pub libs: Vec<String>,
    pub ldflags: Vec<String>,
}

/// Runs `program args` and returns its stdout. Injectable so the finders can
/// be tested without the tools.
pub type Runner<'a> = &'a dyn Fn(&str, &[String]) -> Result<String, String>;

/// Link options other than `-l`/`-L` that go to the linker as they are. A bare
/// `-static` is not one: it would make the whole bin static.
fn is_link_option(token: &str) -> bool {
    token.starts_with("-Wl,") || token == "-rdynamic"
}

/// The value of a flag written as `-X value` or `-Xvalue`, when `token` is
/// that flag.
fn flag_value(
    token: &str,
    prefix: &str,
    rest: &mut impl Iterator<Item = String>,
) -> Option<String> {
    let tail = token.strip_prefix(prefix)?;
    if tail.is_empty() {
        rest.next()
    } else {
        Some(tail.to_owned())
    }
}

/// Splits `text` into words like a shell: whitespace separates, single and
/// double quotes group, and a backslash escapes a space, a quote or another
/// backslash (how `pkg-config` writes a path with a space). Any other
/// backslash stays, so a Windows path is not damaged.
pub fn shell_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match (ch, quote) {
            ('\\', _)
                if quote != Some('\'')
                    && chars
                        .peek()
                        .is_some_and(|n| matches!(n, ' ' | '"' | '\'' | '\\')) =>
            {
                word.push(chars.next().unwrap_or(ch));
                started = true;
            }
            (c, Some(q)) if c == q => quote = None,
            (c @ ('"' | '\''), None) => {
                quote = Some(c);
                started = true;
            }
            (c, None) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (c, _) => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// Sorts the tokens of a `pkg-config` or `cmake --find-package` answer.
/// `-I`, `-L`, `-l` and `-D` accept their value as the next token, `-isystem`
/// always does, and `-framework` keeps both tokens together. A bare token is a
/// library file and goes to the linker.
pub fn split_flags(tokens: &str) -> Found {
    let mut found = Found::default();
    let mut it = shell_words(tokens).into_iter();
    while let Some(token) = it.next() {
        let token = token.as_str();
        if token == "-isystem" {
            if let Some(dir) = it.next() {
                found.include.push(PathBuf::from(dir));
            }
        } else if let Some(dir) = flag_value(token, "-I", &mut it) {
            found.include.push(PathBuf::from(dir));
        } else if let Some(def) = flag_value(token, "-D", &mut it) {
            match def.parse::<Define>() {
                Ok(d) => found.defines.push(d),
                Err(_) => found.cflags.push(format!("-D{def}")),
            }
        } else if let Some(dir) = flag_value(token, "-L", &mut it) {
            found.lib_dirs.push(PathBuf::from(dir));
        } else if let Some(lib) = flag_value(token, "-l", &mut it) {
            found.libs.push(lib);
        } else if token == "-framework" {
            if let Some(name) = it.next() {
                found.ldflags.push(token.to_owned());
                found.ldflags.push(name);
            }
        } else if token == "-static" {
            // Not forwarded: see `is_link_option`.
        } else if token == "-pthread" {
            found.cflags.push(token.to_owned());
            found.ldflags.push(token.to_owned());
        } else if is_link_option(token) || !token.starts_with('-') {
            found.ldflags.push(token.to_owned());
        } else {
            found.cflags.push(token.to_owned());
        }
    }
    found
}

/// Whether every library of `found` is a static archive in one of its library
/// directories with no shared library next to it: only then does the link need
/// the package's private dependencies (`Libs.private`).
fn only_static_archives(found: &Found, exists: &dyn Fn(&Path) -> bool) -> bool {
    let has = |file: String| found.lib_dirs.iter().any(|dir| exists(&dir.join(&file)));
    !found.libs.is_empty()
        && found.libs.iter().all(|lib| {
            has(format!("lib{lib}.a"))
                && !has(format!("lib{lib}.so"))
                && !has(format!("lib{lib}.dylib"))
                && !has(format!("lib{lib}.dll.a"))
        })
}

fn pkg_config(name: &str, run: Runner, exists: &dyn Fn(&Path) -> bool) -> Result<Found, String> {
    run("pkg-config", &["--exists".into(), name.into()])?;
    let mut args = vec!["--cflags".to_string(), "--libs".into(), name.into()];
    let found = split_flags(&run("pkg-config", &args)?);
    // A package that resolves to a static archive needs the libraries that the
    // archive uses (e.g. GLFW's X11 dependencies, in Libs.private). A shared
    // one does not: asking for them would over-link.
    if !only_static_archives(&found, exists) {
        return Ok(found);
    }
    args.insert(0, "--static".to_string());
    Ok(split_flags(&run("pkg-config", &args)?))
}

fn cmake(name: &str, run: Runner) -> Result<Found, String> {
    let mut all = String::new();
    for mode in ["COMPILE", "LINK"] {
        let args = [
            "--find-package".to_string(),
            format!("-DNAME={name}"),
            "-DCOMPILER_ID=GNU".into(),
            "-DLANGUAGE=CXX".into(),
            format!("-DMODE={mode}"),
        ];
        all.push_str(&run("cmake", &args)?);
        all.push(' ');
    }
    let found = split_flags(&all);
    if found == Found::default() || all.contains("not found") {
        return Err(format!("`{name}` not found"));
    }
    Ok(found)
}

/// Finds a package on the host: `pkg-config` first, then CMake.
pub fn find_package(
    name: &str,
    run: Runner,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<Found, String> {
    let pkg = match pkg_config(name, run, exists) {
        Ok(found) => return Ok(found),
        Err(e) => e,
    };
    cmake(name, run).map_err(|cm| {
        format!("cannot find system package `{name}`: pkg-config: {pkg}; cmake: {cm}")
    })
}

/// The real runner: the program runs with stdin closed, in the temporary
/// directory (`cmake --find-package` writes a `CMakeFiles` directory into its
/// working directory).
fn run_process(program: &str, args: &[String]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("{program} is not installed or not on PATH")
            } else {
                format!("cannot run {program}: {e}")
            }
        })?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let detail = format!("{}{}", stdout.trim(), stderr.trim());
        return Err(format!("`{program} {}` failed: {detail}", args.join(" ")));
    }
    Ok(stdout.into_owned())
}

/// The linker spelling of a library directory.
fn lib_dir_flag(dir: &Path, msvc: bool) -> String {
    if msvc {
        format!("/LIBPATH:{}", path_arg(dir))
    } else {
        format!("-L{}", path_arg(dir))
    }
}

fn push_unique<T: PartialEq>(list: &mut Vec<T>, item: T) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// Turns what a finder found into the dependency's public interface. With an
/// MSVC-like linker the GNU-only tokens are dropped, with a warning.
fn to_prepared(
    found: &Found,
    system_headers: bool,
    msvc: bool,
    dep: &str,
    diags: &mut Diagnostics,
) -> Prepared {
    let mut prepared = Prepared::default();
    for path in &found.include {
        let dir = IncludeDir {
            path: path.clone(),
            system: system_headers,
        };
        push_unique(&mut prepared.include, dir);
    }
    for define in &found.defines {
        push_unique(&mut prepared.extra.defines, define.clone());
    }
    for flag in &found.cflags {
        push_unique(&mut prepared.extra.flags, flag.clone());
    }
    for lib in &found.libs {
        push_unique(&mut prepared.libs, lib.clone());
        push_unique(&mut prepared.extra.system, SystemLib::Lib(lib.clone()));
    }
    for dir in &found.lib_dirs {
        push_unique(&mut prepared.lib_dirs, dir.clone());
        push_unique(&mut prepared.extra.link_flags, lib_dir_flag(dir, msvc));
    }
    let mut it = found.ldflags.iter();
    while let Some(flag) = it.next() {
        // A framework is a library, not a flag: as a `SystemLib` it is kept
        // apart from its neighbours and rendered by the link step.
        let framework = (flag == "-framework").then(|| it.next()).flatten();
        let gnu_only = flag.starts_with("-Wl,") || flag == "-pthread" || framework.is_some();
        if msvc && gnu_only {
            diags.warn(format!(
                "dependency `{dep}`: dropped link option `{flag}`, which the MSVC-like linker does not take"
            ));
            continue;
        }
        match framework {
            Some(name) => push_unique(
                &mut prepared.extra.system,
                SystemLib::Framework(name.clone()),
            ),
            None => push_unique(&mut prepared.extra.link_flags, flag.clone()),
        }
    }
    prepared
}

/// Finds a `system` dependency and adds what was found to its public
/// settings, where `effective()` carries it to the compile of every consumer
/// and to the final link of every `bin` that depends on it.
pub fn prepare_system(project: &mut Project, unit: usize) -> Result<Prepared, ConfigError> {
    let u = &project.configured.units[unit];
    let field = format!("dependency.{}", u.name);
    let name = u.name.clone();
    let (find, system_headers) = match u.dep.as_ref() {
        Some(dep) => (
            dep.find.clone().unwrap_or_else(|| name.clone()),
            dep.system_headers,
        ),
        None => return Err(ConfigError::invalid(field, "not a dependency")),
    };
    let found = find_package(&find, &run_process, &|path| path.is_file())
        .map_err(|message| ConfigError::invalid(field, message))?;
    let msvc = matches!(
        project.platform.compiler,
        Compiler::Msvc | Compiler::ClangCl
    );
    let prepared = to_prepared(
        &found,
        system_headers,
        msvc,
        &name,
        &mut project.diagnostics,
    );

    let public = &mut project.configured.units[unit].public;
    for dir in &prepared.include {
        push_unique(&mut public.include, dir.clone());
    }
    for define in &prepared.extra.defines {
        push_unique(&mut public.defines, define.clone());
    }
    for flag in &prepared.extra.flags {
        push_unique(&mut public.flags, flag.clone());
    }
    for flag in &prepared.extra.link_flags {
        push_unique(&mut public.link_flags, flag.clone());
    }
    for lib in &prepared.extra.system {
        push_unique(&mut public.system, lib.clone());
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_pkg_config_output() {
        let f = split_flags("-I/usr/include/x -DX=1 -pthread -L/opt/lib -lfoo -Wl,-rpath,/opt");
        assert_eq!(f.include, [PathBuf::from("/usr/include/x")]);
        assert_eq!(f.defines, ["X=1".parse::<Define>().unwrap()]);
        assert_eq!(f.cflags, ["-pthread"]);
        assert_eq!(f.lib_dirs, [PathBuf::from("/opt/lib")]);
        assert_eq!(f.libs, ["foo"]);
        assert_eq!(f.ldflags, ["-pthread", "-Wl,-rpath,/opt"]);
    }

    #[test]
    fn two_token_forms_frameworks_and_library_files() {
        let f = split_flags(
            "-I /a -isystem /b -D Y -L /c -l d -framework Cocoa /usr/lib/libGL.so -rdynamic -Wall",
        );
        assert_eq!(f.include, [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(f.defines, ["Y".parse::<Define>().unwrap()]);
        assert_eq!(f.lib_dirs, [PathBuf::from("/c")]);
        assert_eq!(f.libs, ["d"]);
        assert_eq!(
            f.ldflags,
            ["-framework", "Cocoa", "/usr/lib/libGL.so", "-rdynamic"]
        );
        assert_eq!(f.cflags, ["-Wall"]);
    }

    #[test]
    fn paths_with_spaces_and_quotes_stay_whole() {
        assert_eq!(
            shell_words(r"-I/Program\ Files/x  -DNAME='a b' -D'Q=1' "),
            ["-I/Program Files/x", "-DNAME=a b", "-DQ=1"]
        );
        assert_eq!(
            shell_words(r#""C:/Program Files/x/lib" -lz"#),
            ["C:/Program Files/x/lib", "-lz"]
        );
        // Only a space, a quote or a backslash is escaped: a Windows path stays.
        assert_eq!(
            shell_words(r"-LC:\lib\foo C:\\x"),
            [r"-LC:\lib\foo", r"C:\x"]
        );
        assert_eq!(shell_words(""), Vec::<String>::new());
        assert_eq!(shell_words("''"), [""]);

        let f = split_flags(r"-I/Program\ Files/inc -L'/opt/my lib' -lfoo -DX=1");
        assert_eq!(f.include, [PathBuf::from("/Program Files/inc")]);
        assert_eq!(f.lib_dirs, [PathBuf::from("/opt/my lib")]);
        assert_eq!(f.defines, ["X=1".parse::<Define>().unwrap()]);
    }

    #[test]
    fn a_bare_static_is_not_forwarded_to_the_linker() {
        let f = split_flags("-lfoo -static -Wl,-Bstatic");
        assert_eq!(f.ldflags, ["-Wl,-Bstatic"]);
        assert!(f.cflags.is_empty());
    }

    #[test]
    fn static_is_asked_for_only_when_the_package_is_a_static_archive() {
        use std::{cell::RefCell, collections::BTreeSet};
        let present: BTreeSet<PathBuf> = [
            "/opt/a/lib/libglfw3.a",
            "/opt/b/lib/libz.a",
            "/opt/b/lib/libz.so",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        let exists = |p: &Path| present.contains(p);
        let calls = RefCell::new(Vec::<String>::new());
        let run = |_: &str, args: &[String]| -> Result<String, String> {
            calls.borrow_mut().push(args.join(" "));
            Ok(if args.iter().any(|a| a == "glfw3") {
                if args.contains(&"--static".to_string()) {
                    "-L/opt/a/lib -lglfw3 -lX11"
                } else {
                    "-L/opt/a/lib -lglfw3"
                }
            } else {
                "-L/opt/b/lib -lz"
            }
            .to_string())
        };

        let f = find_package("glfw3", &run, &exists).unwrap();
        assert_eq!(f.libs, ["glfw3", "X11"]);
        assert_eq!(
            calls.borrow().last().unwrap(),
            "--static --cflags --libs glfw3"
        );

        calls.borrow_mut().clear();
        let f = find_package("zlib", &run, &exists).unwrap();
        assert_eq!(f.libs, ["z"]);
        assert!(
            !calls.borrow().iter().any(|c| c.contains("--static")),
            "{:?}",
            calls.borrow()
        );

        // Without a library directory there is nothing to inspect.
        assert!(!only_static_archives(&split_flags("-lz"), &exists));
    }

    #[test]
    fn falls_back_to_cmake() {
        let run = |program: &str, args: &[String]| -> Result<String, String> {
            if program == "pkg-config" {
                return Err("pkg-config is not installed or not on PATH".into());
            }
            assert_eq!(program, "cmake");
            if args.iter().any(|a| a == "-DMODE=COMPILE") {
                Ok("-I/a".into())
            } else {
                Ok("-lb".into())
            }
        };
        let f = find_package("Thing", &run, &|_| false).unwrap();
        assert_eq!(f.include, [PathBuf::from("/a")]);
        assert_eq!(f.libs, ["b"]);
    }

    #[test]
    fn pkg_config_wins_over_cmake() {
        let run = |program: &str, _: &[String]| -> Result<String, String> {
            assert_eq!(program, "pkg-config");
            Ok("-lz".into())
        };
        assert_eq!(find_package("zlib", &run, &|_| false).unwrap().libs, ["z"]);
    }

    #[test]
    fn not_found_names_both_finders() {
        let run = |_: &str, _: &[String]| -> Result<String, String> { Err("nope".into()) };
        let e = find_package("OpenGL", &run, &|_| false).unwrap_err();
        assert!(
            e.contains("pkg-config") && e.contains("cmake") && e.contains("OpenGL"),
            "{e}"
        );
    }

    #[test]
    fn cmake_answer_without_flags_is_not_found() {
        let run = |program: &str, _: &[String]| -> Result<String, String> {
            if program == "pkg-config" {
                Err("no".into())
            } else {
                Ok("Thing not found.\n".into())
            }
        };
        assert!(
            find_package("Thing", &run, &|_| false)
                .unwrap_err()
                .contains("cmake")
        );
    }

    #[test]
    fn library_directories_follow_the_linker_family() {
        let found = split_flags("-L/opt/lib -lfoo -Wl,-rpath,/opt -pthread -framework Cocoa");
        let mut diags = Diagnostics::default();

        let gcc = to_prepared(&found, true, false, "dep", &mut diags);
        assert_eq!(
            gcc.extra.link_flags,
            ["-L/opt/lib", "-Wl,-rpath,/opt", "-pthread"]
        );
        assert_eq!(
            gcc.extra.system,
            [
                SystemLib::Lib("foo".into()),
                SystemLib::Framework("Cocoa".into())
            ]
        );
        assert_eq!(gcc.extra.flags, ["-pthread"]);
        assert!(diags.items.is_empty());

        let msvc = to_prepared(&found, false, true, "dep", &mut diags);
        assert_eq!(msvc.extra.link_flags, ["/LIBPATH:/opt/lib"]);
        assert_eq!(diags.items.len(), 3);
        assert!(diags.items.iter().all(|(_, m)| m.contains("`dep`")));
    }

    #[test]
    fn every_framework_is_kept_as_a_system_library() {
        let found = split_flags("-lfoo -framework OpenGL -framework Cocoa");
        let mut diags = Diagnostics::default();
        let p = to_prepared(&found, false, false, "dep", &mut diags);
        assert_eq!(
            p.extra.system,
            [
                SystemLib::Lib("foo".into()),
                SystemLib::Framework("OpenGL".into()),
                SystemLib::Framework("Cocoa".into())
            ]
        );
        assert!(!p.extra.link_flags.iter().any(|f| f == "-framework"));
        assert!(diags.items.is_empty());
    }

    #[test]
    fn includes_are_system_includes_only_when_asked() {
        let found = split_flags("-I/x -I/x");
        let mut diags = Diagnostics::default();
        let on = to_prepared(&found, true, false, "d", &mut diags);
        let off = to_prepared(&found, false, false, "d", &mut diags);
        assert_eq!(on.include.len(), 1);
        assert!(on.include[0].system && !off.include[0].system);
    }
}
