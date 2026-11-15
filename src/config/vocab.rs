//! The fixed condition vocabulary (spec 4.2) and the target platform.

use std::str::FromStr;

/// Declares a vocabulary enum with `FromStr`, `as_str` and `ALL`.
macro_rules! vocab_enum {
    ($name:ident, $what:literal, { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The vocabulary spelling.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($text => Ok(Self::$variant),)+
                    _ => Err(format!(concat!("unknown ", $what, " `{}`"), s)),
                }
            }
        }
    };
}

vocab_enum!(Os, "os", {
    Windows => "windows",
    Linux => "linux",
    Macos => "macos",
    Bsd => "bsd",
});

vocab_enum!(Arch, "arch", {
    X86_64 => "x86_64",
    X86 => "x86",
    Arm64 => "arm64",
    Arm => "arm",
});

vocab_enum!(Compiler, "compiler", {
    Msvc => "msvc",
    Gcc => "gcc",
    Clang => "clang",
    AppleClang => "apple_clang",
    ClangCl => "clang_cl",
});

/// A compiler condition: one concrete compiler or a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CompilerCond {
    Is(Compiler),
    GccLike,
    MsvcLike,
}

impl CompilerCond {
    pub const ALL: &'static [Self] = &[
        CompilerCond::Is(Compiler::Msvc),
        CompilerCond::Is(Compiler::Gcc),
        CompilerCond::Is(Compiler::Clang),
        CompilerCond::Is(Compiler::AppleClang),
        CompilerCond::Is(Compiler::ClangCl),
        CompilerCond::GccLike,
        CompilerCond::MsvcLike,
    ];

    /// The vocabulary spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            CompilerCond::Is(c) => c.as_str(),
            CompilerCond::GccLike => "gcc_like",
            CompilerCond::MsvcLike => "msvc_like",
        }
    }

    pub fn matches(self, compiler: Compiler) -> bool {
        match self {
            CompilerCond::Is(c) => c == compiler,
            CompilerCond::GccLike => matches!(
                compiler,
                Compiler::Gcc | Compiler::Clang | Compiler::AppleClang
            ),
            CompilerCond::MsvcLike => matches!(compiler, Compiler::Msvc | Compiler::ClangCl),
        }
    }
}

impl FromStr for CompilerCond {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "gcc_like" => Ok(CompilerCond::GccLike),
            "msvc_like" => Ok(CompilerCond::MsvcLike),
            _ => s
                .parse::<Compiler>()
                .map(CompilerCond::Is)
                .map_err(|_| format!("unknown compiler condition `{s}`")),
        }
    }
}

/// A condition: exactly one name from one axis. The axes use disjoint names,
/// so the axis is inferred from the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Condition {
    Os(Os),
    Compiler(CompilerCond),
    Arch(Arch),
}

impl Condition {
    pub fn parse(s: &str) -> Option<Condition> {
        if let Ok(os) = s.parse::<Os>() {
            Some(Condition::Os(os))
        } else if let Ok(c) = s.parse::<CompilerCond>() {
            Some(Condition::Compiler(c))
        } else {
            s.parse::<Arch>().ok().map(Condition::Arch)
        }
    }

    /// The vocabulary spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Condition::Os(o) => o.as_str(),
            Condition::Compiler(c) => c.as_str(),
            Condition::Arch(a) => a.as_str(),
        }
    }

    pub fn matches(&self, p: &Platform) -> bool {
        match *self {
            Condition::Os(o) => o == p.os,
            Condition::Compiler(c) => c.matches(p.compiler),
            Condition::Arch(a) => a == p.arch,
        }
    }

    /// Merge order within a layer (spec 4.3): os 1, compiler group 2,
    /// compiler member 3, arch 4. Unconditional (0) is handled by the caller.
    pub fn rank(&self) -> u8 {
        match self {
            Condition::Os(_) => 1,
            Condition::Compiler(CompilerCond::GccLike | CompilerCond::MsvcLike) => 2,
            Condition::Compiler(CompilerCond::Is(_)) => 3,
            Condition::Arch(_) => 4,
        }
    }
}

/// The target platform a build is configured for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
    pub compiler: Compiler,
}

impl Platform {
    /// `<arch>-<os>-<compiler>` in vocabulary names.
    pub fn triplet(&self) -> String {
        format!(
            "{}-{}-{}",
            self.arch.as_str(),
            self.os.as_str(),
            self.compiler.as_str()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_and_rank() {
        let p = Platform {
            os: Os::Linux,
            arch: Arch::X86_64,
            compiler: Compiler::Clang,
        };
        assert!(Condition::parse("gcc_like").unwrap().matches(&p));
        assert!(!Condition::parse("msvc_like").unwrap().matches(&p));
        assert!(
            Condition::parse("gcc_like").unwrap().rank()
                < Condition::parse("clang").unwrap().rank()
        );
        assert_eq!(p.triplet(), "x86_64-linux-clang");
    }

    #[test]
    fn vocabulary_is_exact() {
        let names = |v: Vec<&str>| v.join(" ");
        assert_eq!(
            names(Os::ALL.iter().map(|o| o.as_str()).collect()),
            "windows linux macos bsd"
        );
        assert_eq!(
            names(Arch::ALL.iter().map(|o| o.as_str()).collect()),
            "x86_64 x86 arm64 arm"
        );
        assert_eq!(
            names(Compiler::ALL.iter().map(|o| o.as_str()).collect()),
            "msvc gcc clang apple_clang clang_cl"
        );
        for c in Compiler::ALL {
            assert_eq!(c.as_str().parse::<Compiler>(), Ok(*c));
        }
        assert!("windows".parse::<Arch>().is_err());
    }

    #[test]
    fn groups_match_members() {
        let at = |c| Platform {
            os: Os::Windows,
            arch: Arch::Arm64,
            compiler: c,
        };
        let gcc_like = Condition::parse("gcc_like").unwrap();
        let msvc_like = Condition::parse("msvc_like").unwrap();
        for c in [Compiler::Gcc, Compiler::Clang, Compiler::AppleClang] {
            assert!(gcc_like.matches(&at(c)) && !msvc_like.matches(&at(c)));
        }
        for c in [Compiler::Msvc, Compiler::ClangCl] {
            assert!(msvc_like.matches(&at(c)) && !gcc_like.matches(&at(c)));
        }
        assert_eq!(
            at(Compiler::AppleClang).triplet(),
            "arm64-windows-apple_clang"
        );
    }

    #[test]
    fn ranks_and_unknown() {
        assert_eq!(Condition::parse("linux").unwrap().rank(), 1);
        assert_eq!(Condition::parse("msvc_like").unwrap().rank(), 2);
        assert_eq!(Condition::parse("clang_cl").unwrap().rank(), 3);
        assert_eq!(Condition::parse("arm").unwrap().rank(), 4);
        assert!(Condition::parse("linuxx").is_none());
        assert!(Condition::parse("public").is_none());
    }
}
