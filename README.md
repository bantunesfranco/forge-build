# Forge

[![CI](https://github.com/OWNER/REPO/actions/workflows/ci.yml/badge.svg)](https://github.com/OWNER/REPO/actions/workflows/ci.yml)

**Forge** is a small incremental C/C++ build system written in Rust. It explores
dependency tracking, reliable rebuild decisions, parallel task scheduling, and
explainable build plans.

It is a learning project, not a replacement for Make, Ninja or CMake. The point is
to implement the core ideas of an incremental build engine and make its decisions
visible: every rebuild comes with a reason, and `forge-build explain` shows the
plan without running anything.

## Features

- A small `Forge.toml` manifest, validated at startup with errors that name the key and the file.
- C and C++ in one project, each with its own compiler and standard (like Make's `CC`/`CXX`).
- Build profiles (`debug`, `release`, your own) with inheritance and add/remove/set edits.
- Header dependencies from the compiler itself (`-MMD -MF`), not from guessing.
- Rebuild decisions from **file contents**, not timestamps.
- Parallel compilation (`-j N`) that never links before its objects exist.
- A persistent, versioned, atomically written build database, one per profile.
- `explain`: the planner's decision and its reason for every target.
- Failures never look like successes: a failed task is never recorded as up to date.

## Quick start

Requirements: a Rust toolchain, and `gcc` or `clang` (plus `ar` for libraries).

```text
cargo install --path .
cd examples/hello
forge-build build
./build/debug/hello
```

```text
$ forge-build build
Compiling src/main.cpp
Compiling src/greeting.cpp
Linking build/debug/hello

$ forge-build build
Build is up to date

$ touch include/greeting.hpp && echo "// edit" >> include/greeting.hpp
$ forge-build explain
build/debug/obj/src/greeting.cpp.o: rebuild: include/greeting.hpp changed
build/debug/obj/src/main.cpp.o: up to date
build/debug/hello: rebuild: build/debug/obj/src/greeting.cpp.o was rebuilt

2 of 3 tasks need to run.
```

## Commands

| Command | What it does |
|---|---|
| `forge-build build [-j N] [-v]` | Build the selected profile. `-v` prints each command and why it runs. |
| `forge-build explain [TARGET]` | Say what would rebuild, and why. A target can be an output file or a source file. |
| `forge-build graph [--dot]` | Print the task graph; `--dot` emits Graphviz (`... \| dot -Tpng -o graph.png`). |
| `forge-build clean [--all]` | Remove the selected profile's build directory, or all of `build/`. |

Global options: `-p, --profile NAME` (default: `build.default_profile`) and
`-m, --manifest PATH` (a `Forge.toml`, or a directory containing one).

Without `-m`, Forge looks for `Forge.toml` in the current directory and then in each
parent; the nearest wins, and the search stops at a directory containing `.git`.
The directory holding the manifest is the project root. Every relative path means
the same thing wherever you run the command, and compilers run with the root as
their working directory.

## Configuration

```toml
[project]
name = "ft_ssl"
version = "0.1.0"

[build]
compiler = "gcc"            # shorthand for a project with one language
standard = "c11"
source_dirs = ["srcs"]      # .c .cc .cpp .cxx, searched recursively
output = "ft_ssl"           # placed in build/<profile>/
default_profile = "debug"

[build.flags]               # the base layer, shared by every profile
warnings = true             # -Wall -Wextra
pedantic = true             # -Wpedantic
error_on_warning = true     # -Werror
defines = ["USE_FOO", "VERSION=3"]
include_dirs = ["incs", "libft/incs"]
lib_dirs = ["libft"]
libs = ["ft"]

[profile.debug]
opt_level = 0
debug_info = true

[profile.release]
opt_level = 2
defines = { add = ["NDEBUG", "VERSION=4"], remove = ["USE_FOO"] }

[profile.asan]
inherits = ["debug"]
compile_flags = { add = ["-fsanitize=address", "-fno-omit-frame-pointer"] }
link_flags    = { add = ["-fsanitize=address"] }
```

### Compilers and standards

Each language has its own compiler, as in Make. The bare `compiler` and `standard`
keys are a shorthand for a project with **one** language. With both C and C++
sources, set all four:

```toml
c_compiler   = "clang"
c_standard   = "c17"
cxx_compiler = "clang++"
cxx_standard = "c++20"
```

A language only needs a toolchain if the sources contain it. The bare keys in a
mixed project are rejected as ambiguous, and so is a standard that belongs to the
wrong language (`c++20` for C sources). A link uses the C++ driver when any object
is C++, so `libstdc++` gets linked, and the C compiler otherwise. Forge never
searches for a compiler: the manifest names it.

### Profiles

Settings are layered, later layers winning:

1. the base `[build.flags]`,
2. each inherited profile, depth-first, in the order listed,
3. the profile itself.

A profile reached twice (diamond inheritance) is applied once. `inherits` cycles,
unknown parents and an unknown `default_profile` are errors.

List fields (`defines`, `compile_flags`, `link_flags`) accept a plain array, which
appends, or a table with `add`, `remove` and `set`, applied in the order `set`,
`remove`, `add`. Defines are identified by **macro name**, so `VERSION=4` replaces
`VERSION=3` instead of producing two conflicting `-D` flags, and `NAME` differs from
`NAME=`. Re-adding an existing flag moves it to the end.

`opt_level` is `0`, `1`, `2`, `3` or the strings `"g"`, `"s"`, `"z"`, `"fast"`.

### Validation

Everything is checked when the manifest loads:

- Unknown keys are errors, and so are wrongly typed values.
- `source_dirs` and `output` must be relative and stay inside the project root.
  `include_dirs` and `lib_dirs` may point elsewhere but must exist.
- Source directories may not overlap, and `output` may not be a directory or live
  inside a source directory.
- Raw flags are single arguments: no empty entries, no whitespace, and nothing Forge
  controls itself (`-c`, `-o`, `-E`, `-S`, `-M*`). There is no shell, so
  `"-O2 -g"` is rejected, not split.

## How it works

Planning and executing are separate. The planner is a pure function that never
touches a process, so it is unit-tested without a compiler, and `explain` is exactly
the plan that `build` is about to run.

```text
Forge.toml -> config (shape) -> project (paths, sources, toolchains)
                                   |                |
                                   v                v
                     graph (tasks, edges)    resolve (profile -> flags)
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
|---|---|
| `no previous build record` | Never built, or the last attempt left no trustworthy record. |
| `<path> was rebuilt` | An upstream task is running, so this one must too. |
| `command or compiler changed` | Flags, defines, standard, or the compiler's `--version` changed. |
| `output missing: <path>` / `output modified: <path>` | The object, depfile or executable is gone or was tampered with. |
| `list of inputs changed` | A source was added or removed. |
| `<path> changed` | A source, a header, or a linked library differs from what was recorded. |

**Content, not timestamps.** Files are compared by BLAKE3 hash of their bytes, so
touching a file does nothing and a restored older file is noticed. A rebuild that
produces identical bytes does not cascade into the things that use them.

**Headers.** Compiles run with `-MMD -MF`. After a successful compile the depfile is
parsed and the header list is stored with the task, so editing a header rebuilds
exactly the files that include it.

**Libraries.** Each `-l` name is looked up in `lib_dirs`. A library found there is
tracked as an implicit input of the link, so rebuilding it relinks the program.
Libraries outside `lib_dirs` (`m`, `pthread`) are left to the linker.

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
build/<profile>/obj/<source>.o       objects, mirroring the source path
build/<profile>/dep/<source>.o.d     depfiles
build/<profile>/<output>             the executable
build/<profile>/forge.db             records for this profile
```

Profiles do not share anything, so switching between them never rebuilds.

## Source map

| Module | Role |
|---|---|
| `config` | Manifest shape (serde), plus `Define`, `ListEdit`, `OptLevel` |
| `project` | Path validation, source discovery, toolchain choice, build layout |
| `profile`, `resolve` | Profile checks; layering a profile into compiler arguments |
| `graph` | Tasks, edges, topological order, text and DOT output |
| `planner` | Pure dirty-or-clean decisions with typed reasons |
| `database`, `fingerprint` | Records and hashing; command and compiler fingerprints |
| `headers`, `depfile` | Depfile parsing and path normalization |
| `build`, `executor` | The scheduler and the process boundary (a trait, so tests can fake it) |
| `explain` | Renders the plan |

## Development

```text
cargo test                                   # unit and integration tests
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
git config core.hooksPath .githooks          # run the same checks before each commit
```

Unit tests live next to the code. Integration tests in `tests/` drive the real
pipeline with real compilers (`gcc`/`g++`, `ar`, `make`) in temporary directories
and skip themselves when a tool is missing. CI builds and tests on Linux and macOS.

## Platforms and limitations

Linux and macOS with GCC or Clang. On Windows, use WSL. MSVC is not supported.

- **No compiler discovery.** The manifest must name the compiler.
- **Environment variables** that change compilation (`CPATH`, `SDKROOT`, ...) are not
  part of the fingerprint.
- **System headers** (omitted by `-MMD`) are covered only through the compiler's
  `--version`, so a header-only system change goes unnoticed.
- **A new header earlier in the include path that shadows an existing one** is not
  detected. Make and Ninja share this limit.
- **No early cutoff.** If a compile produces identical bytes, the link still runs.
- **A small timing window:** a header edited within milliseconds after its compile
  starts can be recorded as clean.
- **One set of flags for both languages.** C++-only flags reach C files, where GCC warns.
- **Ctrl-C is not handled specially.** A partially written output is caught on the
  next build, because its hash will not match the record.
- **No generated files, static library targets, or dependency fetching** yet.

## Roadmap

1. Static library targets (`ar`), so Forge can build things like `libft` itself.
2. `pkg-config` support, with its flags in the fingerprint.
3. Path dependencies, then fetching (git commit pin, tarball checksum, lockfile).
4. CMake as a single opaque task producing headers and libraries.
5. Early cutoff.

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