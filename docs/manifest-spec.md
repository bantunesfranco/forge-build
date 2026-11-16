# Manifest Spec (draft 0.3)

A declarative build manifest for native C/C++ projects. It targets multiple
operating systems, compilers and architectures without conditionals,
expressions, or scripting.

## Changes from 0.2

| Area          | Change                                                                                                                                                                    |
| ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Lookups       | `${compiler}` added. Lookup set is now `${option.*}`, `${os}`, `${arch}`, `${compiler}`, `${profile}`, `${triplet}` (4.5)                                                 |
| Compatibility | Dependencies accept `alias`, a static per-component renaming of `os`, `arch` and `compiler` for lookups inside that dependency (4.6, 10.3)                                |
| `${triplet}`  | Shape stays `<arch>-<os>-<compiler>`. Each component is aliased when it expands (4.5, 4.6)                                                                                |
| Paths         | Every `path` is relative to the manifest file's directory. `root` on a dependency changes the base for its `files`, `include` and `lib_dir`, never for `path` (6.2, 10.2) |
| Validation    | Alias errors, unresolved prebuilt path error that prints the resolved path (12)                                                                                           |
| Section 15    | Renamed "Deferred". No unresolved question blocks implementation                                                                                                          |
| Guardrails    | 5 updated for the new lookup set. New guardrail 13 covers aliases                                                                                                         |

## 1. Goals and non-goals

### Goals

- A target's full configuration is readable top to bottom in one table.
- Platform differences are data, not control flow.
- Intent is expressed with abstract settings (`warnings = "high"`), and raw
  flags are the exception.
- One consistent shape for every key: visibility, conditions, merging.
- Reproducible builds by default (lockfile).
- Interoperable with existing layouts. Naming differences between ecosystems
  are handled by data (aliases), not by the manifest author writing branches.

### Non-goals

- No `if` / `else`, boolean expressions, variables, functions or includes.
- No arbitrary scripting hooks in the manifest.
- No attempt to be a general-purpose build language. Anything that cannot be
  expressed declaratively belongs in a build plugin, outside the manifest.

## 2. File layout

| File           | Purpose                                                       |
| -------------- | ------------------------------------------------------------- |
| `project.toml` | The manifest (this spec)                                      |
| `project.lock` | Resolved dependency commits and hashes (generated, committed) |

The manifest is TOML. Top-level tables, in conventional order:
`workspace`, `option.*`, `feature.*`, `profile.*`, `dependency.*`,
`lib.*`, `bin.*`.

Unknown tables, unknown keys and unknown condition names are **errors**, never
silently ignored.

## 3. Core model

| Concept       | Meaning                                                                       |
| ------------- | ----------------------------------------------------------------------------- |
| `workspace`   | Project metadata and defaults inherited by every `lib` and `bin`              |
| `lib` / `bin` | A buildable target (static library / executable)                              |
| `dependency`  | External code, fetched and exposed to targets                                 |
| `option`      | A typed, user-selectable setting (CLI or cache)                               |
| `feature`     | A named bundle of defines, flags and libs, applied to targets or dependencies |
| `profile`     | Per-build-type settings (`debug`, `release`, ...)                             |

## 4. Key grammar

Every configuration key has the form:

```md
key[.visibility][.condition]
```

Both parts are optional, and the order is fixed (visibility, then condition).

```toml
defines                 = ["A"]        # private, unconditional
defines.public          = ["B"]        # public, unconditional
defines.windows         = ["C"]        # private, only on Windows
defines.public.macos    = ["D"]        # public, only on macOS
```

A dotted key inside a table body is read as one flat name: `defines.public.macos`
is a single key, and `defines` may sit next to `defines.public` in the same
table. The file is therefore only lexically TOML. Forge reads it with its own
reader, and generic TOML tools, editors and linters may flag these tables.

### 4.1 Visibility

| Value     | Meaning                                                  |
| --------- | -------------------------------------------------------- |
| `private` | Applies to this target only (**default**)                |
| `public`  | Applies to this target and everything that depends on it |

Visibility applies to: `defines`, `include`, `flags`, `link_flags`, `system`,
`deps`. It does not apply to `sources`, `features` or scalar keys.

Anything that changes type layout or code generation (SIMD defines and flags,
ABI-affecting macros) must be `public`, so that consumers compile identically.
The tool warns when a `private` flag in a library is known to be
ABI-affecting.

### 4.2 Conditions

A condition is **one name** drawn from a fixed vocabulary. The three axes have
disjoint names, so the axis is inferred from the name.

| Axis       | Values                                                                                                                               |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `os`       | `windows`, `linux`, `macos`, `bsd`                                                                                                   |
| `compiler` | `msvc`, `gcc`, `clang`, `apple_clang`, `clang_cl`, plus groups `gcc_like` (gcc, clang, apple_clang) and `msvc_like` (msvc, clang_cl) |
| `arch`     | `x86_64`, `x86`, `arm64`, `arm`                                                                                                      |

Rules:

- Exactly **one** condition per key. There is no `windows.x86_64`.
  Combined cases use a `when` table on a feature (7.3).
- Conditions never nest and never negate (`not`, `!=` do not exist).
- A group (`gcc_like`) and a member (`clang`) may both match. See merging.
- `os` and `arch` describe the **target** platform, never the host (11.1).
- Conditions always use the vocabulary above. Aliases (4.6) never affect them.

### 4.3 Merging and precedence

Values come from three layers, applied in this order:

1. `workspace`
2. the active `profile`
3. the target's own table

Inside each layer, keys are resolved in this order:

1. Unconditional key
2. `os` conditions
3. `compiler` conditions (groups first, then specific members)
4. `arch` conditions

Then:

- **Lists append** in layer order, then in condition order, with duplicates
  removed (first occurrence wins).
- **Scalars** (`warnings`, `opt`, ...): the last matching value wins. Across
  layers this means **target > profile > workspace**. Within a layer, more
  specific conditions beat less specific ones.
- Lists never replace. To drop an inherited value, use a `remove.*` key (4.4).

### 4.4 Removal

Rare, but needed when inheriting from `workspace` or a profile:

```toml
remove.defines = ["NOMINMAX"]      # same visibility/condition suffixes allowed
```

`remove.*` is applied after merging. In a target's own table it may only remove
values that are present, and removing a non-existent value is an error. In
`workspace` or a profile, a removal applies to every target, so a target that
does not have the value is skipped instead.

### 4.5 Lookups

Lookups are the only interpolation in the format. Each is a pure, read-only
string substitution with no defaults and no operators.

| Lookup             | Value                                                                                  |
| ------------------ | -------------------------------------------------------------------------------------- |
| `${option.<name>}` | The resolved value of an option (7.1)                                                  |
| `${os}`            | Target os name from the 4.2 vocabulary                                                 |
| `${arch}`          | Target arch name from the 4.2 vocabulary                                               |
| `${compiler}`      | Concrete compiler name from the 4.2 vocabulary (never a group)                         |
| `${profile}`       | Active profile name (`debug`, `release`, ...)                                          |
| `${triplet}`       | `<arch>-<os>-<compiler>`, for example `x86_64-windows-msvc`, `arm64-macos-apple_clang` |

Rules:

- A lookup may appear anywhere a **value** is expected: strings, list items,
  scalar values. It may not appear in keys or table names.
- Lookups are evaluated after option resolution and are not recursive.
- Composition beyond plain substitution inside a string (`lib/${os}-${arch}`)
  is allowed, since it is still substitution.
- An unknown lookup is an error.
- Inside a dependency that declares `alias` (4.6), `${os}`, `${arch}` and
  `${compiler}` yield the aliased names, and `${triplet}` is built from the
  aliased components. Everywhere else they yield vocabulary names.
- `${option.*}` and `${profile}` are never aliased.

### 4.6 Aliases

Ecosystems name the same platform differently (`x64`, `amd64`, `x86_64`;
`macos`, `darwin`, `osx`). An `alias` table renames the components of
`${os}`, `${arch}` and `${compiler}` for the lookups inside **one dependency**,
so a vendored layout can be consumed as-is.

```toml
alias = { arch = { x86_64 = "x64", arm64 = "aarch64" },
          os   = { macos = "darwin" } }
```

Rules:

- `alias` is an inline table. Its keys are `os`, `arch` and `compiler`. Each
  value maps vocabulary names to replacement strings.
- Source names must be concrete values from 4.2. Groups (`gcc_like`) are not
  allowed as sources.
- Names not listed pass through unchanged.
- Aliases are static data. They rewrite lookups **only**. They never affect
  conditions, `when` guards, option values or the lockfile.
- Aliases apply to the keys of the dependency that declares them, and to
  nothing else. There is no workspace-level or global alias.
- `alias` takes no visibility or condition suffixes.
- The naming belongs to the vendor's layout, so it belongs to the dependency.

## 5. `workspace`

Defaults for every `lib` and `bin`. Any target key may appear here. Values merge
with each target's own values using the rules in 4.3. Workspace values are not
applied to `dependency` entries.

```toml
[workspace]
name     = "ft_vox"
version  = "1.0.0"
c        = "11"
cxx      = "20"
requires = { tool = ">=0.1" }

warnings = "high"
defines.public.windows = ["NOMINMAX", "WIN32_LEAN_AND_MEAN"]
```

The workspace **may set public keys**. A `defines.public.*` entry here is
public on every target, so a single line replaces repeating it per target.
A key without `.public` is private on each target.

## 6. Targets (`lib`, `bin`)

```toml
[lib.engine]
root           = "engine"
sources        = ["srcs/**/*.cpp"]
include.public = ["incs"]
deps.public    = ["glad", "glfw", "glm", "stb"]
system.public  = ["opengl", "threads"]
defines.public = ["GLM_FORCE_INLINE", "GLM_ENABLE_EXPERIMENTAL"]
```

### 6.1 Keys

| Key                  | Type        | Visibility | Description                                                               |
| -------------------- | ----------- | ---------- | ------------------------------------------------------------------------- |
| `root`               | path        | no         | Base directory for every path in this table (default: manifest directory) |
| `sources`            | glob list   | no         | Source files. Globs re-evaluated on every build                           |
| `include`            | path list   | yes        | Include directories                                                       |
| `defines`            | string list | yes        | Preprocessor definitions (`NAME` or `NAME=VALUE`)                         |
| `flags`              | string list | yes        | Raw compiler flags (escape hatch)                                         |
| `link_flags`         | string list | yes        | Raw linker flags (escape hatch)                                           |
| `system`             | name list   | yes        | Logical system libraries (section 9)                                      |
| `deps`               | name list   | yes        | Other targets or dependencies                                             |
| `features`           | name list   | no         | Feature bundles to apply (section 7)                                      |
| `warnings`           | enum        | no         | `off`, `default`, `high`, `all`                                           |
| `warnings_as_errors` | bool        | no         | Treat warnings as errors                                                  |
| `opt`                | enum        | no         | Overrides the profile's optimization level for this target's own sources  |
| `output`             | string      | no         | Output name (default: target name)                                        |

`bin` additionally accepts `subsystem = "console" | "windows"` (Windows only,
ignored elsewhere).

### 6.2 Paths and `root`

- Every path in the manifest is relative to **the directory containing the
  manifest file**, unless a rule below says otherwise.
- In a `lib` or `bin`, `root` is a plain path prefix. It applies to `sources`,
  `include` and any other path key in the same table, including condition
  variants. `root` itself is relative to the manifest directory.
- `root` is not inherited from the workspace and is not a default for anything
  else.
- For dependencies, see 10.2.

### 6.3 No implicit defaults

There are no `[lib.*]` or `[bin.*]` default tables and no conventional default
for `sources`. Omitting `sources` is an error for a target that is not header-only.
Everything a target does is visible in its own table plus the workspace.

### 6.4 Abstract settings

Abstract settings map to compiler specifics inside the tool, so they do not
appear in the manifest.

| Setting                     | MSVC           | gcc / clang                  |
| --------------------------- | -------------- | ---------------------------- |
| `warnings = "default"`      | `/W3`          | (none)                       |
| `warnings = "high"`         | `/W4`          | `-Wall -Wextra`              |
| `warnings = "all"`          | `/Wall`        | `-Wall -Wextra -Wpedantic`   |
| `warnings_as_errors = true` | `/WX`          | `-Werror`                    |
| conformance (always on)     | `/permissive-` | `-std=c++NN` (no extensions) |

`warnings` and `warnings_as_errors` apply to **the target's own sources only**
and never propagate to consumers or dependencies.

## 7. Options and features

### 7.1 Options

Typed, user-selectable values. Set with `--opt name=value` or the cache.

```toml
[option.simd]
type    = "enum"
values  = ["none", "sse42", "avx", "avx2", "neon"]
default = "avx2"
default.arm64 = "neon"          # same suffix grammar as everywhere else
doc     = "SIMD level for math code. Use `none` for the widest compatibility."

[option.werror]
type    = "bool"
default = true
```

Types: `bool`, `enum`, `string`, `int`. An enum option needs `values`.
`default` accepts condition suffixes. An explicit user value always wins.

An option is read with `${option.name}` (4.5):

```toml
[bin.ft_vox]
warnings_as_errors = "${option.werror}"
```

### 7.2 Features

A feature is a named bundle of target keys. It is applied to every `lib`, `bin`
or `dependency` that lists it in `features`.

A feature that has **the same name as an option** is a variant switch. It has
one sub-table per option value, and only the selected one applies.

```toml
[feature.simd.avx2]
when           = { arch = "x86_64" }
defines.public = ["GLM_FORCE_AVX2"]
flags.public.msvc_like = ["/arch:AVX2"]
flags.public.gcc_like  = ["-mavx2", "-mfma"]

[feature.simd.avx]
when           = { arch = "x86_64" }
defines.public = ["GLM_FORCE_AVX"]
flags.public.msvc_like = ["/arch:AVX"]
flags.public.gcc_like  = ["-mavx"]

[feature.simd.neon]
when = { arch = "arm64" }

[feature.simd.none]
```

A feature with no matching option is an always-on bundle:

```toml
[feature.opengl_deprecation]
defines.public.macos = ["GL_SILENCE_DEPRECATION"]
```

If several targets or dependencies list the same feature, its values are merged
once (duplicates removed per 4.3).

### 7.3 `when` guards

`when` is an optional table of equalities, one per axis, **all of which must
match** (implicit AND). It is the only place where axes combine.

```toml
when = { os = "windows", compiler = "clang_cl" }
```

Rules:

- Keys are `os`, `compiler`, `arch`. Values are single names from the
  vocabulary in 4.2 (groups allowed).
- No lists, no OR, no negation, no nested tables.
- If the user selects a variant whose guard does not match the current
  platform, the configure step **fails** with a clear message. Defaults never
  trip this, since `default.<arch>` exists to avoid it.

## 8. Profiles

Per-build-type settings. `debug`, `release` and `relwithdebinfo` are built in
and may be redefined. Profile names are a closed set defined by the manifest
and are **not** a condition axis (see 15).

```toml
[profile.debug]
opt        = "speed"           # none | size | speed | max
debug_info = true
asserts    = true
lto        = false

[profile.release]
opt        = "max"
debug_info = false
asserts    = false
lto        = true
defines    = ["NDEBUG"]

[profile.relwithdebinfo]
opt        = "speed"
debug_info = true
lto        = true
```

| `opt`   | MSVC       | gcc / clang |
| ------- | ---------- | ----------- |
| `none`  | `/Od`      | `-O0`       |
| `size`  | `/O1`      | `-Os`       |
| `speed` | `/O2`      | `-O2`       |
| `max`   | `/O2 /Ob3` | `-O3`       |

The tool is responsible for making combinations valid. For example, on MSVC an
optimized debug profile drops `/RTC1` and `/ZI` (edit-and-continue) rather than
making the user do it.

A target's own `opt` beats the profile's (4.3). Header-only code is compiled
inside the consumer's translation units, so it follows the **consumer's**
resolved `opt`, not the `opt` of the target that defines it.

### 8.1 What crosses into dependencies

| Key                                   | Applies to dependencies built in this profile                                        |
| ------------------------------------- | ------------------------------------------------------------------------------------ |
| `opt`, `debug_info`, `lto`, `asserts` | Yes. Mapped to the dependency's build (for `cmake` kind, through its compiler flags) |
| `defines`, `flags`                    | Yes                                                                                  |
| `warnings`, `warnings_as_errors`      | **No.** Dependencies keep their own defaults (10.4)                                  |
| Workspace keys                        | No (section 5)                                                                       |

Profile `defines`, `flags` and the rest follow the same key grammar.

## 9. Logical system libraries

`system` lists logical names. The tool resolves each name per platform, so
manifests need no platform tables for them.

| Name      | Windows                | Linux           | macOS                   |
| --------- | ---------------------- | --------------- | ----------------------- |
| `opengl`  | `opengl32`             | `GL` (via find) | `-framework OpenGL`     |
| `threads` | (none)                 | `pthread`       | (none)                  |
| `dl`      | (none)                 | `dl`            | (none)                  |
| `sockets` | `ws2_32`               | (none)          | (none)                  |
| `math`    | (none)                 | `m`             | (none)                  |
| `window`  | `gdi32 user32 shell32` | (none)          | Cocoa, IOKit, CoreVideo |

Rules:

- An unknown name is an error.
- If the name resolves to nothing on a platform, it is simply a no-op.
- The table is part of the tool, not the manifest. A raw escape hatch exists as
  `system.windows = ["winmm"]`, treated as a literal library name.
- A plain name in `system` is logical. A name in `system.<os>` is literal.
  This distinction is deliberate and documented in tool errors.
- Logical system names go in `system`, never in `deps`. A name in `deps` that
  is a logical system name is an error with a hint.

## 10. Dependencies

One shape for all dependencies: a `kind` field.

```toml
[dependency.glm]
git      = "https://github.com/g-truc/glm"
rev      = "1.0.2"
kind     = "header-only"
features = ["simd"]

[dependency.stb]
git  = "https://github.com/nothings/stb"
rev  = "master"                      # allowed: the lockfile pins the commit
kind = "header-only"

[dependency.glad]
path           = "engine/vendor/glad"
kind           = "sources"
files          = ["src/glad.c"]
include.public = ["include"]
system         = ["dl"]

[dependency.glfw]
git     = "https://github.com/glfw/glfw"
rev     = "3.4"
kind    = "cmake"
options = { GLFW_BUILD_EXAMPLES = false, GLFW_BUILD_TESTS = false,
            GLFW_BUILD_DOCS = false, GLFW_INSTALL = false,
            BUILD_SHARED_LIBS = false }
options.linux = { GLFW_BUILD_X11 = true, GLFW_BUILD_WAYLAND = false }
expose = { target = "glfw" }

[dependency.imgui]
git            = "https://github.com/ocornut/imgui"
rev            = "v1.93.0"
kind           = "sources"
files          = ["imgui.cpp", "imgui_draw.cpp", "imgui_tables.cpp",
                  "imgui_widgets.cpp", "imgui_demo.cpp",
                  "backends/imgui_impl_glfw.cpp",
                  "backends/imgui_impl_opengl3.cpp"]
include.public = [".", "backends"]
deps.public    = ["glfw"]
system.public  = ["opengl"]

[dependency.foo]                      # vendored prebuilt, vcpkg-style layout
path    = "vendor/foo"
kind    = "prebuilt"
lib_dir = "lib/${arch}-${os}/${profile}"
libs    = ["foo"]
alias   = { arch = { x86_64 = "x64" }, os = { macos = "osx" } }
```

### 10.1 Kinds

| Kind          | Behavior                                                   |
| ------------- | ---------------------------------------------------------- |
| `header-only` | Only exposes include dirs                                  |
| `sources`     | Compiles listed files into a static library                |
| `cmake`       | Configures and builds with CMake, exposes the built target |
| `prebuilt`    | Links existing libs (`libs`, `lib_dir` keys)               |
| `system`      | Finds a host package by name (`find = "OpenGL"`)           |

### 10.2 Source, pinning and paths

- Exactly one of `git` or `path`.
- `path` is always relative to **the directory containing the manifest file**.
- A dependency's **source directory** is its `path`, or its fetched checkout
  for `git`.
- `files`, `include` and `lib_dir` are relative to the source directory. A
  `root` on a dependency adds a subdirectory under the source directory and
  changes the base for those keys only. It never changes `path`.
- `rev` accepts a tag, a branch or a commit hash.
- The lockfile records the **resolved commit hash** and, for archives, a
  content hash. Builds use the lockfile unless `update` is run.
- A moving ref (`master`, a branch) is allowed in the manifest and is safe
  because of the lockfile.

### 10.3 Keys

Dependencies accept the same keys as targets (`include`, `defines`, `system`,
`deps`, `flags`, `features`, `root`) with the same visibility and condition
grammar, plus:

| Key              | Applies to          | Description                                                            |
| ---------------- | ------------------- | ---------------------------------------------------------------------- |
| `options`        | `cmake`             | Cache variables. Condition suffixes allowed                            |
| `files`          | `sources`           | Files to compile, relative to the source directory                     |
| `expose`         | `cmake`, `prebuilt` | Targets or libs the dependency provides                                |
| `libs`           | `prebuilt`          | Library names to link                                                  |
| `lib_dir`        | `prebuilt`          | Library directory. Lookups and condition suffixes allowed              |
| `find`           | `system`            | Host package name                                                      |
| `alias`          | all                 | Renames `os`, `arch`, `compiler` lookups inside this dependency (4.6)  |
| `system_headers` | all                 | Default `true`: include dirs are system includes (warnings suppressed) |

`options` on a `header-only` dependency produces a configure-time warning
rather than being silently accepted.

### 10.4 Warnings isolation

Dependency include directories are marked as **system includes** by default
(`-isystem`, `-external:I`), and dependencies are compiled with warnings at
their own defaults, never with the dependent target's `warnings_as_errors`.

### 10.5 Features on dependencies

A dependency may list `features`. The feature's **public** parts (defines,
flags, include, system, deps) attach to the dependency and therefore travel to
every consumer. This is the intended way to keep ABI-affecting settings (SIMD
for a math library) with the code they affect, so no consumer can forget them.

How each part is applied depends on the dependency kind:

| Kind          | Public parts of a feature | Private parts of a feature                        |
| ------------- | ------------------------- | ------------------------------------------------- |
| `header-only` | Propagate to consumers    | Warning, nothing to compile                       |
| `sources`     | Propagate to consumers    | Applied to the dependency's own compile           |
| `cmake`       | Propagate to consumers    | Warning. Build-side settings go through `options` |
| `prebuilt`    | Propagate to consumers    | Warning, nothing to compile                       |

The tool never injects flags into a foreign `cmake` build, and it says so with
a warning instead of ignoring the private part.

## 11. Resolution and configure

1. Read `project.toml`, validate the schema, and reject unknown keys.
2. Determine the target platform (11.1) and the compiler.
3. Resolve options (CLI > cache > default with conditions).
4. Select feature variants and validate their `when` guards.
5. For each target, apply `workspace`, the profile, then the target's keys,
   merging with 4.3. For each dependency, apply the profile (8.1) and its own
   keys, including features.
6. Evaluate lookups (4.5), applying each dependency's `alias` (4.6) to the
   lookups in that dependency's keys, and resolve logical `system` names to
   concrete libs and frameworks.
7. Resolve dependencies from the lockfile, fetching if needed. Check that
   `prebuilt` library directories exist.
8. Emit the build graph.

The tool provides `explain <target>`, which prints the fully merged result for
the current platform, showing which table each value came from. For
dependencies it also prints resolved lookups, including the final `lib_dir`
after aliasing. This replaces the need to mentally assemble the manifest.

### 11.1 Target platform and cross-compilation

- `os` and `arch` describe the **target** machine. `--target <arch>-<os>` sets
  both together. Individual override of only one is not supported.
- The **compiler** is separate: it comes from detection or from a toolchain
  file passed to the tool.
- Every condition (4.2), `when` guard and lookup (4.5) is evaluated against the
  target, never the host.
- Combinations that cannot exist (for example `msvc` targeting `macos`) are a
  configure error.
- Host-side tools such as code generators that must run on the build machine are
  out of scope for this revision.

## 12. Validation rules

| Situation                                                                              | Result                                      |
| -------------------------------------------------------------------------------------- | ------------------------------------------- |
| Unknown key, table, condition name or lookup                                           | Error                                       |
| Two conditions in one key (`defines.windows.x86_64`)                                   | Error                                       |
| Visibility on a key that doesn't accept it                                             | Error                                       |
| `remove.*` in a target table of a value that isn't present                             | Error                                       |
| Variant guard not matching selected option value                                       | Error                                       |
| Unknown logical `system` name                                                          | Error                                       |
| Logical system name placed in `deps`                                                   | Error                                       |
| Non-header-only target without `sources`                                               | Error                                       |
| Impossible `--target` and compiler combination                                         | Error                                       |
| `alias` key other than `os`, `arch`, `compiler`                                        | Error                                       |
| `alias` source name outside the 4.2 vocabulary (typo such as `x86-64`) or a group name | Error                                       |
| `alias` with visibility or condition suffix                                            | Error                                       |
| `prebuilt` library directory does not exist after lookups and aliasing                 | Error. The message prints the resolved path |
| `options` on a `header-only` dependency                                                | Warning                                     |
| Private feature part on a `header-only`, `cmake` or `prebuilt` dependency              | Warning                                     |
| `private` ABI-affecting flag on a library                                              | Warning                                     |
| `alias` declared but no lookup in the dependency uses it                               | Warning                                     |
| `master` / branch ref without a lockfile entry                                         | Info (will pin)                             |

## 13. Full example

```toml
[workspace]
name     = "ft_vox"
version  = "1.0.0"
c        = "11"
cxx      = "20"
warnings = "high"
defines.public.windows = ["NOMINMAX", "WIN32_LEAN_AND_MEAN"]

[option.simd]
type          = "enum"
values        = ["none", "sse42", "avx", "avx2", "neon"]
default       = "avx2"
default.arm64 = "neon"

[option.werror]
type    = "bool"
default = true

[feature.simd.avx2]
when           = { arch = "x86_64" }
defines.public = ["GLM_FORCE_AVX2"]
flags.public.msvc_like = ["/arch:AVX2"]
flags.public.gcc_like  = ["-mavx2", "-mfma"]

[feature.simd.neon]
when = { arch = "arm64" }

[feature.simd.none]

[feature.opengl_deprecation]
defines.public.macos = ["GL_SILENCE_DEPRECATION"]

[profile.debug]
opt        = "speed"
debug_info = true

[profile.release]
opt        = "max"
lto        = true
defines    = ["NDEBUG"]

[lib.engine]
root           = "engine"
sources        = ["srcs/**/*.cpp"]
include.public = ["incs"]
deps.public    = ["glad", "glfw", "glm", "stb"]
system.public  = ["opengl", "threads"]
defines.public = ["GLM_FORCE_INLINE", "GLM_ENABLE_EXPERIMENTAL"]
features       = ["opengl_deprecation"]

[lib.net]
root           = "server"
sources        = ["srcs/**/*.cpp"]
include.public = ["incs"]
system.public  = ["threads", "sockets"]
defines.public.windows = ["_WIN32_WINNT=0x0A00"]

[bin.ft_vox]
sources            = ["srcs/**/*.cpp"]
include            = ["incs"]
deps               = ["engine", "net", "imgui"]
warnings_as_errors = "${option.werror}"
defines.windows    = ["_CRT_SECURE_NO_WARNINGS"]

[dependency.glm]
git      = "https://github.com/g-truc/glm"
rev      = "1.0.2"
kind     = "header-only"
features = ["simd"]

[dependency.stb]
git  = "https://github.com/nothings/stb"
rev  = "master"
kind = "header-only"

[dependency.glad]
path           = "engine/vendor/glad"
kind           = "sources"
files          = ["src/glad.c"]
include.public = ["include"]
system         = ["dl"]

[dependency.glfw]
git     = "https://github.com/glfw/glfw"
rev     = "3.4"
kind    = "cmake"
options = { GLFW_BUILD_EXAMPLES = false, GLFW_BUILD_TESTS = false,
            GLFW_BUILD_DOCS = false, GLFW_INSTALL = false,
            BUILD_SHARED_LIBS = false }
options.linux = { GLFW_BUILD_X11 = true, GLFW_BUILD_WAYLAND = false }
expose  = { target = "glfw" }

[dependency.imgui]
git            = "https://github.com/ocornut/imgui"
rev            = "v1.93.0"
kind           = "sources"
files          = ["imgui.cpp", "imgui_draw.cpp", "imgui_tables.cpp",
                  "imgui_widgets.cpp", "imgui_demo.cpp",
                  "backends/imgui_impl_glfw.cpp",
                  "backends/imgui_impl_opengl3.cpp"]
include.public = [".", "backends"]
deps.public    = ["glfw"]
system.public  = ["opengl"]

# Example of a prebuilt dependency with aliases (vcpkg-style layout):
#   vendor/foo/lib/x64-windows/release/foo.lib
#   vendor/foo/lib/aarch64-osx/debug/libfoo.a
#
# [dependency.foo]
# path    = "vendor/foo"
# kind    = "prebuilt"
# lib_dir = "lib/${arch}-${os}/${profile}"
# libs    = ["foo"]
# alias   = { arch = { x86_64 = "x64", arm64 = "aarch64" },
#             os   = { macos = "osx" } }
#
# Go-style layout (vendor/bar/windows_amd64):
#
# [dependency.bar]
# path    = "vendor/bar"
# kind    = "prebuilt"
# lib_dir = "${os}_${arch}"
# alias   = { arch = { x86_64 = "amd64" } }
```

## 14. Design guardrails

These are the rules that keep the format from growing into a scripting language.
Any proposal that breaks one of them needs a new spec revision, not a patch.

1. **One condition per key.** Combined cases use a feature's `when`.
2. **No nesting of conditions.** Not in keys, not in `when`.
3. **No negation, no OR, no expressions.** Add a group name to the vocabulary
   instead (as with `gcc_like`).
4. **Fixed condition vocabulary.** Users cannot define their own conditions,
   and profile names are not conditions.
5. **Lookups are substitution only.** The set is fixed: `${option.*}`, `${os}`,
   `${arch}`, `${compiler}`, `${profile}`, `${triplet}`. No defaults, no
   operators, no user-defined lookups.
6. **Intent over flags.** If the same raw flag pair shows up for several
   compilers more than twice, it deserves an abstract setting.
7. **Logical names over platform tables.** If a system library differs per OS,
   it belongs in the tool's name table.
8. **Same shape everywhere.** Every list key takes visibility and conditions.
   Every dependency has a `kind`. Targets and dependencies accept the same keys.
9. **Additive merging.** Lists never replace. Use `remove.*` explicitly.
10. **Unknown means error.** Typos fail loudly.
11. **Explicit over implicit.** No inherited target defaults, no default
    `sources`. A target's behavior is its own table plus the workspace and profile.
12. **Conditions describe the target.** Never the host.
13. **Aliases are data, scoped to one dependency.** A static name mapping for
    lookups only. They never touch conditions, never apply globally, and never
    take logic.

## 15. Deferred

Deliberate omissions. None of them blocks implementation. They are listed so
they are not reopened by accident.

- **Profile as a condition axis.** A per-profile override on a single target
  (for example `opt = "max"` on `engine` in debug only) would need `profile`
  as a fourth axis. Deferred because it adds a dimension and profile names are
  user-defined. The profile-wide `opt` and the per-target `opt` cover the
  common cases.
- **Host tools.** Build-time code generators that run on the build machine need
  a host/target split that this revision does not define.
- **Alias presets.** Built-in mappings such as `alias.preset = "vcpkg"` or
  `"llvm"` would save retyping common renamings. They would be data tables
  shipped with the tool, overridden by explicit `alias` entries. Add them once
  real manifests show which mappings repeat.
- **`${profile}` aliasing.** Prebuilt layouts that capitalize `Debug` and
  `Release` cannot be matched by aliases today. Add a `profile` alias key if
  real layouts need it.
