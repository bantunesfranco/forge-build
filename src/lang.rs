//! Source languages and the toolchain used for each.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lang {
    C,
    Cxx,
}

impl Lang {
    /// Classifies a source file by extension. Matching is case-sensitive.
    pub fn from_path(path: &Path) -> Option<Lang> {
        match path.extension()?.to_str()? {
            "c" => Some(Lang::C),
            "cc" | "cpp" | "cxx" => Some(Lang::Cxx),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::C => "C",
            Lang::Cxx => "C++",
        }
    }
}

/// Compiler driver family. `clang-cl` uses the MSVC command-line interface too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Driver {
    Gnu,
    Msvc,
}

/// The compiler driver and language standard for one language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolchain {
    pub program: String,
    pub standard: String,
}

impl Toolchain {
    pub fn driver(&self) -> Driver {
        let name = Path::new(&self.program)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&self.program)
            .to_ascii_lowercase();
        if matches!(name.as_str(), "cl" | "cl.exe" | "clang-cl" | "clang-cl.exe") {
            Driver::Msvc
        } else {
            Driver::Gnu
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_extension() {
        let l = |p: &str| Lang::from_path(Path::new(p));
        assert_eq!(l("a.c"), Some(Lang::C));
        assert_eq!(l("a.cc"), Some(Lang::Cxx));
        assert_eq!(l("dir/a.cpp"), Some(Lang::Cxx));
        assert_eq!(l("a.cxx"), Some(Lang::Cxx));
        assert_eq!(l("a.h"), None);
        assert_eq!(l("a.hpp"), None);
        assert_eq!(l("a.C"), None); // case-sensitive
        assert_eq!(l("a.CPP"), None);
        assert_eq!(l("Makefile"), None);
    }
}
