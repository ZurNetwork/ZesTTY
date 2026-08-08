//! Node binding for the ZesTTY (zts) compiler.
//!
//! Each compile runs on a dedicated thread with a 64 MiB stack: Node
//! worker stacks are small, `swc` parsing/lowering is recursive, and a
//! stack overflow would SIGABRT the whole host process (Vite dev server),
//! not just the transform.

#[macro_use]
extern crate napi_derive;

#[napi(object)]
pub struct CompileResult {
    /// Generated TypeScript (no sourceMappingURL comment).
    pub code: String,
    /// Sourcemap JSON mapping the TS back to the zts source.
    pub map: String,
}

#[napi(object)]
#[derive(Default)]
pub struct CompileOptions {
    /// Parse JSX (for `.ztsx`). Default false.
    pub tsx: Option<bool>,
    /// Parse decorators. Default true.
    pub decorators: Option<bool>,
    /// Embed original source text in the sourcemap. Default true.
    pub inline_sources_content: Option<bool>,
    /// Import `__ztsAbsurd` from @zestty/core instead of emitting the
    /// per-file helper. Default TRUE since Phase 6 (issue #47, "universal
    /// absurd"); pass false for output that must not depend on
    /// @zestty/core (virtual twins, dep-less consumers).
    pub preamble_import: Option<bool>,
}

const COMPILE_STACK_SIZE: usize = 64 * 1024 * 1024;

/// Format zts source (Phase 7: zts-fmt via the dprint fork). Returns
/// null when the input is already formatted. Runs on the same 64 MiB
/// thread discipline as compile — the formatter parses recursively too.
#[napi]
pub fn format(source: String, filename: String) -> napi::Result<Option<String>> {
    let handle = std::thread::Builder::new()
        .name("zts-fmt".into())
        .stack_size(COMPILE_STACK_SIZE)
        .spawn(move || zts_fmt::format_zts(std::path::Path::new(&filename), source))
        .map_err(|e| napi::Error::from_reason(format!("zts-fmt: failed to spawn: {e}")))?;

    match handle.join() {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(err)) => Err(napi::Error::from_reason(format!("zts-fmt: {err}"))),
        Err(_) => Err(napi::Error::from_reason(
            "zts-fmt: formatter panicked; this is a bug in zts, please report it",
        )),
    }
}

#[napi]
pub fn compile(
    source: String,
    filename: String,
    options: Option<CompileOptions>,
) -> napi::Result<CompileResult> {
    let options = options.unwrap_or_default();
    let defaults = zestty::Options::default();
    let opts = zestty::Options {
        tsx: options.tsx.unwrap_or(defaults.tsx),
        decorators: options.decorators.unwrap_or(defaults.decorators),
        inline_sources_content: options
            .inline_sources_content
            .unwrap_or(defaults.inline_sources_content),
        preamble_import: options.preamble_import.unwrap_or(defaults.preamble_import),
    };

    let handle = std::thread::Builder::new()
        .name("zestty-compile".into())
        .stack_size(COMPILE_STACK_SIZE)
        .spawn(move || zestty::compile_source(&filename, source, opts))
        .map_err(|e| napi::Error::from_reason(format!("zestty: failed to spawn: {e}")))?;

    match handle.join() {
        Ok(Ok(out)) => Ok(CompileResult {
            code: out.code,
            map: out.map,
        }),
        Ok(Err(failure)) => Err(napi::Error::from_reason(failure.diagnostics)),
        Err(_) => Err(napi::Error::from_reason(
            "zestty: compiler panicked; this is a bug in zts, please report it",
        )),
    }
}
