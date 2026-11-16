//! Dependencies that live on this machine: `header-only`, `sources` and
//! `prebuilt` with a `path` source (spec 10.1, 10.2).

use std::path::{Path, PathBuf};

use crate::{
    config::{ConfigError, types::DepKind, vocab::Compiler},
    configure::{
        lookup::Alias,
        model::{Configured, UnitKind},
    },
    deps::Prepared,
    discover::{check_patterns, discover_globs, slash_relative},
    executor::path_arg,
};

/// The file names a library `name` may have, best first.
fn lib_file_names(name: &str, msvc: bool) -> Vec<String> {
    if msvc {
        vec![format!("{name}.lib")]
    } else {
        vec![
            format!("lib{name}.a"),
            format!("lib{name}.so"),
            format!("lib{name}.dylib"),
            format!("{name}.lib"),
            format!("{name}.a"),
        ]
    }
}

/// Every existing file of library `name` in `dir`, in preference order: the
/// first one is the one that gets linked.
pub fn find_lib_files(dir: &Path, name: &str, msvc: bool) -> Vec<PathBuf> {
    lib_file_names(name, msvc)
        .into_iter()
        .map(|file| dir.join(file))
        .filter(|path| path.is_file())
        .collect()
}

/// Prepares one local dependency. `header-only` has an empty result, `sources`
/// finds its files (they become a graph target, so there is no result) and
/// `prebuilt` resolves the library files it links.
pub fn prepare_local(
    c: &mut Configured,
    root: &Path,
    unit: usize,
) -> Result<Option<Prepared>, ConfigError> {
    let UnitKind::Dep(kind) = c.units[unit].kind else {
        return Err(ConfigError::invalid(
            &c.units[unit].name,
            "internal error: only dependencies are prepared",
        ));
    };
    match kind {
        DepKind::HeaderOnly => Ok(Some(Prepared::default())),
        DepKind::Sources => {
            let files = sources_of(c, root, unit)?;
            c.units[unit].sources = files;
            Ok(None)
        }
        DepKind::Prebuilt => prebuilt(c, root, unit).map(Some),
        DepKind::Cmake | DepKind::System => Err(ConfigError::invalid(
            format!("dependency.{}", c.units[unit].name),
            format!("kind `{}` is not a local dependency", kind.as_str()),
        )),
    }
}

/// The files of a `sources` dependency, relative to the project root when
/// inside it.
fn sources_of(c: &Configured, root: &Path, unit: usize) -> Result<Vec<PathBuf>, ConfigError> {
    let u = &c.units[unit];
    let field = format!("dependency.{}", u.name);
    let dep = u.dep.as_ref().expect("a dependency unit has details");
    if dep.files.is_empty() {
        return Err(ConfigError::invalid(field, "kind `sources` needs `files`"));
    }
    let found = discover_globs(root, &u.base_dir, &dep.files)?;
    if found.is_empty() {
        return Err(ConfigError::at_key(
            &field,
            "files",
            format!(
                "`files` ({}) matched no source files in {}",
                dep.files.join(", "),
                display_path(&u.base_dir)
            ),
        ));
    }
    check_patterns("files", root, &u.base_dir, &dep.files, &found)
        .map_err(|message| ConfigError::at_key(&field, "files", message))?;
    Ok(found)
}

fn prebuilt(c: &Configured, root: &Path, unit: usize) -> Result<Prepared, ConfigError> {
    let u = &c.units[unit];
    let field = format!("dependency.{}", u.name);
    let dep = u.dep.as_ref().expect("a dependency unit has details");

    // `expose.target` names are libraries too.
    let mut libs: Vec<&str> = Vec::new();
    for name in dep.libs.iter().chain(&dep.expose) {
        if !libs.contains(&name.as_str()) {
            libs.push(name);
        }
    }
    if libs.is_empty() {
        return Err(ConfigError::invalid(field, "kind `prebuilt` needs `libs`"));
    }

    let dir = match &dep.lib_dir {
        Some(rel) => u.base_dir.join(rel),
        None => u.base_dir.clone(),
    };
    if !dir.is_dir() {
        let mut message = format!("library directory does not exist: {}", display_path(&dir));
        if let Some(sourced) = u.merged.scalar("lib_dir") {
            let template = sourced
                .template
                .clone()
                .or_else(|| sourced.value.as_str().map(str::to_string))
                .unwrap_or_default();
            message.push_str(&format!(" (from \"{template}\""));
            if let Some(alias) = &dep.alias
                && let Some(note) = alias_note(alias, c, &template)
            {
                message.push_str(&format!("; {note}"));
            }
            message.push(')');
        }
        return Err(ConfigError::invalid(field, message));
    }

    let msvc = matches!(c.platform.compiler, Compiler::Msvc | Compiler::ClangCl);
    let mut prepared = Prepared::default();
    for name in libs {
        let Some(file) = find_lib_files(&dir, name, msvc).into_iter().next() else {
            return Err(ConfigError::at_key(
                field,
                "libs",
                format!(
                    "library `{name}` not found in {}; expected one of: {}",
                    display_path(&dir),
                    lib_file_names(name, msvc).join(", ")
                ),
            ));
        };
        prepared.lib_files.push(slash_relative(root, &file));
    }
    Ok(prepared)
}

/// The alias entries that rewrote a lookup in `template` on this platform,
/// e.g. `alias arch x86_64->x64`.
pub(crate) fn alias_note(alias: &Alias, c: &Configured, template: &str) -> Option<String> {
    let p = &c.platform;
    let used =
        |axis: &str| template.contains(&format!("${{{axis}}}")) || template.contains("${triplet}");
    let mut parts = Vec::new();
    if let Some(to) = alias.os.get(&p.os)
        && used("os")
    {
        parts.push(format!("os {}->{to}", p.os.as_str()));
    }
    if let Some(to) = alias.arch.get(&p.arch)
        && used("arch")
    {
        parts.push(format!("arch {}->{to}", p.arch.as_str()));
    }
    if let Some(to) = alias.compiler.get(&p.compiler)
        && used("compiler")
    {
        parts.push(format!("compiler {}->{to}", p.compiler.as_str()));
    }
    (!parts.is_empty()).then(|| format!("alias {}", parts.join(", ")))
}

/// A path for a message: no `\?\` prefix, `/` separators on every OS.
fn display_path(path: &Path) -> String {
    path_arg(path).replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), "").unwrap();
    }

    #[test]
    fn variants_come_in_preference_order() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        for f in ["foo.a", "libfoo.so", "libfoo.a", "foo.lib", "libbar.a"] {
            touch(d, f);
        }
        let names = |v: Vec<PathBuf>| -> Vec<String> {
            v.iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            names(find_lib_files(d, "foo", false)),
            ["libfoo.a", "libfoo.so", "foo.lib", "foo.a"]
        );
        assert_eq!(names(find_lib_files(d, "foo", true)), ["foo.lib"]);
        assert_eq!(names(find_lib_files(d, "bar", true)), Vec::<String>::new());
        assert!(find_lib_files(d, "baz", false).is_empty());
    }
}
