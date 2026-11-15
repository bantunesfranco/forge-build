//! Source languages.

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
