//! The JSON compilation database (`compile_commands.json`), which clangd,
//! CLion and other editors read to understand a project without a build
//! system. One entry per compile task, with the command `build` runs.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

use serde::Serialize;

use crate::{
    build::command_for,
    executor::path_arg,
    graph::{Graph, TaskKind},
    project::Project,
};

/// The file's name, in the build directory and in `build/`.
pub const FILE: &str = "compile_commands.json";

/// One entry of the database, in the field order the format documents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// The absolute project root: the working directory of every compile.
    pub directory: String,
    /// The absolute path of the source.
    pub file: String,
    /// The absolute path of the object.
    pub output: String,
    /// The program followed by its arguments, as `build` runs them but
    /// without the arguments that write a dependency file.
    pub arguments: Vec<String>,
}

/// An absolute path as editors expect it: `..` resolved and `/` as the
/// separator on every OS.
fn absolute(root: &Path, path: &Path) -> String {
    let mut out = PathBuf::new();
    for component in root.join(path).components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    path_arg(&out).replace('\\', "/")
}

/// `args` without what makes the compiler write a dependency file, so a tool
/// that replays the command does not write one. Only the file forge asked for
/// is dropped: `-MF`, `/sourceDependencies` and `/clang:-MF` name it.
fn without_depfile_args(args: Vec<String>, depfile: &str) -> Vec<String> {
    let clang_mf = format!("/clang:-MF{depfile}");
    let mut kept = Vec::with_capacity(args.len());
    let mut skip_value = false;
    for arg in args {
        if std::mem::take(&mut skip_value) {
            continue;
        }
        match arg.as_str() {
            "-MMD" | "/clang:-MMD" => {}
            "-MF" | "/sourceDependencies" => skip_value = true,
            _ if arg == clang_mf => {}
            _ => kept.push(arg),
        }
    }
    kept
}

/// The entries of every compile task in `graph`, whether or not the task is up
/// to date, ordered by target name and then source path.
pub fn entries(project: &Project, graph: &Graph) -> Vec<Entry> {
    let directory = absolute(&project.root, Path::new(""));
    let mut entries: Vec<(&str, Entry)> = graph
        .tasks
        .iter()
        .filter(|task| task.kind == TaskKind::Compile)
        .map(|task| {
            let spec = command_for(task, project);
            let depfile = path_arg(&task.outputs[1]);
            let mut arguments = vec![spec.program];
            arguments.extend(without_depfile_args(spec.args, &depfile));
            let entry = Entry {
                directory: directory.clone(),
                file: absolute(&project.root, &task.inputs[0]),
                output: absolute(&project.root, &task.outputs[0]),
                arguments,
            };
            (project.targets[task.target].name.as_str(), entry)
        })
        .collect();
    entries.sort_by(|(a_target, a), (b_target, b)| {
        (a_target, &a.file, &a.output).cmp(&(b_target, &b.file, &b.output))
    });
    entries.into_iter().map(|(_, entry)| entry).collect()
}

/// The database as the text that is written to disk.
pub fn render(entries: &[Entry]) -> String {
    let mut text = serde_json::to_string_pretty(entries).expect("entries are plain strings");
    text.push('\n');
    text
}

/// Writes `text` unless the file holds exactly these bytes already, so an
/// editor that watches the file does not reindex for nothing. Returns whether
/// the file was written.
fn write_if_changed(file: &Path, text: &str) -> io::Result<bool> {
    if fs::read(file).is_ok_and(|old| old == text.as_bytes()) {
        return Ok(false);
    }
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(file, text)?;
    Ok(true)
}

/// Where the database is written for this invocation: in the build directory,
/// and a copy in `build/` that holds whichever was written last, which is
/// where clangd looks.
pub fn locations(project: &Project) -> [PathBuf; 2] {
    [
        project.root.join(project.build_dir()).join(FILE),
        project.root.join(project.build_root()).join(FILE),
    ]
}

/// Writes the database for the whole graph to both of its places, each only if
/// its content changed. Returns the files that were written.
pub fn write(project: &Project, graph: &Graph) -> io::Result<Vec<PathBuf>> {
    let text = render(&entries(project, graph));
    let mut written = Vec::new();
    for file in locations(project) {
        let changed = write_if_changed(&file, &text).map_err(|e| {
            io::Error::new(e.kind(), format!("cannot write {}: {e}", file.display()))
        })?;
        if changed {
            written.push(file);
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn every_depfile_spelling_is_dropped() {
        assert_eq!(
            without_depfile_args(
                s(&["-O2", "-MMD", "-MF", "d/a.o.d", "-c", "a.c"]),
                "d/a.o.d"
            ),
            s(&["-O2", "-c", "a.c"])
        );
        assert_eq!(
            without_depfile_args(
                s(&[
                    "/nologo",
                    "/sourceDependencies",
                    "d/a.obj.json",
                    "/c",
                    "a.c"
                ]),
                "d/a.obj.json"
            ),
            s(&["/nologo", "/c", "a.c"])
        );
        assert_eq!(
            without_depfile_args(
                s(&["/clang:-MMD", "/clang:-MFd/a.obj.d", "/c", "a.c"]),
                "d/a.obj.d"
            ),
            s(&["/c", "a.c"])
        );
    }

    #[test]
    fn paths_are_absolute_with_forward_slashes_and_no_dots() {
        let root = std::env::current_dir().unwrap();
        let got = absolute(&root, Path::new("../lib/./a.c"));
        let want = path_arg(&root.parent().unwrap().join("lib/a.c")).replace('\\', "/");
        assert_eq!(got, want);
        assert!(!got.contains('\\'));
    }
}
