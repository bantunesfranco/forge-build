# Forge Manifest Specification

**Edition:** 2026 · **Status:** proposed base release

This is a new format for Forge's first release; compatibility with development manifests is not required. It aims to make a one-executable project easy to describe while supporting explicit graphs of libraries, binaries, tests, and dependencies.

## 1. Rules

- Manifests describe project intent. Compiler locations come from the environment or a user toolchain file.
- Dependency source and build method are independent choices.
- Dependencies install into Forge-managed prefixes. Consumers never reach into another dependency's private build directory.
- Targets and dependencies form one named, acyclic graph. Edges define build order and interface propagation.
- Inheritance and platform overlays append lists, override scalars, and merge tables by key. `defines` merge by macro name; later values replace earlier ones.
- Unknown keys are errors with spelling suggestions. Reserved names have a “not supported in this edition” error.

## 2. Manifest and package

The UTF-8 TOML 1.0 file is `Forge.toml`. Paths are relative to its directory, use `/`, and may not be absolute or escape the project with `..`, except exposure paths in section 8.5.

```toml
edition = "2026"

[package]
name = "ft_vox"
version = "1.0.0"
authors = []
description = "Voxel editor"
license = "MIT"
```

`edition` defaults to `"2026"`; any other value is rejected by this edition. `package.name` and `package.version` are required. The package name matches `[a-z0-9][a-z0-9_-]*`; version follows semantic versioning. `authors` defaults to `[]`; `description` and `license` default to `""`. Target, dependency, and dev-dependency names match `[A-Za-z_][A-Za-z0-9_-]*` and are unique across all three groups.

## 3. Languages, tools, and project defaults

```toml
[lang]
c = "c11"
cxx = "c++20"

[tools]
cmake = ">=3.20"

[build]
default_profile = "debug"

[build.flags]
warnings = true
pedantic = true
error_on_warning = false
include_dirs = ["include"]
defines = ["APP_VERSION=1"]
cflags = []
cxxflags = []
ldflags = []
system_libs = []
```

`lang.c` and `lang.cxx` set project standards; targets may override them. A standard is required only for languages actually used by a target. Compiler selection is `CC` / `CXX`, user toolchain file, then `cc` / `c++`. Compiler paths do not belong in the manifest.

The optional user toolchain file is machine-local: `%APPDATA%/forge/toolchain.toml` on Windows, otherwise `$XDG_CONFIG_HOME/forge/toolchain.toml` (or `~/.config/forge/toolchain.toml` when unset). `FORGE_TOOLCHAIN` selects another file. It may contain `[toolchain] c = "..."` and `cxx = "..."`; omitted values defer to the environment/default. This file is never checked into a project or lockfile.

`tools` names required executables with version constraints. This edition accepts `>=MAJOR.MINOR[.PATCH]`. Forge checks declared tools before building. Each backend also requires its executable; naming it in `tools` enables an explicit version check.

### 3.1 Defaults for optional keys

| Omitted key | Default |
| --- | --- |
| `[lang].c`, `[lang].cxx` | Unset; required only if a target contains that language. |
| `[tools]` | Empty table. |
| Toolchain file | Missing file and missing compiler entries are treated as unset; `CC` / `CXX` take precedence. |
| `[build].default_profile` | `"debug"`. |
| `[build].source_dirs`, `[build].output` | Unset; the implicit target requires at least one source directory. `output` defaults to package name. |
| `[build.flags].warnings`, `pedantic`, `error_on_warning` | `false`. |
| `[build.flags].include_dirs`, `defines`, `cflags`, `cxxflags`, `ldflags`, `system_libs` | Empty lists. |
| `[profile]` | Built-in `debug` and `release` profiles only. |
| Profile `sanitize` | Empty list. |
| Profile `lto` | `false`. |
| Unset profile warning/raw-flag keys | Inherit; built-ins fall through to `[build.flags]`. |
| Target source, exclude, deps, system library, define, and raw flag lists | Empty; compiled targets still require sources. |
| Target `include` | Empty public and private lists. |
| Binary `output` | Target name. |
| Library `kind` | `"static"`. |
| `[test]`, `[dev-dependencies]` | Empty. |
| Dependency `resolve` | See section 8.2. |
| Dependency `pkg_config`, `version` | No system lookup / no version constraint; `pkg_config` is required when `resolve` includes `system`. |
| Dependency `build` | `"auto"` for source-backed dependencies; `"none"` for system-only dependencies. |
| Backend options, args, vars, configure args, steps, patches, and deps | Empty. |
| Make `install_target` | `"install"`. |
| Dependency `expose` | Discovered interface only. |
| Dependency `patches` and `profile` | Empty patch list; inherit the consumer profile. |
| Platform tables | No platform-specific additions. |

An omitted build method does not silently mean “do nothing”: `auto` detects a supported backend or fails. Use `build = "header-only"` to explicitly request no build.

## 4. Simple-project form

For a single executable, use project source directories:

```toml
[build]
source_dirs = ["src"]
output = "hello" # optional; defaults to package.name
```

If `source_dirs` is present and no explicit target table is declared, Forge creates one binary target named by `output` or `package.name`. It receives every regular dependency and project defaults. Without `source_dirs` or an explicit target, the manifest is invalid. If any `[bin.*]`, `[lib.*]`, or `[test.*]` table exists, `build.source_dirs` and `build.output` are errors. Project flags, tools, standards, and profiles still apply.

## 5. Profiles

`debug` and `release` always exist:

```toml
[profile.debug]
opt_level = 0
debug_info = true

[profile.release]
opt_level = 2
debug_info = false
defines = ["NDEBUG"]
```

Projects may override built-ins. Custom profiles inherit one parent, then apply local values:

```toml
[profile.profiling]
inherits = "release"
debug_info = true
sanitize = ["address", "undefined"]
```

Keys: `inherits`, `opt_level` (`0`, `1`, `2`, `3`, `"s"`, `"z"`), `debug_info`, `defines`, `sanitize`, `lto`, `warnings`, `pedantic`, `error_on_warning`, `cflags`, `cxxflags`, `ldflags`. Optional scalars inherit the parent/built-in value; optional lists add nothing. Built-in `sanitize` is empty and `lto` is false; built-in warning booleans and raw flags fall through to `[build.flags]`. `sanitize` accepts `address`, `undefined`, `thread`, or `memory`; `lto` accepts `false`, `true`, or `"thin"`. Warning booleans override `[build.flags]`. Define lists merge by macro name. Custom profiles require `inherits`; missing parents and cycles are errors. Dependencies inherit the consumer's profile unless they set `profile`.

## 6. Targets

Targets are `[lib.NAME]`, `[bin.NAME]`, or `[test.NAME]` tables:

```toml
[lib.engine]
sources = ["engine/src/**/*.cpp"]
include = { public = ["engine/incs"], private = ["engine/src"] }
deps = ["glfw", "glm"]
system_libs = ["pthread"]

[bin.editor]
sources = ["src/**/*.cpp"]
include = ["incs"] # shorthand for private includes
deps = ["engine"]
```

| Key | Meaning |
| --- | --- |
| `sources` | Source-file globs. |
| `source_dirs` | Directories scanned recursively for C/C++ sources. |
| `exclude` | Globs removed from combined source results. |
| `include` | Private path list, or `{ public = [...], private = [...] }`. |
| `defines` | Private define list, or `{ public = [...], private = [...] }`. |
| `deps` | Local library targets and dependencies used by this target. |
| `system_libs` | System libraries used by this target; library values propagate to consumers. |
| `c_standard`, `cxx_standard` | Per-target standard overrides. |
| `cflags`, `cxxflags`, `ldflags` | Raw compiler/linker argv entries. |
| `kind` | Library only: `static`, `shared`, or `header-only`; default `static`. |
| `output` | Binary only: filename; default target name. |

Compiled binaries, tests, and libraries need at least one of `sources` or `source_dirs`; they may be combined. Recognized extensions are `.c`, `.cc`, `.cpp`, and `.cxx`. A header-only library may omit them when it only exposes headers. Tests are executable targets run by `forge test`.

Glob syntax uses `/`: `*` and `?` match within one path component; `**` matches zero or more directories. Matching is case-sensitive, sorted, de-duplicated, and does not follow symlinks. A `sources` glob matching nothing is an error. An empty source directory warns if another pattern found sources; a compiled target with no sources is an error.

`forge build` builds all regular binaries and libraries. `forge build NAME` builds the named target and its dependency closure. `forge test` builds and runs tests; `forge test NAME` selects one. A test defaults to no arguments, project-root working directory, inherited environment, and success exit code `0`; `[test.NAME.run]` may set `args`, `env`, `working_dir`, and `timeout` (seconds). Tests run sequentially by default; `--jobs` opts into parallel execution. Static libraries link in graph order, consumers before dependencies.

`forge graph [NAME]` prints the selected build DAG in text; `--dot` emits Graphviz DOT. `forge explain [PATH|NAME]` explains whether the matching task is current or why it must run. With no selector, both commands describe the default build set. They use the same target closure and profile as `forge build`; neither invokes compiler or linker commands. `explain` may inspect the existing build database and input files. Graph nodes identify target names as well as outputs, including local library archive edges.

Artifacts live under profile-specific directories: binaries in `build/PROFILE/bin/`, tests in `build/PROFILE/test/`, and libraries in `build/PROFILE/lib/`. Platform-native filename prefixes/suffixes are applied automatically. Switching profiles never reuses another profile's artifacts.

## 7. Platform overlays

Targets and dependencies may have `platform.linux`, `platform.macos`, or `platform.windows` tables. They accept the parent's keys except dependency source identity. Only the current platform is merged. Lists append, scalars override, tables merge.

```toml
[lib.engine.platform.linux]
system_libs = ["GL", "pthread", "dl", "X11"]

[lib.engine.platform.macos]
ldflags = ["-framework", "OpenGL", "-framework", "Cocoa"]
```

## 8. Dependencies

`[dependencies]` declares packages available to regular targets; targets use a package only when it appears in `deps`. `[dev-dependencies]` has the same schema but is available only to tests and is resolved only by `forge test`. A test target may name regular and dev-dependencies in its `deps` list.

```toml
[dependencies.glfw]
git = "https://github.com/glfw/glfw"
tag = "3.4"
pkg_config = "glfw3"
resolve = ["system", "source"]
build = { system = "cmake", options = { GLFW_BUILD_EXAMPLES = false, GLFW_BUILD_TESTS = false, BUILD_SHARED_LIBS = false } }

[dependencies.imgui]
git = "https://github.com/ocornut/imgui"
tag = "v1.91.0"
deps = ["glfw"]
build = { system = "sources", files = ["imgui*.cpp", "backends/imgui_impl_glfw.cpp", "backends/imgui_impl_opengl3.cpp"] }
expose = { include = [".", "backends"], libs = ["imgui"] }
```

### 8.1 Source identity

Exactly one source is required: git (`git` URL and exactly one of `tag`, `rev`, `branch`); archive (`url` and mandatory `sha256`); local (`path`); or system-only (`pkg_config`, optional `version`). Source keys may be flat or contained in one `source = { ... }` table (for Git, for example, `{ git = "https://example.org/lib", tag = "1.2" }`); forms may not be mixed. `rev` accepts a commit hash or tag; use `branch` for a moving reference. The lockfile pins git references to commits. A local path is relative to the manifest and must be a directory. A system-only dependency is never fetched or built; its backend settings are invalid.

### 8.2 Resolution order

`resolve` is an ordered list of unique `system` and `source` strategies. `system` requires `pkg_config`; `source` requires git, archive, or local source. Defaults: `['system', 'source']` when both are available, `['system']` for system-only, otherwise `['source']`. The chosen strategy is locked. `version` constrains system package discovery and accepts `>=VERSION` or `=VERSION`.

### 8.3 Build methods

`build` is a backend name or table with `system` (alias `type`) and backend-specific keys:

| Backend | Behavior |
| --- | --- |
| omitted / `auto` | Detect CMake, Meson, Autotools, then Make; error if none is present. |
| `header-only` | No build; defaults to source root include exposure. |
| `sources` | Compile required `files` globs with consumer profile/compiler and archive to a static library in prefix. |
| `cmake` | Configure, build, install to managed prefix. |
| `meson` | Setup, compile, install to managed prefix. |
| `autotools` | Configure, make, install to managed prefix. |
| `make` | Run make with `vars` and `args`, then install target (default `install`). |
| `steps` | Run argv-array commands from source root, in order, without a shell. |

`options` is a typed table for CMake/Meson: booleans become `ON`/`OFF` for CMake and `true`/`false` for Meson; strings/integers pass through. Keys emit sorted. `args` appends after generated options. `configure_args` is for Autotools; `vars` is a string table for Make; `install_target` selects its install target. `files` is mandatory for `sources` and must match at least one file. `steps` is a list of argv arrays.

Every backend installs under `build/deps/NAME/PROFILE/prefix`. Source, patches, options, profile, toolchain, or dependency-input changes invalidate the dependency build.

### 8.4 Graph and propagation

Dependency `deps` entries name other dependencies and define build order/interfaces. Target `deps` may name local libraries or dependencies. All names must resolve and the combined graph must be acyclic.

Dependency interfaces and a local library's public includes/defines flow to direct consumers and transitively along edges. Private includes/defines stay local. Exposed libraries and system libraries propagate transitively for static linking. Dependency builds receive interfaces only from their declared dependency edges.

### 8.5 Discovery and exposure

After installation, Forge discovers an interface in order:

1. `.pc` files in prefix `lib/pkgconfig` and `lib64/pkgconfig`. One file is automatic. With several, `pkg_config` selects one; otherwise `expose` is required.
2. Conventional prefix `include/`, `lib/`, and `lib64/`. Infer a library named after the dependency only if `.pc` metadata supplied no libraries.
3. `expose`, which adds or overrides `include`, `defines`, `cflags`, `lib_dirs`, `libs`, `ldflags`, and `system_libs`. An omitted exposure field keeps discovery; a provided field is authoritative for that field. Directories are de-duplicated, and define values merge by macro name.

Relative exposure paths resolve under prefix, then source root. `{prefix}` and `{src}` force a base. Paths escaping both roots are errors.

### 8.6 Variables

Variables are expanded only in `steps`, `args`, `configure_args`, `vars`, and exposure paths: `{prefix}`, `{src}`, `{cc}`, `{cxx}`, `{jobs}`, `{profile}`, and `{deps.NAME.prefix}`. The last form requires NAME in that dependency's `deps`. Unknown or out-of-scope placeholders are errors. Arguments stay separate argv values; Forge never joins them into a shell command.

## 9. Patches and lockfile

Dependencies may declare `patches = ["patches/fix.patch"]`. Unified diffs apply in listed order with `-p1` after fetch and before build. Paths are manifest-relative; patch contents and order contribute to the lock hash.

Forge writes `Forge.lock` beside `Forge.toml`; projects should commit it. It records git commits, archive hashes, patch hashes, selected resolution strategies, and system package versions when available. `forge lock` creates/completes the lock without advancing already locked commits. `forge update [NAME ...]` refreshes selected packages, or all when omitted. Builds use locked revisions and fail on missing/mismatched entries. System packages remain machine-provided; lock records do not make them hermetic.

## 10. Validation and reserved names

Forge validates manifest shape, tools, standards, paths, globs, names, and the full graph before building. A dependency has exactly one source. `prefer` is not a key in this edition; use `resolve`. Unknown backend options and misplaced variables are errors. Archives require SHA-256.

Reserved and rejected: `[features]`, `[workspace]`, `[install]`, `[assets]`, `[generate]`, and `private_deps`.

## 11. Complete example

```toml
edition = "2026"

[package]
name = "ft_vox"
version = "1.0.0"

[lang]
c = "c11"
cxx = "c++20"

[tools]
cmake = ">=3.20"

[profile.debug]
opt_level = 0
debug_info = true

[profile.release]
opt_level = 2
debug_info = true

[dependencies.glm]
git = "https://github.com/g-truc/glm"
tag = "1.0.2"
build = "header-only"

[dependencies.stb]
git = "https://github.com/nothings/stb"
branch = "master"
build = "header-only"

[dependencies.glad]
path = "engine/vendor/glad"
build = { system = "sources", files = ["src/*.c"] }
expose = { include = ["include"] }

[dependencies.glfw]
git = "https://github.com/glfw/glfw"
tag = "3.4"
pkg_config = "glfw3"
resolve = ["system", "source"]
build = { system = "cmake", options = { GLFW_BUILD_EXAMPLES = false, GLFW_BUILD_TESTS = false, GLFW_BUILD_DOCS = false, BUILD_SHARED_LIBS = false } }

[dependencies.imgui]
git = "https://github.com/ocornut/imgui"
tag = "v1.91.0"
deps = ["glfw"]
build = { system = "sources", files = ["imgui*.cpp", "backends/imgui_impl_glfw.cpp", "backends/imgui_impl_opengl3.cpp"] }
expose = { include = [".", "backends"], libs = ["imgui"] }

[lib.engine]
sources = ["engine/srcs/**/*.cpp"]
include = { public = ["engine/incs"] }
deps = ["glad", "glfw", "glm", "imgui", "stb"]

[lib.engine.platform.linux]
system_libs = ["GL", "pthread", "dl", "X11"]

[bin.ft_vox]
sources = ["srcs/**/*.cpp"]
include = ["incs"]
deps = ["engine"]
```
