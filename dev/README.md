# Development scripts

These scripts test Forge against real C and C++ toolchains. They are developer tools, not part of the Rust test suite, and are not run by `cargo test`.

## Requirements

- Python 3
- A built Forge binary
- A C compiler (`clang` or `gcc`)
- A C++ compiler (`clang++` or `g++`)
- An archiver (`ar` or `llvm-ar`) if required by the examples

## Building Forge

Build the binary first:

```sh
cargo build --release
```

## Integration tests — `test_examples.py`

Run the integration tests for the example projects:

```sh
python3 dev/test_examples.py
```

The suite invokes Forge as a real subprocess and tests the projects under `examples/`:

- **`c-library`** — builds and runs a C application, tests the release profile, and checks cleaning.
- **`cpp-profiles`** — builds and runs a C++ application with debug and release profiles, and checks graph and explain output.
- **`mixed-language`** — builds and runs a C++ application containing C source files, using the configured C++ compiler.
- **`dependencies`** — builds and runs an application against the sibling `c-library` source dependency.

Each example is copied into a temporary directory before testing, so generated files and build artifacts do not modify the original examples.

By default, the script looks for the Forge binary at `target/release/forge`. Override this with `FORGE_BIN` if needed:

```sh
FORGE_BIN=target/debug/forge \
  python3 dev/test_examples.py
```

Compiler selection follows the environment (`CC` and `CXX`). Set them to choose the compilers used by Forge and the test environment:

```sh
CC=clang CXX=clang++ \
  python3 dev/test_examples.py
```

## Platform support

The integration suite currently targets Linux and macOS. Windows support can be added when Forge's Windows support is ready.

## Scope

These tests verify representative end-to-end workflows using Forge's documented examples. They complement the Rust test suite; they do not replace focused tests for malformed manifests, invalid configuration, error handling, or other edge cases. Such cases should use isolated temporary projects rather than complicating the user-facing examples.
