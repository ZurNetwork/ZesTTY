//! Node binding for the zts compiler.
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
}

const COMPILE_STACK_SIZE: usize = 64 * 1024 * 1024;

#[napi]
pub fn compile(
    source: String,
    filename: String,
    options: Option<CompileOptions>,
) -> napi::Result<CompileResult> {
    let options = options.unwrap_or_default();
    let defaults = ztsc::Options::default();
    let opts = ztsc::Options {
        tsx: options.tsx.unwrap_or(defaults.tsx),
        decorators: options.decorators.unwrap_or(defaults.decorators),
        inline_sources_content: options
            .inline_sources_content
            .unwrap_or(defaults.inline_sources_content),
    };

    let handle = std::thread::Builder::new()
        .name("ztsc-compile".into())
        .stack_size(COMPILE_STACK_SIZE)
        .spawn(move || ztsc::compile_source(&filename, source, opts))
        .map_err(|e| napi::Error::from_reason(format!("ztsc: failed to spawn: {e}")))?;

    match handle.join() {
        Ok(Ok(out)) => Ok(CompileResult {
            code: out.code,
            map: out.map,
        }),
        Ok(Err(failure)) => Err(napi::Error::from_reason(failure.diagnostics)),
        Err(_) => Err(napi::Error::from_reason(
            "ztsc: compiler panicked; this is a bug in zts, please report it",
        )),
    }
}
