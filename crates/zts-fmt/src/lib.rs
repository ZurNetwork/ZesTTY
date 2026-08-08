//! zts-fmt library surface: one function, shared by the CLI and the
//! napi binding (which the language-server uses for
//! `textDocument/formatting`).

use std::path::Path;

use dprint_plugin_typescript::configuration::{Configuration, ConfigurationBuilder};
use dprint_plugin_typescript::{FormatTextOptions, format_text};

/// Matches the repo's prettier width so `.zts` and `.ts` neighbors agree.
pub const DEFAULT_LINE_WIDTH: u32 = 80;

fn default_config() -> Configuration {
    // dprint-plugin-typescript defaults are prettier-adjacent already
    // (2-space indent, double quotes); line width is the one divergence
    // (dprint 120 vs prettier 80).
    ConfigurationBuilder::new()
        .line_width(DEFAULT_LINE_WIDTH)
        .build()
}

/// Format one zts source. `Ok(None)` = already formatted.
pub fn format_zts(path: &Path, source: String) -> anyhow::Result<Option<String>> {
    let config = default_config();
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
