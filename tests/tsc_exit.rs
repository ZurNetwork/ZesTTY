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
    // Since issue #47 the default emit imports __ztsAbsurd from
    // @zestty/core, so resolution flags matching a real consumer are part
    // of the default check environment.
    tsc_with(
        file,
        &[
            "--strict",
            "--module",
            "esnext",
            "--moduleResolution",
            "bundler",
        ],
    )
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
    // Default emit imports @zestty/core (issue #47) — every tsc run needs
    // its dist built, not just the Result test.
    ensure_core_dist();
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
fn inline_preamble_output_is_self_contained() {
    // The --inline-preamble opt-out (issue #47): output must pass tsc
    // WITHOUT any module-resolution environment — no @zestty/core, no
    // resolution flags — because that's its entire reason to exist.
    let fixture_path = repo_root().join("tests/fixtures/match_basic.zts");
    let opts = zestty::Options {
        preamble_import: false,
        ..Default::default()
    };
    let (out, diags) = common::compile_fixture_with(&fixture_path, opts)
        .unwrap_or_else(|(e, d)| panic!("match_basic failed to compile: {e}\n{d}"));
    assert_eq!(diags, "");
    assert!(
        !out.code.contains("@zestty/core"),
        "inline mode must not import the core package:\n{}",
        out.code
    );
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(dir).unwrap();
    let ts_path = dir.join("exit_inline_preamble.ts");
    std::fs::write(&ts_path, &out.code).unwrap();
    let (ok, text) = tsc_with(&ts_path, &["--strict"]);
    assert!(ok, "tsc rejected self-contained inline output:\n{text}");
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
    let user_import = code.find("import ").expect("user import missing");
    let helper_import = code
        .find("import { __ztsAbsurd }")
        .expect("helper import missing");
    assert!(
        first_directive < user_import && user_import < helper_import,
        "expected directives, then imports (helper import after user imports):\n{code}"
    );
    assert_eq!(
        code.matches("import { __ztsAbsurd }").count(),
        1,
        "two matches must share one helper import"
    );
    assert_eq!(
        code.matches("function __ztsAbsurd").count(),
        0,
        "default emit must not inline the helper (issue #47)"
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
fn zts_type_shadow_cannot_forge_exhaustiveness() {
    // SECURITY (0.5.0 review, finding 1). Hygiene is not TS-type-aware, so
    // a user `type __ztsRange0 = number` is neither renamed nor a tsc
    // error — it silently re-points the range predicate and makes a match
    // covering 2 of 5 members certify as exhaustive. Reproduced before the
    // fix: tsc exited 0 and the program threw at runtime.
    //
    // The compile must now fail in the SEMANTIC pass, so no TypeScript
    // ever exists for tsc to bless.
    let path = repo_root().join("tests/fixtures/errors/err_zts_type_shadow.zts");
    let (_, diags) = common::compile_fixture(&path)
        .expect_err("a __zts type-plane shadow must not compile at all");
    assert!(
        diags.contains("reserved for zts-generated code"),
        "the shadow must be rejected by the __zts reservation:\n{diags}"
    );
    // Every type-name binder in the AST, one diagnostic each: type alias,
    // type parameter, interface, namespace, module-scope alias, and the
    // three `import X = Ns.T` routes (plain, `export import`, nested
    // namespace) that survived the first cut of this fix.
    assert_eq!(
        diags.matches("reserved for zts-generated code").count(),
        8,
        "every type-name binder must be covered:\n{diags}"
    );

    // Each import-equals route on its own, so a regression in one cannot
    // hide behind the others — and each must die in the SEMANTIC pass,
    // with no TypeScript produced for tsc to bless.
    for (name, src) in [
        (
            "plain",
            "namespace H { export type W = number; }\n\
             namespace A {\n\
             \x20 import __ztsRange0 = H.W;\n\
             \x20 export const u: __ztsRange0 = 1;\n\
             }\n",
        ),
        (
            "export import",
            "namespace H { export type W = number; }\n\
             export namespace A { export import __ztsRange0 = H.W; }\n",
        ),
        (
            "nested namespace",
            "namespace H { export type W = number; }\n\
             export namespace A { export namespace B {\n\
             \x20 import __ztsRange0 = H.W;\n\
             \x20 export const u: __ztsRange0 = 1;\n\
             } }\n",
        ),
    ] {
        let err = zestty::compile_source("ieq.zts", src.to_string(), Default::default())
            .expect_err(&format!("{name} import-equals shadow must not compile"));
        assert!(
            err.diagnostics.contains("reserved for zts-generated code"),
            "{name}: {}",
            err.diagnostics
        );
    }

    // ... and the shadow really was load-bearing: rename the alias and the
    // very same 2-of-5 match is the TS2345 it always should have been.
    let renamed = "type Code = 1 | 2 | 3 | 4 | 5;\n\
                   export const f = (c: Code): string => {\n\
                   \x20 type NotReserved = number;\n\
                   \x20 const _use: NotReserved = 1;\n\
                   \x20 return match (c) {\n\
                   \x20   1..=2 => \"low\",\n\
                   \x20 };\n\
                   };\n";
    let out = zestty::compile_source(
        "shadow_renamed.zts",
        renamed.to_string(),
        Default::default(),
    )
    .expect("the renamed form must still compile");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(dir).unwrap();
    let ts_path = dir.join("exit_shadow_renamed.ts");
    std::fs::write(&ts_path, &out.code).unwrap();
    let (ok, text) = tsc(&ts_path);
    assert!(!ok, "a 2-of-5 ranged match must not typecheck");
    assert!(
        text.contains("TS2345") && text.contains("3"),
        "tsc must name the uncovered members:\n{text}"
    );
}

#[test]
fn ranged_match_over_closed_union_passes_tsc() {
    // 0.5.0 range arms, the payload case: every member of a closed numeric
    // literal union is covered by SOME range, so the keystone discharges
    // and no `_` is needed. The type-predicate shape is what makes this
    // possible — a `switch`/`===` expansion would emit TS2678/TS2367 for
    // every enumerated value not in the union.
    let (ts_path, _) = compile_to("match_range_basic.zts", "exit_range_basic.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected exhaustive ranged match:\n{text}");

    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        code.contains("__ztsAbsurd"),
        "an exhaustive ranged match must still carry the keystone:\n{code}"
    );
    assert!(
        code.contains("__ztsInRange<__ztsRange0>(__m, 200, 299)"),
        "range arm must lower to the type-predicate call:\n{code}"
    );
    // The alias is a real, erased literal union — not a computed type.
    assert!(
        code.contains("type __ztsRange0 = 200 | 201 |"),
        "range alias must enumerate its members:\n{code}"
    );
}

#[test]
fn ranged_match_missing_coverage_fails_tsc_naming_the_value() {
    // Delete one range arm: the uncovered member must survive to the
    // keystone and be NAMED, exactly as a missing literal arm is today.
    let (ts_path, _) = compile_to("match_range_basic.zts", "exit_range_gap.ts");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    let broken = code.replace(
        "if (__ztsInRange<__ztsRange3>(__m, 500, 599)) {",
        "if (false as boolean) {",
    );
    assert_ne!(code, broken, "fixture drifted; update the arm surgery");
    let broken_path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("exit_range_gap_broken.ts");
    std::fs::write(&broken_path, broken).unwrap();
    let (ok, text) = tsc(&broken_path);
    assert!(!ok, "keystone must reject an uncovered union member");
    assert!(
        text.contains("TS2345") && text.contains("500"),
        "tsc must name the uncovered value:\n{text}"
    );
}

#[test]
fn ranged_match_over_open_number_needs_a_wildcard() {
    // The compiler NEVER decides whether `_` is required — tsc does. Over
    // an open `number`, a range proves nothing, so the keystone fires...
    let (ts_path, _) = compile_to("match_range_open_number.zts", "exit_range_open.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(!ok, "a range cannot discharge an open `number` scrutinee");
    assert!(
        text.contains("TS2345") && text.contains("number"),
        "tsc must name the undischarged type:\n{text}"
    );

    // ... and the same shape WITH a `_` passes, keystone removed.
    let (ok_path, _) = compile_to("match_range_mixed.zts", "exit_range_mixed.ts");
    let (ok, text) = tsc(&ok_path);
    assert!(ok, "tsc rejected a `_`-terminated ranged match:\n{text}");
    let code = std::fs::read_to_string(&ok_path).unwrap();
    assert!(
        !code.contains("__ztsAbsurd"),
        "`_` must still replace the keystone in a ranged match:\n{code}"
    );
    // The bounds the author wrote reach the call verbatim; only the erased
    // alias is enumerated.
    assert!(
        code.contains("(__m, 0x10, 0x1f)"),
        "range bound literals must keep their raw form:\n{code}"
    );
}

#[test]
fn ranged_match_runs_with_correct_semantics() {
    // The type says "one of these integer literals"; only executing the
    // output proves the runtime agrees — 404.5, NaN and "404" must all
    // fall through a `400..=499` arm.
    let (ts_path, _) = compile_to("match_range_exec.zts", "exit_range_exec.ts");
    let out = Command::new("node")
        .arg(&ts_path)
        .output()
        .expect("failed to spawn node");
    assert!(
        out.status.success(),
        "ranged match produced wrong runtime values:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn ranged_match_inline_preamble_is_self_contained() {
    // The --inline-preamble opt-out must cover __ztsInRange too, or a
    // dep-less consumer (plain zts-check, the language server) gets a
    // missing-module error the author cannot act on.
    let fixture_path = repo_root().join("tests/fixtures/match_range_basic.zts");
    let opts = zestty::Options {
        preamble_import: false,
        ..Default::default()
    };
    let (out, diags) = common::compile_fixture_with(&fixture_path, opts)
        .unwrap_or_else(|(e, d)| panic!("match_range_basic failed to compile: {e}\n{d}"));
    assert_eq!(diags, "");
    assert!(
        !out.code.contains("@zestty/core"),
        "inline mode must not import the core package:\n{}",
        out.code
    );
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(dir).unwrap();
    let ts_path = dir.join("exit_range_inline.ts");
    std::fs::write(&ts_path, &out.code).unwrap();
    let (ok, text) = tsc_with(&ts_path, &["--strict"]);
    assert!(ok, "tsc rejected self-contained ranged output:\n{text}");
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
fn newtype_passes_tsc() {
    let (ts_path, _) = compile_to("newtype_basic.zts", "exit_newtype.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected newtype output:\n{text}");
}

#[test]
fn exported_newtype_passes_tsc_and_stays_exported() {
    let (ts_path, _) = compile_to("newtype_export.zts", "exit_newtype_export.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected exported newtype output:\n{text}");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        code.contains("export type UserId") && code.contains("export const UserId"),
        "both the branded type and the factory must stay exported:\n{code}"
    );
}

#[test]
fn newtype_id_confusion_fails_tsc() {
    // The safety property: same underlying type, different brands — a raw
    // string and a UserId must both be rejected where AccountId is
    // expected. Must hold without --strict.
    let (ts_path, _) = compile_to("newtype_id_confusion.zts", "exit_newtype_confusion.ts");
    let (ok, text) = tsc_with(&ts_path, &[]);
    assert!(!ok, "brand must hold without --strict");
    let ts2345 = text.matches("error TS2345").count();
    assert!(
        ts2345 == 2,
        "expected exactly 2 brand violations (raw string + wrong newtype), got {ts2345}:\n{text}"
    );
}

#[test]
fn try_operator_passes_tsc() {
    ensure_core_dist();
    let (ts_path, _) = compile_to("try_basic.zts", "exit_try.ts");
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
    assert!(ok, "tsc rejected try-operator output:\n{text}");
}

#[test]
fn try_on_non_result_fails_tsc() {
    // `?` on a non-Result: the generated `__t.kind` has nothing to read.
    let (ts_path, _) = compile_to("try_on_non_result.zts", "exit_try_nonresult.ts");
    let (ok, text) = tsc_with(&ts_path, &[]);
    assert!(!ok, "`?` on a non-Result must fail tsc (without --strict)");
    assert!(
        text.contains("error TS2339") && text.contains("kind"),
        "expected a missing-`kind` error:\n{text}"
    );
}

#[test]
fn try_err_type_mismatch_fails_tsc() {
    // The propagated Err must satisfy the enclosing return type — tsc
    // enforces error-type compatibility on the generated `return __t;`.
    ensure_core_dist();
    let (ts_path, _) = compile_to("try_err_type_mismatch.zts", "exit_try_mismatch.ts");
    let (ok, text) = tsc_with(
        &ts_path,
        &["--module", "esnext", "--moduleResolution", "bundler"],
    );
    assert!(
        !ok,
        "incompatible Err type must fail tsc (without --strict)"
    );
    assert!(
        text.contains("error TS2322"),
        "expected a return-type assignability error:\n{text}"
    );
}

#[test]
fn try_in_single_stmt_slots_passes_tsc() {
    // Gate finding #1: these used to PANIC in codegen (ZtsTry survived
    // lowering because the slots are not part of a Vec<Stmt>).
    let (ts_path, _) = compile_to("try_single_stmt_slots.zts", "exit_try_slots.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected single-statement-slot try output:\n{text}");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        !code.contains("ZtsTry"),
        "no try node may survive lowering:\n{code}"
    );
}

#[test]
fn newtype_over_union_id_confusion_fails_tsc() {
    // Gate finding #2: without the parens the brand attached to the LAST
    // union member only and the raw literal slipped through.
    let (ts_path, _) = compile_to("newtype_union.zts", "exit_newtype_union.ts");
    let (ok, text) = tsc_with(&ts_path, &[]);
    assert!(!ok, "union brand must hold without --strict");
    assert_eq!(
        text.matches("error TS2345").count(),
        1,
        "exactly the raw-literal call must fail:\n{text}"
    );
}

#[test]
fn newtype_typeof_underlying_passes_tsc() {
    // Gate finding #4: a `value`-named factory param was captured by
    // `typeof value` underlying types (TS2502).
    let (ts_path, _) = compile_to("newtype_typeof.zts", "exit_newtype_typeof.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected typeof/function-type newtypes:\n{text}");
}

#[test]
fn try_with_destructuring_default_passes_tsc() {
    // Gate finding #5: the permit for the statement's own `?` was burned
    // by the arrow IIFE inside the destructuring default.
    let (ts_path, _) = compile_to("try_destructuring_default.zts", "exit_try_destr.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected destructuring-default try:\n{text}");
}

#[test]
fn bigint_and_null_literal_match_pass_tsc() {
    let (ts_path, _) = compile_to("match_bigint.zts", "exit_bigint.ts");
    let (ok, text) = tsc_with(&ts_path, &["--strict", "--target", "es2020"]);
    assert!(ok, "tsc rejected bigint/null literal match:\n{text}");
}

#[test]
fn try_guard_maps_to_original_question_mark() {
    // The generated `if (__t.kind === "Err")` guard must map back to the
    // `.zts` line carrying the `?` (breakpoint story).
    let (ts_path, map_json) = compile_to("try_basic.zts", "exit_try_map.ts");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    let (gen_line, gen_col) = code
        .lines()
        .enumerate()
        .find_map(|(i, l)| l.find("__t.kind").map(|c| (i as u32, c as u32)))
        .expect("try guard not found");

    let sm = swc_sourcemap::SourceMap::from_slice(map_json.as_bytes()).expect("invalid sourcemap");
    let token = sm
        .lookup_token(gen_line, gen_col)
        .expect("no token for try guard");

    let src = std::fs::read_to_string(repo_root().join("tests/fixtures/try_basic.zts")).unwrap();
    let src_line = src.lines().nth(token.get_src_line() as usize).unwrap();
    assert!(
        src_line.contains('?'),
        "try guard maps to {:?} (line {}), expected the `?` statement",
        src_line,
        token.get_src_line() + 1
    );
}

#[test]
fn object_accessor_try_passes_tsc() {
    // Security verification V3: annotated object-literal getters have
    // real function context for `?`.
    let (ts_path, _) = compile_to("try_object_accessors.zts", "exit_try_getter.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected object-accessor try output:\n{text}");
}

#[test]
fn union_passes_tsc_and_guard_narrows() {
    // `union` (Zuri-approved 2026-08-06): has() must narrow a raw string
    // to the vocabulary type under --strict, and the closed side stays
    // exhaustively matchable.
    let (ts_path, _) = compile_to("union_basic.zts", "exit_union.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected union output:\n{text}");
}

#[test]
fn exported_union_passes_tsc_and_stays_exported() {
    let (ts_path, _) = compile_to("union_export.zts", "exit_union_export.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected exported union output:\n{text}");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        code.contains("export type Level") && code.contains("export const Level"),
        "both halves must stay exported:\n{code}"
    );
}

#[test]
fn union_guard_is_es5_clean() {
    // Review finding 3: has() must not raise the emitted-TS lib floor
    // (`includes` is ES2016). indexOf keeps the guard ES5-clean.
    let (ts_path, _) = compile_to("union_basic.zts", "exit_union_es2015.ts");
    let (ok, text) = tsc_with(&ts_path, &["--strict", "--target", "es2015"]);
    assert!(ok, "union guard must typecheck at --target es2015:\n{text}");
}

#[test]
fn numeric_union_passes_tsc_and_guard_narrows() {
    // 0.5.0 numeric members: `has` takes a `number` and narrows a raw wire
    // value into the vocabulary, the closed side stays exhaustively
    // matchable, and impls still merge into the same factory const.
    let (ts_path, _) = compile_to("union_numeric.zts", "exit_union_numeric.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected numeric union output:\n{text}");

    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        code.contains("has: (__ztsRaw: number): __ztsRaw is HttpStatus"),
        "an all-numeric union's guard must take a number:\n{code}"
    );
    // The ES5-clean guard shape and the cast-the-ARGUMENT discipline are
    // unchanged: a receiver cast would need parens the fixer strips.
    assert!(
        code.contains("HttpStatus.values.indexOf(__ztsRaw as HttpStatus) !== -1"),
        "guard shape must be unchanged for numeric members:\n{code}"
    );
    let (ok, text) = tsc_with(&ts_path, &["--strict", "--target", "es2015"]);
    assert!(
        ok,
        "numeric union guard must typecheck at --target es2015:\n{text}"
    );
}

#[test]
fn numeric_union_guard_rejects_a_string() {
    // The guard's parameter type comes from the member SHAPES: an
    // all-numeric vocabulary must not silently accept a string.
    let (ts_path, _) = compile_to("union_numeric.zts", "exit_union_numeric_arg.ts");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    let bad = format!("{code}\nHttpStatus.has(\"404\");\n");
    let bad_path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("exit_union_numeric_bad.ts");
    std::fs::write(&bad_path, bad).unwrap();
    let (ok, text) = tsc(&bad_path);
    assert!(!ok, "a numeric union's has() must reject a string");
    assert!(text.contains("TS2345"), "unexpected failure:\n{text}");
}

#[test]
fn mixed_union_passes_tsc_and_guard_takes_both() {
    let (ts_path, _) = compile_to("union_mixed.zts", "exit_union_mixed.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected mixed union output:\n{text}");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        code.contains("has: (__ztsRaw: string | number): __ztsRaw is Ans"),
        "a mixed union's guard must take both primitives:\n{code}"
    );
}

#[test]
fn string_union_output_is_byte_identical() {
    // The 0.5.0 member-shape change must not move a single byte for the
    // vocabularies that already existed. (The snapshots pin this too; this
    // one says it out loud where a reviewer will look.)
    let (ts_path, _) = compile_to("union_basic.zts", "exit_union_unchanged.ts");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        code.contains("has: (__ztsRaw: string): __ztsRaw is DeleteOutcome"),
        "an all-string union's guard must still take a string:\n{code}"
    );
}

#[test]
fn numeric_union_and_ranges_compose() {
    // THE COMPOSITION KEYSTONE: wire guard -> closed numeric vocabulary ->
    // exhaustive RANGED match, all proven by tsc with no `_` anywhere.
    let (ts_path, _) = compile_to("union_range_composition.zts", "exit_union_range.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected the union+range composition:\n{text}");
    let code = std::fs::read_to_string(&ts_path).unwrap();
    assert!(
        code.contains("__ztsAbsurd"),
        "the composition must keep the keystone live:\n{code}"
    );

    // Delete the 3xx range arm: the members it covered must survive to the
    // keystone and be named.
    let broken = code.replace(
        "if (__ztsInRange<__ztsRange1>(__m, 300, 399)) {",
        "if (false as boolean) {",
    );
    assert_ne!(code, broken, "fixture drifted; update the arm surgery");
    let broken_path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("exit_union_range_broken.ts");
    std::fs::write(&broken_path, broken).unwrap();
    let (ok, text) = tsc(&broken_path);
    assert!(!ok, "deleting a range arm must break exhaustiveness");
    assert!(
        text.contains("TS2345") && text.contains("301"),
        "tsc must name the uncovered member:\n{text}"
    );
}

#[test]
fn union_missing_arm_fails_tsc_naming_the_literal() {
    let (ts_path, _) = compile_to("union_missing_arm.zts", "exit_union_missing.ts");
    let (ok, text) = tsc_with(&ts_path, &[]);
    assert!(!ok, "union exhaustiveness must hold without --strict");
    assert!(
        text.contains("error TS2345") && text.contains("\"error\""),
        "tsc must name the missing member:\n{text}"
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
    // Tests run in parallel; only one npm build may run (and everyone
    // must wait for it), or concurrent builds race on dist/.
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
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
    });
}

#[test]
fn result_from_core_composes_with_match() {
    // The whole point: @zestty/core's Result + expression-if + match, one
    // file, verified by tsc end-to-end (feature #2's exit test).
    let (ts_path, _) = compile_to("result_match.zts", "exit_result.ts");
    let (ok, text) = tsc(&ts_path);
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
    let (ok, text) = tsc(&broken_path);
    assert!(
        !ok,
        "keystone must reject a Result match missing its Err arm"
    );
    assert!(text.contains("TS2345"), "unexpected failure:\n{text}");
}

#[test]
fn non_empty_array_passes_tsc() {
    // Phase 7 `T[+]`: proven-non-empty call sites pass, xs[0] is T even
    // under noUncheckedIndexedAccess (the whole point of the tuple).
    let (ts_path, _) = compile_to("non_empty_array.zts", "exit_nonempty.ts");
    let (ok, text) = tsc_with(
        &ts_path,
        &[
            "--strict",
            "--noUncheckedIndexedAccess",
            "--module",
            "esnext",
            "--moduleResolution",
            "bundler",
        ],
    );
    assert!(ok, "tsc rejected valid non-empty array usage:\n{text}");
}

#[test]
fn non_empty_array_rejects_possibly_empty() {
    // The safety property: [] and plain T[] cannot satisfy T[+].
    let (ts_path, _) = compile_to("non_empty_array_empty_call.zts", "exit_nonempty_bad.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(!ok, "tsc must reject possibly-empty arguments for T[+]");
    assert_eq!(
        text.matches("error TS2345").count(),
        2,
        "expected one TS2345 per bad call site:\n{text}"
    );
}

#[test]
fn mut_field_mutation_passes_tsc() {
    // Phase 7: `mut` fields opt out of readonly — mutating one is fine.
    let (ts_path, _) = compile_to("enum_mut_field.zts", "exit_mut_field.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected mutation of a mut field:\n{text}");
}

#[test]
fn readonly_payload_mutation_fails_tsc() {
    // THE 0.4.0 breaking change: non-mut payload writes and kind writes
    // are TS2540, one per mutation site (that's the migration story).
    let (ts_path, _) = compile_to("enum_readonly_mutation.zts", "exit_readonly.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(!ok, "tsc must reject readonly payload mutation");
    assert_eq!(
        text.matches("error TS2540").count(),
        2,
        "expected one TS2540 per mutation site (field + kind):\n{text}"
    );
}

#[test]
fn impl_output_passes_tsc() {
    // Phase 6 traits, whole loop: trait interface + enum + impl + direct
    // call + generic dictionary call, all verified by tsc.
    let (ts_path, _) = compile_to("impl_basic.zts", "exit_impl.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected impl output:\n{text}");
    let (ts_path, _) = compile_to("impl_multi_trait.zts", "exit_impl_multi.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected multi-trait impl output:\n{text}");
}

#[test]
fn impl_wrong_return_fails_satisfies() {
    // Conformance safety property: a method that does not satisfy its
    // trait is a tsc error on the generated `satisfies` clause.
    let (ts_path, _) = compile_to("impl_wrong_return.zts", "exit_impl_wrong.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(!ok, "tsc must reject a non-conforming impl");
    // The error lands on the METHOD (its span survives into the twin), as
    // an assignability failure against the trait's member type.
    assert!(
        text.contains("TS2322") && text.contains("not assignable"),
        "expected a satisfies assignability error on the method:\n{text}"
    );
}

#[test]
fn constrict_true_claims_pass_tsc() {
    // Phase 7 item 2: every claim in the fixture is TRUE — the module
    // type-checks, and the emit is types only (nothing new at runtime).
    let (ts_path, _) = compile_to("constrict_ok.zts", "exit_constrict_ok.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected true constrict claims:\n{text}");

    // Inline mode (no @zestty/core): the helper aliases are emitted
    // locally and the file is fully self-contained.
    let fixture_path = repo_root().join("tests/fixtures/constrict_ok.zts");
    let opts = zestty::Options {
        preamble_import: false,
        ..Default::default()
    };
    let (out, diags) = common::compile_fixture_with(&fixture_path, opts)
        .unwrap_or_else(|(e, d)| panic!("constrict_ok failed inline: {e}\n{d}"));
    assert_eq!(diags, "");
    assert!(!out.code.contains("@zestty/core"));
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let ts_path = dir.join("exit_constrict_inline.ts");
    std::fs::write(&ts_path, &out.code).unwrap();
    let (ok, text) = tsc_with(&ts_path, &["--strict"]);
    assert!(ok, "inline constrict output not self-contained:\n{text}");
}

#[test]
fn constrict_false_claim_fails_tsc() {
    // The safety property: a false claim is a TS2344 (constraint of
    // __ztsExpect not satisfied) at the assert's own alias.
    let (ts_path, _) = compile_to("constrict_fail.zts", "exit_constrict_fail.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(!ok, "tsc must reject a false constrict claim");
    assert!(
        text.contains("TS2344"),
        "expected the constraint-violation error:\n{text}"
    );
}

#[test]
fn impl_assoc_and_multi_from_pass_tsc() {
    // Traits v2: associated functions with type-args, and the
    // comma-header multi-instantiation with one union body, both
    // verified end-to-end (calls + dictionary passing included).
    let (ts_path, _) = compile_to("impl_assoc_from.zts", "exit_impl_assoc.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected associated-function impl output:\n{text}");
    let (ts_path, _) = compile_to("impl_multi_from.zts", "exit_impl_multi_from.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected comma-header impl output:\n{text}");
}

#[test]
fn impl_newtype_and_union_pass_tsc() {
    // Phase 7 item 5: newtype impls (Object.assign keeps the factory
    // callable AND carrying methods) and union impls (object merge),
    // both with working dictionary passing.
    let (ts_path, _) = compile_to("impl_newtype_union.zts", "exit_impl_nu.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(ok, "tsc rejected newtype/union impl output:\n{text}");
}

#[test]
fn impl_from_wrong_arg_type_fails_satisfies() {
    // Traits v2 conformance: names match (semantic passes), the body's
    // param type contradicts the header's instantiation — tsc rejects
    // the satisfies obligation.
    let (ts_path, _) = compile_to("impl_from_wrong.zts", "exit_impl_from_wrong.ts");
    let (ok, text) = tsc(&ts_path);
    assert!(!ok, "tsc must reject a From<string> impl taking number");
    assert!(
        text.contains("error TS"),
        "expected a satisfies failure:\n{text}"
    );
}
// NOTE: cross-impl method collisions moved from a tsc exit test to the
// SEMANTIC pass (traits v2 early checks — errors/impl_cross_dup.zts),
// superseding the "left to tsc" disposition, re-decided with Zuri.

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
