# Third-party notices

Forge is licensed under either of the Apache License 2.0 (`LICENSE-APACHE`) or the
MIT license (`LICENSE-MIT`), at your option. One file contains code adapted from
other projects, whose notices are recorded here.

## `src/depfile.rs` — dependency-file parser

Adapted, unmodified, from the `depfile` module of the **shuriken** crate
(<https://docs.rs/shuriken>), which is available under **MIT OR Apache-2.0**, the
same terms as Forge.

shuriken's parser in turn reproduces the behavior of the depfile parser in
**Ninja** (`src/depfile_parser.in.cc`), which is licensed under the
**Apache License 2.0**.

| Project | License | Copyright |
| --- | --- | --- |
| shuriken | MIT OR Apache-2.0 | Copyright (c) 2026 Alec Nunn |
| Ninja | Apache-2.0 | Copyright 2011 Google Inc. All Rights Reserved. |

Both upstream copyright notices must stay with the code. They are copied from the
upstream files (Ninja's from the header of `src/depfile_parser.in.cc`).

Neither shuriken nor Ninja ships a `NOTICE` file, so there are no further
attribution notices to reproduce.
