# zts

A Rust-flavored **superset of TypeScript** that compiles to plain TypeScript.

**The point is not Rust cosplay — it is safety, enforced at the compiler
level.** Rust-like syntax is the vehicle; the goal is that entire classes of
bugs (unhandled variants, unchecked errors) become *compile errors*, while
Rust-style dev patterns (`match`, tagged enums, `Result`) stay ergonomic and
cheap to use. Every feature must earn its place by making something unsafe
inexpressible or loudly rejected — not by merely looking Rusty.

Vanilla TS flows through untouched. New Rust-style constructs (`match`, enums-with-data, expression `if`, `Result`) are parsed, semantically checked, and **lowered to idiomatic TS** — then `tsc` verifies the output. We never reimplement TypeScript's type system; we generate code *for* it and weaponize its checker as our backend verifier.

```
zts  →  TS  →  JS
```

Each step erases a layer: zts sugar erases into TS syntax + types; TS types erase into JS. What survives to runtime is only what was always real data (tagged objects).

---

## Architecture

Two sibling repos:

```
~/code/
  swc_rustify/  ← fork of swc-project/swc (ProgrammingCheetah/swc_rustify).
                 Stage 1 lives here: extended AST (swc_ecma_ast) + extended
                 parser (swc_ecma_parser). zts changes go on a `zts` branch;
                 `main` stays clean tracking upstream.
  zts/          ← this repo. The compiler driver: semantic pass, lowering
                 pass, emit, CLI. Path-depends on ../swc_rustify/crates/*.
```

### Pipeline

```
.zts file
  → extended parser        (fork: knows match/enum/expr-if syntax)
  → semantic pass          (zts: exhaustiveness setup, later: move checking)
  → lowering pass          (zts: custom nodes → vanilla TS AST)
  → emit                   (stock swc_ecma_codegen, unmodified — lowering
                            happens BEFORE codegen, so codegen never sees
                            custom nodes)
  → plain .ts + sourcemap
  → tsc/svelte-check       (their typechecker does the heavy lifting)
```

**Spans from the original `.zts` source are preserved through lowering.** They are the sourcemap story and the diagnostics story. Never synthesize spans when an original one exists.

---

## Current status

- ✅ Milestone 0: identity compiler (round-trip confirmed).
- ✅ Fork cloned (`../swc_rustify`), `zts` branch carrying the extensions,
  `main` clean tracking upstream.
- ✅ Phase 1: `match` vertical slice complete (see checklist below). Crate
  renamed to `ztsc`; `cargo test` runs snapshot + tsc exit tests
  (needs `npm install` for the local tsc).
- ✅ Phase 1 review gate: adversarial code review + security review (two
  independent reviewers, two verification rounds each). Highlights baked in:
  packrat memo + single-parse commit path (nested-match DoS), stacker-backed
  parser recursion + semantic depth limit 2048 + leak-don't-drop for
  over-deep ASTs, directive-prologue-safe helper injection, IIFE lowering
  for narrowing, globalThis.Error keystone throw with `ztsTag`.
- ✅ Phase 2: toolchain. npm workspace under `packages/` (`@zts/native`,
  `@zts/vite-plugin`, `@zts/svelte-preprocess`), all with node:test suites;
  `npm test` runs them, `npm run build:native` rebuilds the binding.
- ✅ Phase 4: all four locked features (match, enums-with-data,
  expression if, `@zts/core` Result). See the checklist below.
- ⬜ Phase 3 (DX: TextMate grammar, LSP proxy) — the remaining roadmap item.

---

## Language features (locked scope — do not add others)

### 1. `match` expressions

Rust-style, **compiler-enforced exhaustiveness**.

```ts
// zts source
const area = match (shape) {
  Circle { radius } => PI * radius ** 2,
  Square { side }   => side ** 2,
};
```

Lowers to:

```ts
// generated TS (helpers injected once per module)
function __ztsAbsurd(x: never): never { throw { name: "ZtsNonExhaustiveMatch", ... }; }
function __ztsMatch<T, R>(v: T, f: (v: T) => R): R { return f(v); }

const area = __ztsMatch(shape, (__m) => {
  const __k = __m.kind;
  if (__k === "Circle") { const { radius } = __m; return PI * radius ** 2; }
  if (__k === "Square") { const { side } = __m; return side ** 2; }
  return __ztsAbsurd(__k);
});
```

`__ztsAbsurd(x: never): never` is the keystone: if an arm is missing, `__k`
does not narrow to `never` and **tsc rejects the generated code**. TypeScript's
own checker proves exhaustiveness — zts just aims it. The reported error span
must map back to the original `match` in the `.zts` file.

Three load-bearing details, learned the hard way (review-gated):
- The generic `__ztsMatch` helper (not a bare IIFE) is what types `__m`:
  an IIFE parameter is implicitly `any`, and evaluating the discriminant as
  an argument keeps `await`/`yield`/`this` in it working.
- Testing the alias `__k` still narrows `__m` (TS 4.4 aliased discriminant
  narrowing), and passing `__k` to the keystone works for both union and
  single-variant types — `__m` itself would be `never` (TS2339) in one case
  and never-narrowing in the other.
- `__ztsAbsurd` throws a plain object, never `new Error(...)`: a global
  `Error` reference would make hygiene rename user classes shadowing it,
  silently changing what type annotations mean.

Parser note: `match` is a **contextual keyword**. `str.match(re)` must keep
working. On `match (expr) {`, checkpoint (`ParserCheckpoint`), attempt a
match-expression parse, backtrack to a call expression on failure.

### 2. `Result<T, E>` with `map` / `map_err`

A **library feature**, not syntax. Ships as a tiny runtime package
(`@zts/core`):

```ts
type Result<T, E> =
  | { kind: "Ok";  value: T }
  | { kind: "Err"; error: E };
```

Plus `Ok()`, `Err()` constructors and `map` / `map_err` combinators. Must use
the same `kind` discriminant convention as everything else so it composes with
`match`. Zero fork changes.

### 3. Enums-with-data

Rust-style enums, lowered to **tagged objects + factory functions**:

```ts
// zts source
enum Shape {
  Circle { radius: number },
  Square { side: number },
}
```

Lowers to a discriminated union type + constructors:

```ts
// generated TS
type Shape =
  | { kind: "Circle"; radius: number }
  | { kind: "Square"; side: number };

const Shape = {
  Circle: (radius: number): Shape => ({ kind: "Circle", radius }),
  Square: (side: number): Shape => ({ kind: "Square", side }),
};
```

**Never emit TypeScript `enum`.** Not ever. Tagged unions only.

Note: `enum` in zts source *shadows* TS's enum keyword — this is a deliberate
semantic replacement, the one place zts is not a strict superset. TS `enum`
syntax should be a hard error with a friendly diagnostic.

### 4. Expression `if`

Blocks are expressions; a block's value is its tail expression.

```ts
// zts source
const a = if (b === 0) { 3 } else { 4 };
```

Lowering: simple branches → ternary; multi-statement branches → IIFE (same
machinery as `match`). Applies to `if` and `match` arm bodies. `if` used as an
expression without `else` is a compile error.

### Deliberately deferred (do not implement yet)

`Option<T>`, the `?` operator, `let`/`let mut`, traits (dictionary passing),
newtypes, no-untracked-throws, move checking. These are on the horizon but
**nothing gets built until Phase 2 below is green.**

---

## Conventions (locked decisions)

- Discriminant field is **`kind`** (string literal). Everywhere. Non-negotiable.
- Generated helper identifiers use the `__zts` prefix (`__m`, `__ztsAbsurd`).
  Use SWC syntax contexts (hygiene) so generated names cannot collide with
  user code.
- New AST nodes are **never** handled in codegen. Codegen arms for custom
  nodes are `unreachable!("must be lowered before emit")`. If codegen panics,
  the lowering pass has a bug.
- Fork discipline: all fork changes on the `zts` branch. When adding a node,
  use the **donor-node technique**: pick a structurally similar existing node
  (`CondExpr` for `MatchExpr`), `grep -rn '\bCondExpr\b'` across
  `swc_ecma_ast`, `swc_ecma_visit`, `swc_ecma_codegen`, and mirror every
  registration site. Compile errors are the checklist.
- Snapshot tests (`insta` crate) from day one: `.zts` in → generated TS out.
  Every feature lands with snapshots covering the happy path and the
  should-fail-under-tsc path.

---

## Roadmap

### Phase 1 — `match` vertical slice ✅
- [x] AST: `MatchExpr { span, discriminant, arms }`, `MatchArm { span, variant, binding, body }`, `Expr::Match` variant (fork)
- [x] Regenerate/extend `swc_ecma_visit` for the new nodes (fork, `cargo test -p generate-code test_ecmascript`)
- [x] Parser: contextual `match`, checkpoint/backtrack, `str.match(re)` survives (fork)
- [x] Lowering pass: match → IIFE + if-chain + `__ztsAbsurd` (zts, plus resolver+hygiene for `__zts` name collisions)
- [x] Emit via stock codegen, original spans preserved (zts)
- [x] CLI: `ztsc file.zts` → `file.ts` (+ `.ts.map`) (zts)
- [x] **Exit test:** `tests/tsc_exit.rs` — deleting an arm makes tsc emit TS2345 on the generated TS, and the error position maps through the sourcemap back to the original `match`

Phase 1 scope notes (locked by Zuri): arms are strictly `Variant { bindings } => expr` —
no wildcards, no bare variants, no guards, no literals. `await`/`yield` directly in an
arm body is a compile error until arms can lower to async IIFEs.

### Phase 2 — Toolchain ✅ (npm side under `packages/`)
- [x] napi-rs binding (`crates/ztsc-napi` → `@zts/native`; each compile on a
  64 MiB-stack thread so deep input can't SIGABRT the host; linux-x64 build
  via `npm run build:native`)
- [x] Vite plugin (`@zts/vite-plugin`: `transform` filters `.zts`/`.ztsx`,
  native zts→TS, then vite's `transformWithEsbuild` TS→JS with `inMap` so
  the composed map reaches back to the `.zts`)
- [x] Svelte preprocessor (`@zts/svelte-preprocess`: `<script lang="zts">`
  → compiled TS + `lang="ts"` attribute rewrite, chain before
  `vitePreprocess`)
- [x] Sourcemap proof — headless form: plugin test asserts a position in
  the final JS maps through both stages back to the originating `.zts`
  match arm. (Manual browser-devtools breakpoint check still worth one
  eyeball pass in a real app.)

### Phase 3 — DX
- [ ] TextMate grammar for syntax highlighting
- [ ] LSP proxy: run tsserver over generated TS, map diagnostics back through sourcemaps (Civet's approach)

### Phase 4 — Next features (each repeats the Phase 1 loop)
- [x] Enums-with-data (feature #3): zts `enum` grammar in the fork
  (`parse_any_enum_decl` dispatch; TS member syntax / `const enum` /
  `declare enum` get friendly errors), lowered pre-resolver to a tagged
  union type alias + typed factory const. `kind` is a reserved field name.
- [x] Expression `if` (feature #4): mandatory `else`, else-if chains,
  blocks-as-expressions (`{ stmts; tail }`). Statement-free chains lower
  to ternaries (await stays legal); chains with statements lower to an
  IIFE (await/yield rejected with a diagnostic). Match arm bodies accept
  the block form too (`=> {` is a block, like arrow bodies — object
  literals need parens).
- [x] `@zts/core` with `Result`, `map`, `map_err` (feature #2): plus
  `is_ok`/`is_err` guards and `unwrap`/`unwrap_or`, all on the `kind`
  convention. Exit test proves Result + expression-if + match compose and
  the keystone still fires when the `Err` arm is deleted.
- [ ] Then and only then: revisit the deferred list

**Discipline rule: nothing from Phase 4 ships before Phase 2 is green.**
Language projects die with five features parsed and zero usable in an editor.

---

## For Claude Code

- You are extending a **working identity compiler**, not starting from
  scratch. Run it first; keep the round-trip green at all times.
- The fork at `../swc_rustify` is part of this project. Edits there are
  expected and correct — but only on the `zts` branch (create it off `main`
  if it doesn't exist yet), only mirroring existing patterns.
- Do not add language features beyond the four above. Do not "improve" the
  scope. Ergonomic polish within a feature is welcome; new features are not.
- Do not reimplement any part of TypeScript's type checker. If a guarantee
  can be obtained by shaping the generated TS so tsc enforces it, that is
  always the right design.
- Preserve original spans through every transformation. Sourcemaps and
  diagnostics both depend on it.
- Prior art for technique questions: Civet (superset → TS, LSP-over-sourcemaps),
  Borgo (Rust-flavored syntax → simpler host language).
