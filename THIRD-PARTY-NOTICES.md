# Third-party notices

Forge's own code is licensed under the MIT License (see `LICENSE`). One file
contains code adapted from other projects, whose notices are recorded here.

## `src/depfile.rs` — dependency-file parser

This parser is adapted from the `depfile` module of the **shuriken** crate
(<https://docs.rs/shuriken>), which is available under the terms of
**MIT OR Apache-2.0**. Forge uses it under the MIT license.

shuriken's parser in turn reproduces the behavior of the depfile parser in
**Ninja** (`src/depfile_parser.in.cc`), which is licensed under the
**Apache License 2.0**.

> **TODO before publishing:** copy the exact copyright lines from the upstream
> sources into this file, and include the full text of the MIT license as
> distributed with shuriken. The copyright holder of shuriken is named in that
> crate's `LICENSE-MIT` file, and Ninja's is named in the header of
> `depfile_parser.in.cc`. They are deliberately not guessed here.

Suggested header for `src/depfile.rs`:

```rust
//! Parser for Makefile-style dependency files (`gcc -MD` output).
//!
//! Adapted from the `depfile` module of the `shuriken` crate
//! (https://docs.rs/shuriken), used under the MIT license; see
//! THIRD-PARTY-NOTICES.md. shuriken's parser reproduces the behavior of
//! Ninja's `depfile_parser.in.cc` (Apache-2.0).
```
