//! Feature selection and `when` guards (spec 7.2, 7.3).

use std::collections::BTreeMap;

use crate::config::{
    ConfigError,
    schema::{FeatureBody, FeatureDecl, Manifest, When},
    vocab::Platform,
};
use crate::configure::options::{OptionSource, ResolvedOption};

/// The body of a feature that applies on this platform.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveFeature<'a> {
    /// `feature.simd.avx2` for a selected variant, `feature.<name>` for a bundle.
    pub origin: String,
    pub body: &'a FeatureBody,
}

/// Picks, for every declared feature, the body that applies. `None` means the
/// feature contributes nothing on this platform.
pub fn select_features<'a>(
    m: &'a Manifest,
    opts: &BTreeMap<String, ResolvedOption>,
    p: &Platform,
) -> Result<BTreeMap<String, Option<ActiveFeature<'a>>>, ConfigError> {
    let mut out = BTreeMap::new();
    for (name, decl) in &m.features {
        let active = match decl {
            FeatureDecl::Bundle(body) => body.when.matches(p).then(|| ActiveFeature {
                origin: format!("feature.{name}"),
                body,
            }),
            FeatureDecl::Variants(variants) => {
                let opt = opts.get(name).ok_or_else(|| {
                    ConfigError::invalid(
                        format!("feature.{name}"),
                        format!("internal error: no resolved option `{name}` to select a variant"),
                    )
                })?;
                let value = opt.value.to_string();
                match variants.get(&value) {
                    None => None,
                    Some(body) if body.when.matches(p) => Some(ActiveFeature {
                        origin: format!("feature.{name}.{value}"),
                        body,
                    }),
                    Some(body) => {
                        return Err(guard_mismatch(name, &value, body, opt, p));
                    }
                }
            }
        };
        out.insert(name.clone(), active);
    }
    Ok(out)
}

/// The failing guard written as `os = windows, compiler = clang_cl`.
fn describe_guard(w: &When) -> String {
    let mut axes = Vec::new();
    if let Some(os) = w.os {
        axes.push(format!("os = {}", os.as_str()));
    }
    if let Some(c) = w.compiler {
        axes.push(format!("compiler = {}", c.as_str()));
    }
    if let Some(a) = w.arch {
        axes.push(format!("arch = {}", a.as_str()));
    }
    axes.join(", ")
}

/// The `default.<condition>` keys that would select another value, one per
/// axis of the guard the platform fails.
fn failing_defaults(w: &When, p: &Platform) -> String {
    let mut keys = Vec::new();
    if w.arch.is_some_and(|a| a != p.arch) {
        keys.push(format!("`default.{}`", p.arch.as_str()));
    }
    if w.os.is_some_and(|o| o != p.os) {
        keys.push(format!("`default.{}`", p.os.as_str()));
    }
    if w.compiler.is_some_and(|c| !c.matches(p.compiler)) {
        keys.push(format!("`default.{}`", p.compiler.as_str()));
    }
    keys.join(" or ")
}

fn guard_mismatch(
    name: &str,
    value: &str,
    body: &FeatureBody,
    opt: &ResolvedOption,
    p: &Platform,
) -> ConfigError {
    let origin = format!("feature.{name}.{value}");
    let how = match opt.source {
        OptionSource::Cli => format!("selected with `--opt {name}={value}`"),
        OptionSource::Cache => {
            format!(
                "selected in the option cache, options.toml in the build directory (`{name}` = `{value}`)"
            )
        }
        OptionSource::Default => format!(
            "the default of option `{name}`; add {} to the option to pick a value for this platform",
            failing_defaults(&body.when, p)
        ),
    };
    ConfigError::at_key(
        origin,
        "when",
        format!(
            "its `when` guard requires {}, but the current platform is {} ({how})",
            describe_guard(&body.when),
            p.triplet()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Diagnostics;
    use crate::config::{
        flat::parse_flat,
        schema::parse_manifest,
        vocab::{Arch, Compiler, Os},
    };
    use crate::configure::options::resolve_options;

    const X64: Platform = Platform {
        os: Os::Linux,
        arch: Arch::X86_64,
        compiler: Compiler::Gcc,
    };
    const ARM64: Platform = Platform {
        os: Os::Linux,
        arch: Arch::Arm64,
        compiler: Compiler::Gcc,
    };

    const SPEC: &str = r#"
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
"#;

    fn manifest(src: &str) -> Manifest {
        parse_manifest(parse_flat(src).unwrap()).unwrap()
    }

    fn run<'a>(
        m: &'a Manifest,
        p: &Platform,
        cli: &[(&str, &str)],
        cache: &[(&str, &str)],
    ) -> Result<BTreeMap<String, Option<ActiveFeature<'a>>>, ConfigError> {
        let cli: Vec<_> = cli
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let cache: BTreeMap<_, _> = cache
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let opts =
            resolve_options(&m.options, &cli, &cache, p, &mut Diagnostics::default()).unwrap();
        select_features(m, &opts, p)
    }

    fn origin(r: &BTreeMap<String, Option<ActiveFeature<'_>>>, k: &str) -> Option<String> {
        r[k].as_ref().map(|a| a.origin.clone())
    }

    #[test]
    fn selects_variant_from_option() {
        let m = manifest(SPEC);
        let r = run(&m, &X64, &[], &[]).unwrap();
        assert_eq!(origin(&r, "simd").unwrap(), "feature.simd.avx2");
        let r = run(&m, &ARM64, &[], &[]).unwrap();
        assert_eq!(origin(&r, "simd").unwrap(), "feature.simd.neon");
    }

    #[test]
    fn missing_variant_table_is_noop() {
        let m = manifest(SPEC);
        let r = run(&m, &X64, &[("simd", "sse42")], &[]).unwrap();
        assert!(r["simd"].is_none());
    }

    #[test]
    fn user_selected_guard_mismatch_fails() {
        let m = manifest(SPEC);
        let e = run(&m, &ARM64, &[("simd", "avx2")], &[])
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("feature.simd.avx2")
                && e.contains("arch = x86_64")
                && e.contains("--opt simd=avx2")
                && e.contains("arm64-linux-gcc"),
            "{e}"
        );
    }

    #[test]
    fn cache_selected_guard_mismatch_names_the_cache() {
        let m = manifest(SPEC);
        let e = run(&m, &ARM64, &[], &[("simd", "avx2")])
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("selected in the option cache") && e.contains("feature.simd.avx2"),
            "{e}"
        );
    }

    #[test]
    fn default_guard_mismatch_hints_default_suffix() {
        let m = manifest(
            r#"
[option.simd]
type    = "enum"
values  = ["none", "avx2"]
default = "avx2"

[feature.simd.avx2]
when = { arch = "x86_64" }
"#,
        );
        let e = run(&m, &ARM64, &[], &[]).unwrap_err().to_string();
        assert!(
            e.contains("`default.arm64`") && e.contains("the default of option `simd`"),
            "{e}"
        );
    }

    #[test]
    fn default_guard_hint_names_the_failing_axis() {
        let m = manifest(
            r#"
[option.simd]
type    = "enum"
values  = ["none", "avx2"]
default = "avx2"

[feature.simd.avx2]
when = { os = "windows", compiler = "msvc" }
"#,
        );
        let e = run(&m, &X64, &[], &[]).unwrap_err().to_string();
        assert!(
            e.contains("`default.linux` or `default.gcc`") && !e.contains("default.x86_64"),
            "{e}"
        );
    }

    #[test]
    fn guard_lists_every_axis() {
        let m = manifest(
            r#"
[option.simd]
type    = "enum"
values  = ["none", "avx2"]
default = "none"

[feature.simd.avx2]
when = { os = "windows", compiler = "clang_cl" }
"#,
        );
        let e = run(&m, &X64, &[("simd", "avx2")], &[])
            .unwrap_err()
            .to_string();
        assert!(e.contains("os = windows, compiler = clang_cl"), "{e}");
    }

    #[test]
    fn bundle_with_unmatched_when_is_skipped() {
        let m = manifest("[feature.b]\nwhen = { os = \"windows\" }\ndefines = [\"X\"]\n");
        let r = run(&m, &X64, &[], &[]).unwrap();
        assert!(r["b"].is_none());
    }

    #[test]
    fn bundle_with_matching_when_applies() {
        let m = manifest("[feature.b]\nwhen = { os = \"linux\" }\ndefines = [\"X\"]\n");
        let r = run(&m, &X64, &[], &[]).unwrap();
        assert_eq!(origin(&r, "b").unwrap(), "feature.b");
    }

    #[test]
    fn bundle_without_when_applies() {
        let m = manifest(SPEC);
        let r = run(&m, &X64, &[], &[]).unwrap();
        assert_eq!(
            origin(&r, "opengl_deprecation").unwrap(),
            "feature.opengl_deprecation"
        );
    }

    #[test]
    fn variant_feature_without_resolved_option_is_internal_error() {
        let m = manifest(SPEC);
        let e = select_features(&m, &BTreeMap::new(), &X64)
            .unwrap_err()
            .to_string();
        assert!(e.contains("feature.simd") && e.contains("simd"), "{e}");
    }
}
