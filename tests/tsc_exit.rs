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
    tsc_with(file, &["--strict"])
}

fn tsc_with(file: &Path, extra: &[&str]) -> (bool, String) {
    let tsc_bin = repo_root().join("node_modules/.bin/tsc");
    assert!(
        tsc_bin.exists(),
        "tsc not found at {}; run `npm install` in the repo root",
        tsc_bin.display()
    );
    let out = Command::new(tsc_bin)
        .args(["--noEmit", "--pretty", "false"])
        .args(extra)
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
fn single_variant_exhaustive_match_passes_tsc() {
    // tsc only narrows unions; the keystone must therefore narrow the
    // discriminant *property*, or this correct program gets rejected.
    let (ts_path, _) = compile_to("match_single_variant.zts", "exit_single.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(
        ok,
        "tsc rejected a single-variant exhaustive match:\n{text}"
    );
}

#[test]
fn outer_let_narrowing_survives_in_arm_bodies() {
    // TS preserves control-flow narrowing of outer `let`s inside an
    // IIFE, but NOT inside a callback passed to a named helper. The
    // lowering must stay an IIFE or this correct program is rejected.
    let (ts_path, _) = compile_to("match_outer_narrowing.zts", "exit_outer_narrowing.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "outer let narrowing lost in arm bodies:\n{text}");
}

#[test]
fn await_in_discriminant_passes_tsc() {
    let (ts_path, _) = compile_to("match_await_discriminant.zts", "exit_await_disc.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected await-in-discriminant output:\n{text}");
}

#[test]
fn shadowed_error_class_passes_tsc() {
    // The absurd helper must not reference the global `Error`: hygiene
    // would rename a user class shadowing it and silently change what the
    // (unrenamed) type annotations refer to.
    let (ts_path, _) = compile_to("match_shadowed_error.zts", "exit_shadowed_error.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected shadowed-Error output:\n{text}");
}

#[test]
fn decorated_class_passes_tsc() {
    let (ts_path, _) = compile_to("match_decorated_class.zts", "exit_decorated.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected decorated-class output:\n{text}");
}

#[test]
fn asi_regression_passes_tsc_and_keeps_calls() {
    let (ts_path, _) = compile_to("match_asi_regression.zts", "exit_asi.ts");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        code.contains("match(1)") && code.contains("match(3)"),
        "ASI call statements must survive compilation:\n{code}"
    );
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected ASI regression output:\n{text}");
}

#[test]
fn directives_stay_in_prologue() {
    let (ts_path, _) = compile_to("match_directives_imports.zts", "exit_directives.ts");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    let first_directive = code.find("\"use client\"").expect("directive missing");
    let helper = code.find("__ztsAbsurd").expect("helper missing");
    let import_pos = code.find("import ").expect("import missing");
    assert!(
        first_directive < import_pos && import_pos < helper,
        "expected directives, then imports, then the helper:\n{code}"
    );
    assert_eq!(
        code.matches("function __ztsAbsurd").count(),
        1,
        "two matches must share one helper"
    );
}

#[test]
fn missing_arm_fails_tsc_even_without_strict() {
    // Users compile generated TS under their own tsconfig; the keystone
    // must not depend on --strict.
    let (ts_path, _) = compile_to("match_missing_arm.zts", "exit_missing_nostrict.ts");
    let (ok, text) = tsc_with(&ts_path, &[]);
    assert!(!ok, "keystone must hold without --strict");
    assert!(
        text.contains("error TS2345"),
        "unexpected failure mode:\n{text}"
    );
}

#[test]
fn literal_match_passes_tsc() {
    let (ts_path, _) = compile_to("match_literal.zts", "exit_literal.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected literal-mode match output:\n{text}");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        !code.contains("__k"),
        "literal mode must not emit the `.kind` alias:\n{code}"
    );
}

#[test]
fn missing_literal_arm_fails_tsc_naming_the_literal() {
    // The literal-mode keystone: equality narrowing runs `__m` to `never`
    // only when every literal has an arm. Must hold without --strict too.
    let (ts_path, _) = compile_to("match_missing_literal_arm.zts", "exit_missing_lit.ts");
    let (ok, text) = tsc_with(&ts_path, &[]);
    assert!(!ok, "literal keystone must hold without --strict");
    assert!(
        text.contains("error TS2345") && text.contains("\"warn\""),
        "tsc must name the missing literal:\n{text}"
    );
}

#[test]
fn wildcard_match_passes_tsc_and_disables_keystone() {
    let (ts_path, _) = compile_to("match_wildcard.zts", "exit_wildcard.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected wildcard match output:\n{text}");
    // Two variants are deliberately unhandled; `_` must have replaced the
    // absurd tail, so no keystone (and no helper) appears at all.
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        !code.contains("__ztsAbsurd"),
        "wildcard arm must remove the exhaustiveness keystone:\n{code}"
    );
}

#[test]
fn arm_body_maps_to_original_span() {
    // Phase 2's "breakpoint in .zts" story depends on arm bodies mapping
    // back to their source, not just the absurd call.
    let (ts_path, map_json) = compile_to("match_basic.zts", "exit_map_body.ts");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    let (gen_line, gen_col) = code
        .lines()
        .enumerate()
        .find_map(|(i, l)| l.find("PI * radius").map(|c| (i as u32, c as u32)))
        .expect("lowered arm body not found");

    let sm = swc_sourcemap::SourceMap::from_slice(map_json.as_bytes()).expect("invalid sourcemap");
    let token = sm
        .lookup_token(gen_line, gen_col)
        .expect("no token for arm body");

    let src = std::fs::read_to_string(repo_root().join("tests/fixtures/match_basic.zts")).unwrap();
    let src_line = src.lines().nth(token.get_src_line() as usize).unwrap();
    assert!(
        src_line.contains("PI * radius"),
        "arm body maps to {:?} (line {}), expected the Circle arm",
        src_line,
        token.get_src_line() + 1
    );
}

#[test]
fn if_expr_basic_passes_tsc() {
    let (ts_path, _) = compile_to("if_expr_basic.zts", "exit_if_basic.ts");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        code.contains('?') && !code.contains("(()=>"),
        "statement-free if-expressions must lower to ternaries:\n{code}"
    );
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected ternary-lowered if-expression:\n{text}");
}

#[test]
fn if_expr_multi_stmt_passes_tsc() {
    let (ts_path, _) = compile_to("if_expr_multi_stmt.zts", "exit_if_multi.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected IIFE-lowered if-expression:\n{text}");
}

#[test]
fn if_expr_await_in_simple_branch_passes_tsc() {
    // Ternary lowering keeps await legal in statement-free branches.
    let (ts_path, _) = compile_to("if_expr_await_simple.zts", "exit_if_await.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected await inside ternary-lowered if:\n{text}");
}

#[test]
fn match_block_arm_bodies_pass_tsc() {
    let (ts_path, _) = compile_to("match_block_arm_bodies.zts", "exit_arm_blocks.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected block-bodied match arms:\n{text}");
    // Block arm bodies splice — no nested IIFE for the Circle arm.
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        code.contains("const r2 = radius * radius;"),
        "arm block statements must splice into the arm block:\n{code}"
    );
}

#[test]
fn enum_basic_passes_tsc() {
    let (ts_path, _) = compile_to("enum_basic.zts", "exit_enum_basic.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected lowered enum + match:\n{text}");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        !code.contains("enum "),
        "output must never contain a TS enum:\n{code}"
    );
}

#[test]
fn enum_export_passes_tsc() {
    let (ts_path, _) = compile_to("enum_export_multi_field.zts", "exit_enum_export.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected exported enum output:\n{text}");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        code.matches("export ").count() >= 4,
        "both the type alias and the factory const must stay exported:\n{code}"
    );
}

#[test]
fn enum_recursive_passes_tsc() {
    let (ts_path, _) = compile_to("enum_generic_types.zts", "exit_enum_tree.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected recursive enum output:\n{text}");
}

#[test]
fn enum_wrong_factory_arg_fails_tsc() {
    // The factories must be TYPED: passing a string where the field says
    // number has to be a tsc error.
    let (out, diags) =
        common::compile_fixture(&repo_root().join("tests/fixtures/enum_basic.zts")).unwrap();
    assert_eq!(diags, "");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let bad = format!("{}\nShape.Circle(\"oops\");\n", out.code);
    let bad_path = dir.join("exit_enum_bad_arg.ts");
    std::fs::write(&bad_path, bad).unwrap();
    let (ok, text) = tsc(&bad_path);
    assert!(!ok, "tsc must reject a mistyped factory argument");
    assert!(text.contains("error TS"), "unexpected output:\n{text}");
}

#[test]
fn if_expr_precedence_passes_tsc() {
    let (ts_path, _) = compile_to("if_expr_precedence.zts", "exit_if_prec.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected precedence fixture:\n{text}");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        code.contains("(c ? 1 : 2) + 1") || code.contains("(c ? 1 : 2)+1"),
        "lowered ternary must be parenthesized inside operators:\n{code}"
    );
}

#[test]
fn generated_js_runs_with_correct_semantics() {
    // tsc cannot see precedence miscompiles (`-c ? 1 : 2` typechecks); only
    // executing the output proves the VALUES are right.
    let (ts_path, _) = compile_to("exec_semantics.zts", "exit_exec.ts");
    let out = Command::new("node")
        .arg(&ts_path)
        .output()
        .expect("failed to spawn node");
    assert!(
        out.status.success(),
        "generated code produced wrong runtime values:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `@zestty/core` resolves through its `dist/` build output; make sure it
/// exists so `cargo test` is self-sufficient (locally AND in CI) instead of
/// depending on a prior `npm test` having built it as a side effect.
fn ensure_core_dist() {
    let dist = repo_root().join("packages/core/dist/index.d.ts");
    if dist.exists() {
        return;
    }
    let out = Command::new("npm")
        .args(["run", "build", "-w", "@zestty/core"])
        .current_dir(repo_root())
        .output()
        .expect("failed to spawn npm to build @zestty/core");
    assert!(
        out.status.success(),
        "building @zestty/core failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn result_from_core_composes_with_match() {
    // The whole point: @zestty/core's Result + expression-if + match, one
    // file, verified by tsc end-to-end (feature #2's exit test).
    ensure_core_dist();
    let (ts_path, _) = compile_to("result_match.zts", "exit_result.ts");
    let (ok, text) = tsc_with(
        &ts_path,
        &[
            "--strict",
            "--module",
            "esnext",
            "--moduleResolution",
            "bundler",
        ],
    );
    assert!(ok, "tsc rejected Result+match composition:\n{text}");

    // Delete the Err arm: the keystone must fire for Results too.
    let code = std::fs::read_to_string(&ts_path).unwrap();
    let broken = code.replace(
        "if (__k === \"Err\") {",
        "if (false as boolean) { const __nope = __k;",
    );
    assert_ne!(code, broken, "fixture drifted; update the arm surgery");
    let broken_path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("exit_result_broken.ts");
    std::fs::write(&broken_path, broken).unwrap();
    let (ok, text) = tsc_with(
        &broken_path,
        &[
            "--strict",
            "--module",
            "esnext",
            "--moduleResolution",
            "bundler",
        ],
    );
    assert!(
        !ok,
        "keystone must reject a Result match missing its Err arm"
    );
    assert!(text.contains("TS2345"), "unexpected failure:\n{text}");
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
