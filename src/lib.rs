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

pub fn zts_syntax() -> Syntax {
    Syntax::Typescript(TsSyntax {
        zts: true,
        ..Default::default()
    })
}

struct MapConfig;

impl SourceMapGenConfig for MapConfig {
    fn file_name_to_source(&self, f: &FileName) -> String {
        f.to_string()
    }

    fn inline_sources_content(&self, _: &FileName) -> bool {
        true
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
) -> Result<Output> {
    let fm = cm.new_source_file(filename.into(), source);

    let comments = SingleThreadedComments::default();
    let lexer = Lexer::new(
        zts_syntax(),
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

    semantic::check(&module, handler)?;

    let mut program = Program::Module(module);

    GLOBALS.set(&Default::default(), || {
        let unresolved_mark = Mark::new();
        let top_level_mark = Mark::new();
        program.mutate(resolver(unresolved_mark, top_level_mark, true));
        program.mutate(lower::lower(unresolved_mark));
        program.mutate(hygiene());
    });

    let module = program.expect_module();

    let mut buf = Vec::new();
    let mut srcmap = Vec::new();
    {
        let wr = JsWriter::new(cm.clone(), "\n", &mut buf, Some(&mut srcmap));
        let mut emitter = Emitter {
            cfg: Config::default(),
            cm: cm.clone(),
            comments: Some(&comments),
            wr,
        };
        emitter.emit_module(&module).context("emit failed")?;
    }

    let code = String::from_utf8(buf).context("codegen produced invalid utf-8")?;

    let map = cm.build_source_map(&srcmap, None, MapConfig);
    let mut map_buf = Vec::new();
    map.to_writer(&mut map_buf)
        .context("failed to serialize sourcemap")?;
    let map = String::from_utf8(map_buf).context("sourcemap is invalid utf-8")?;

    Ok(Output { code, map })
}

/// Compiles a `.zts` file on disk (convenience wrapper for the CLI and
/// tests).
pub fn compile_file(cm: &Lrc<SourceMap>, handler: &Handler, path: &Path) -> Result<Output> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    compile(cm, handler, FileName::Real(path.to_path_buf()), source)
}
