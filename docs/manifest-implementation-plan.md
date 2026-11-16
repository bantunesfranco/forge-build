# Manifest Spec 0.3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **First action after approval:** copy this file to `docs/manifest-implementation-plan.md` (the user asked for the plan in `./docs`) and commit it together with Task 0.

**Goal:** Replace forge-build's current hybrid manifest (legacy `[project]/[build]` plus a half-finished package format) with `project.toml` as defined in `docs/manifest-spec.md` (draft 0.3). That covers the key grammar, conditions, merging, lookups, aliases, options, features, profiles, logical system libraries, the five dependency kinds, the lockfile, cross-target selection, and `explain <target>`.

**Context:** Today all compile flags are global: one `resolve::Resolved` per profile is shared by every target (`src/resolve.rs`). Dependencies are merged into that global state by mutating the manifest (`src/dependencies.rs::prepare`). `[workspace]`, `option`, `feature` and `root` are parsed but only flattened into the legacy model (`src/config/manifest.rs::normalize`). The spec needs per-target settings with visibility propagation, so the core of this work is a new configure pipeline that produces per-target compile and link arguments. The existing incremental engine is kept unchanged: graph, planner, database, executor, depfiles, fingerprints.

**Architecture:** The pipeline is `flat TOML reader → schema (typed tables of key entries) → configure (platform → options → features → layered merge with lookups → per-unit model → propagation) → flags mapping → Project/Graph`. A compile/link `Task` carries a `target` index, and `command_for` reads that target's own arguments. Dependencies become units of the same model: `sources` deps are ordinary static-library targets in the graph, while `cmake`/`prebuilt`/`system`/`header-only` deps contribute an interface plus external library files.

**Tech Stack:** Rust 2024 (MSRV 1.89), `toml` 1.x, `toml_parser` 1.1.5 (new direct dependency, already in `Cargo.lock`), `serde_json` (CMake File API), `walkdir`, `thiserror`, `tempfile` (tests), git and cmake CLIs at runtime.

**Spec:** `docs/manifest-spec.md` (draft 0.3). Executors read both documents. Section numbers below (§4.3 and so on) refer to the spec.

## Decisions already made (with the user)

1. **Flat-key reader.** The spec grammar (`defines` next to `defines.public`, `default` next to `default.arm64`, `options` next to `options.linux`) is not valid TOML, and the `toml` crate rejects it. We keep the spec syntax. `toml_parser` lexes the file, every dotted key in a table body is rewritten into one quoted key (`"defines.public.macos"`), and the result is parsed with `toml`. Table headers and inline tables keep normal TOML meaning. Consequence: the files are lexically TOML only, and generic TOML tools will flag them.
2. **Clean break.** Only spec 0.3 is accepted. The following are removed: `[project]`, `[build]`, `[build.flags]`, `edition`, `[package]`, `[lang]`, `[tools]`, `[dependencies.*]`, profile `inherits`, `ListEdit` add/remove/set, `prefer`/`resolve`/pkg-config finders, url/tar sources, and the make/meson/autotools/steps/self backends. Any of these tables now gets the unknown-table error. Examples and test fixtures are migrated.

## Spec gaps resolved by this plan (veto any before execution)

| Gap | Resolution |
|---|---|
| How a `lib` is header-only (§6.3, §12) | A `lib` with no `sources` key and at least one `include` key is an **interface lib**: no archive task, and consumers compile its headers with their own `opt` (§8). A `lib` with neither key is an error. A `bin` always needs `sources`. A `sources` key that matches no file is an error. |
| Which keys a feature may hold | `when`, plus the list keys `defines`, `include`, `flags`, `link_flags`, `system`, `deps` with any suffixes. A scalar inside a feature is an error. A feature's keys merge into the target layer right after the target's own keys, in `features` list order. |
| Visibility on link-side keys | Static archives need everything at the final link. `system`, `link_flags` and library artifacts therefore always reach the link of every `bin` that depends on the unit, directly or transitively. Visibility controls the compile side only (`defines`, `include`, `flags`) and whether a `deps` entry's compile interface is re-exported. |
| Built-in profiles (§8) | `debug` = {opt none, debug_info true, asserts true, lto false}. `release` = {max, false, false, true}. `relwithdebinfo` = {speed, true, false, false}. A manifest `[profile.<builtin>]` overlays its keys on the built-in values. Default profile: `debug`. |
| `asserts` mapping | `asserts = false` adds the define `NDEBUG` to the profile layer. |
| Option cache (§7.1) | Stored in `build/options.toml`. Each `--opt` value is written into it, and later runs reuse it (CLI > cache > default). A cache entry for an undeclared option produces a warning and is dropped. An invalid cached value is an error that names the file. |
| Variant feature with no sub-table for the selected value | No-op. §13 omits `sse42`/`avx` on purpose. A sub-table whose name is not one of the option's values is an error. |
| `when` mismatch | On a variant selected by CLI or cache: error (§7.3). On a variant selected by the default: also an error, with the hint "add `default.<arch>`". On an always-on bundle: the bundle is skipped. |
| `remove.*` matching | Exact string match, within the same visibility list. A remove key whose condition does not match is skipped. An unconditional remove of an absent value is an error whose message suggests reusing the condition suffix. |
| Dependency sources (§10.2) | `system` kind: neither `git` nor `path`. Every other kind: exactly one of them. `rev` is required with `git` and forbidden with `path`. Path deps are not locked. Archive hashing is deferred, because the spec has no archive source. |
| `system` kind `find` | Try `pkg-config <find>` first, then `cmake --find-package -DNAME=<find>`. Reuse today's code in `src/dependencies.rs` (`pkg_config`, the CMake arm of `find_system`). |
| `cmake` includes and libs | Libraries come from the CMake File API (codemodel v2) artifacts of each `expose.target`, plus the static-library targets they depend on. No install step is needed (glfw sets `GLFW_INSTALL=false`). Includes come from the dep's `include` keys if it declares any. Otherwise they are the File API compile-group includes of the exposed targets that lie inside the source or build dir. |
| `expose` on `prebuilt` | `expose.target` names are added to `libs`. |
| Dependency keys | `kind git rev path root alias find system_headers expose options files libs lib_dir features`, plus `include defines flags link_flags system deps` with suffixes. `sources`, `warnings`, `opt` and `output` are errors on a dependency. |
| Workspace keys | `name version c cxx requires`, plus every target key except `root`, `output` and `subsystem` (error). `requires` accepts only `tool`, compared against `CARGO_PKG_VERSION`. |
| Build dir with `--target` | `build/<profile>` for the host. `build/<arch>-<os>/<profile>` when `--target` is given. |
| `explain <name>` vs the existing `explain <path>` | A lib, bin or dependency name prints the merged configuration (§11), followed by the rebuild detail of that unit's archive or link task. A path, or no argument, keeps today's behavior. |
| Paths | Absolute paths and `..` are accepted in `include`, `lib_dir` and dependency `path`. Glob results are stored relative to the project root. |

## Global Constraints

- Manifest file `project.toml`, lockfile `project.lock`, both next to each other (§2).
- Unknown tables, keys, condition names, lookups → **error** (§2, §12, guardrail 10).
- Vocabulary is exactly: os `windows linux macos bsd`; compiler `msvc gcc clang apple_clang clang_cl` + groups `gcc_like` (gcc, clang, apple_clang), `msvc_like` (msvc, clang_cl); arch `x86_64 x86 arm64 arm` (§4.2).
- Lookups are exactly `${option.<name>} ${os} ${arch} ${compiler} ${profile} ${triplet}`; `${triplet}` = `<arch>-<os>-<compiler>` (§4.5).
- Abstract mapping tables in §6.4 and §8 (`opt`) are copied verbatim into `src/flags.rs`.
- Logical system library table §9 is copied verbatim into `src/configure/system_libs.rs`.
- `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked --all-targets` must pass at the end of every task (CI: `.github/workflows/ci.yml`, ubuntu + macos; dev machine is Windows/MSVC).
- No new crates except `toml_parser = "1.1.5"`.
- CLI diagnostics print as `error: …`, `warning: …`, `info: …`.
- Commit messages: one imperative sentence, ending with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Unconditional `remove.*` of a value only added under another platform's condition.** Example: workspace `defines.public.windows = ["NOMINMAX"]`, target `remove.defines.public = ["NOMINMAX"]`, built on Linux. A person expects a message that says to use `remove.defines.public.windows`. Test: Task 8 `remove_absent_value_hints_condition`.
2. **`root` + `**` globs on Windows.** Results must be `engine/srcs/a.cpp` with forward slashes, the same on every OS, so object paths and command hashes stay stable. Test: Task 9 `root_prefixes_globs_with_forward_slashes`.
3. **A lookup that expands into a bad flag.** For example a string option `"-O2 -g"` used in `flags`. The person expects the whitespace error that names the key, not a broken compile. Test: Task 9 `flags_checked_after_lookup_expansion`.
4. **Header-only `lib` consumed by a `bin`.** The person expects no archive task, the bin compiling with the lib's include dir, and no `opt` coming from the lib. Test: Task 11 `interface_lib_has_no_archive_and_exports_includes`.
5. **LTO with static archives.** `release` turns `lto` on by default, and clang thin-LTO archives on Linux need `llvm-ar`. The person expects a working release build or a clear "install llvm-ar" error. Test: Task 10 `archiver_selection_for_lto`.

---

## File Structure

| Path | Responsibility | Status |
|---|---|---|
| `src/config.rs` | `find_manifest`, `load(path) -> Manifest`, `ConfigError`, `Diagnostics` | modify |
| `src/config/flat.rs` | dotted-key → flat-key TOML reading | create |
| `src/config/vocab.rs` | `Os Arch Compiler CompilerCond Condition Platform` | create |
| `src/config/key.rs` | key grammar `key[.visibility][.condition]`, `remove.` prefix | create |
| `src/config/schema.rs` | typed manifest: tables of `Entry`, allowed-key rules per table kind | create (replaces `manifest.rs`) |
| `src/config/types.rs` | `Define`, `Opt`, `Warnings`, `Subsystem`, `DepKind`, `check_flag` | modify (drop `ListEdit`, `OptLevel`, `Prefer`, `FindSpec`, `Source`, `DepBuild`, `Expose`) |
| `src/platform.rs` | host os/arch, `--target` parsing, compiler classification, toolchain file, combination checks | create |
| `src/configure/mod.rs` | `configure()` orchestration (§11 steps 2–6) | create |
| `src/configure/lookup.rs` | `${…}` expansion + `Alias` | create |
| `src/configure/options.rs` | option resolution + cache | create |
| `src/configure/features.rs` | variant selection, `when` guards | create |
| `src/configure/merge.rs` | layered/conditional merge, `remove.*`, provenance | create |
| `src/configure/system_libs.rs` | §9 table | create |
| `src/configure/model.rs` | `Unit` per lib/bin/dependency, validation, propagation | create |
| `src/flags.rs` | abstract settings → compiler/linker/archiver args | create (replaces `resolve.rs`) |
| `src/deps/{mod,local,lockfile,git,cmake,system}.rs` | dependency preparation per kind | create (replaces `dependencies.rs`) |
| `src/project.rs` | `Project::load`, build layout, `BuildTarget` with per-target args | rewrite |
| `src/graph.rs`, `src/build.rs`, `src/explain.rs`, `src/cli.rs` | per-target tasks/commands, config explain, new flags | modify |
| `src/profile.rs`, `src/resolve.rs`, `src/dependencies.rs`, `src/config/manifest.rs` | legacy | delete (Task 11) |
| `tests/common/mod.rs` + all `tests/*.rs` | new-format fixtures | rewrite (Task 11) |
| `tests/fixtures/spec_example.toml` | §13 verbatim (`[dependency.*]` git deps included) | create (Task 1) |
| `examples/*/project.toml`, `dev/test_examples.py`, `README.md` | migrated | Tasks 11, 12, 18 |

---

### Task 0: Baseline

**Files:** none changed.

- [ ] **Step 1:** Ask the user to commit or stash the uncommitted MSVC work currently in the tree (`src/build.rs`, `src/executor.rs`, `src/resolve.rs`, …). Do not commit it on their behalf.
- [ ] **Step 2:** Run `cargo test --all-targets 2>&1 | tail -30` and record which tests fail. The working copy renamed `MANIFEST_NAME` to `project.toml` while the fixtures still write `Forge.toml`, so failures in `tests/` are expected until Task 11. Write the list into the commit message of Task 1.
- [ ] **Step 3:** Branch `feat/manifest-0.3` from the current branch.

---

### Task 1: Flat-key TOML reader

**Files:**
- Create: `src/config/flat.rs`, `tests/fixtures/spec_example.toml` (§13 code block verbatim, with the commented `foo`/`bar` deps uncommented)
- Modify: `src/config.rs` (add `pub mod flat;`), `Cargo.toml` (`toml_parser = "1.1.5"`)

**Interfaces:**
- Produces: `pub fn parse_flat(src: &str) -> Result<toml::Table, FlatError>`. `pub struct FlatError { pub line: usize, pub message: String }` implements `Display` as `line {line}: {message}`.

- [ ] **Step 1: Write failing unit tests in `src/config/flat.rs`**

```rust
#[test] fn flat_keys_coexist() {
    let t = parse_flat("defines = [\"A\"]\ndefines.public = [\"B\"]\ndefines.windows = [\"C\"]\ndefines.public.macos = [\"D\"]\n").unwrap();
    assert_eq!(t.keys().cloned().collect::<Vec<_>>(), ["defines", "defines.public", "defines.public.macos", "defines.windows"]);
    assert_eq!(t["defines.public.macos"].as_array().unwrap()[0].as_str(), Some("D"));
}
#[test] fn headers_stay_nested() {
    let t = parse_flat("[feature.simd.avx2]\ndefines.public = [\"X\"]\n").unwrap();
    assert!(t["feature"]["simd"]["avx2"].get("defines.public").is_some());
}
#[test] fn inline_tables_untouched() {
    let t = parse_flat("alias = { arch = { x86_64 = \"x64\" } }\n").unwrap();
    assert_eq!(t["alias"]["arch"]["x86_64"].as_str(), Some("x64"));
}
#[test] fn scalar_and_suffixed_scalar_coexist() {
    let t = parse_flat("default = \"avx2\"\ndefault.arm64 = \"neon\"\noptions = { A = false,\n  B = true }\noptions.linux = { C = true }\n").unwrap();
    assert_eq!(t["default.arm64"].as_str(), Some("neon"));
    assert!(t["options.linux"].is_table());
}
#[test] fn strings_and_comments_with_dots_untouched() {
    let t = parse_flat("a = \"x.y\" # c.d\n").unwrap();
    assert_eq!(t["a"].as_str(), Some("x.y"));
}
#[test] fn duplicate_flat_key_is_error() { assert!(parse_flat("a.b = 1\na.b = 2\n").is_err()); }
#[test] fn quoted_segment_with_dot_is_error() {
    assert!(parse_flat("\"a.b\".c = 1\n").unwrap_err().message.contains("`.`"));
}
#[test] fn error_line_is_preserved() { assert_eq!(parse_flat("a = 1\nb.c = 2\nd = [\n").unwrap_err().line, 3); }
#[test] fn spec_example_parses() {
    parse_flat(include_str!("../../tests/fixtures/spec_example.toml")).unwrap();
}
```

- [ ] **Step 2:** Run `cargo test --lib config::flat`. Expected: FAIL (`parse_flat` not defined).
- [ ] **Step 3: Implement `parse_flat`.** Lex with `toml_parser::Source::new(src).lex().into_vec()`. Collect events with `toml_parser::parser::parse_document(&tokens, &mut |e: Event| events.push(e), &mut errors)`. Any parse error becomes a `FlatError` (the line is the 1-based count of `\n` before the error span start). Walk the events while tracking `in_header` (StdTable/ArrayTable open..close), `inline_depth` and `array_depth`. When all three are zero, a run of `SimpleKey (KeySep SimpleKey)+` ending at `KeyValSep` is one dotted key. Decode each segment with `Source::get(span).decode_key`. A decoded segment that contains `.` is an error. Record `(span of first key .. span of last key, "\"seg1.seg2…\"")`. Splice the replacements from the last one to the first, then `toml::from_str::<toml::Table>`. Map `toml::de::Error::span()` back to a line. The splice never adds newlines, so line numbers stay the same.
- [ ] **Step 4:** Run `cargo test --lib config::flat`. Expected: PASS.
- [ ] **Step 5:** Commit: `git add Cargo.toml Cargo.lock src/config.rs src/config/flat.rs tests/fixtures/spec_example.toml && git commit -m "Add flat-key TOML reader for the manifest key grammar"`.

---

### Task 2: Vocabulary and key grammar

**Files:** Create `src/config/vocab.rs`, `src/config/key.rs`. Modify `src/config.rs` (mods).

**Interfaces:**
- Produces (vocab): `enum Os { Windows, Linux, Macos, Bsd }`, `enum Arch { X86_64, X86, Arm64, Arm }`, `enum Compiler { Msvc, Gcc, Clang, AppleClang, ClangCl }`, `enum CompilerCond { Is(Compiler), GccLike, MsvcLike }`, `enum Condition { Os(Os), Compiler(CompilerCond), Arch(Arch) }`, and `#[derive(Clone, Copy)] struct Platform { pub os: Os, pub arch: Arch, pub compiler: Compiler }`. Each enum has `FromStr`, `fn as_str(self) -> &'static str` (the vocabulary spelling) and `const ALL: &[Self]`. Also `Condition::parse(&str) -> Option<Condition>`, `Condition::matches(&self, &Platform) -> bool`, `Condition::rank(&self) -> u8` (os 1, compiler group 2, compiler member 3, arch 4; unconditional is 0 at the call site), `CompilerCond::matches(Compiler) -> bool`, and `Platform::triplet(&self) -> String`.
- Produces (key): `enum Visibility { Private, Public }`, `struct Key { pub name: String, pub visibility: Visibility, pub condition: Option<Condition>, pub remove: bool }`, `#[derive(Clone, Copy)] struct KeyRule { pub visibility: bool, pub condition: bool, pub list: bool }`, and `fn parse_key(raw: &str, rule_for: impl Fn(&str) -> Option<KeyRule>) -> Result<Key, String>`.

- [ ] **Step 1: Write failing tests** (in `key.rs`; `r` returns rules for `defines` {vis, cond, list}, `sources` {cond, list}, `warnings` {cond}, `alias` {}):

```rust
#[test] fn parses_all_forms() {
    assert_eq!(parse_key("defines", r).unwrap().visibility, Visibility::Private);
    let k = parse_key("defines.public.macos", r).unwrap();
    assert_eq!((k.visibility, k.condition), (Visibility::Public, Some(Condition::Os(Os::Macos))));
    assert!(parse_key("remove.defines.windows", r).unwrap().remove);
    assert_eq!(parse_key("defines.gcc_like", r).unwrap().condition, Some(Condition::Compiler(CompilerCond::GccLike)));
}
#[test] fn rejects_two_conditions() { assert!(parse_key("defines.windows.x86_64", r).unwrap_err().contains("one condition")); }
#[test] fn rejects_visibility_where_not_allowed() { assert!(parse_key("sources.public", r).unwrap_err().contains("does not accept visibility")); }
#[test] fn rejects_wrong_order() { assert!(parse_key("defines.windows.public", r).unwrap_err().contains("before the condition")); }
#[test] fn rejects_unknown_condition() { assert!(parse_key("defines.linuxx", r).unwrap_err().contains("unknown condition `linuxx`")); }
#[test] fn rejects_unknown_key() { assert!(parse_key("defnes", r).unwrap_err().contains("unknown key `defnes`")); }
#[test] fn rejects_suffix_on_alias() { assert!(parse_key("alias.windows", r).is_err()); }
#[test] fn remove_only_on_lists() { assert!(parse_key("remove.warnings", r).unwrap_err().contains("not a list")); }
#[test] fn matching_and_rank() {
    let p = Platform { os: Os::Linux, arch: Arch::X86_64, compiler: Compiler::Clang };
    assert!(Condition::parse("gcc_like").unwrap().matches(&p));
    assert!(!Condition::parse("msvc_like").unwrap().matches(&p));
    assert!(Condition::parse("gcc_like").unwrap().rank() < Condition::parse("clang").unwrap().rank());
    assert_eq!(p.triplet(), "x86_64-linux-clang");
}
```

- [ ] **Step 2:** `cargo test --lib config::key config::vocab` → FAIL.
- [ ] **Step 3:** Implement. Split `raw` on `.`. A leading `remove` sets `remove`. The next segment is the name, checked through `rule_for`. An optional `public`/`private` segment follows. An optional condition follows that. Any extra segment that is itself a condition gives the "one condition" error, a visibility segment after a condition gives the "before the condition" error, and anything else gives an unknown-condition error. The axis is inferred from the name, because the axis vocabularies are disjoint.
- [ ] **Step 4:** Tests PASS.
- [ ] **Step 5:** Commit `"Add condition vocabulary and manifest key grammar"`.

---

### Task 3: Schema: typed tables and per-table key rules

**Files:** Create `src/config/schema.rs`. Modify `src/config/types.rs`: add `Opt { None, Size, Speed, Max }`, `Warnings { Off, Default, High, All }`, `Subsystem { Console, Windows }` and `DepKind { HeaderOnly, Sources, Cmake, Prebuilt, System }`, each with `FromStr` using the spec spellings (`"header-only"`); move `check_flag` here from `resolve.rs`. Modify `src/config.rs`: `load(path) -> Result<Manifest, ConfigError>` = read + `parse_flat` + `parse_manifest`; add `Diagnostics`.

**Interfaces:**
- Consumes: `parse_flat`, `parse_key`, `KeyRule`, vocab types.
- Produces:
  - `struct Entry { pub key: Key, pub raw_key: String, pub value: toml::Value }`
  - `struct Table { pub origin: String /* e.g. "lib.engine", "profile.release", "feature.simd.avx2" */, pub entries: Vec<Entry> }` with `fn scalar(&self, name: &str) -> impl Iterator<Item=&Entry>`
  - `struct Workspace { pub name: String, pub version: String, pub c: Option<String>, pub cxx: Option<String>, pub requires: BTreeMap<String,String>, pub settings: Table }`
  - `enum OptionType { Bool, Enum, String, Int }`, `struct OptionDecl { pub ty: OptionType, pub values: Vec<String>, pub defaults: Vec<(Option<Condition>, toml::Value)>, pub doc: Option<String> }`
  - `struct When { pub os: Option<Os>, pub compiler: Option<CompilerCond>, pub arch: Option<Arch> }` with `fn matches(&self, &Platform) -> bool`
  - `struct FeatureBody { pub when: When, pub settings: Table }`, `enum FeatureDecl { Bundle(FeatureBody), Variants(BTreeMap<String, FeatureBody>) }`
  - `enum DepSource { Git { url: String, rev: String }, Path(PathBuf), Host }`
  - `struct DependencyDecl { pub kind: DepKind, pub source: DepSource, pub alias: Option<toml::Value>, pub find: Option<String>, pub system_headers: bool, pub expose: Vec<String>, pub settings: Table /* every other key, incl. options/files/libs/lib_dir/root */ }`
  - `enum TargetKind { Lib, Bin }`, `struct TargetDecl { pub kind: TargetKind, pub settings: Table }`
  - `struct Manifest { pub workspace: Workspace, pub options: BTreeMap<String,OptionDecl>, pub features: BTreeMap<String,FeatureDecl>, pub profiles: BTreeMap<String,Table>, pub dependencies: BTreeMap<String,DependencyDecl>, pub targets: BTreeMap<String,TargetDecl> }`
  - `fn parse_manifest(t: toml::Table) -> Result<Manifest, ConfigError>`
  - In `config.rs`: `#[derive(Default)] struct Diagnostics { pub items: Vec<(Level, String)> }`, `enum Level { Warning, Info }`, `fn warn(&mut self, impl Into<String>)`, `fn info(&mut self, impl Into<String>)`.

Key rule sets (constants in `schema.rs`):
- `VIS_LISTS = include defines flags link_flags system deps` (visibility, condition, list)
- `TARGET_LISTS_NOVIS = sources features` (condition, list)
- `TARGET_SCALARS = warnings warnings_as_errors opt output` (condition); `root` (no suffixes); `bin` also `subsystem`
- profile: `VIS_LISTS` minus `deps`, plus scalars `warnings warnings_as_errors opt debug_info asserts lto`
- workspace: the decisions table above; feature: `VIS_LISTS` plus `when`; dependency: the decisions table above (`options` accepts a condition only; `files libs` are lists with a condition; `lib_dir` is a scalar with a condition)

- [ ] **Step 1: Write failing tests** in `src/config/schema.rs` (`m(src)` = `parse_manifest(parse_flat(src).unwrap())`):

```rust
#[test] fn spec_example_is_valid() { m(include_str!("../../tests/fixtures/spec_example.toml")).unwrap(); }
#[test] fn unknown_table_is_error() { assert!(err("[build]\n").contains("unknown table `build`")); }
#[test] fn legacy_dependencies_table_is_error() { assert!(err("[dependencies.x]\npath=\"a\"\n").contains("unknown table `dependencies`")); }
#[test] fn unknown_key_names_table() { assert!(err("[lib.a]\nsources=[\"a.c\"]\ndefnes=[]\n").contains("lib.a: unknown key `defnes`")); }
#[test] fn variant_feature_detected_by_option_name() {
    let x = m("[option.simd]\ntype=\"enum\"\nvalues=[\"none\",\"avx2\"]\ndefault=\"none\"\n[feature.simd.avx2]\nwhen={arch=\"x86_64\"}\n[feature.simd.none]\n").unwrap();
    assert!(matches!(&x.features["simd"], FeatureDecl::Variants(v) if v.len() == 2));
}
#[test] fn variant_name_must_be_option_value() { assert!(err(/* simd values [none], [feature.simd.avx9] */).contains("avx9")); }
#[test] fn enum_option_needs_values() { assert!(err("[option.o]\ntype=\"enum\"\ndefault=\"a\"\n").contains("values")); }
#[test] fn dependency_needs_one_source() {
    assert!(err("[dependency.d]\nkind=\"header-only\"\n").contains("exactly one of `git` or `path`"));
    assert!(err("[dependency.d]\nkind=\"header-only\"\npath=\"a\"\ngit=\"u\"\nrev=\"x\"\n").contains("exactly one"));
    assert!(err("[dependency.d]\nkind=\"system\"\nfind=\"X\"\npath=\"a\"\n").contains("system"));
}
#[test] fn git_needs_rev() { assert!(err("[dependency.d]\nkind=\"header-only\"\ngit=\"u\"\n").contains("rev")); }
#[test] fn when_rejects_lists_and_unknown_axes() {
    assert!(err("[feature.f]\nwhen={os=[\"linux\"]}\n").contains("when"));
    assert!(err("[feature.f]\nwhen={host=\"linux\"}\n").contains("host"));
}
#[test] fn feature_rejects_scalars() { assert!(err("[feature.f]\nwarnings=\"high\"\n").contains("warnings")); }
#[test] fn workspace_rejects_root() { assert!(err("[workspace]\nname=\"a\"\nversion=\"1\"\nroot=\"x\"\n").contains("root")); }
#[test] fn requires_only_tool() { assert!(err("[workspace]\nname=\"a\"\nversion=\"1\"\nrequires={cmake=\">=3\"}\n").contains("cmake")); }
```

- [ ] **Step 2:** `cargo test --lib config::schema` → FAIL.
- [ ] **Step 3:** Implement `parse_manifest`. Iterate the top-level table. Allowed names are `workspace option feature profile dependency lib bin`; every other name gets the unknown-table error. Every entry key goes through `parse_key` with the rule set for its table kind. Value-type validation (string/array/bool) happens here only for structural keys: `kind`, `git`, `rev`, `path`, `type`, `values`, `when`, `alias` (must be an inline table), and `expose` (`{ target = "x" | ["x"] }`). Settings values stay raw `toml::Value`, because lookups are expanded later. `alias` contents are validated in Task 4. A name that appears in both `lib` and `bin`, or in a target and a dependency, is an error ("declared more than once"). Names must match `[A-Za-z_][A-Za-z0-9_-]*`; reuse the check from `project.rs:111-118`.
- [ ] **Step 4:** Tests PASS. The old `config::tests` (find_manifest) stay green.
- [ ] **Step 5:** Commit `"Parse spec 0.3 manifest tables and validate keys per table kind"`.

---

### Task 4: Lookups and aliases

**Files:** Create `src/configure/mod.rs` (module declarations only for now) and `src/configure/lookup.rs`. Modify `src/lib.rs` (`pub mod configure;`).

**Interfaces:**
- Produces:
  - `struct Alias { pub os: BTreeMap<Os,String>, pub arch: BTreeMap<Arch,String>, pub compiler: BTreeMap<Compiler,String> }` with `fn parse(v: &toml::Value) -> Result<Alias,String>`
  - `struct LookupCtx<'a> { pub options: &'a BTreeMap<String, String> /* rendered option values */, pub platform: Platform, pub profile: &'a str, pub alias: Option<&'a Alias> }`
  - `fn expand(s: &str, ctx: &LookupCtx) -> Result<String,String>`
  - `fn uses_platform_lookup(s: &str) -> bool` (true if the string contains `${os}`, `${arch}`, `${compiler}` or `${triplet}`)

- [ ] **Step 1: Failing tests**

```rust
// ctx: options {werror:"true", simd:"avx2"}, platform (x86_64, macos, clang), profile "release",
// alias arch{x86_64:"x64"} os{macos:"osx"}
#[test] fn expands_with_alias() { assert_eq!(expand("lib/${arch}-${os}/${profile}", &aliased).unwrap(), "lib/x64-osx/release"); }
#[test] fn triplet_uses_aliased_components() { assert_eq!(expand("${triplet}", &aliased).unwrap(), "x64-osx-clang"); }
#[test] fn without_alias_vocabulary_names() { assert_eq!(expand("${triplet}", &plain).unwrap(), "x86_64-macos-clang"); }
#[test] fn options_and_profile_never_aliased() { assert_eq!(expand("${option.simd}-${profile}", &aliased).unwrap(), "avx2-release"); }
#[test] fn not_recursive() { /* option s = "${os}" */ assert_eq!(expand("${option.s}", &c).unwrap(), "${os}"); }
#[test] fn unknown_lookup_errors() {
    assert!(expand("${host}", &plain).unwrap_err().contains("unknown lookup `${host}`"));
    assert!(expand("${option.nope}", &plain).unwrap_err().contains("nope"));
    assert!(expand("${os", &plain).unwrap_err().contains("unterminated"));
}
#[test] fn plain_dollar_is_literal() { assert_eq!(expand("$HOME/a", &plain).unwrap(), "$HOME/a"); }
#[test] fn alias_validation() {
    assert!(Alias::parse(&v("{ platform = { x = \"y\" } }")).unwrap_err().contains("platform"));
    assert!(Alias::parse(&v("{ arch = { x86-64 = \"x64\" } }")).unwrap_err().contains("x86-64"));
    assert!(Alias::parse(&v("{ compiler = { gcc_like = \"g\" } }")).unwrap_err().contains("group"));
}
```

- [ ] **Step 2:** FAIL → **Step 3:** implement a single-pass scanner. The substituted text is never rescanned. Names not listed in the alias pass through unchanged. → **Step 4:** PASS → **Step 5:** commit `"Add lookup expansion and per-dependency aliases"`.

---

### Task 5: Platform, compiler detection, toolchain file

**Files:** Create `src/platform.rs`. Modify `src/lib.rs`. Move `load_user_toolchain` out of `src/config/manifest.rs` into this file; its precedence and file paths stay the same.

**Interfaces:**
- Consumes: `Executor`, `CommandSpec` (`src/executor.rs`).
- Produces:
  - `fn host_os_arch() -> (Os, Arch)`
  - `fn parse_target(s: &str) -> Result<(Os, Arch), String>` (form `<arch>-<os>`)
  - `fn classify_compiler(program: &str, version_output: &str) -> Option<Compiler>`
  - `fn detect_compiler(exec: &dyn Executor, program: &str, cwd: &Path) -> Result<Compiler, ConfigError>`
  - `fn check_combination(p: &Platform) -> Result<(), String>`
  - `fn llvm_triple(os: Os, arch: Arch) -> String`
  - `struct Toolchains { pub c: String, pub cxx: String, pub ar: Option<String> }`
  - `fn load_toolchains(explicit: Option<&Path>) -> Result<Toolchains, ConfigError>` (precedence: `--toolchain` file > `CC`/`CXX`/`AR` env > user toolchain file > defaults `cl`/`cl` on Windows, `cc`/`c++` elsewhere; file format `[toolchain] c, cxx, ar`)

- [ ] **Step 1: Failing tests**

```rust
#[test] fn classifies_compilers() {
    assert_eq!(classify_compiler("cl", ""), Some(Compiler::Msvc));
    assert_eq!(classify_compiler("C:/x/clang-cl.exe", ""), Some(Compiler::ClangCl));
    assert_eq!(classify_compiler("cc", "Apple clang version 15.0.0"), Some(Compiler::AppleClang));
    assert_eq!(classify_compiler("clang++", "clang version 17.0.6"), Some(Compiler::Clang));
    assert_eq!(classify_compiler("g++", "g++ (GCC) 13.2.0"), Some(Compiler::Gcc));
    assert_eq!(classify_compiler("cc", "cc (Ubuntu 13.2.0) ... Free Software Foundation"), Some(Compiler::Gcc));
    assert_eq!(classify_compiler("clang++", ""), Some(Compiler::Clang)); // name fallback
}
#[test] fn parses_target() {
    assert_eq!(parse_target("arm64-macos").unwrap(), (Os::Macos, Arch::Arm64));
    assert!(parse_target("macos-arm64").is_err());
    assert!(parse_target("x86_64").unwrap_err().contains("<arch>-<os>"));
}
#[test] fn impossible_combinations() {
    assert!(check_combination(&pf(Os::Macos, Arch::Arm64, Compiler::Msvc)).is_err());
    assert!(check_combination(&pf(Os::Linux, Arch::X86_64, Compiler::ClangCl)).is_err());
    assert!(check_combination(&pf(Os::Linux, Arch::X86_64, Compiler::AppleClang)).is_err());
    assert!(check_combination(&pf(Os::Linux, Arch::Arm64, Compiler::Clang)).is_ok());
}
#[test] fn toolchain_file_overrides_env() { /* write tmp toolchain.toml; set CC; load_toolchains(Some(path)).c == file value */ }
```

- [ ] **Step 2–4:** FAIL → implement. Classification: the name decides `cl`/`clang-cl`; otherwise the version text is checked for "Apple clang", then "clang version", then "gcc"/"Free Software Foundation"; failing that, the name is checked for clang*, gcc*, g++*, cc or c++ (gcc). `detect_compiler` runs `<program> --version` through `exec`; MSVC needs no run. It errors if neither the output nor the name classifies. → PASS.
- [ ] **Step 5:** Commit `"Detect target platform and concrete compiler"`.

---

### Task 6: Options

**Files:** Create `src/configure/options.rs`.

**Interfaces:**
- Consumes: `OptionDecl`, `Platform`, `Condition::rank`, `Diagnostics`.
- Produces:
  - `enum OptionValue { Bool(bool), Enum(String), Str(String), Int(i64) }` + `Display` (bool `true`/`false`, int decimal)
  - `enum OptionSource { Cli, Cache, Default }`
  - `struct ResolvedOption { pub value: OptionValue, pub source: OptionSource }`
  - `fn parse_cli_opt(s: &str) -> Result<(String,String),String>`
  - `fn resolve_options(decls: &BTreeMap<String,OptionDecl>, cli: &[(String,String)], cache: &BTreeMap<String,String>, platform: &Platform, diags: &mut Diagnostics) -> Result<BTreeMap<String,ResolvedOption>, ConfigError>`
  - `fn load_cache(path: &Path) -> Result<BTreeMap<String,String>, ConfigError>` (missing file → empty)
  - `fn save_cache(path: &Path, values: &BTreeMap<String,String>) -> io::Result<()>`

- [ ] **Step 1: Failing tests** (decl `simd` enum [none, sse42, avx, avx2, neon], default avx2, default.arm64 neon; `werror` bool default true)

```rust
#[test] fn conditional_default() { assert_eq!(val(&resolve(arm64, &[], &cache0)), "neon"); assert_eq!(val(&resolve(x64, &[], &cache0)), "avx2"); }
#[test] fn cli_beats_cache_beats_default() {
    let c = BTreeMap::from([("simd".into(), "avx".into())]);
    assert_eq!(get(resolve(x64, &[], &c), "simd").source, OptionSource::Cache);
    assert_eq!(get(resolve(x64, &[("simd".into(),"none".into())], &c), "simd").value.to_string(), "none");
}
#[test] fn rejects_bad_values() {
    assert!(err(&[("simd","avx512")]).contains("avx512") && err(&[("simd","avx512")]).contains("avx2"));
    assert!(err(&[("werror","yes")]).contains("true or false"));
    assert!(err(&[("nope","1")]).contains("unknown option `nope`"));
}
#[test] fn stale_cache_entry_warns() { /* cache {gone: "1"} → Ok, diags has warning mentioning `gone` */ }
#[test] fn missing_default_requires_cli() { /* string option without default → error suggests --opt name=... */ }
#[test] fn parse_cli_opt_forms() { assert_eq!(parse_cli_opt("a=b=c").unwrap(), ("a".into(),"b=c".into())); assert!(parse_cli_opt("a").is_err()); }
```

- [ ] **Step 2–4:** FAIL → implement. Conditional defaults are ordered by `rank`, and the last match wins. The cache is a flat `name = "value"` TOML table. → PASS.
- [ ] **Step 5:** Commit `"Resolve typed options from CLI, cache and conditional defaults"`.

---

### Task 7: Feature selection and `when` guards

**Files:** Create `src/configure/features.rs`.

**Interfaces:**
- Consumes: `Manifest.features`, `ResolvedOption`, `When::matches`.
- Produces: `struct ActiveFeature<'a> { pub origin: String /* "feature.simd.avx2" | "feature.opengl_deprecation" */, pub body: &'a FeatureBody }` and `fn select_features<'a>(m: &'a Manifest, opts: &BTreeMap<String,ResolvedOption>, p: &Platform) -> Result<BTreeMap<String, Option<ActiveFeature<'a>>>, ConfigError>` (key = feature name; `None` = applies nothing).

- [ ] **Step 1: Failing tests** (manifest = spec §13 option/feature tables)

```rust
#[test] fn selects_variant_from_option() { assert_eq!(sel(x64_default)["simd"].as_ref().unwrap().origin, "feature.simd.avx2"); }
#[test] fn missing_variant_table_is_noop() { assert!(sel_cli(x64, "simd=sse42")["simd"].is_none()); }
#[test] fn user_selected_guard_mismatch_fails() {
    let e = sel_err(arm64, "simd=avx2");
    assert!(e.contains("feature.simd.avx2") && e.contains("arch = x86_64") && e.contains("--opt simd=avx2"));
}
#[test] fn default_guard_mismatch_hints_default_suffix() { /* option without default.arm64, on arm64 */ assert!(e.contains("default.arm64")); }
#[test] fn bundle_with_unmatched_when_is_skipped() { /* [feature.b] when={os="windows"} on linux → None */ }
#[test] fn bundle_without_when_applies() { assert!(sel(x64_default)["opengl_deprecation"].is_some()); }
```

- [ ] **Step 2–4:** FAIL → implement → PASS. **Step 5:** commit `"Select feature variants and validate when guards"`.

---

### Task 8: Layered merge, removal, provenance

**Files:** Create `src/configure/merge.rs`.

**Interfaces:**
- Consumes: `Table`, `Entry`, `Key`, `Condition::rank`, `expand`, `LookupCtx`, `Alias`.
- Produces:
  - `struct Layer<'a> { pub table: &'a Table, pub alias: Option<&'a Alias> }`
  - `#[derive(Clone)] struct Sourced { pub value: toml::Value /* strings expanded */, pub origin: String, pub raw_key: String, pub template: Option<String> /* pre-expansion text when a lookup was used */ }`
  - `#[derive(Default)] struct Merged { pub lists: BTreeMap<(String, Visibility), Vec<Sourced>>, pub scalars: BTreeMap<String, Sourced> }` with `fn list(&self, name, vis) -> &[Sourced]` and `fn scalar(&self, name) -> Option<&Sourced>`
  - `fn merge(layers: &[Layer], ctx: &LookupCtx /* alias ignored; per-layer alias used */) -> Result<Merged, ConfigError>`

Algorithm (§4.3/§4.4): for each layer in the given order, take the entries whose condition matches the platform. Stable-sort them by `rank` (unconditional 0), so file order is kept within a rank. Lists: each value must be an array; expand each string item and append it unless an equal value is already present (first occurrence wins, keeping its `Sourced`). Scalars: they must not be arrays; expand strings; a later entry replaces the earlier one. After all layers, apply the remove entries whose conditions match, in the same order. Each removed value must be present in `(name, visibility)`, otherwise error.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn layer_order_and_origins() { /* ws ["W"], profile ["P"], lib.a ["T"] */ assert_eq!(strs(m.list("defines", Private)), ["W","P","T"]); assert_eq!(m.list("defines", Private)[1].origin, "profile.debug"); }
#[test] fn condition_order_within_layer() {
    // lib.a, in file order: defines.x86_64=["A"], defines.clang=["C"], defines.gcc_like=["G"], defines.linux=["L"], defines=["U"]
    assert_eq!(strs(merged_on(linux_x64_clang).list("defines", Private)), ["U","L","G","C","A"]);
}
#[test] fn duplicates_first_wins() { /* ["A","B"] then ["A"] */ assert_eq!(strs(..), ["A","B"]); }
#[test] fn scalar_precedence() {
    // ws warnings="high", lib warnings="off" → "off"
    // opt="speed", opt.msvc="size": msvc → size, gcc → speed
    // opt.gcc_like="size", opt.clang="max": clang → max
}
#[test] fn non_matching_condition_ignored() {}
#[test] fn remove_applies_after_merge() { /* ws defines.public.windows=["NOMINMAX"], lib remove.defines.public.windows=["NOMINMAX"] on windows → absent */ }
#[test] fn remove_absent_value_hints_condition() { // Review Focus 1
    let e = /* same manifest but unconditional remove.defines.public, on linux */;
    assert!(e.contains("`NOMINMAX` is not present") && e.contains("x86_64-linux-") && e.contains("condition"));
}
#[test] fn remove_respects_visibility() { /* public NOMINMAX, remove.defines (private) → error */ }
#[test] fn alias_only_in_its_layer() { /* profile layer "${arch}" → "x86_64"; dep layer with alias → "x64" */ }
#[test] fn list_key_needs_array() { /* defines = "A" → error naming lib.a defines */ }
```

- [ ] **Step 2–4:** FAIL → implement → PASS. **Step 5:** commit `"Merge workspace, profile and target layers with conditions and removal"`.

---

### Task 9: Unit model: typing, paths, system libs, validation, propagation

**Files:** Create `src/configure/system_libs.rs`, `src/configure/model.rs`, and `configure()` in `src/configure/mod.rs`. Modify `src/discover.rs`: `discover_patterns(root: &Path, base: &Path, patterns: &[String]) -> Result<Vec<PathBuf>, ConfigError>`, where patterns are relative to `base`, results are relative to `root` with `/` separators, and `discover_sources` (source_dirs) is deleted.

**Interfaces:**
- Consumes: Tasks 3–8.
- Produces:
  - `enum SystemLib { Lib(String), Framework(String) }`, `fn resolve_logical(name: &str, os: Os) -> Option<Vec<SystemLib>>` (`None` = unknown name), `const LOGICAL: &[&str]`
  - `struct Invocation { pub profile: Option<String>, pub options: Vec<(String,String)>, pub target: Option<String>, pub toolchain: Option<PathBuf> }` (`Default`)
  - `struct IncludeDir { pub path: PathBuf, pub system: bool }`
  - `#[derive(Default, Clone)] struct Settings { pub defines: Vec<Define>, pub include: Vec<IncludeDir>, pub flags: Vec<String>, pub link_flags: Vec<String>, pub system: Vec<SystemLib> }`
  - `struct ProfileScalars { pub opt: Opt, pub debug_info: bool, pub asserts: bool, pub lto: bool }`
  - `enum UnitKind { Lib, InterfaceLib, Bin, Dep(DepKind) }`
  - `struct DepDetails { pub source: DepSource, pub source_dir: Option<PathBuf> /* path deps now; git after fetch */, pub base_dir_rel: Option<PathBuf> /* `root` */, pub alias: Option<Alias>, pub find: Option<String>, pub system_headers: bool, pub expose: Vec<String>, pub cmake_options: BTreeMap<String,String>, pub files: Vec<String>, pub libs: Vec<String>, pub lib_dir: Option<String> }`
  - `struct Unit { pub name: String, pub kind: UnitKind, pub private: Settings, pub public: Settings, pub deps: Vec<(String, Visibility)>, pub base_dir: PathBuf, pub sources: Vec<PathBuf>, pub warnings: Option<Warnings>, pub warnings_as_errors: bool, pub opt: Option<Opt>, pub output: String, pub subsystem: Option<Subsystem>, pub dep: Option<DepDetails>, pub merged: Merged }`
  - `struct Configured { pub platform: Platform, pub profile: String, pub profile_scalars: ProfileScalars, pub profile_settings: Settings /* profile defines/flags, for deps §8.1 */, pub options: BTreeMap<String,ResolvedOption>, pub c_std: Option<String>, pub cxx_std: Option<String>, pub units: Vec<Unit>, pub link_order: Vec<usize> /* topological, consumers first */, pub diagnostics: Diagnostics }`
  - `fn configure(m: &Manifest, root: &Path, inv: &Invocation, platform: Platform, cache: &BTreeMap<String,String>) -> Result<Configured, ConfigError>`
  - `struct Effective { pub compile: Settings /* own private+public + transitive public interfaces */, pub link_units: Vec<usize> /* static-lib closure, consumers first */, pub link_system: Vec<SystemLib>, pub link_flags: Vec<String> }`, `fn effective(c: &Configured, unit: usize) -> Effective`

Rules pinned here:
- Layers for a lib/bin: `workspace.settings`, then the profile (built-in overlaid by the manifest), then the target table, then each listed active feature. Layers for a dependency: profile list keys `defines`/`flags` and scalars (§8.1, never workspace), then the dep table (with its alias), then its features (with its alias). `warnings`/`warnings_as_errors` from the profile are ignored for deps (§8.1).
- Paths: target `include`/`sources` are relative to `manifest_dir.join(root)`. Dependency `files`/`include`/`lib_dir` are relative to `source_dir.join(root)`, and a dependency's `path` is relative to the manifest dir (§6.2, §10.2). A dependency's includes have `system = system_headers` (default true); a target's includes have `system = false`.
- `system`: a plain-visibility logical name without a condition goes through `resolve_logical`, and an unknown name is an error. A name in `system.<os>` is a literal library. A logical name in `deps` is an error with the hint "move it to `system`" (§9).
- `deps` must name a known lib, bin or dependency (`bin` is an error: "cannot depend on an executable"). A cycle is an error that lists the chain.
- Scalars are typed after expansion: `"${option.werror}"` → `"true"` → bool. A wrong type is an error naming key and origin.
- `defines` parse through `Define::from_str`. `flags`/`link_flags` go through `check_flag` after expansion.
- Diagnostics: warn on a private ABI-affecting flag or define on a `Lib` or `sources` dep. Flag prefixes: `-m`, `/arch:`, `-fpack-struct`, `/Zp`, `-fshort-enums`, `-fshort-wchar`, `-fno-rtti`, `/GR-`. Define names: `_GLIBCXX_USE_CXX11_ABI`, `_ITERATOR_DEBUG_LEVEL`, `_HAS_ITERATOR_DEBUGGING`. Also warn on `options` on `header-only`, a private feature part on a `header-only`/`cmake`/`prebuilt` dep, and an `alias` with no `uses_platform_lookup` in any of the dep's string values.
- `requires.tool`: compare with `env!("CARGO_PKG_VERSION")`, reusing `compare_versions` (moved from `project.rs`).
- `c`/`cxx`: allowed values c `89 99 11 17 23`, cxx `11 14 17 20 23`; validation against MSVC happens in Task 10.
- A git dependency has `source_dir = None` until Task 13 fetches it. Its `files` glob and its source-relative paths stay unresolved (`sources` empty), and `prepare_all` (Task 12) resolves them once the checkout exists. Every validation that needs no files still runs in `configure`.

- [ ] **Step 1: Failing tests** in `src/configure/model.rs` (helper `cfg(src, platform)` writes files into a tempdir and calls `configure`):

```rust
#[test] fn spec_example_configures_on_three_platforms() { /* fixture with stub source files; git deps skipped via a test-only "no fetch" source_dir = None */ }
#[test] fn root_prefixes_globs_with_forward_slashes() { // Review Focus 2
    assert_eq!(unit(&c, "engine").sources, [PathBuf::from("engine/srcs/a.cpp"), PathBuf::from("engine/srcs/sub/b.cpp")]);
}
#[test] fn flags_checked_after_lookup_expansion() { // Review Focus 3
    /* option s string default "-O2 -g"; flags = ["${option.s}"] */ assert!(e.contains("whitespace") && e.contains("lib.a"));
}
#[test] fn werror_lookup_becomes_bool() { assert!(unit(&c, "ft_vox").warnings_as_errors); }
#[test] fn logical_system_names_resolve_per_os() {
    assert_eq!(resolve_logical("opengl", Os::Windows), Some(vec![SystemLib::Lib("opengl32".into())]));
    assert_eq!(resolve_logical("threads", Os::Windows), Some(vec![]));
    assert_eq!(resolve_logical("window", Os::Macos).unwrap().len(), 3);
    assert!(resolve_logical("openGL", Os::Linux).is_none());
}
#[test] fn literal_system_under_os_condition() { /* system.windows=["winmm"] → Lib("winmm") on windows */ }
#[test] fn logical_name_in_deps_hints_system() { assert!(e.contains("`threads` is a system library") && e.contains("system")); }
#[test] fn unknown_dep_and_cycle() {}
#[test] fn lib_without_sources_or_include_is_error() {}
#[test] fn lib_with_only_include_is_interface() { assert!(matches!(unit(&c,"h").kind, UnitKind::InterfaceLib)); }
#[test] fn bin_without_sources_is_error() {}
#[test] fn sources_matching_nothing_is_error() {}
#[test] fn public_propagates_transitively_private_does_not() {
    // a: defines.public ["A"], defines ["a_priv"]; b deps.public=["a"]; c deps=["b"]
    assert!(has_define(&effective(&c, idx("c")), "A"));
    assert!(!has_define(&effective(&c, idx("c")), "a_priv"));
    // b: deps = ["a"] (private) → c does not see A
}
#[test] fn link_closure_ignores_visibility() { /* c → b (private) → a(sources) : link_units of c contain b and a, b before a */ }
#[test] fn workspace_public_define_on_every_target() {}
#[test] fn dependency_ignores_workspace_and_profile_warnings() {}
#[test] fn abi_flag_private_on_lib_warns() {}
#[test] fn unused_alias_warns() {}
#[test] fn builtin_profile_overlay() { /* [profile.debug] opt="speed" → debug_info still true */ }
#[test] fn unknown_profile_lists_defined() {}
```

- [ ] **Step 2–4:** FAIL → implement → PASS. **Step 5:** commit `"Build per-unit configuration model with propagation and validation"`.

---

### Task 10: Flags mapping

**Files:** Create `src/flags.rs`.

**Interfaces:**
- Consumes: `Configured`, `Effective`, `Unit`, `Compiler`, `Lang`, `Driver` (`src/lang.rs`).
- Produces:
  - `fn std_flag(compiler: Compiler, lang: Lang, std: &str) -> Result<String, String>` (gcc_like: `-std=c11` / `-std=c++20`, never `gnu`; msvc_like: `/std:c11`, `/std:c17`, `/std:c++14|17|20`, and `/std:c++latest` for 23; c 89/99 or cxx 11 on msvc_like is an error)
  - `fn compile_args(c: &Configured, unit: usize, lang: Lang) -> Result<Vec<String>, ConfigError>`
  - `fn link_args(c: &Configured, unit: usize, artifact: &dyn Fn(usize) -> PathBuf, external_libs: &[PathBuf]) -> Vec<String>`
  - `fn archiver(compiler: Compiler, os: Os, lto: bool, ar_override: Option<&str>) -> Result<String, String>`
  - `fn cmake_build_type(profile: &str, s: &ProfileScalars) -> &'static str` (debug→Debug, release→Release, relwithdebinfo→RelWithDebInfo; other profiles: opt none→Debug, else debug_info→RelWithDebInfo, else Release)
  - `fn profile_args(c: &Configured, lang: Lang) -> Vec<String>` (opt/debug/lto/asserts/profile defines+flags only, for cmake deps)

Argument order (one place, so hashes stay stable): `std` → MSVC `/permissive-` (C++ only) and `/EHsc` (C++) → MSVC runtime `/MDd` if `cmake_build_type == "Debug"` else `/MD` → warnings (lib/bin only, §6.4 table; `off` → `-w`/`/W0`) → `-Werror`/`/WX` (lib/bin only) → opt (unit `opt` overrides the profile; §8 table verbatim, `max` on MSVC = `/O2 /Ob3`) → debug info (`-g` / `/Zi`) → lto (gcc `-flto -ffat-lto-objects`; clang/apple_clang `-flto=thin`; msvc `/GL`; clang_cl `-flto=thin`) → defines (`-D`/`/D`) → non-system includes (`-I`/`/I`) → system includes (`-isystem <d>` / `/external:I<d>` plus a single `/external:W0`) → flags. Link: inputs, then the static-lib closure in `link_order` order, then external lib files, then system libs (gcc_like `-l<n>`, `-framework <F>`; msvc `<n>.lib`), then link_flags, then `-flto`/`/LTCG`, `/DEBUG`, and subsystem on Windows (`/SUBSYSTEM:WINDOWS /ENTRY:mainCRTStartup` for msvc_like, `-mwindows` for gcc_like; ignored elsewhere). Paths go through `executor::path_arg`, relative to the root when inside it (reuse `resolve.rs::arg_path`).

- [ ] **Step 1: Failing tests**

```rust
#[test] fn warnings_table() { /* high: msvc ["/W4"], gcc ["-Wall","-Wextra"]; all: msvc ["/Wall"], clang ["-Wall","-Wextra","-Wpedantic"]; default: msvc ["/W3"], gcc [] */ }
#[test] fn opt_table() { /* none -O0 /Od; size -Os /O1; speed -O2 /O2; max -O3 | "/O2","/Ob3" */ }
#[test] fn conformance_no_gnu_extensions() { assert_eq!(std_flag(Compiler::Gcc, Lang::Cxx, "20").unwrap(), "-std=c++20"); assert!(std_flag(Compiler::Msvc, Lang::Cxx, "11").is_err()); }
#[test] fn dependency_gets_no_warning_flags_even_with_werror_target() {}
#[test] fn system_includes_for_dependencies() { /* gcc: "-isystem", dir ; msvc: "/external:I<dir>" and "/external:W0" once */ }
#[test] fn header_only_follows_consumer_opt() { /* bin opt=max consuming interface lib with opt=none → "-O3" */ }
#[test] fn asserts_false_defines_ndebug_once() { /* release + explicit defines=["NDEBUG"] → exactly one -DNDEBUG */ }
#[test] fn archiver_selection_for_lto() { // Review Focus 5
    assert_eq!(archiver(Compiler::Clang, Os::Linux, true, None).unwrap(), "llvm-ar");
    assert_eq!(archiver(Compiler::Gcc, Os::Linux, true, None).unwrap(), "ar");
    assert_eq!(archiver(Compiler::AppleClang, Os::Macos, true, None).unwrap(), "ar");
    assert_eq!(archiver(Compiler::Msvc, Os::Windows, true, None).unwrap(), "lib.exe");
    assert_eq!(archiver(Compiler::Clang, Os::Linux, true, Some("my-ar")).unwrap(), "my-ar");
}
#[test] fn subsystem_only_on_windows() {}
#[test] fn link_order_consumer_first() {}
```

- [ ] **Step 2–4:** FAIL → implement → PASS. MSVC archives with LTO need `lib.exe /LTCG`; `command_for` adds that in Task 11. **Step 5:** commit `"Map abstract settings to compiler, linker and archiver arguments"`.

---

### Task 11: Switch the build over to the new pipeline (local targets)

**Files:**
- Rewrite: `src/project.rs`, `tests/common/mod.rs`
- Modify: `src/graph.rs` (`Task` gains `pub target: usize`; `BuildTargetKind::Interface` produces no tasks), `src/build.rs` (`command_for(task, project)`; `build`/`prepare` lose the `&Resolved` parameter), `src/explain.rs` (signature), `src/cli.rs` (`--opt NAME=VALUE` repeatable and global, `--profile`; writes the option cache; prints diagnostics), `src/config.rs`, `src/lib.rs`
- Delete: `src/profile.rs`, `src/resolve.rs`, `src/dependencies.rs`, `src/config/manifest.rs`, legacy items in `src/config/types.rs`
- Migrate: `tests/{project,graph,planner,explain,toolchain}.rs` to the new format. `tests/libraries.rs` moves to Task 12. Examples `c-library`, `cpp-profiles`, `mixed-language` become `project.toml` (delete each `Forge.toml`). `dev/test_examples.py` looks for `project.toml`.

**Interfaces:**
- Consumes: `configure`, `effective`, `flags::*`, `load_toolchains`, `detect_compiler`, `host_os_arch`.
- Produces:
  - `enum BuildTargetKind { StaticLibrary, Binary, Interface }`
  - `struct BuildTarget { pub name: String, pub kind: BuildTargetKind, pub unit: usize, pub sources: Vec<PathBuf>, pub compile_c: Vec<String>, pub compile_cxx: Vec<String>, pub link: Vec<String>, pub link_inputs: Vec<PathBuf> /* external lib files → implicit inputs */, pub deps: Vec<String> /* static libs to archive-order */ }`
  - `struct Project { pub root: PathBuf, pub profile: String, pub platform: Platform, pub target_override: bool, pub toolchains: Toolchains, pub archiver: String, pub configured: Configured, pub targets: Vec<BuildTarget>, pub diagnostics: Diagnostics }`
  - `fn Project::load(manifest: &Path, inv: &Invocation, exec: &dyn Executor) -> Result<Project, ConfigError>`
  - Layout methods without the `profile` argument: `build_dir()`, `target_object_path(target, src)`, `target_depfile_path(target, src)`, `target_output_path(&BuildTarget)` (lib name `lib<output>.a` / `<output>.lib`; bin `<output>[.exe]`)
  - `fn uses_msvc(&self) -> bool` = `platform.compiler` is msvc_like
  - `tests/common::Fixture::new(manifest: &str) -> Fixture` writes `project.toml` and `toolchain.toml` (`c="clang"`, `cxx="clang++"`), plus `write(rel, text)`, `touch(rel)`, `load() -> Result<Project, ConfigError>` (`Invocation{toolchain: Some(root/toolchain.toml)}` and a stub executor answering `--version` with `clang version 17.0.0`)

Notes: `Project::load` does the full §11 pipeline: `load` → host platform + `--target` (Task 16) → `load_toolchains` → `detect_compiler` for the C and C++ programs (a family mismatch between them is an error) → `check_combination` → load the cache from `build/options.toml` → `configure` → build `targets` from the lib/bin units. A C or C++ source with no `c`/`cxx` in the workspace is an error that names the file, reusing the message from `project.rs:505-517`. `select_target` and `graph` keep their behavior. The planner hashes per-target commands, so one target's settings changing rebuilds only that target.

- [ ] **Step 1: Failing tests** (port the existing ones, plus these new ones)

```rust
// tests/graph.rs
#[test] fn per_target_defines_only_reach_their_target() { /* lib a defines=["ONLY_A"]; bin b deps=["a"]; compile spec of b's source lacks -DONLY_A */ }
#[test] fn public_define_reaches_consumer_compile() {}
#[test] fn interface_lib_has_no_archive_and_exports_includes() { // Review Focus 4
    /* [lib.h] include.public=["inc"]; [bin.app] sources=["main.c"] deps=["h"] */
    assert!(g.tasks.iter().all(|t| t.kind != TaskKind::Archive));
    assert!(compile_args_of("main.c").iter().any(|a| a == "-Iinc"));
}
// tests/planner.rs
#[test] fn changing_one_targets_defines_rebuilds_only_that_target() {}
#[test] fn option_change_rebuilds() { /* --opt werror=false flips -Werror → CommandChanged */ }
// tests/project.rs
#[test] fn examples_are_valid() { for e in ["c-library","cpp-profiles","mixed-language"] { Project::load(...) } }
#[test] fn legacy_manifest_is_rejected_with_unknown_table() {}
#[test] fn missing_cxx_standard_names_the_file() {}
// src/cli.rs unit test
#[test] fn opt_flag_is_saved_to_cache_and_reused() {}
```

- [ ] **Step 2:** `cargo test --all-targets` → the new tests fail.
- [ ] **Step 3:** Implement. Then delete the legacy modules and fix every compile error. `rg "Resolved|layer\(|ListEdit|OptLevel|manifest::|dependencies::" src tests` must come back empty.
- [ ] **Step 4:** Run `cargo test --all-targets && cargo clippy --all-targets -- -D warnings && cargo build --release && python dev/test_examples.py`. Expected: all pass except the `dependencies` example, which Task 12 migrates. Temporarily drop that example from the `PROJECTS` dict in `dev/test_examples.py` and say so in the commit message.
- [ ] **Step 5:** Commit `"Build targets from the spec 0.3 configure pipeline with per-target commands"`.

---

### Task 12: Local dependency kinds: `header-only`, `sources`, `prebuilt` (path sources)

**Files:** Create `src/deps/mod.rs`, `src/deps/local.rs`. Modify `src/project.rs` (dep units → `BuildTarget`s), `src/cli.rs`. Rewrite `tests/libraries.rs`. Migrate `examples/dependencies/project.toml` and restore it in `dev/test_examples.py`.

**Interfaces:**
- Consumes: `Unit`, `DepDetails`, `discover_patterns`, `expand` (with the dep's alias), `flags::*`.
- Produces:
  - `struct Prepared { pub include: Vec<IncludeDir>, pub lib_files: Vec<PathBuf>, pub lib_dirs: Vec<PathBuf>, pub libs: Vec<String>, pub extra: Settings }` (what an external dep adds to its public interface)
  - `fn prepare_local(c: &mut Configured, root: &Path, unit: usize) -> Result<Option<Prepared>, ConfigError>` (`None` for `sources`, which becomes a graph target)
  - `fn find_lib_files(dir: &Path, name: &str, msvc: bool) -> Vec<PathBuf>`: moved from `Project::library_files` with the same variant rules
  - `fn prepare_all(project: &mut Project, phase: Phase, exec: &dyn Executor, jobs: usize) -> Result<(), ConfigError>`, `enum Phase { Configure /* graph, explain */, Build }`

Rules:
- `sources`: a `StaticLibrary` BuildTarget whose files are globbed relative to `source_dir/root`. Object paths go under `obj/<dep>/…`. Paths outside the root (path deps given with `..`) are mapped to `build/…/obj/<dep>/<path relative to source_dir>`, so objects never land outside `build/`.
- `prebuilt`: expand `lib_dir` with the alias, then require `is_dir`, otherwise error `dependency.foo: library directory does not exist: <resolved absolute path> (from "lib/${arch}-${os}/${profile}")`. Every `libs` name must be found by `find_lib_files`, otherwise error. The files are link inputs and graph implicit inputs.
- `header-only`: interface only. `options` produces a warning (§10.3).

- [ ] **Step 1: Failing tests** (`tests/libraries.rs`)

```rust
#[test] fn header_only_path_dep_include_is_system() { /* clang compile args contain "-isystem" "vendor/h/include" */ }
#[test] fn system_headers_false_uses_plain_include() {}
#[test] fn sources_dep_is_archived_and_linked_without_target_warnings() { /* workspace warnings="all" + werror: dep compile has no -Wall/-Werror; profile opt present */ }
#[test] fn sources_dep_private_feature_applies_to_own_compile_only() {}
#[test] fn prebuilt_lib_dir_with_alias_resolves() { /* vendor/foo/lib/x64-linux/debug/libfoo.a, alias x86_64=x64 → link input */ }
#[test] fn prebuilt_missing_dir_prints_resolved_path() { assert!(e.contains("vendor/foo/lib/x64-linux/debug") && e.contains("${arch}")); }
#[test] fn rebuilding_a_prebuilt_library_relinks_and_nothing_else() { /* port the existing test from tests/libraries.rs:169 */ }
#[test] fn options_on_header_only_warns() {}
```

- [ ] **Step 2–4:** FAIL → implement → PASS, including `python dev/test_examples.py` with `dependencies` restored.
- [ ] **Step 5:** Commit `"Support header-only, sources and prebuilt dependencies"`.

---

### Task 13: Git sources, `project.lock`, `forge update`

**Files:** Create `src/deps/lockfile.rs`, `src/deps/git.rs`. Modify `src/deps/mod.rs`, `src/cli.rs` (`Update { dep: Option<String> }` subcommand).

**Interfaces:**
- Produces:
  - `struct LockEntry { pub git: String, pub rev: String, pub commit: String }`, `#[derive(Default)] struct Lockfile { pub entries: BTreeMap<String, LockEntry> }`
  - `fn Lockfile::load(path: &Path) -> Result<Lockfile, ConfigError>` (missing → empty), `fn save(&self, path: &Path) -> io::Result<()>`
  - `fn resolve_rev(url: &str, rev: &str) -> Result<String, String>`: `git ls-remote <url> <rev> refs/tags/<rev>^{}`; peeled tags are preferred; a 7–40 hex `rev` with no matching ref is returned as-is
  - `fn checkout(root: &Path, name: &str, url: &str, commit: &str) -> Result<PathBuf, String>`: `build/deps/<name>/source`. Reuse the clone/repair/fetch/checkout logic of the deleted `dependencies.rs::source_dir`, git arm (fetching `commit`, falling back to a full `fetch --tags origin` when the server refuses fetch-by-sha)
  - `fn pin(lock: &mut Lockfile, name: &str, url: &str, rev: &str, force: bool, diags: &mut Diagnostics) -> Result<String, ConfigError>`

File format:

```toml
# Generated by forge. Commit this file.
version = 1

[dependency.glm]
git = "https://github.com/g-truc/glm"
rev = "1.0.2"
commit = "<40 hex>"
```

Rules: if the lock entry matches the manifest `git` and `rev`, its commit is used and nothing is resolved. If the entry is missing or stale, the rev is resolved, `info: pinned <name> <rev> -> <short commit>` is emitted, and the lock is saved after a successful configure. Entries for removed dependencies are pruned. `forge update [dep]` re-resolves (all, or one) and saves. Fetching runs in `Phase::Configure` too (§11 step 7), so `graph`/`explain` work on a fresh clone.

- [ ] **Step 1: Failing tests** (`tests/git_deps.rs`; skip when `git` is missing; the fixture creates a local repo with `git init`, two commits and a tag `v1`, and uses a `file://` URL)

```rust
#[test] fn lockfile_round_trip() {}
#[test] fn first_build_pins_and_writes_lock_with_info() {}
#[test] fn locked_commit_is_used_even_after_branch_moves() { /* rev="master"; commit again upstream; build → old commit checked out */ }
#[test] fn update_moves_to_new_commit() {}
#[test] fn changed_rev_in_manifest_repins() {}
#[test] fn tag_resolves_to_peeled_commit() {}
#[test] fn removed_dependency_is_pruned_from_lock() {}
```

- [ ] **Step 2–4:** FAIL → implement → PASS. **Step 5:** commit `"Fetch git dependencies through project.lock and add forge update"`.

---

### Task 14: `cmake` dependencies

**Files:** Create `src/deps/cmake.rs`, `tests/fixtures/cmake-reply/` (a small hand-written File API reply: `index-*.json`, `codemodel-v2-*.json`, two `target-*.json`, glfw-like: target `glfw` with artifact `src/libglfw3.a`). Modify `src/deps/mod.rs`.

**Interfaces:**
- Produces:
  - `fn configure_args(src: &Path, build: &Path, build_type: &str, compilers: Option<(&str,&str)>, flags_c: &[String], flags_cxx: &[String], msvc_runtime: Option<&str>, options: &BTreeMap<String,String>) -> Vec<String>`
  - `struct CmakeTarget { pub name: String, pub kind: String, pub artifacts: Vec<PathBuf>, pub includes: Vec<PathBuf>, pub deps: Vec<String> }`
  - `fn read_reply(build: &Path) -> Result<Vec<CmakeTarget>, String>`
  - `fn exposed(targets: &[CmakeTarget], names: &[String]) -> Result<(Vec<PathBuf> /* libs incl. static deps */, Vec<PathBuf> /* includes */), String>`
  - `fn build_cmake(c: &Configured, root: &Path, unit: usize, toolchains: &Toolchains, jobs: usize) -> Result<Prepared, String>`

Rules: build dir `build[/<arch>-<os>]/<profile>/deps/<name>/cmake`. Before configuring, create the empty query file `.cmake/api/v1/query/codemodel-v2`. Configure args: `-S <src/root> -B <build> -DCMAKE_BUILD_TYPE=<cmake_build_type>`; `-DCMAKE_C_COMPILER/-DCMAKE_CXX_COMPILER` only when the compiler is not msvc; `-DCMAKE_<LANG>_FLAGS_<CONFIG_UPPER>=<profile_args joined by space>` (replaces CMake's per-config defaults, §8.1); for msvc_like, `-DCMAKE_POLICY_DEFAULT_CMP0091=NEW -DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDebugDLL|MultiThreadedDLL`; then the merged `options` as `-DKEY=VALUE` (bool → `ON`/`OFF`, condition suffixes already merged in Task 9). Re-configure only when the hash of (args + source commit or path) differs from `<build>/forge-configure.hash`. Then always run `cmake --build <build> --config <type> --parallel <jobs>`. Only in `Phase::Build`; in `Phase::Configure` the dep contributes the includes from its `include` keys only, and `explain` shows "not built yet". The library files become link inputs. An unknown `expose.target` is an error that lists the available targets. Private feature parts produce the §10.5 warning (Task 9 already emits it).

- [ ] **Step 1: Failing tests**

```rust
#[test] fn configure_args_shape() { /* bools → ON/OFF; msvc runtime only for msvc; flags var uses CONFIG_UPPER */ }
#[test] fn reads_file_api_reply() { let t = read_reply(fixture).unwrap(); assert_eq!(t[0].artifacts, [PathBuf::from("src/libglfw3.a")]); }
#[test] fn exposed_includes_static_deps() {}
#[test] fn unknown_expose_target_lists_available() {}
#[test] fn real_cmake_static_lib_links() { if !have("cmake") { return; } /* tiny CMakeLists add_library(m STATIC m.c) + bin using it → build succeeds; second build does not reconfigure */ }
```

- [ ] **Step 2–4:** FAIL → implement → PASS. **Step 5:** commit `"Build cmake dependencies and expose targets through the CMake File API"`.

---

### Task 15: `system` dependencies

**Files:** Create `src/deps/system.rs`. Port `pkg_config` and the CMake `--find-package` code from the deleted `dependencies.rs:65-125` into it.

**Interfaces:**
- Produces:
  - `struct Found { pub include: Vec<PathBuf>, pub defines: Vec<Define>, pub cflags: Vec<String>, pub lib_dirs: Vec<PathBuf>, pub libs: Vec<String>, pub ldflags: Vec<String> }`
  - `fn split_flags(tokens: &str) -> Found`
  - `fn find_package(name: &str, run: &dyn Fn(&str, &[String]) -> Result<String, String>) -> Result<Found, String>` (pkg-config first, then cmake; the error names both)
- The result becomes the dep's public interface: includes are system includes, and libraries reach the link.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn splits_pkg_config_output() { /* "-I/usr/include/x -DX=1 -pthread -L/opt/lib -lfoo -Wl,-rpath,/opt" */ }
#[test] fn falls_back_to_cmake() { /* run: pkg-config fails, cmake returns "-I/a" / "-lb" */ }
#[test] fn not_found_names_both_finders() { assert!(e.contains("pkg-config") && e.contains("cmake") && e.contains("OpenGL")); }
```

- [ ] **Step 2–4:** FAIL → implement → PASS. **Step 5:** commit `"Find system dependencies with pkg-config or CMake"`.

---

### Task 16: Target platform selection (`--target`, `--toolchain`)

**Files:** Modify `src/cli.rs`, `src/project.rs`, `src/flags.rs`.

**Interfaces:**
- Consumes: `parse_target`, `check_combination`, `llvm_triple`, `Invocation.target`, `Invocation.toolchain`.
- Produces: global CLI flags `--target <arch>-<os>` and `--toolchain <file>`. `Project::build_dir()` returns `build/<arch>-<os>/<profile>` when a target was given. `compile_args`/`link_args` add `--target=<llvm_triple>` for `clang`/`clang_cl` when the target differs from the host; nothing is added for gcc or msvc, whose cross compilers come from the toolchain file.

- [ ] **Step 1: Failing tests**

```rust
#[test] fn target_flag_drives_conditions() { /* host linux, --target arm64-macos with clang toolchain → defines.macos applied, ${triplet} == "arm64-macos-clang" */ }
#[test] fn target_build_dir_is_separate() {}
#[test] fn clang_gets_target_triple() { assert!(args.contains(&"--target=arm64-apple-macos".to_string())); }
#[test] fn msvc_targeting_macos_is_error() {}
#[test] fn only_one_axis_is_rejected() { assert!(cli_err(&["--target", "arm64"]).contains("<arch>-<os>")); }
```

- [ ] **Step 2–4:** FAIL → implement → PASS. **Step 5:** commit `"Select the target platform with --target and a toolchain file"`.

---

### Task 17: `explain <target>` configuration view

**Files:** Modify `src/explain.rs`, `src/cli.rs`.

**Interfaces:**
- Consumes: `Unit.merged` (`Sourced` origin/raw_key/template), `Configured`, `Effective`, `Prepared`.
- Produces: `fn explain_config(project: &Project, name: &str) -> Result<String, ExplainError>`. CLI: `explain <arg>`. A unit name prints `explain_config`, then a blank line, then the existing `detail` of the unit's archive/link task (if it has one). Anything else keeps the current path behavior.

Output format (fixed so the tests can match it):

```
lib.engine  [lib]  x86_64-windows-msvc  profile debug
  defines (public):
    NOMINMAX                 workspace  (defines.public.windows)
    GLM_FORCE_INLINE         lib.engine (defines.public)
  include (public):
    engine/incs              lib.engine (include.public)
  warnings = high            workspace
  opt = none                 profile.debug (built-in)
  from deps:
    glm: GLM_FORCE_AVX2      feature.simd.avx2 (defines.public)
dependency.foo  [prebuilt]
  lib_dir = vendor/foo/lib/x64-windows/debug   from "lib/${arch}-${os}/${profile}" (alias arch x86_64->x64)
```

- [ ] **Step 1: Failing tests** (`tests/explain.rs`)

```rust
#[test] fn explain_target_shows_origins() { assert!(t.contains("NOMINMAX") && t.contains("workspace") && t.contains("defines.public.windows")); }
#[test] fn explain_dependency_shows_resolved_lib_dir_and_alias() { assert!(t.contains("x64") && t.contains("${arch}") && t.contains("x86_64->x64")); }
#[test] fn explain_shows_option_sources() { /* "simd = avx2 (default)" vs "(--opt)" */ }
#[test] fn explain_path_still_explains_rebuild() { /* existing explain_matches_what_build_then_does_and_runs_nothing still passes */ }
#[test] fn unknown_name_lists_units_and_outputs() {}
```

- [ ] **Step 2–4:** FAIL → implement → PASS. **Step 5:** commit `"Explain a target's merged configuration with value origins"`.

---

### Task 18: Documentation and end-to-end examples

**Files:** `README.md`, `dev/README.md`, `docs/manifest-spec.md` (add one note under §4 saying that dotted keys inside a table body are read as flat names, so the file is lexically TOML and generic TOML tools may flag it; add no other spec changes), and a new example `examples/features/project.toml` (option + variant feature + `${option.*}` + workspace public define + `remove.*` + `system = ["threads","math"]`) with `src/main.c`. Add that example to `dev/test_examples.py`.

- [ ] **Step 1:** Add the `features` example to `PROJECTS` in `dev/test_examples.py`, including one run with `--opt` and one with `--profile release`. Run `cargo build --release && python dev/test_examples.py`: it fails because the example does not exist yet.
- [ ] **Step 2:** Write the example and update the README (manifest reference link, `project.lock`, `--opt`, `--target`, `--toolchain`, `update`, `explain <name>`, the removed legacy format).
- [ ] **Step 3:** Run the full CI recipe locally: `cargo fmt --all -- --check && cargo clippy --locked --all-targets -- -D warnings && cargo test --locked --all-targets && cargo test --locked --release --all-targets && cargo doc --locked --no-deps && cargo build --locked --release && python dev/test_examples.py`. Expected: all green.
- [ ] **Step 4:** Commit `"Document spec 0.3 manifests and add a features example"`.

---

## Verification (end-to-end)

1. Full CI recipe from Task 18 Step 3, on Windows (MSVC from a Developer prompt) and on Linux or macOS.
2. Manual smoke test on a copy of `tests/fixtures/spec_example.toml` with stub sources: `forge explain engine`, `forge explain foo`, `forge --opt simd=none build`, a second `build` reporting "Build is up to date", `forge --profile release build`, and `forge update`. Check that `project.lock` and `build/options.toml` were written.
3. `forge --target arm64-macos graph` on Linux with a clang toolchain file shows `-arm64-macos` paths and no compile errors at configure time.

## Execution (chosen by the user)

Execution is **subagent-driven** (superpowers:subagent-driven-development):
- Each task (0–18) is implemented by a fresh **Sonnet 5.5 agent at high effort** (`model: "sonnet"`, `effort: "high"`). Its prompt contains the task text, the Global Constraints, the decisions tables, and the Interfaces blocks of earlier tasks.
- After each task, the orchestrator reviews the agent's output against the task's tests, interfaces and the spec: diff, `cargo test`, `cargo clippy`. If anything is wrong, the orchestrator sends the same agent change requests (SendMessage) until the task is correct, and only then starts the next task.
- Task 0 Step 1 (commit or stash the user's uncommitted work) needs the user's answer before any agent starts.

## Coverage map (spec → task)

§2 file layout/unknowns → T1, T3, T11 · §4.1–4.2 → T2 · §4.3–4.4 → T8 · §4.5–4.6 → T4, T8 · §5 → T3, T9 · §6 → T3, T9, T10 · §7.1 → T6 · §7.2–7.3 → T7, T9 · §8 → T9, T10, T14 · §9 → T9, T10 · §10.1–10.5 → T3, T9, T12–T15 · §11 steps 1–8 → T11 (pipeline), T12–T15 (step 7), T17 (explain) · §11.1 → T5, T16 · §12 table → T2, T3, T4, T7, T8, T9, T12 · §14 guardrails → enforced by T2/T3/T4 rejections · §15 deferred → not implemented.
