//! ztsc — the zts compiler driver.
//!
//! Pipeline: extended parser (fork) → semantic pass → lowering pass →
//! stock codegen → plain TS + sourcemap. TypeScript's own checker is the
//! backend verifier; we never reimplement it.

pub mod lower;
pub mod semantic;

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use swc_common::{
    FileName, GLOBALS, Mark, SourceMap, comments::SingleThreadedComments, errors::Handler,
    source_map::SourceMapGenConfig, sync::Lrc,
};
use swc_ecma_ast::Program;
use swc_ecma_codegen::{Config, Emitter, text_writer::JsWriter};
use swc_ecma_parser::{Lexer, Parser, StringInput, Syntax, TsSyntax};
use swc_ecma_transforms_base::{hygiene::hygiene, resolver};

/// Result of compiling one `.zts` module.
#[derive(Debug)]
pub struct Output {
    /// Generated TypeScript, without a `sourceMappingURL` comment.
    pub code: String,
    /// Sourcemap JSON mapping the generated TS back to the original `.zts`.
    pub map: String,
}

/// Compiler options. `Default` matches the plain `.zts` CLI path.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Parse JSX (`.ztsx`). Off by default: enabling it makes `<T>expr`
    /// type assertions ambiguous, exactly as in `.ts` vs `.tsx`.
    pub tsx: bool,
    /// Parse decorators. On by default — they are load-bearing TS.
    pub decorators: bool,
    /// Embed the original source text in the sourcemap. On for dev;
    /// build pipelines shipping maps publicly should turn it off.
    pub inline_sources_content: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            tsx: false,
            decorators: true,
            inline_sources_content: true,
        }
    }
}

pub fn zts_syntax(opts: Options) -> Syntax {
    Syntax::Typescript(TsSyntax {
        zts: true,
        tsx: opts.tsx,
        decorators: opts.decorators,
        ..Default::default()
    })
}

struct MapConfig {
    inline_sources_content: bool,
}

impl SourceMapGenConfig for MapConfig {
    fn file_name_to_source(&self, f: &FileName) -> String {
        f.to_string()
    }

    fn inline_sources_content(&self, _: &FileName) -> bool {
        self.inline_sources_content
    }
}

/// Compiles zts source text to TypeScript.
///
/// Diagnostics are emitted through `handler`; an `Err` means compilation
/// failed and diagnostics describe why.
pub fn compile(
    cm: &Lrc<SourceMap>,
    handler: &Handler,
    filename: FileName,
    source: String,
    opts: Options,
) -> Result<Output> {
    let fm = cm.new_source_file(filename.into(), source);

    let comments = SingleThreadedComments::default();
    let lexer = Lexer::new(
        zts_syntax(opts),
        Default::default(),
        StringInput::from(&*fm),
        Some(&comments),
    );
    let mut parser = Parser::new_from(lexer);

    let mut had_parse_error = false;
    for e in parser.take_errors() {
        had_parse_error = true;
        e.into_diagnostic(handler).emit();
    }
    let module = match parser.parse_module() {
        Ok(m) => m,
        Err(e) => {
            e.into_diagnostic(handler).emit();
            bail!("failed to parse module");
        }
    };
    for e in parser.take_errors() {
        had_parse_error = true;
        e.into_diagnostic(handler).emit();
    }
    if had_parse_error {
        bail!("failed to parse module");
    }

    if let Err(failure) = semantic::check(&module, handler) {
        if failure.depth_exceeded {
            // Drop for Expr recurses without stack protection; dropping an
            // over-deep AST would SIGABRT after the diagnostic. Leak it —
            // this path only fires on inputs we refuse to compile.
            std::mem::forget(module);
        }
        return Err(failure.into());
    }

    let mut program = Program::Module(module);

    GLOBALS.set(&Default::default(), || {
        let unresolved_mark = Mark::new();
        let top_level_mark = Mark::new();
        program.mutate(resolver(unresolved_mark, top_level_mark, true));
        program.mutate(lower::lower());
        program.mutate(hygiene());
    });

    let module = program.expect_module();

    let mut buf = Vec::new();
    let mut srcmap = Vec::new();
    {
        let wr = JsWriter::new(cm.clone(), "\n", &mut buf, Some(&mut srcmap));
        let mut emitter = Emitter {
            // inline_script: escape `</script>` etc. in string literals —
            // the Svelte preprocessor writes output back into <script> tags.
            cfg: Config::default().with_inline_script(true),
            cm: cm.clone(),
            comments: Some(&comments),
            wr,
        };
        emitter.emit_module(&module).context("emit failed")?;
    }

    let code = String::from_utf8(buf).context("codegen produced invalid utf-8")?;

    let map = cm.build_source_map(
        &srcmap,
        None,
        MapConfig {
            inline_sources_content: opts.inline_sources_content,
        },
    );
    let mut map_buf = Vec::new();
    map.to_writer(&mut map_buf)
        .context("failed to serialize sourcemap")?;
    let map = String::from_utf8(map_buf).context("sourcemap is invalid utf-8")?;

    Ok(Output { code, map })
}

/// Compilation failed; `diagnostics` holds the rendered error text.
#[derive(Debug)]
pub struct CompileFailure {
    pub diagnostics: String,
}

impl std::fmt::Display for CompileFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.diagnostics)
    }
}

impl std::error::Error for CompileFailure {}

/// Self-contained entry point for embedders (napi bindings, plugins):
/// creates its own SourceMap and a diagnostics-capturing handler, and
/// returns the rendered diagnostics on failure instead of printing them.
pub fn compile_source(
    filename: &str,
    source: String,
    opts: Options,
) -> std::result::Result<Output, CompileFailure> {
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(b)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let cm: Lrc<SourceMap> = Default::default();
    let buf = Buf::default();
    let emitter = swc_common::errors::EmitterWriter::new(
        Box::new(buf.clone()),
        Some(cm.clone()),
        false,
        false,
    );
    let handler = Handler::with_emitter_and_flags(Box::new(emitter), Default::default());

    let result = compile(
        &cm,
        &handler,
        FileName::Custom(filename.to_string()),
        source,
        opts,
    );

    result.map_err(|_| CompileFailure {
        diagnostics: String::from_utf8_lossy(&buf.0.lock().unwrap()).into_owned(),
    })
}

/// Compiles a `.zts`/`.ztsx` file on disk (convenience wrapper for the CLI
/// and tests). `.ztsx` turns on JSX parsing, mirroring `.ts`/`.tsx`.
pub fn compile_file(cm: &Lrc<SourceMap>, handler: &Handler, path: &Path) -> Result<Output> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let opts = Options {
        tsx: path.extension().is_some_and(|e| e == "ztsx"),
        ..Default::default()
    };
    compile(
        cm,
        handler,
        FileName::Real(path.to_path_buf()),
        source,
        opts,
    )
}
