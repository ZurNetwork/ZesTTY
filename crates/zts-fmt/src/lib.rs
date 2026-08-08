//! zts-fmt library surface, shared by the CLI and the napi binding
//! (which the language-server uses for `textDocument/formatting`).
//!
//! Configuration (issue #70): a prettier-shaped subset — `printWidth`,
//! `useTabs`, `singleQuote`, plus the `sortImports` opt-in — resolved
//! from a `zts-fmt.json` discovered upward from the formatted file,
//! with explicit options (CLI flags, napi `options`) overlaid on top.
//! Unknown keys are errors, not ignored (fail-closed, like zts-check).
//! Deliberately NOT `.prettierrc`: `.zts` is excluded from prettier in
//! consumers, and half-sharing a config file invites drift.

use std::path::{Path, PathBuf};

use dprint_plugin_typescript::configuration::{
    Configuration, ConfigurationBuilder, NamedTypeImportsExportsOrder, QuoteStyle, SortOrder,
};
use dprint_plugin_typescript::{FormatTextOptions, format_text};

/// Matches the repo's prettier width so `.zts` and `.ts` neighbors agree.
pub const DEFAULT_LINE_WIDTH: u32 = 80;

/// Config file discovered upward from each formatted file.
pub const CONFIG_FILE: &str = "zts-fmt.json";

/// The prettier-shaped option subset. `None` = "not specified here" so
/// layers overlay cleanly; defaults apply only when every layer is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FmtOptions {
    /// prettier `printWidth`; default 80.
    pub print_width: Option<u32>,
    /// prettier `useTabs`; default false.
    pub use_tabs: Option<bool>,
    /// prettier `singleQuote`; default false.
    pub single_quote: Option<bool>,
    /// Opt back in to dprint's import/export sorting. Default false:
    /// canonical emit never reorders module declarations or named
    /// specifiers (prettier posture — side-effect import order makes
    /// sorting a semantic hazard, ruled in issue #70).
    pub sort_imports: Option<bool>,
}

impl FmtOptions {
    /// Per-field overlay: fields set in `over` win.
    pub fn overlay(mut self, over: &FmtOptions) -> FmtOptions {
        if over.print_width.is_some() {
            self.print_width = over.print_width;
        }
        if over.use_tabs.is_some() {
            self.use_tabs = over.use_tabs;
        }
        if over.single_quote.is_some() {
            self.single_quote = over.single_quote;
        }
        if over.sort_imports.is_some() {
            self.sort_imports = over.sort_imports;
        }
        self
    }
}

/// Parse a `zts-fmt.json`. Fail-closed: unknown keys and wrong types
/// are errors that name the offending key.
pub fn parse_options(json: &str) -> anyhow::Result<FmtOptions> {
    let value: serde_json::Value = serde_json::from_str(json)?;
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{CONFIG_FILE}: expected a JSON object"))?;
    let mut opts = FmtOptions::default();
    for (key, v) in obj {
        match key.as_str() {
            "printWidth" => {
                opts.print_width = Some(v.as_u64().filter(|w| (1..=10_000).contains(w)).ok_or_else(
                    || anyhow::anyhow!("{CONFIG_FILE}: printWidth must be a positive integer"),
                )? as u32)
            }
            "useTabs" => {
                opts.use_tabs =
                    Some(v.as_bool().ok_or_else(|| {
                        anyhow::anyhow!("{CONFIG_FILE}: useTabs must be a boolean")
                    })?)
            }
            "singleQuote" => {
                opts.single_quote = Some(v.as_bool().ok_or_else(|| {
                    anyhow::anyhow!("{CONFIG_FILE}: singleQuote must be a boolean")
                })?)
            }
            "sortImports" => {
                opts.sort_imports = Some(v.as_bool().ok_or_else(|| {
                    anyhow::anyhow!("{CONFIG_FILE}: sortImports must be a boolean")
                })?)
            }
            other => anyhow::bail!(
                "{CONFIG_FILE}: unknown option \"{other}\" (supported: printWidth, useTabs, singleQuote, sortImports)"
            ),
        }
    }
    Ok(opts)
}

/// Walk upward from `start_dir` for a [`CONFIG_FILE`]; parse the nearest
/// one. `Ok(None)` when no config file exists anywhere up the tree.
pub fn discover_options(start_dir: &Path) -> anyhow::Result<Option<(PathBuf, FmtOptions)>> {
    let mut dir = Some(start_dir.to_path_buf());
    while let Some(d) = dir {
        let candidate = d.join(CONFIG_FILE);
        if candidate.is_file() {
            let text = std::fs::read_to_string(&candidate)
                .map_err(|e| anyhow::anyhow!("{}: {e}", candidate.display()))?;
            let opts = parse_options(&text)
                .map_err(|e| anyhow::anyhow!("{}: {e}", candidate.display()))?;
            return Ok(Some((candidate, opts)));
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    Ok(None)
}

/// Discover the config governing `path` (upward from its directory —
/// relative paths resolve against the current directory, like the file
/// itself would).
pub fn resolve_for(path: &Path) -> anyhow::Result<FmtOptions> {
    let start = match path.parent() {
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) => p,
        None => Path::new("."),
    };
    // Canonicalize so bare filenames and relative paths still walk the
    // full ancestor chain; a start dir that doesn't exist (virtual
    // documents) just means "no config".
    let start = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    Ok(discover_options(&start)?
        .map(|(_, o)| o)
        .unwrap_or_default())
}

fn build_config(opts: &FmtOptions) -> Configuration {
    // dprint-plugin-typescript defaults are prettier-adjacent already
    // (2-space indent, double quotes); the divergences we correct are
    // line width (dprint 120 vs prettier 80) and sorting (dprint sorts
    // module declarations + named specifiers, prettier never reorders).
    let mut b = ConfigurationBuilder::new();
    b.line_width(opts.print_width.unwrap_or(DEFAULT_LINE_WIDTH))
        .use_tabs(opts.use_tabs.unwrap_or(false))
        .quote_style(if opts.single_quote.unwrap_or(false) {
            QuoteStyle::PreferSingle
        } else {
            QuoteStyle::PreferDouble
        });
    if !opts.sort_imports.unwrap_or(false) {
        b.module_sort_import_declarations(SortOrder::Maintain)
            .module_sort_export_declarations(SortOrder::Maintain)
            .import_declaration_sort_named_imports(SortOrder::Maintain)
            .export_declaration_sort_named_exports(SortOrder::Maintain)
            .import_declaration_sort_type_only_imports(NamedTypeImportsExportsOrder::None)
            .export_declaration_sort_type_only_exports(NamedTypeImportsExportsOrder::None);
    }
    b.build()
}

/// Format one zts source with explicit options (no config discovery).
/// `Ok(None)` = already formatted.
pub fn format_zts_with(
    path: &Path,
    source: String,
    opts: &FmtOptions,
) -> anyhow::Result<Option<String>> {
    let config = build_config(opts);
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_string());
    Ok(format_text(FormatTextOptions {
        path,
        extension: extension.as_deref(),
        text: source,
        config: &config,
        external_formatter: None,
    })?)
}

/// Format one zts source under the config file governing `path` (if
/// any). `Ok(None)` = already formatted.
pub fn format_zts(path: &Path, source: String) -> anyhow::Result<Option<String>> {
    format_zts_with(path, source, &resolve_for(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_the_subset() {
        let o = parse_options(
            r#"{ "printWidth": 100, "useTabs": true, "singleQuote": true, "sortImports": true }"#,
        )
        .unwrap();
        assert_eq!(o.print_width, Some(100));
        assert_eq!(o.use_tabs, Some(true));
        assert_eq!(o.single_quote, Some(true));
        assert_eq!(o.sort_imports, Some(true));
    }

    #[test]
    fn parse_fails_closed_on_unknown_keys() {
        let err = parse_options(r#"{ "semi": false }"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown option \"semi\""), "{err}");
    }

    #[test]
    fn parse_rejects_wrong_types() {
        assert!(parse_options(r#"{ "useTabs": "yes" }"#).is_err());
        assert!(parse_options(r#"{ "printWidth": 0 }"#).is_err());
        assert!(parse_options(r#"[]"#).is_err());
    }

    #[test]
    fn overlay_prefers_the_top_layer() {
        let file = FmtOptions {
            print_width: Some(100),
            use_tabs: Some(true),
            ..Default::default()
        };
        let cli = FmtOptions {
            print_width: Some(120),
            ..Default::default()
        };
        let merged = file.overlay(&cli);
        assert_eq!(merged.print_width, Some(120));
        assert_eq!(merged.use_tabs, Some(true));
    }

    #[test]
    fn tabs_and_single_quotes_shape_the_output() {
        let src = "const x = \"a\";\nfunction f() {\nreturn x;\n}\n";
        let opts = FmtOptions {
            use_tabs: Some(true),
            single_quote: Some(true),
            ..Default::default()
        };
        let out = format_zts_with(Path::new("t.zts"), src.into(), &opts)
            .unwrap()
            .expect("should reformat");
        assert!(out.contains("const x = 'a';"), "{out}");
        assert!(out.contains("\treturn x;"), "{out}");
    }

    #[test]
    fn imports_are_not_reordered_by_default() {
        // Side-effect import order is semantic; canonical emit must
        // keep it (issue #70 ruling). `./b` before `./a`, `z` before
        // `a` in the braces — all preserved.
        let src = "import { z, a } from \"./b\";\nimport \"./a\";\nexport const k = a + z;\n";
        let out = format_zts_with(Path::new("t.zts"), src.into(), &FmtOptions::default()).unwrap();
        assert_eq!(out, None, "already canonical — sorting must not kick in");
    }

    #[test]
    fn sort_imports_opts_back_in_to_dprint_sorting() {
        let src = "import { z, a } from \"./b\";\nimport { b } from \"./a\";\nexport const k = a + z + b;\n";
        let opts = FmtOptions {
            sort_imports: Some(true),
            ..Default::default()
        };
        let out = format_zts_with(Path::new("t.zts"), src.into(), &opts)
            .unwrap()
            .expect("sorting should reformat");
        let b_pos = out.find("./a").unwrap();
        let z_pos = out.find("./b").unwrap();
        assert!(b_pos < z_pos, "modules sorted: {out}");
        assert!(out.contains("{ a, z }"), "named specifiers sorted: {out}");
    }

    #[test]
    fn discovery_finds_the_nearest_config_upward() {
        let root = std::env::temp_dir().join(format!("zts-fmt-disc-{}", std::process::id()));
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join(CONFIG_FILE), r#"{ "useTabs": true }"#).unwrap();
        let (found, opts) = discover_options(&nested).unwrap().expect("config exists");
        assert_eq!(found, root.join(CONFIG_FILE));
        assert_eq!(opts.use_tabs, Some(true));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
