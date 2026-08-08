//! zts-fmt — format `.zts` / `.ztsx` sources (Phase 7 items 9-10).
//!
//! Usage: `zts-fmt [--check] [paths...]` (default path: `.`)
//!
//! Formats in place; `--check` writes nothing and exits 1 listing files
//! that would change. Engine: the ZesTTY fork of
//! dprint-plugin-typescript — prettier-style layout, zts print rules,
//! idempotence pinned by 1144 fixed-point specs in the fork.
//!
//! Deliberately zts-only: `.ts`/`.js` formatting stays whatever the
//! consumer already uses (prettier/dprint); zts-fmt exists because no
//! other formatter can parse zts.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use zts_fmt::{DEFAULT_LINE_WIDTH, FmtOptions, format_zts_with, resolve_for};

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".svelte-kit",
    ".zts-check",
];

fn collect(path: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if path.is_dir() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if SKIP_DIRS.contains(&name) {
            return Ok(());
        }
        for entry in std::fs::read_dir(path)? {
            collect(&entry?.path(), out)?;
        }
    } else if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "zts" || e == "ztsx")
    {
        out.push(path.to_path_buf());
    }
    Ok(())
}

fn main() -> ExitCode {
    let mut check = false;
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut cli_opts = FmtOptions::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--use-tabs" => cli_opts.use_tabs = Some(true),
            "--single-quote" => cli_opts.single_quote = Some(true),
            "--sort-imports" => cli_opts.sort_imports = Some(true),
            "--print-width" => {
                cli_opts.print_width = match args.next().and_then(|w| w.parse().ok()) {
                    Some(w) if w > 0 => Some(w),
                    _ => {
                        eprintln!("zts-fmt: --print-width requires a positive integer");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: zts-fmt [--check] [--print-width <n>] [--use-tabs] [--single-quote] [--sort-imports] [paths...]"
                );
                eprintln!(
                    "options default to a zts-fmt.json discovered upward from each file (flags override), then printWidth {DEFAULT_LINE_WIDTH}, spaces, double quotes, imports kept in source order"
                );
                return ExitCode::SUCCESS;
            }
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    if paths.is_empty() {
        paths.push(PathBuf::from("."));
    }

    let mut files = Vec::new();
    for p in &paths {
        if let Err(e) = collect(p, &mut files) {
            eprintln!("zts-fmt: {}: {e}", p.display());
            return ExitCode::FAILURE;
        }
    }

    let mut unformatted = 0usize;
    let mut errors = 0usize;
    for file in &files {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("zts-fmt: {}: {e}", file.display());
                errors += 1;
                continue;
            }
        };
        let opts = match resolve_for(file) {
            Ok(discovered) => discovered.overlay(&cli_opts),
            Err(e) => {
                eprintln!("zts-fmt: {}: {e}", file.display());
                errors += 1;
                continue;
            }
        };
        match format_zts_with(file, source.clone(), &opts) {
            Ok(None) => {}
            Ok(Some(formatted)) => {
                if check {
                    eprintln!("zts-fmt: would reformat {}", file.display());
                    unformatted += 1;
                } else if let Err(e) = std::fs::write(file, formatted) {
                    eprintln!("zts-fmt: {}: {e}", file.display());
                    errors += 1;
                }
            }
            Err(e) => {
                eprintln!(
                    "zts-fmt: {}: {}",
                    file.display(),
                    e.to_string().lines().next().unwrap_or("format error")
                );
                errors += 1;
            }
        }
    }

    if errors > 0 || (check && unformatted > 0) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
