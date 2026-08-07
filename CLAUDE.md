# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

ZesTTY (language: zts) — a Rust-flavored superset of TypeScript that compiles to plain TypeScript (`zts → TS → JS`). Custom constructs (`match`, enums-with-data, expression `if`, `Result`) are parsed by a forked swc parser, then lowered to idiomatic TS; `tsc` verifies the output. We never reimplement TypeScript's type system — we shape generated TS so tsc enforces our guarantees (e.g. exhaustiveness via `__ztsAbsurd(x: never): never` fed the `__k = __m.kind` alias).

The point is compiler-level **safety**, not Rust cosplay: features exist to turn bug classes (unhandled variants, unchecked errors) into compile errors while keeping Rust-style patterns ergonomic.

Read README.md before doing anything — it is the authoritative spec: locked feature scope, lowering shapes (each documents WHY its shape is load-bearing — narrowing, hygiene, and ASI constraints were review-gated), roadmap, and conventions.

## Commands

- `cargo test` — the main gate: insta snapshot tests (`.zts` in → TS out, `tests/fixtures/**`) + tsc exit tests (`tests/tsc_exit.rs`, needs `npm install` once for the local tsc). Snapshot review: `INSTA_UPDATE=always cargo test` then inspect, or `cargo insta review`.
- `cargo run --bin zestty -- file.zts [-o out.ts] [--no-map]` — the CLI; writes `file.ts` + `file.ts.map`.
- `npm test` — all packages/* node:test suites (native binding, Vite plugin, Svelte preprocessor, @zestty/core).
- `npm run build:native` — rebuild the napi binding after Rust changes (copies the .node into packages/zestty-native).
- Fork tests: `cd ../swc_rustify && cargo test -p swc_ecma_parser --features typescript --lib zts::` (all zts parser tests live in `src/parser/zts.rs`).
- Fork visit regeneration after ANY AST node change: `cd ../swc_rustify && cargo test -p generate-code test_ecmascript` (never hand-edit `generated.rs`).

## Layout

- `src/` — compiler driver: `lib.rs` (pipeline: parse → semantic → lower_enums → resolver → lower → hygiene → codegen), `semantic.rs`, `lower_enums.rs` (pre-resolver), `lower.rs` (post-resolver: match + if-expr), `main.rs` (CLI).
- `crates/zestty-napi` — Node binding; every compile runs on a 64MiB-stack thread.
- `packages/` — npm workspace: `zestty-native`, `vite-plugin-zestty`, `svelte-preprocess-zestty`, `core` (@zestty/core Result).
- `../swc_rustify` (fork of swc, branch `zts`, `main` tracks upstream) — extended AST/parser. All zts parser code in `crates/swc_ecma_parser/src/parser/zts.rs`; AST nodes in `swc_ecma_ast` (`expr.rs`, `decl.rs`).

## Locked rules (from README — non-negotiable)

- Scope: the four locked features (`match`, `Result` library, enums-with-data, expression `if`) plus Zuri-approved additions (`not` prefix sugar; newtypes + `?` in Phase 5; `union`; traits + universal absurd approved 2026-08-06 as Phase 6; Phase 7 = 0.4.0 approved 2026-08-06: readonly payloads + `mut`, `static_assert`, `T[+]`, traits v2, `zts-fmt` via dprint fork — see README). Anything else on the deferred list needs Zuri's explicit go-ahead. Traits extension requests: check "the permanent boundary" table in README feature 8 before designing.
- The type-plane rule (README, Conventions) governs all new features: guarantees live in emitted types, decisions from syntax alone, tsc is the oracle.
- Discriminant field is `kind` (string literal), everywhere. `kind` is a reserved field name in enum variants.
- Never emit TypeScript `enum` — tagged unions + factory functions only. TS `enum` member syntax in zts source is a hard error.
- Lowering happens BEFORE codegen. Codegen arms for custom AST nodes are `unreachable!("must be lowered before emit")` — a codegen panic means a lowering bug.
- Preserve original `.zts` spans through every transformation. Never synthesize a span when an original exists.
- Generated helpers use the `__zts` prefix; generated idents get fresh marks + the `hygiene()` pass. Never emit a bare global reference from generated code (`globalThis.X` only — hygiene is not TS-type-aware and bare refs rename user shadows, silently changing type meaning).
- Fork edits: donor-node technique only (grep an existing similar node, mirror every registration site; compile errors are the checklist). New Expr/Decl variants are APPENDED (encoding stability).
- `match` is a contextual keyword: checkpoint/backtrack with the failure memo (`zts_match_speculation_failures`) and error-buffer rollback (`Tokens::truncate_errors`). Never rewind a SUCCESSFUL speculation (that reintroduces exponential parsing); never widen the speculation window without re-checking `Parser::state` checkpoint gaps.
- ASI guards are load-bearing: `match(...)` + newline + `{` stays a call+block; the `{` of a match must share a line with `)`.
- Every feature lands with: fork parser tests, snapshot fixtures (happy + error), and a tsc exit test proving the safety property fires.
- **Editor + formatter parity (Zuri, 2026-08-07): every syntax change ships WITH its syntax-highlighting updates (VS Code TextMate grammar AND nvim) and its zts-fmt print-rule updates, together in ONE patch-version commit** — not as follow-ups. Practical notes: nvim currently has no viable per-construct highlight mechanism (zts constructs are bare ERROR identifier nodes to the reused TS tree-sitter parser — PR #52 disposition); until one exists, each change records its nvim disposition explicitly instead of silently skipping. zts-fmt print rules apply once the formatter lands (Phase 7 items 6-10).

## Branch Strategy

- `main` — stable; the only long-lived branch. All PRs target it. **Never push directly to `main`** (a GitHub ruleset enforces this).
- `feature/*` — a unit of new work, branched from `main` (e.g. `feature/<ticket>-short-slug`).
- `bug/*` — a bug fix, branched from `main`.
- `chore/*`, `docs/*`, `hotfix/*` — maintenance, documentation, and urgent fixes.

## Commits

- PRs merge via **Squash and merge** only (other methods are disabled), so `main` keeps **one commit per PR**; the branch itself may carry several granular commits. Merged branches auto-delete.
- Required CI checks (`format` · `lint` · `test`) must pass before merge. CI clones the swc fork (`ProgrammingCheetah/swc_rustify@zts`) as a sibling directory — the Cargo path-deps require it.
- Local `/understand` + `/remember` briefings live in `.understand/` (tracked).

## Review-gate discipline

Milestones get adversarial code review + security review (designer agents), findings fixed, then a verification round by the same reviewers. Phase 1 took two full rounds — the regression list in README's match section is what they caught. Keep that bar.
