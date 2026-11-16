# Changelog

## 0.2.0

Second test release. Adds shared libraries; manifests written for 0.1.0 keep working unchanged (libraries stay static unless they say `kind = "shared"`).

Also on crates.io from this release: `cargo install forge-build`.

### Added

- **Shared libraries:** `kind = "shared"` on a `lib` builds a `.dll` (with its import library) on Windows, a `.so` on Linux and BSD and a `.dylib` on macOS. `kind` may be a lookup such as `"${option.lib_kind}"`. See section 6.5 of the manifest specification (draft 0.4) and `examples/shared-library`.
  - forge defines `<NAME>_SHARED` (public) and `<NAME>_BUILDING` (private) so a header can pick `dllexport`, `dllimport` or default visibility.
  - A shared library links the static libraries and `sources` dependencies it reaches; consumers link only the shared library. Link-side settings (`system`, `link_flags`, library files) of what is inside it stop at the library.
  - Static libraries, `sources` dependencies and `cmake` dependencies that go into a shared library are built position-independent (not on Windows).
  - Programs run from the build directory without setup: DLLs sit next to the executables on Windows, and executables and shared libraries get `$ORIGIN` or `@loader_path` run paths elsewhere.
  - Spec draft 0.4.

### Known limitations

- Shared libraries from dependencies (a `cmake` shared target, a prebuilt `.so`/`.dll`) are not supported yet, and libraries have no version in their file names (`libfoo.so.1`).
- The warning for a static library linked both directly and through a shared library covers your own libraries and `sources` dependencies only.
- `output` names get `.dll` appended on Windows even when they already end in `.dll`.

## 0.1.0

First test release. The manifest format ([spec draft 0.3](docs/manifest-spec.md)) may still change between 0.x releases.

### What works

- **Manifest:** a declarative `project.toml` with one key grammar for visibility and platform conditions (`defines.public.windows`), typed options (`--opt name=value`), variant features with `when` guards, profiles, `remove.*`, lookups (`${os}`, `${option.x}`, ...) and per-dependency aliases. Unknown keys, tables, conditions and lookups are errors, reported as `project.toml:<line>:`.
- **Targets:** static libraries, header-only libraries and executables, with C and C++ in one project.
- **Dependencies:** `header-only`, `sources`, `prebuilt`, `cmake` (CMake File API, system libraries detected) and `system` (pkg-config, then CMake). They come from a local `path` or `git`, pinned in `project.lock`, with submodules. `forge update` re-pins them.
- **Compilers:** gcc, clang, Apple clang, MSVC `cl` and `clang-cl` on Linux, macOS and Windows. Abstract settings (`warnings`, `opt`, `lto`, `debug_info`) map to each compiler.
- **Cross builds:** `--target <arch>-<os>` and `--toolchain <file>`, with pkg-config for the target platform.
- **Builds:** content-based rebuild decisions, compiler-reported header dependencies, parallel builds, and a build database per profile and target.
- **Commands:**
  - `build`
  - `run`
  - `init`
  - `explain` (merged configuration with value origins, and why each task rebuilds)
  - `graph`
  - `compdb`, plus `compile_commands.json` written on every build
  - `update`
  - `clean`

### Known limitations

- No shared libraries yet (`kind = "shared"` is planned for 0.2.0).
- No test targets and no install step.
- `system` dependencies that ship only a static library in a default directory may lose their private link libraries.
- Static cmake libraries need CMake ≥ 3.19 for their system libraries to be detected.

### Binaries

Each archive contains `forge` and `forge-build`, which are the same program. Use `forge-build` if another `forge` (for example Foundry's) is already on your `PATH`.
