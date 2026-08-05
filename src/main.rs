//! ztsc — compile `.zts` files to plain TypeScript.
//!
//! Usage: `ztsc <input.zts> [-o <output.ts>] [--no-map]`
//!
//! Writes `<input>.ts` and `<input>.ts.map` next to the input unless `-o`
//! is given.

use std::{path::PathBuf, process::ExitCode};

use swc_common::{
    SourceMap,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};

struct Args {
    input: PathBuf,
    output: Option<PathBuf>,
    emit_map: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut input = None;
    let mut output = None;
    let mut emit_map = true;

    let mut argv = std::env::args_os().skip(1);
    while let Some(arg) = argv.next() {
        match arg.to_str() {
            Some("-o") | Some("--output") => {
                let v = argv
                    .next()
                    .ok_or_else(|| "missing value for -o".to_string())?;
                output = Some(PathBuf::from(v));
            }
            Some("--no-map") => emit_map = false,
            Some("-h") | Some("--help") => {
                return Err("usage: ztsc <input.zts> [-o <output.ts>] [--no-map]".to_string());
            }
            _ if input.is_none() => input = Some(PathBuf::from(arg)),
            _ => return Err(format!("unexpected argument: {}", arg.to_string_lossy())),
        }
    }

    Ok(Args {
        input: input
            .ok_or_else(|| "usage: ztsc <input.zts> [-o <output.ts>] [--no-map]".to_string())?,
        output,
        emit_map,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };

    let out_path = args
        .output
        .clone()
        .unwrap_or_else(|| args.input.with_extension("ts"));
    let map_path = {
        let mut s = out_path.clone().into_os_string();
        s.push(".map");
        PathBuf::from(s)
    };

    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Auto, true, false, Some(cm.clone()));

    let output = match ztsc::compile_file(&cm, &handler, &args.input) {
        Ok(o) => o,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let mut code = output.code;
    if args.emit_map {
        let map_file = map_path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        // The basename is interpolated into the emitted module; a newline
        // (or comment terminator) in it would inject code into the output.
        if map_file
            .chars()
            .any(|c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
            || map_file.contains("*/")
        {
            eprintln!("error: refusing output filename with newline or '*/': {map_file:?}");
            return ExitCode::FAILURE;
        }
        code.push_str(&format!("//# sourceMappingURL={map_file}\n"));
    }

    if let Err(err) = std::fs::write(&out_path, code) {
        eprintln!("error: failed to write {}: {err}", out_path.display());
        return ExitCode::FAILURE;
    }
    if args.emit_map
        && let Err(err) = std::fs::write(&map_path, output.map)
    {
        eprintln!("error: failed to write {}: {err}", map_path.display());
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
