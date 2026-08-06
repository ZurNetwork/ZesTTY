//! Generated adversarial inputs that must produce DIAGNOSTICS, not stack
//! overflows: uncatchable SIGABRTs are unacceptable once the compiler runs
//! in-process (napi/Vite).

use zestty::{Options, compile_source};

/// Run on a 64 MiB-stack thread — the same configuration the napi binding
/// uses in production. (Bare #[test] threads get 2 MiB, far below what a
/// legitimate depth-2048 semantic walk needs in a debug build.)
fn on_napi_sized_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .expect("compile thread panicked or aborted")
}

#[test]
fn deep_else_if_chain_is_a_diagnostic_not_a_crash() {
    // else-if links are not Expr nodes; the depth guard must charge them
    // explicitly or this overflows the recursive passes.
    let n = 5000;
    let mut src = String::from("declare const b: boolean;\nexport const v = if (b) { 0 }");
    for i in 0..n {
        src.push_str(&format!(" else if (b) {{ {i} }}"));
    }
    src.push_str(" else { -1 };\n");

    let err =
        on_napi_sized_thread(move || compile_source("deep_chain.zts", src, Options::default()))
            .expect_err("a 5000-link else-if chain must be rejected");
    assert!(
        err.diagnostics.contains("nesting exceeds"),
        "expected the depth diagnostic, got:\n{}",
        err.diagnostics
    );
}

#[test]
fn moderate_else_if_chain_compiles() {
    let n = 200;
    let mut src = String::from("declare const b: boolean;\nexport const v = if (b) { 0 }");
    for i in 0..n {
        src.push_str(&format!(" else if (b) {{ {i} }}"));
    }
    src.push_str(" else { -1 };\n");

    let out = on_napi_sized_thread(move || compile_source("chain.zts", src, Options::default()))
        .expect("200-link chain should compile");
    assert!(out.code.contains('?'), "should lower to ternaries");
}

#[test]
fn deep_type_nesting_is_a_diagnostic_not_a_crash() {
    // Security-gate F3: 30k-deep parenthesized type annotations used to
    // SIGABRT at parse time (no maybe_grow on parse_ts_type) and the
    // semantic pass never counted TsType nesting at all.
    let n = 30_000;
    let src = format!(
        "declare const y: {}string{};\n",
        "(".repeat(n),
        ")".repeat(n)
    );
    let err =
        on_napi_sized_thread(move || compile_source("deep_type.zts", src, Options::default()))
            .expect_err("a 30k-deep type nest must be rejected");
    assert!(
        err.diagnostics.contains("nesting exceeds"),
        "expected the depth diagnostic, got:\n{}",
        err.diagnostics
    );
}

#[test]
fn deep_if_stmt_nesting_is_a_diagnostic_not_a_crash() {
    // Security-gate F4: 100k nested `if` statements overflowed the
    // post-parse passes (the parser has maybe_grow; the visitors did not
    // charge Stmt nesting).
    let n = 100_000;
    let mut src = String::from("declare const c: boolean;\nfunction f(): void {\n");
    src.push_str(&"if (c) ".repeat(n));
    src.push(';');
    src.push_str("\n}\n");
    let err = on_napi_sized_thread(move || compile_source("deep_if.zts", src, Options::default()))
        .expect_err("a 100k-deep if nest must be rejected");
    assert!(
        err.diagnostics.contains("nesting exceeds"),
        "expected the depth diagnostic, got:\n{}",
        err.diagnostics
    );
}

#[test]
fn diagnostics_are_bounded_on_pathological_input() {
    // Security-gate F2: one error per character of a single-line input
    // used to render ~831 MB of diagnostics from 406 KB of source. Both
    // the semantic error count and the rendered buffer are capped now.
    let n = 2100;
    let mut src = String::from("//");
    src.push_str(&"x".repeat(400_000));
    src.push('\n');
    src.push_str("const v = ");
    src.push_str(&"(".repeat(n));
    src.push('1');
    src.push_str(&")?".repeat(n));
    src.push_str(";\n");
    let err = on_napi_sized_thread(move || compile_source("amplify.zts", src, Options::default()))
        .expect_err("nested tries must be rejected");
    assert!(
        err.diagnostics.len() < 2 * 1024 * 1024,
        "diagnostics must be bounded, got {} bytes",
        err.diagnostics.len()
    );
}

#[test]
fn deep_binding_pattern_is_a_diagnostic_not_a_crash() {
    // Security verification V1: 70k-deep nested object binding patterns
    // used to SIGABRT at parse time (no maybe_grow on pattern recursion)
    // and were uncounted by the depth budget afterwards.
    let n = 70_000;
    let src = format!("const {}a{} = z;\n", "{a:".repeat(n), "}".repeat(n));
    let err = on_napi_sized_thread(move || compile_source("deep_pat.zts", src, Options::default()))
        .expect_err("a 70k-deep binding pattern must be rejected");
    assert!(
        err.diagnostics.contains("nesting exceeds"),
        "expected the depth diagnostic, got:\n{}",
        err.diagnostics
    );
}

#[test]
fn deep_nested_match_is_a_diagnostic_not_a_crash() {
    let n = 3000;
    let mut src = String::from("declare const t: { kind: \"K\"; v: number };\nconst r = ");
    let mut open = String::new();
    for _ in 0..n {
        open.push_str("match (t) { K { v } => ");
    }
    src.push_str(&open);
    src.push('v');
    for _ in 0..n {
        src.push_str(" }");
    }
    src.push_str(";\n");

    let err =
        on_napi_sized_thread(move || compile_source("deep_match.zts", src, Options::default()))
            .expect_err("a 3000-deep match nest must be rejected");
    assert!(
        err.diagnostics.contains("nesting exceeds"),
        "expected the depth diagnostic, got:\n{}",
        err.diagnostics
    );
}
