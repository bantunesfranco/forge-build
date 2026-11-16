# Forge

[![CI](https://github.com/OWNER/REPO/actions/workflows/ci.yml/badge.svg)](https://github.com/OWNER/REPO/actions/workflows/ci.yml)

**Forge** is a small incremental C/C++ build system written in Rust. It explores
dependency tracking, reliable rebuild decisions, parallel task scheduling, and
explainable build plans.

It is a learning project, not a replacement for Make, Ninja or CMake. The point is
to implement the core ideas of an incremental build engine and make its decisions
visible: every rebuild comes with a reason, and `forge explain` shows the
plan without running anything.

## Features

- A declarative `project.toml` manifest ([spec 0.3](docs/manifest-spec.md)): no conditionals or scripting, one key grammar for platforms, compilers and architectures.
- Typed options (`--opt name=value`) and features that bundle defines, flags and libraries.
- C and C++ in one project, static libraries and executables, and dependencies from a local path, git, CMake or the system.
- `project.lock` pins git dependencies to a commit.
- Header dependencies from the compiler itself, not from guessing.
- Rebuild decisions from **file contents**, not timestamps.
- Parallel compilation (`-j N`) that never links before its objects exist.
- A persistent, versioned, atomically written build database, one per profile and target platform.
- `explain`: the merged configuration of a target with the origin of every value, and the planner's decision and reason for every task.
- Failures never look like successes: a failed task is never recorded as up to date.

## Quick start

Requirements: a Rust toolchain and a C/C++ compiler (`gcc`, `clang`, Apple clang, MSVC `cl` or `clang-cl`).

```text
cargo install --path .
cd examples/cpp-profiles
forge build
./build/debug/bin/cpp-profiles
```

```text
$ forge build
Compiling src/main.cpp
Compiling src/greeting.cpp
Linking build/debug/bin/cpp-profiles
Build successful

$ forge build
Build is up to date
Build successful
```

`forge explain` shows what a rebuild would do and why, without running anything.
The `examples/` directory has more projects. `examples/features` uses an option, a
variant feature, a `${option.*}` lookup, `remove.*` and logical system libraries.

## Commands

| Command | What it does |
| --- | --- |
| `forge build [NAME] [-j N] [-v]` | Build all targets, or the named target and its dependency closure. `-v` prints each command and why it runs. |
| `forge explain [NAME\|PATH]` | With a lib, bin or dependency name: its merged configuration and where every value came from, then why its archive or link task would rebuild. With a path: why that file is rebuilt. With nothing: one line per task. |
| `forge graph [NAME] [--dot]` | Print all tasks or a target's dependency closure; `--dot` emits Graphviz (`... \| dot -Tpng -o graph.png`). |
| `forge clean [--all]` | Remove the selected profile's build directory, or all of `build/`. |
| `forge update [DEP]` | Resolve git dependencies again and rewrite `project.lock`. Builds nothing. |

Global options:

| Option | Meaning |
| --- | --- |
| `-p, --profile NAME` | Build profile: `debug` (default), `release`, `relwithdebinfo` or one the manifest declares. |
| `--opt NAME=VALUE` | Set an option declared in the manifest. Repeatable. The value is saved in `options.toml` in the build directory (`build/options.toml`, or `build/<arch>-<os>/options.toml` with `--target`), so later runs of the same build reuse it (command line, then cache, then the option's default). |
| `--target ARCH-OS` | Build for another platform, for example `arm64-macos`. Output goes to `build/<arch>-<os>/<profile>`. See Cross builds below. |
| `--toolchain FILE` | A file with a `[toolchain]` table naming `c`, `cxx` and `ar`. Precedence: this file, then `CC`/`CXX`/`AR` from the environment, then the user toolchain file, then the default compilers. |
| `-m, --manifest PATH` | A `project.toml`, or a directory containing one. |

Without `-m`, Forge looks for `project.toml` in the current directory and then in each
parent; the nearest wins, and the search stops at a directory containing `.git`.
The directory holding the manifest is the project root. Every relative path means
the same thing wherever you run the command, and compilers run with the root as
their working directory.

## Configuration

The manifest is `project.toml`. The [manifest specification](docs/manifest-spec.md)
is the reference for every key, the condition and lookup vocabulary, profiles,
options, features and dependencies. A minimal project:

```toml
[workspace]
name = "hello"
version = "1.0.0"
cxx  = "20"

[bin.hello]
sources = ["src/**/*.cpp"]
include = ["include"]
defines.windows = ["NOMINMAX"]
```

Any key can take a visibility and a condition suffix (`defines.public.macos`).
Targets are `[lib.<name>]` (a static library, `build/<profile>/lib/`) and
`[bin.<name>]` (an executable, `build/<profile>/bin/`). Unknown tables, keys and
conditions are errors that name the key and the file.

The dotted-key grammar is only lexically TOML: Forge has its own reader, and generic
TOML tools or editors may flag `project.toml` files.

### Dependencies and `project.lock`

`[dependency.<name>]` has a `kind`: `header-only`, `sources`, `prebuilt` (from a
`path` or a `git` checkout), `cmake` (built through CMake, with `expose.target`)
or `system` (`find`, tried with `pkg-config` and then CMake's `find-package`).
Git dependencies are checked out under `build/deps/` and removed by
`forge clean --all`. Their submodules are fetched recursively.

`project.lock` sits next to `project.toml` and records the commit each git
dependency resolved to. Commit it. Builds use the pinned commit; `forge update`
(or `forge update <dep>`) resolves again and rewrites it.

Only dependencies that a lib or bin reaches on the current platform (through `deps`,
of any visibility, or through other dependencies) are fetched, found or built; the
others are skipped with an `info:` line, so a dependency that only one platform uses
does not have to exist on the others. Their `project.lock` entries are kept.
`forge clean` fetches and builds nothing. A relative `git` URL (`./repo`,
`../repo`) is taken from the manifest directory.

forge detects the system libraries that the exposed targets need (`gdi32`, `m`,
`-framework Cocoa`, ...) through a probe executable that CMake never builds, so they
reach the link of every bin without `system` keys; this needs CMake 3.19 or newer
(with an older one forge warns, and the keys are needed). Only static libraries can be exposed: a
`SHARED_LIBRARY` target is an error (set `BUILD_SHARED_LIBS=OFF` or expose a static
target). forge picks the CMake generator so that the dependency is built with its own
compiler: `CMAKE_GENERATOR` if set, otherwise Ninja if it is installed, otherwise on
Windows the Visual Studio default for `cl` (plus `-T ClangCL` for `clang-cl`) or
`MinGW Makefiles` for `gcc`; `clang` on Windows needs Ninja. `llvm-ar` and `llvm-lib`
are passed on as `CMAKE_AR`, so LTO builds archive the same way as forge's own
libraries. A changed `options` entry or generator configures from a clean cache.

### Cross builds

With `--target`, `clang`, Apple clang and `clang-cl` are given `--target=<triple>`
for compiling and linking, when the platform differs from the host's. `gcc` and MSVC
choose their target by which compiler runs, so they get nothing added: name their
cross compilers in a `--toolchain` file (or `CC`/`CXX`). Without one, forge prints
`warning: --target <t>: <compiler> cannot cross-compile by flag` and builds for the
host. `cmake`
dependencies also get `CMAKE_SYSTEM_NAME` and `CMAKE_SYSTEM_PROCESSOR`, plus
`CMAKE_<LANG>_COMPILER_TARGET` for clang.

### Migrating from the old format

Earlier versions read `Forge.toml` with `[project]`, `[build]`, `[package]`,
`[lang]`, `[tools]`, `[dependencies.*]` and profile `inherits`. That format is no
longer read, and any of those tables is reported as an unknown table. Write a
`project.toml` as described in the specification. The make, meson, autotools,
`steps` and `self` dependency backends and url/tar sources were removed as well.

## How it works

Planning and executing are separate. The planner is a pure function that never
touches a process, so it is unit-tested without a compiler, and `explain` is exactly
the plan that `build` is about to run.

```text
project.toml -> config (shape) -> configure (options, features, merge)
                                   |
                                   v
                     project (paths, sources, toolchains, deps)
                                   |                |
                                   v                v
                     graph (tasks, edges)    flags (settings -> arguments)
 PLAN TIME                         |
   snapshot (hash files) + saved records -> planner -> dirty tasks + reasons
 ----------------------------------------------------------------------------
 EXECUTE TIME            (`explain` stops above this line)
   scheduler (-j N) -> executor -> compiler / linker
        '-> on success: read the depfile, hash inputs and outputs, record
        '-> forge.db, written atomically
```

**Graph.** Tasks declare input and output files, and edges are derived by matching
outputs to inputs, as Ninja does. Compile tasks produce an object and a depfile; the
link task consumes the objects. Cycles and duplicate outputs are rejected.

**Why a task is rebuilt.** The planner visits tasks dependencies-first and reports
the first reason that applies:

| Reason | Meaning |
| --- | --- |
| `no previous build record` | Never built, or the last attempt left no trustworthy record. |
| `<path> was rebuilt` | An upstream task is running, so this one must too. |
| `command or compiler changed` | Flags, defines, standard, or the compiler's identity changed (`--version`, or the `cl` banner, which names its host and target architecture). |
| `output missing: <path>` / `output modified: <path>` | The object, depfile or executable is gone or was tampered with. |
| `list of inputs changed` | A source was added or removed. |
| `<path> changed` | A source, a header, or a linked library differs from what was recorded. |

**Content, not timestamps.** Files are compared by BLAKE3 hash of their bytes, so
touching a file does nothing and a restored older file is noticed. A task that
reruns always reruns its dependents, even if it produced identical bytes (see
Limitations).

**Headers.** Compiles write a dependency file: `-MMD -MF` for gcc and clang,
`/clang:-MMD` for `clang-cl` and `/sourceDependencies` (JSON) for `cl`. After a
successful compile it is parsed and the header list is stored with the task, so
editing a header rebuilds exactly the files that include it.

**Archives.** A static library's archive is deleted before it is archived again, so
a removed or renamed source never stays in it.

**Debug info.** With MSVC-like compilers, `/Zi` writes its PDB into
`build/<profile>/obj/<target>/` (one per target), never into the project directory.

**Libraries.** Static libraries of the project and of its dependencies are inputs of
the link, so rebuilding one relinks the program. Logical system libraries
(`system = ["threads", "math"]`) are mapped to linker arguments for the platform
and are left to the linker.

**Correctness rules.** When state is missing or uncertain, rebuild.

- Only successful tasks are recorded. A failed task never looks up to date.
- Inputs are hashed before a task launches, so an edit during a compile is seen next time.
- A source or header modified after a task launched is not recorded, so it is rebuilt.
- A missing, corrupt or wrong-version database means a full rebuild, with a warning.
- The database is written to a temporary file and renamed, so an interrupted write
  cannot corrupt it.

**Scheduler.** Worker threads run commands and report back over a channel; the main
thread owns all state, including the database. After the first failure nothing new
is launched, running tasks finish, and every failure is reported. Output is printed
per task, in one piece, so parallel builds stay readable.

**Layout.**

```text
build/<profile>/obj/<target>/<source>.o  objects, grouped by target
build/<profile>/dep/<target>/<source>.o.d depfiles
build/<profile>/bin/<target>             executable targets
build/<profile>/lib/lib<target>.a        static library targets
build/<profile>/forge.db                 records for this profile
build/options.toml                       cached --opt values (host builds)
build/<arch>-<os>/options.toml           cached --opt values (--target builds)
build/deps/<name>/                       fetched and built dependencies
```

Object and library names follow the platform (`.obj` and `.lib` with MSVC). With
`--target`, `<profile>` is `<arch>-<os>/<profile>`. Profiles do not share anything,
so switching between them never rebuilds.

## Source map

| Module | Role |
| --- | --- |
| `config` | Reads `project.toml` (flat-key reader), key grammar, vocabulary and the typed manifest |
| `configure` | Options and their cache, features, lookups, layered merge with origins, per-unit model |
| `platform` | Host and `--target` platform, compiler classification, toolchain files |
| `project` | Path validation, source discovery, toolchain choice, build layout |
| `deps` | Dependency kinds (local, git, CMake, system) and `project.lock` |
| `flags` | Turns abstract settings into compiler, linker and archiver arguments |
| `graph` | Tasks, edges, topological order, text and DOT output |
| `planner` | Pure dirty-or-clean decisions with typed reasons |
| `database`, `fingerprint` | Records and hashing; command and compiler fingerprints |
| `headers`, `depfile` | Depfile parsing and path normalization |
| `build`, `executor` | The scheduler and the process boundary (a trait, so tests can fake it) |
| `explain` | Renders merged configuration and the plan |

## Development

```text
cargo test                                   # unit and integration tests
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo build --release && python3 dev/test_examples.py   # the example projects
```

Unit tests live next to the code. Integration tests in `tests/` drive the real
pipeline with real compilers (`gcc`/`g++`, `ar`) in temporary directories
and skip themselves when a tool is missing.

CI (`.github/workflows/ci.yml`) runs clippy and the Rust tests on Linux, macOS and
Windows, builds the examples with gcc and clang (Linux), Apple clang (macOS), MSVC,
clang-cl and clang (Windows), and runs the Rust tests of the Windows build under Wine.

## Platforms and limitations

Linux, macOS (Apple clang or Homebrew LLVM) and Windows (MSVC `cl`, `clang-cl`, `clang` or MinGW `gcc`).

- **Environment variables** that change compilation (`CPATH`, `SDKROOT`, ...) are not
  part of the fingerprint.
- **System headers** are not listed by `-MMD` (gcc, clang, `clang-cl`), so they are
  covered only through the compiler's identity and a header-only system change goes
  unnoticed. `cl` lists them in its JSON and tracks them.
- **A new header earlier in the include path that shadows an existing one** is not
  detected. Make and Ninja share this limit.
- **No early cutoff.** If a compile produces identical bytes, the archive and the link still run.
- **A small timing window:** a header edited within milliseconds after its compile
  starts can be recorded as clean.
- **Ctrl-C is not handled specially.** A partially written output is caught on the
  next build, because its hash will not match the record.
- **Deferred features** are listed in section 15 of the [manifest specification](docs/manifest-spec.md).

## Roadmap

1. Dependency task caching and build logs.
2. Early cutoff.

## License

Licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](LICENSE-MIT))

at your option.

The dependency-file parser (`src/depfile.rs`) is adapted from other projects; see
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in this project by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
