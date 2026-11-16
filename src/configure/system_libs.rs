//! Logical system libraries (spec 9): one name, resolved per target os.

use crate::config::vocab::Os;

/// A concrete library or macOS framework to link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemLib {
    Lib(String),
    Framework(String),
}

/// Every logical name `system` accepts.
pub const LOGICAL: &[&str] = &["opengl", "threads", "dl", "sockets", "math", "window"];

fn libs(names: &[&str]) -> Vec<SystemLib> {
    names.iter().map(|n| SystemLib::Lib((*n).into())).collect()
}

fn frameworks(names: &[&str]) -> Vec<SystemLib> {
    names
        .iter()
        .map(|n| SystemLib::Framework((*n).into()))
        .collect()
}

/// What logical `name` links on `os`. `None` means the name is unknown;
/// an empty list means it needs nothing on that os.
///
/// The spec table has no BSD column. The BSDs follow Linux, except `dl`:
/// libdl is part of libc there.
pub fn resolve_logical(name: &str, os: Os) -> Option<Vec<SystemLib>> {
    Some(match (name, os) {
        ("opengl", Os::Windows) => libs(&["opengl32"]),
        // The spec table says "`GL` (via find)"; this links `-lGL` and does not
        // look for it, which is enough where libGL is on the default path.
        ("opengl", Os::Linux | Os::Bsd) => libs(&["GL"]),
        ("opengl", Os::Macos) => frameworks(&["OpenGL"]),

        ("threads", Os::Linux | Os::Bsd) => libs(&["pthread"]),
        ("threads", Os::Windows | Os::Macos) => vec![],

        ("dl", Os::Linux) => libs(&["dl"]),
        ("dl", Os::Windows | Os::Macos | Os::Bsd) => vec![],

        ("sockets", Os::Windows) => libs(&["ws2_32"]),
        ("sockets", Os::Linux | Os::Macos | Os::Bsd) => vec![],

        ("math", Os::Linux | Os::Bsd) => libs(&["m"]),
        ("math", Os::Windows | Os::Macos) => vec![],

        ("window", Os::Windows) => libs(&["gdi32", "user32", "shell32"]),
        ("window", Os::Macos) => frameworks(&["Cocoa", "IOKit", "CoreVideo"]),
        ("window", Os::Linux | Os::Bsd) => vec![],

        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_system_names_resolve_per_os() {
        assert_eq!(
            resolve_logical("opengl", Os::Windows),
            Some(vec![SystemLib::Lib("opengl32".into())])
        );
        assert_eq!(resolve_logical("threads", Os::Windows), Some(vec![]));
        assert_eq!(resolve_logical("window", Os::Macos).unwrap().len(), 3);
        assert!(resolve_logical("openGL", Os::Linux).is_none());
    }

    #[test]
    fn every_logical_name_resolves_on_every_os() {
        for name in LOGICAL {
            for os in Os::ALL {
                assert!(resolve_logical(name, *os).is_some(), "{name} on {os:?}");
            }
        }
    }

    #[test]
    fn table_matches_spec_9() {
        let l = |n: &str| SystemLib::Lib(n.into());
        let f = |n: &str| SystemLib::Framework(n.into());
        assert_eq!(resolve_logical("opengl", Os::Linux), Some(vec![l("GL")]));
        assert_eq!(
            resolve_logical("opengl", Os::Macos),
            Some(vec![f("OpenGL")])
        );
        assert_eq!(
            resolve_logical("threads", Os::Linux),
            Some(vec![l("pthread")])
        );
        assert_eq!(resolve_logical("dl", Os::Linux), Some(vec![l("dl")]));
        assert_eq!(
            resolve_logical("sockets", Os::Windows),
            Some(vec![l("ws2_32")])
        );
        assert_eq!(resolve_logical("math", Os::Linux), Some(vec![l("m")]));
        assert_eq!(
            resolve_logical("window", Os::Windows),
            Some(vec![l("gdi32"), l("user32"), l("shell32")])
        );
        assert_eq!(
            resolve_logical("window", Os::Macos),
            Some(vec![f("Cocoa"), f("IOKit"), f("CoreVideo")])
        );
        // BSD: like Linux, but libdl is in libc.
        assert_eq!(resolve_logical("dl", Os::Bsd), Some(vec![]));
        assert_eq!(
            resolve_logical("threads", Os::Bsd),
            Some(vec![l("pthread")])
        );
    }
}
