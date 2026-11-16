//! `forge-build explain`: prints the planner's decisions without running anything.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    path::{Path, PathBuf},
};

use crate::{
    build::{BuildPlan, prepare},
    config::{key::Visibility, schema::DepSource, types::DepKind},
    configure::{
        merge::Sourced,
        model::{Configured, Unit, UnitKind},
        options::OptionSource,
        root_relative,
        system_libs::SystemLib,
    },
    deps::local::alias_note,
    executor::Executor,
    graph::{Graph, GraphError, TaskId, TaskKind},
    headers,
    project::{BuildTargetKind, Project},
};

#[derive(Debug, thiserror::Error)]
pub enum ExplainError {
    #[error("`{}` is not a target of this project ({})", target.display(), listing(units, known))]
    UnknownTarget {
        target: PathBuf,
        /// The output paths of the tasks.
        known: Vec<String>,
        /// The lib, bin and dependency names, qualified. Empty when the caller
        /// only knows the graph.
        units: Vec<String>,
    },
    #[error(transparent)]
    Graph(#[from] GraphError),
}

fn listing(units: &[String], known: &[String]) -> String {
    if units.is_empty() {
        format!("targets: {}", known.join(", "))
    } else {
        format!("units: {}; outputs: {}", units.join(", "), known.join(", "))
    }
}

/// Finds the task a user-typed path refers to.
///
/// The path may be root-relative, cwd-relative, or absolute inside the root. It
/// matches any output of a task (object, depfile, executable); failing that, a
/// source file selects its compile task.
pub fn find_task(graph: &Graph, root: &Path, cwd: &Path, raw: &Path) -> Option<TaskId> {
    let candidates = [
        headers::normalize(root, &raw.to_string_lossy()),
        headers::normalize(root, &cwd.join(raw).to_string_lossy()),
    ];
    graph
        .tasks
        .iter()
        .position(|t| t.outputs.iter().any(|o| candidates.contains(o)))
        .or_else(|| {
            graph.tasks.iter().position(|t| {
                t.kind == TaskKind::Compile && t.inputs.iter().any(|i| candidates.contains(i))
            })
        })
}

/// With no target, one line per task (dependencies first). With a target, the
/// full picture for that task.
pub fn explain(
    graph: &Graph,
    plan: &BuildPlan,
    target: Option<&Path>,
    root: &Path,
    cwd: &Path,
) -> Result<String, ExplainError> {
    match target {
        None => Ok(summary(graph, plan)),
        Some(raw) => {
            let id =
                find_task(graph, root, cwd, raw).ok_or_else(|| ExplainError::UnknownTarget {
                    target: raw.to_path_buf(),
                    known: graph
                        .tasks
                        .iter()
                        .map(|t| t.outputs[0].display().to_string())
                        .collect(),
                    units: Vec::new(),
                })?;
            Ok(detail(graph, plan, id))
        }
    }
}

/// What `forge explain <arg>` prints. A lib, bin or dependency name (bare, or
/// as `lib.x`, `bin.x`, `dependency.x`) prints the merged configuration and,
/// after a blank line, the rebuild detail of the unit's archive or link task
/// if it has one. Anything else is a path, or nothing for the summary.
pub fn explain_arg(
    project: &Project,
    arg: Option<&Path>,
    cwd: &Path,
    exec: &(dyn Executor + Sync),
) -> Result<String, ExplainError> {
    let mut graph = Graph::from_project(project)?;
    let mut text = String::new();
    let mut path = arg.map(Path::to_path_buf);
    if let Some(name) = arg.and_then(Path::to_str)
        && let Some(unit) = unit_index(project, name)
    {
        text = explain_config(project, name)?;
        let task_target = project
            .targets
            .iter()
            .find(|t| t.unit == unit && t.kind != BuildTargetKind::Interface);
        let Some(target) = task_target else {
            return Ok(text);
        };
        graph = graph.select_target(project, &target.name)?;
        path = Some(project.target_output_path(target));
        text.push('\n');
    }
    let plan = prepare(&graph, project, exec);
    match explain(&graph, &plan, path.as_deref(), &project.root, cwd) {
        Ok(detail) => {
            text.push_str(&detail);
            Ok(text)
        }
        Err(ExplainError::UnknownTarget { target, known, .. }) => {
            Err(ExplainError::UnknownTarget {
                target,
                known,
                units: project
                    .configured
                    .units
                    .iter()
                    .map(qualified_name)
                    .collect(),
            })
        }
        Err(e) => Err(e),
    }
}

// ------------------------------------------------- the merged configuration

/// `lib`, `bin` or `dependency`: the table a unit is written in.
fn table_name(unit: &Unit) -> &'static str {
    match unit.kind {
        UnitKind::Lib | UnitKind::SharedLib | UnitKind::InterfaceLib => "lib",
        UnitKind::Bin => "bin",
        UnitKind::Dep(_) => "dependency",
    }
}

fn qualified_name(unit: &Unit) -> String {
    format!("{}.{}", table_name(unit), unit.name)
}

/// The unit a user-typed name refers to: `engine` or `lib.engine`.
pub fn unit_index(project: &Project, name: &str) -> Option<usize> {
    let units = &project.configured.units;
    units
        .iter()
        .position(|u| u.name == name)
        .or_else(|| units.iter().position(|u| qualified_name(u) == name))
}

/// A path with `/` separators on every OS.
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn shown(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// An `include` value as the compiler sees it: relative to the project root.
fn include_path(project: &Project, unit: &Unit, value: &str) -> String {
    let given = Path::new(value);
    if given.is_absolute() || unit.base_dir.as_os_str().is_empty() {
        return slash(given);
    }
    slash(&root_relative(&project.root, &unit.base_dir.join(given)))
}

/// The text of one list item: includes are resolved, the rest is as written.
fn item_text(project: &Project, unit: &Unit, key: &str, item: &Sourced) -> String {
    match (key, item.value.as_str()) {
        ("include", Some(value)) => include_path(project, unit, value),
        _ => shown(&item.value),
    }
}

/// `<text padded> <origin> (<key as written>)`, with the lookup it came from.
fn origin_line(text: &str, item: &Sourced) -> String {
    let mut line = format!("{text:<24} {} ({})", item.origin, item.raw_key);
    if let Some(template) = &item.template {
        let _ = write!(line, "  from \"{template}\"");
    }
    line
}

/// List keys in the order they are printed.
const LIST_ORDER: &[&str] = &[
    "sources",
    "features",
    "deps",
    "defines",
    "include",
    "flags",
    "link_flags",
    "system",
    "files",
    "libs",
];

/// The keys whose visibility matters; the others are always private.
const VISIBLE_KEYS: &[&str] = &[
    "deps",
    "defines",
    "include",
    "flags",
    "link_flags",
    "system",
];

/// Every unit whose public compile interface `unit` sees: its direct
/// dependencies, and what those export through public edges, the way
/// `effective` collects them.
fn exported_units(c: &Configured, unit: usize) -> Vec<usize> {
    let index: HashMap<&str, usize> = c
        .units
        .iter()
        .enumerate()
        .map(|(i, u)| (u.name.as_str(), i))
        .collect();
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    fn visit(
        c: &Configured,
        index: &HashMap<&str, usize>,
        unit: usize,
        seen: &mut HashSet<usize>,
        order: &mut Vec<usize>,
    ) {
        if !seen.insert(unit) {
            return;
        }
        order.push(unit);
        for (name, vis) in &c.units[unit].deps {
            if *vis == Visibility::Public
                && let Some(&dep) = index.get(name.as_str())
            {
                visit(c, index, dep, seen, order);
            }
        }
    }
    for (name, _) in &c.units[unit].deps {
        if let Some(&dep) = index.get(name.as_str()) {
            visit(c, &index, dep, &mut seen, &mut order);
        }
    }
    order
}

fn system_text(lib: &SystemLib) -> String {
    match lib {
        SystemLib::Lib(name) => name.clone(),
        SystemLib::Framework(name) => format!("framework {name}"),
    }
}

/// The merged configuration of one lib, bin or dependency for this platform
/// and profile, with the table and key every value came from (spec 11).
pub fn explain_config(project: &Project, name: &str) -> Result<String, ExplainError> {
    let c = &project.configured;
    let index = unit_index(project, name).ok_or_else(|| ExplainError::UnknownTarget {
        target: PathBuf::from(name),
        known: Vec::new(),
        units: c.units.iter().map(qualified_name).collect(),
    })?;
    let unit = &c.units[index];
    let is_dep = matches!(unit.kind, UnitKind::Dep(_));
    let mut out = String::new();

    let kind = match unit.kind {
        UnitKind::Lib => "lib".to_string(),
        UnitKind::SharedLib => "lib, shared".to_string(),
        UnitKind::InterfaceLib => "lib, header-only".to_string(),
        UnitKind::Bin => "bin".to_string(),
        UnitKind::Dep(kind) => kind.as_str().to_string(),
    };
    let _ = writeln!(
        out,
        "{}  [{kind}]  {}  profile {}",
        qualified_name(unit),
        c.platform.triplet(),
        c.profile
    );

    if !is_dep && !c.options.is_empty() {
        out.push_str("  options:\n");
        for (option, resolved) in &c.options {
            let source = match resolved.source {
                OptionSource::Cli => "--opt",
                OptionSource::Cache => "cache",
                OptionSource::Default => "default",
            };
            let _ = writeln!(out, "    {option} = {} ({source})", resolved.value);
        }
    }

    for key in LIST_ORDER {
        for vis in [Visibility::Private, Visibility::Public] {
            let items = unit.merged.list(key, vis);
            if items.is_empty() {
                continue;
            }
            if VISIBLE_KEYS.contains(key) {
                let _ = writeln!(out, "  {key} ({}):", vis.as_str());
            } else {
                let _ = writeln!(out, "  {key}:");
            }
            for item in items {
                let text = item_text(project, unit, key, item);
                let _ = writeln!(out, "    {}", origin_line(&text, item));
            }
        }
    }

    for (key, item) in &unit.merged.scalars {
        match unit.dep.as_ref().filter(|_| key == "lib_dir") {
            Some(dep) => {
                let dir = shown(&item.value);
                let path = slash(&root_relative(&project.root, &unit.base_dir.join(&dir)));
                let _ = write!(out, "  lib_dir = {path}  ");
                match &item.template {
                    Some(template) => {
                        let _ = write!(out, " from \"{template}\"");
                        if let Some(note) = dep
                            .alias
                            .as_ref()
                            .and_then(|alias| alias_note(alias, c, template))
                        {
                            let _ = write!(out, " ({note})");
                        }
                    }
                    None => {
                        let _ = write!(out, "{}", item.origin);
                    }
                }
                out.push('\n');
            }
            None => {
                let head = format!("{key} = {}", shown(&item.value));
                let _ = write!(out, "  {head:<26} {}", item.origin);
                if let Some(template) = &item.template {
                    let _ = write!(out, "  from \"{template}\"");
                }
                out.push('\n');
            }
        }
    }

    if let Some(dep) = &unit.dep {
        match &dep.source {
            DepSource::Git { url, rev } => {
                let _ = writeln!(out, "  source = git {url} @ {rev}");
            }
            DepSource::Path(path) => {
                let _ = writeln!(out, "  source = path {}", slash(path));
            }
            DepSource::Host => {}
        }
        if let Some(find) = &dep.find {
            let _ = writeln!(out, "  find = {find}");
        }
        if !dep.expose.is_empty() {
            let _ = writeln!(out, "  expose = {}", dep.expose.join(", "));
        }
        found_block(&mut out, project, index);
    }

    if !is_dep {
        from_deps(&mut out, project, index);
    }
    Ok(out)
}

/// What a dependency's preparation found or built, which the manifest does not say.
fn found_block(out: &mut String, project: &Project, index: usize) {
    let unit = &project.configured.units[index];
    let UnitKind::Dep(kind) = unit.kind else {
        return;
    };
    let prepared = project.prepared.get(&index);
    if kind == DepKind::Cmake && prepared.is_none_or(|p| p.not_built) {
        out.push_str("  not built yet (forge build builds it)\n");
        return;
    }
    let Some(p) = prepared else {
        return;
    };
    let mut lines: Vec<String> = Vec::new();
    lines.extend(
        p.include
            .iter()
            .map(|i| format!("include   {}", slash(&i.path))),
    );
    lines.extend(
        p.lib_files
            .iter()
            .map(|f| format!("lib       {}", slash(f))),
    );
    lines.extend(p.lib_dirs.iter().map(|d| format!("lib_dir   {}", slash(d))));
    lines.extend(p.libs.iter().map(|l| format!("libs      {l}")));
    lines.extend(p.extra.defines.iter().map(|d| format!("define    {d}")));
    lines.extend(p.extra.flags.iter().map(|f| format!("flag      {f}")));
    lines.extend(p.extra.link_flags.iter().map(|f| format!("link_flag {f}")));
    lines.extend(
        p.extra
            .system
            .iter()
            .map(|l| format!("system    {}", system_text(l))),
    );
    if lines.is_empty() {
        return;
    }
    out.push_str(if kind == DepKind::Cmake {
        "  built:\n"
    } else {
        "  found:\n"
    });
    for line in lines {
        let _ = writeln!(out, "    {line}");
    }
}

/// The public compile-side values that the dependencies pass on to this unit.
fn from_deps(out: &mut String, project: &Project, index: usize) {
    let c = &project.configured;
    let mut lines: Vec<String> = Vec::new();
    let mut seen: HashSet<(&str, String)> = HashSet::new();
    for dep_index in exported_units(c, index) {
        let dep = &c.units[dep_index];
        for key in ["defines", "include", "flags"] {
            for item in dep.merged.list(key, Visibility::Public) {
                let text = item_text(project, dep, key, item);
                if seen.insert((key, text.clone())) {
                    let label = format!("{}: {text}", dep.name);
                    lines.push(origin_line(&label, item));
                }
            }
        }
        // Found by preparing a cmake or system dependency: no manifest key.
        if let Some(p) = project.prepared.get(&dep_index) {
            let found = p
                .include
                .iter()
                .map(|i| ("include", slash(&i.path)))
                .chain(p.extra.defines.iter().map(|d| ("defines", d.to_string())))
                .chain(p.extra.flags.iter().map(|f| ("flags", f.clone())));
            for (key, text) in found {
                if seen.insert((key, text.clone())) {
                    let label = format!("{}: {text}", dep.name);
                    lines.push(format!("{label:<24} found ({key})"));
                }
            }
        }
    }
    if lines.is_empty() {
        return;
    }
    out.push_str("  from deps:\n");
    for line in lines {
        let _ = writeln!(out, "    {line}");
    }
}

fn summary(graph: &Graph, plan: &BuildPlan) -> String {
    let mut out = String::new();
    for &id in graph.order() {
        let name = graph.tasks[id].outputs[0].display();
        match plan.dirty.get(&id) {
            Some(reason) => writeln!(out, "{name}: rebuild: {reason}").unwrap(),
            None => writeln!(out, "{name}: up to date").unwrap(),
        }
    }
    let n = plan.dirty.len();
    if n == 0 {
        out.push_str("\nNothing to do.\n");
    } else {
        writeln!(out, "\n{n} of {} tasks need to run.", graph.tasks.len()).unwrap();
    }
    out
}

fn list(paths: impl Iterator<Item = impl AsRef<Path>>) -> String {
    paths
        .map(|p| p.as_ref().display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn detail(graph: &Graph, plan: &BuildPlan, id: TaskId) -> String {
    let task = &graph.tasks[id];
    let kind = match task.kind {
        TaskKind::Compile => "compile",
        TaskKind::Archive => "archive",
        TaskKind::Link => "link",
    };
    let mut out = String::new();
    writeln!(
        out,
        "{}  [{kind} {}]",
        task.outputs[0].display(),
        task.lang.name()
    )
    .unwrap();
    match plan.dirty.get(&id) {
        Some(reason) => {
            writeln!(out, "  status:   rebuild").unwrap();
            writeln!(out, "  reason:   {reason}").unwrap();
        }
        None => writeln!(out, "  status:   up to date").unwrap(),
    }
    writeln!(out, "  inputs:   {}", list(task.inputs.iter())).unwrap();
    if !task.implicit_inputs.is_empty() {
        writeln!(out, "  implicit: {}", list(task.implicit_inputs.iter())).unwrap();
    }
    // headers found by the compiler last time, if the task has a record
    if let Some(rec) = plan.db.tasks.get(&task.outputs[0]) {
        let extra: Vec<&PathBuf> = rec
            .inputs
            .keys()
            .filter(|p| !rec.declared_inputs.contains(p))
            .collect();
        if !extra.is_empty() {
            writeln!(out, "  headers:  {}", list(extra.into_iter())).unwrap();
        }
    }
    writeln!(out, "  outputs:  {}", list(task.outputs.iter())).unwrap();
    let spec = &plan.specs[id];
    writeln!(out, "  command:  {} {}", spec.program, spec.args.join(" ")).unwrap();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        database::Database, executor::CommandSpec, graph::Task, lang::Lang, planner::DirtyReason,
    };
    use std::collections::BTreeMap;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// compile(src/a.cpp) -> obj/a.o + dep/a.d ; link(obj/a.o) -> app
    fn graph() -> Graph {
        Graph::new(vec![
            Task {
                kind: TaskKind::Compile,
                target: 0,
                lang: Lang::Cxx,
                inputs: vec![p("src/a.cpp")],
                implicit_inputs: vec![],
                outputs: vec![p("build/debug/obj/a.o"), p("build/debug/dep/a.d")],
            },
            Task {
                kind: TaskKind::Link,
                target: 0,
                lang: Lang::Cxx,
                inputs: vec![p("build/debug/obj/a.o")],
                implicit_inputs: vec![],
                outputs: vec![p("build/debug/app")],
            },
        ])
        .unwrap()
    }

    fn plan(dirty: &[(TaskId, DirtyReason)]) -> BuildPlan {
        let spec = |prog: &str| CommandSpec {
            program: prog.into(),
            args: vec!["-c".into()],
            cwd: p("."),
        };
        BuildPlan {
            db: Database::new(),
            specs: vec![spec("g++"), spec("g++")],
            hashes: vec![[0; 32]; 2],
            dirty: dirty.iter().cloned().collect::<BTreeMap<_, _>>(),
        }
    }

    /// An absolute path on every OS: Windows needs a drive letter.
    fn abs(path: &str) -> String {
        if cfg!(windows) {
            format!("C:{path}")
        } else {
            path.to_string()
        }
    }

    #[test]
    fn target_may_be_given_in_any_form() {
        let g = graph();
        let root_dir = abs("/proj");
        let root = Path::new(&root_dir);
        let find = |cwd: &str, raw: &str| find_task(&g, root, Path::new(cwd), Path::new(raw));
        let proj = |rest: &str| abs(&format!("/proj/{rest}"));

        assert_eq!(find(&root_dir, "build/debug/obj/a.o"), Some(0)); // root-relative
        assert_eq!(find(&root_dir, "./build/debug/obj/a.o"), Some(0)); // with ./
        assert_eq!(find(&proj("src"), "../build/debug/obj/a.o"), Some(0)); // cwd-relative
        assert_eq!(find(&abs("/elsewhere"), &proj("build/debug/app")), Some(1)); // absolute
        assert_eq!(find(&root_dir, "build/debug/dep/a.d"), Some(0)); // any output counts
    }

    #[test]
    fn a_source_file_selects_its_compile_task() {
        assert_eq!(
            find_task(
                &graph(),
                Path::new("/proj"),
                Path::new("/proj"),
                Path::new("src/a.cpp")
            ),
            Some(0)
        );
    }

    #[test]
    fn unknown_target_lists_the_valid_ones() {
        let e = explain(
            &graph(),
            &plan(&[]),
            Some(Path::new("nope")),
            Path::new("/proj"),
            Path::new("/proj"),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("nope") && e.contains("build/debug/app"), "{e}");
    }

    #[test]
    fn summary_shows_reasons_and_counts() {
        let pl = plan(&[
            (0, DirtyReason::InputChanged(p("include/a.hpp"))),
            (1, DirtyReason::InputRebuilt(p("build/debug/obj/a.o"))),
        ]);
        let text = explain(&graph(), &pl, None, Path::new("/proj"), Path::new("/proj")).unwrap();
        assert!(
            text.contains("build/debug/obj/a.o: rebuild: include/a.hpp changed"),
            "{text}"
        );
        assert!(
            text.contains("build/debug/app: rebuild: build/debug/obj/a.o was rebuilt"),
            "{text}"
        );
        assert!(text.contains("2 of 2 tasks need to run."), "{text}");
    }

    #[test]
    fn clean_summary_says_nothing_to_do() {
        let text = explain(
            &graph(),
            &plan(&[]),
            None,
            Path::new("/proj"),
            Path::new("/proj"),
        )
        .unwrap();
        assert!(
            text.contains("build/debug/app: up to date") && text.contains("Nothing to do."),
            "{text}"
        );
    }

    #[test]
    fn detail_shows_status_inputs_outputs_and_command() {
        let pl = plan(&[(0, DirtyReason::OutputMissing(p("build/debug/obj/a.o")))]);
        let text = explain(
            &graph(),
            &pl,
            Some(Path::new("build/debug/obj/a.o")),
            Path::new("/proj"),
            Path::new("/proj"),
        )
        .unwrap();
        for want in [
            "[compile C++]",
            "status:   rebuild",
            "output missing: build/debug/obj/a.o",
            "src/a.cpp",
            "command:  g++ -c",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
    }
}
