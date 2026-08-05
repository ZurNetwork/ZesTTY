//! Snapshot tests: `.zts` in → generated TS out.
//!
//! `fixtures/*.zts` must compile; the generated TS is snapshotted.
//! `fixtures/errors/*.zts` must fail semantic checking; the diagnostics are
//! snapshotted.

mod common;

#[test]
fn compile_snapshots() {
    insta::glob!("fixtures/*.zts", |path| {
        let (out, diags) = common::compile_fixture(path)
            .unwrap_or_else(|(e, diags)| panic!("{} failed: {e}\n{diags}", path.display()));
        assert_eq!(diags, "", "unexpected diagnostics for {}", path.display());
        insta::assert_snapshot!(out.code);
    });
}

#[test]
fn error_snapshots() {
    insta::glob!("fixtures/errors/*.zts", |path| {
        let (_, diags) = common::compile_fixture(path)
            .expect_err(&format!("{} unexpectedly compiled", path.display()));
        assert!(!diags.is_empty(), "no diagnostics for {}", path.display());
        insta::assert_snapshot!(diags);
    });
}
