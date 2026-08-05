//! The Phase 1 exit test from the README:
//!
//! 1. An exhaustive match compiles and passes `tsc --strict`.
//! 2. Deleting an arm makes tsc reject the generated TS with a `never`
//!    assignability error, and the error span maps back through the
//!    sourcemap to the original `match` in the `.zts` file.

mod common;

use std::{path::Path, process::Command};

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn tsc(file: &Path) -> (bool, String) {
    let tsc_bin = repo_root().join("node_modules/.bin/tsc");
    assert!(
        tsc_bin.exists(),
        "tsc not found at {}; run `npm install` in the repo root",
        tsc_bin.display()
    );
    let out = Command::new(tsc_bin)
        .args(["--noEmit", "--strict", "--pretty", "false"])
        .arg(file)
        .output()
        .expect("failed to spawn tsc");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn compile_to(fixture: &str, out_name: &str) -> (std::path::PathBuf, String) {
    let fixture_path = repo_root().join("tests/fixtures").join(fixture);
    let (out, diags) = common::compile_fixture(&fixture_path)
        .unwrap_or_else(|(e, d)| panic!("{fixture} failed to compile: {e}\n{d}"));
    assert_eq!(diags, "", "unexpected diagnostics for {fixture}");

    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(dir).unwrap();
    let ts_path = dir.join(out_name);
    std::fs::write(&ts_path, &out.code).unwrap();
    (ts_path, out.map)
}

#[test]
fn exhaustive_match_passes_tsc() {
    let (ts_path, _) = compile_to("match_basic.zts", "exit_basic.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected exhaustive match output:\n{text}");
}

#[test]
fn hygiene_output_passes_tsc() {
    let (ts_path, _) = compile_to("match_hygiene.zts", "exit_hygiene.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected hygiene fixture output:\n{text}");
}

#[test]
fn nested_match_passes_tsc() {
    let (ts_path, _) = compile_to("match_nested.zts", "exit_nested.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected nested match output:\n{text}");
}

#[test]
fn missing_arm_fails_tsc_and_maps_to_match() {
    let (ts_path, map_json) = compile_to("match_missing_arm.zts", "exit_missing.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(!ok, "tsc unexpectedly accepted a non-exhaustive match");
    assert!(
        text.contains("error TS2345") && text.contains("'never'"),
        "expected a never-assignability error, got:\n{text}"
    );

    // Parse `file.ts(line,col): error TS2345: ...`
    let needle = "): error TS2345";
    let line_col = text
        .lines()
        .find(|l| l.contains(needle))
        .and_then(|l| {
            let open = l.rfind('(')?;
            let close = l[open..].find(')')? + open;
            let mut it = l[open + 1..close].split(',');
            let line: u32 = it.next()?.trim().parse().ok()?;
            let col: u32 = it.next()?.trim().parse().ok()?;
            Some((line, col))
        })
        .unwrap_or_else(|| panic!("could not parse tsc error position from:\n{text}"));

    // Map the (1-based) tsc position back through the sourcemap.
    let sm =
        swc_sourcemap::SourceMap::from_slice(map_json.as_bytes()).expect("invalid sourcemap JSON");
    let token = sm
        .lookup_token(line_col.0 - 1, line_col.1 - 1)
        .expect("no sourcemap token covers the tsc error position");

    // The token must point into the original `match` expression.
    let fixture = repo_root().join("tests/fixtures/match_missing_arm.zts");
    let src = std::fs::read_to_string(fixture).unwrap();
    let src_line = src
        .lines()
        .nth(token.get_src_line() as usize)
        .expect("mapped line out of range");
    let mapped = &src_line[token.get_src_col() as usize..];
    assert!(
        mapped.starts_with("match"),
        "expected error to map to the original `match`, but it maps to line {:?} col {} ({mapped:?})",
        token.get_src_line() + 1,
        token.get_src_col() + 1,
    );
}
