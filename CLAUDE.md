# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

zts: a Rust-flavored superset of TypeScript that compiles to plain TypeScript (`zts → TS → JS`). Custom constructs (`match`, enums-with-data, expression `if`, `Result`) are parsed by a forked swc parser, then lowered to idiomatic TS; `tsc` verifies the output. We never reimplement TypeScript's type system — we shape generated TS so tsc enforces our guarantees (e.g. exhaustiveness via `__ztsAbsurd(x: never): never`).

The point is compiler-level **safety**, not Rust cosplay: features exist to turn bug classes (unhandled variants, unchecked errors) into compile errors while keeping Rust-style patterns ergonomic.

Read README.md before doing anything — it is the authoritative spec: locked feature scope, lowering examples, roadmap, and conventions.

## Commands

- `cargo run` — builds and runs the compiler driver: parses `test.zts` (path is hardcoded in `src/main.rs`) and prints the emitted TS to stdout. This is the round-trip check; keep it green at all times.
- `cargo check` / `cargo build` — standard.
- No tests yet. When they land they'll be `insta` snapshot tests (`.zts` in → generated TS out); run with `cargo test`, review snapshots with `cargo insta review`.

## Two-repo architecture

This repo path-depends on a sibling swc fork at `../swc_rustify` (ProgrammingCheetah/swc_rustify). Fork edits go on a `zts` branch — not yet created; branch off `main` before the first fork edit. `main` stays clean tracking upstream.

- `../swc_rustify` (fork of swc-project/swc) — Stage 1: extended AST (`swc_ecma_ast`), extended parser (`swc_ecma_parser`), plus `swc_ecma_visit` and `swc_ecma_codegen`.
- This repo (`zurswc_rustify` crate) — the compiler driver: semantic pass, lowering pass, emit via *stock unmodified* codegen, CLI.

Pipeline: extended parser → semantic pass → lowering (custom nodes → vanilla TS AST) → stock codegen → plain `.ts` + sourcemap → tsc/svelte-check.

Currently at Milestone 0: `src/main.rs` is an identity compiler (parse vanilla TS → emit). Everything else is roadmap.

## Locked rules (from README — non-negotiable)

- Scope is exactly four features: `match`, `Result` (library, zero fork changes), enums-with-data, expression `if`. Do not add or implement deferred features (`Option`, `?`, `let mut`, traits, etc.) until Phase 2 (toolchain) is green.
- Discriminant field is `kind` (string literal), everywhere.
- Never emit TypeScript `enum` — tagged unions + factory functions only. TS `enum` syntax in zts source is a hard error.
- Lowering happens BEFORE codegen. Codegen arms for custom AST nodes are `unreachable!("must be lowered before emit")` — a codegen panic means a lowering bug.
- Preserve original `.zts` spans through every transformation (sourcemaps + diagnostics depend on it). Never synthesize a span when an original exists.
- Generated helpers use the `__zts` prefix with SWC syntax contexts (hygiene) to avoid collisions.
- Fork edits: only mirror existing patterns via the donor-node technique — pick a similar existing node (e.g. `CondExpr` for `MatchExpr`), grep for it across `swc_ecma_ast`/`swc_ecma_visit`/`swc_ecma_codegen`, mirror every registration site; compile errors are the checklist.
- `match` is a contextual keyword — `str.match(re)` must keep working (checkpoint/backtrack in the parser).
