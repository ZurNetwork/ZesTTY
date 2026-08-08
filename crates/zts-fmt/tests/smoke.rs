//! Smoke + idempotence over a small canonical corpus (item 10).
//!
//! The heavyweight suites live in the formatter fork (669 specs, 1144
//! idempotence fixed points); this pins the INTEGRATION — the crate's
//! default config formats real zts and is stable. Deliberately NOT run
//! over tests/fixtures/: several fixtures encode deliberate layout
//! (the ASI regression fixture REQUIRES `match(1)` + newline + block),
//! and reformatting them would destroy what they test.

use std::path::Path;

use zts_fmt::format_zts;

const CORPUS: &[(&str, &str)] = &[
    (
        "canon.zts",
        "enum Shape {\n  Circle { r: number },\n  Rect { mut w: number, h: number },\n}\n\nimpl Display for Shape {\n  fmt(self): string {\n    return match (self) {\n      Circle { r } => `circle r=${r}`,\n      Rect { w, h } => `rect ${w}x${h}`,\n    };\n  }\n}\n\nunion Level = \"info\" | \"warn\";\nnewtype UserId = string;\nconstrict UserId != string;\n\nconst x: number[+] = [1];\nconst ready = not (x.length > 1);\nconst label = if (ready) { \"go\" } else { \"wait\" };\n",
    ),
    (
        "messy.zts",
        "enum   E{A{x:number},B{}}\nconst   v =   match(  ({kind:\"A\",x:1}) as E ){A{x}=>x,B{}=>0,};\n",
    ),
];

#[test]
fn corpus_formats_and_is_idempotent() {
    for (name, src) in CORPUS {
        let path = Path::new(name);
        let once = format_zts(path, src.to_string())
            .unwrap_or_else(|e| panic!("{name} failed to format: {e}"))
            .unwrap_or_else(|| src.to_string());
        let twice = format_zts(path, once.clone())
            .unwrap_or_else(|e| panic!("{name} second pass failed: {e}"));
        assert!(
            twice.is_none() || twice.as_deref() == Some(once.as_str()),
            "{name} is not idempotent:\nfirst:\n{once}\nsecond:\n{:?}",
            twice
        );
        // The zts constructs survived (round-trip, not desugar).
        if *name == "canon.zts" {
            for token in [
                "match (",
                "impl Display for Shape",
                "union Level",
                "newtype UserId",
                "constrict UserId",
                "[+]",
                "not ",
                "mut w",
            ] {
                assert!(once.contains(token), "{name}: lost `{token}`:\n{once}");
            }
        }
    }
}
