# ZesTTY

A Rust-flavored **superset of TypeScript** that compiles to plain TypeScript.

**The point is not Rust cosplay — it is safety, enforced at the compiler
level.** Rust-like syntax is the vehicle; the goal is that entire classes of
bugs (unhandled variants, unchecked errors) become _compile errors_, while
Rust-style dev patterns (`match`, tagged enums, `Result`) stay ergonomic and
cheap to use. Every feature must earn its place by making something unsafe
inexpressible or loudly rejected — not by merely looking Rusty.

Vanilla TS flows through untouched. New Rust-style constructs (`match`, enums-with-data, expression `if`, `Result`) are parsed, semantically checked, and **lowered to idiomatic TS** — then `tsc` verifies the output. We never reimplement TypeScript's type system; we generate code _for_ it and weaponize its checker as our backend verifier.

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
  renamed to `zestty`; `cargo test` runs snapshot + tsc exit tests
  (needs `npm install` for the local tsc).
- ✅ Phase 1 review gate: adversarial code review + security review (two
  independent reviewers, two verification rounds each). Highlights baked in:
  packrat memo + single-parse commit path (nested-match DoS), stacker-backed
  parser recursion + semantic depth limit 2048 + leak-don't-drop for
  over-deep ASTs, directive-prologue-safe helper injection, IIFE lowering
  for narrowing, globalThis.Error keystone throw with `ztsTag`.
- ✅ Phase 2: toolchain. npm workspace under `packages/` (`@zestty/native`,
  `@zestty/vite-plugin`, `@zestty/svelte-preprocess`), all with node:test suites;
  `npm test` runs them, `npm run build:native` rebuilds the binding.
- ✅ Phase 4: all four locked features (match, enums-with-data,
  expression if, `@zestty/core` Result). See the checklist below.
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
// generated TS (helper injected once per module, after directives+imports)
function __ztsAbsurd(x: never): never {
  const e: any = new globalThis.Error("zts: non-exhaustive match");
  e.ztsTag = x;
  throw e;
}

const area = ((__m) => {
  const __k = __m.kind;
  if (__k === "Circle") {
    const { radius } = __m;
    return PI * radius ** 2;
  }
  if (__k === "Square") {
    const { side } = __m;
    return side ** 2;
  }
  return __ztsAbsurd(__k);
})(shape);
```

`__ztsAbsurd(x: never): never` is the keystone: if an arm is missing, `__k`
does not narrow to `never` and **tsc rejects the generated code**, naming the
missing variant. TypeScript's own checker proves exhaustiveness — zts just
aims it. The reported error span must map back to the original `match` in the
`.zts` file.

Load-bearing details, learned the hard way (review-gated, two rounds):

- It must be an **IIFE**, not a named helper call: TS contextually types
  IIFE parameters from call arguments AND preserves outer `let` narrowing
  only through IIFEs. The discriminant is the _argument_, so `await`/
  `yield`/`this` inside it keep working.
- Testing the alias `__k` still narrows `__m` (TS 4.4 aliased discriminant
  narrowing), and passing `__k` — not `__m` — to the keystone works for both
  union and single-variant types.
- The helper throws via `globalThis.Error` (real Error: stack, instanceof)
  with the unmatched tag on `ztsTag` — never a bare `Error` reference
  (hygiene is not TS-type-aware; bare global refs rename user shadows and
  silently change type meaning), and never a `kind` field (the thrown object
  must not impersonate a domain tagged union).

Arm patterns (Phase 5): besides `Variant { bindings }`, arms take
string/number/bigint/boolean/`null` literals (`"active" =>`, `404 =>`,
`-1 =>`, `1n =>`, `true =>`, `null =>`) and a `_` wildcard. `undefined` is
NOT a pattern (it gets a dedicated diagnostic); `0` and `-0` are the same
arm (they are `===` in JS). A match is either **variant-mode or literal-mode, never
mixed**; `_` is legal in both but must be the single LAST arm and carries no
binding. Literal mode drops the `__k` alias — arms test `__m === <lit>` and
the keystone receives `__m` itself (equality narrowing runs `__m` to `never`
when exhaustive; a missing literal is a TS2345 naming it, no `--strict`
required). A `_` arm **replaces** the `return __ztsAbsurd(...)` tail: it is
the explicit, greppable opt-out of the exhaustiveness keystone.

Parser note: `match` is a **contextual keyword**. `str.match(re)` must keep
working. On `match (expr) {`, checkpoint (`ParserCheckpoint`), attempt a
match-expression parse, backtrack to a call expression on failure.

### 2. `Result<T, E>` with `map` / `map_err`

A **library feature**, not syntax. Ships as a tiny runtime package
(`@zestty/core`):

```ts
type Result<T, E> = { kind: "Ok"; value: T } | { kind: "Err"; error: E };
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
  { kind: "Circle"; radius: number } | { kind: "Square"; side: number };

const Shape = {
  Circle: (radius: number): Shape => ({ kind: "Circle", radius }),
  Square: (side: number): Shape => ({ kind: "Square", side }),
};
```

**Never emit TypeScript `enum`.** Not ever. Tagged unions only.

Note: `enum` in zts source _shadows_ TS's enum keyword — this is a deliberate
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

### 5. Newtypes (Phase 5)

Distinct identities over the same underlying type — the ID-confusion bug
class becomes a compile error.

```ts
// zts source
newtype AccountId = string;

// generated TS
type AccountId = (string) & { readonly __ztsNewtype: "AccountId" };
const AccountId = (__ztsValue: string): AccountId => __ztsValue as AccountId;
```

The parens around the underlying type are load-bearing: `&` binds tighter
than `|`, and stock codegen has no type-level fixer — unwrapped, a union
underlying type would brand only its last member. The factory parameter is
`__ztsValue` (locked `__zts` prefix): a bare `value` would be captured by
`typeof value` underlying types.

Known limitation (recorded, open decision for the Engineer): the brand is
the newtype's NAME, so two `newtype Id = string` declarations in different
scopes/modules are the same type to tsc. Options if this bites: qualify the
brand with a module discriminator, or a unique-symbol brand. Until decided,
same-named newtypes share identity.

The brand property exists only at the type level; the factory is an identity
cast, so newtypes are **zero runtime cost**. Two newtypes over the same
underlying type are mutually unassignable, and the raw type is not assignable
to either (both are TS2345, no `--strict` needed). The underlying type flows
the other way — a `Meters` is still a `number`, so arithmetic keeps working.

Rules: `newtype` is a contextual keyword committing on exactly the
`type`-alias rule (same-line identifier follows), so `newtype` stays a valid
variable name everywhere else. No type parameters in v1. `declare newtype` is
a hard error (declare the lowered shape instead). Lowering runs pre-resolver
next to enums: one decl becomes two (type + const, legal declaration
merging), hoisted past the directive prologue/imports with the same
order-independence enums get, `export` preserved on both halves.

### 6. `?` try operator (Phase 5)

Postfix `?` propagates `Err` with an early return — unchecked errors stay a
compile error, checked ones stop needing ceremony.

```ts
// zts source
function boot(raw: string): Result<number, string> {
  const port = parsePort(raw)?;
  return Ok(port + 1);
}

// generated TS (statement-level hoist)
function boot(raw: string): Result<number, string> {
  const __t = parsePort(raw);
  if (__t.kind === "Err") {
    return __t;
  }
  const port = __t.value;
  return Ok(port + 1);
}
```

tsc enforces the whole contract on the generated shape alone: `.kind` on a
non-Result is TS2339, and `return __t` fails (TS2322) unless the enclosing
return type accepts the `Err` side — error-type compatibility with zero
checker work on our side.

Parse rule (locked): `?` is a try operator ONLY where a ternary is
impossible — immediately before `;` `)` `,` `]`. One-token lookahead, fully
deterministic; `?.` stays optional chaining, `??` stays nullish coalescing,
optional parameters (`(a, b?) => …`) keep winning for bare identifiers. The
operand is the whole conditional-level expression (`a + f()?` tries
`a + f()`).

v1 statement-shape lock (review this before widening): `?` must be the
WHOLE right-hand side of a `const`/`let` declaration, a `return` argument,
or a bare expression statement, inside a real function body. Nested uses
(`g(f()?)`, `[f()?]`) are semantic errors — hoisting them would silently
reorder side effects (`g(a(), f()?)` would run `f` before `a`). Also banned:
module top level (nothing to return from) and match-arm / if-expression
blocks (they lower to IIFEs, which would hijack the early return; a nested
real function resets the rule).

### Deliberately deferred (do not implement yet)

`Option<T>`, `let`/`let mut`, traits (dictionary passing),
no-untracked-throws, move checking. (Newtypes and `?` shipped in Phase 5 —
see features 5 and 6 above.)

**Shipped 2026-08-06 (Zuri-approved): `not` as a prefix operator** —
pure sugar, `not <unary-expr>` → `!expr`, same precedence as `!` (so
`not a === b` is `(!a) === b`). Rationale: `!expr` is visually easy to
skip when reading, unlike `||`/`&&`; a loud negation keyword reduces
misread-logic bugs. Disambiguation is a deterministic one-token rule, no
speculation: negation only when the operand token can never legally
follow an identifier (a word or literal, on the same line). Everything
else keeps vanilla meaning: `not(x)` calls, `not.foo`, `not => x`,
`not instanceof F`, and ASI (`not⏎x` is two statements).

Considered and REJECTED (Zuri, 2026-08-05): paren-less `if` conditions.
Statement `if` must stay vanilla TS (superset promise), and the `) {`
boundary is load-bearing for the ASI guards; Rust only affords this by
banning struct literals in conditions. Not worth re-opening that ambiguity
class for cosmetics.

These are on the horizon but
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

- [x] AST: `MatchExpr { span, discriminant, arms }`, `MatchArm { span, pattern, body }` (pattern-based since Phase 5: `MatchPat::Variant/Lit/Wildcard`), `Expr::Match` variant (fork)
- [x] Regenerate/extend `swc_ecma_visit` for the new nodes (fork, `cargo test -p generate-code test_ecmascript`)
- [x] Parser: contextual `match`, checkpoint/backtrack, `str.match(re)` survives (fork)
- [x] Lowering pass: match → IIFE + if-chain + `__ztsAbsurd` (zts, plus resolver+hygiene for `__zts` name collisions)
- [x] Emit via stock codegen, original spans preserved (zts)
- [x] CLI: `zestty file.zts` → `file.ts` (+ `.ts.map`) (zts)
- [x] **Exit test:** `tests/tsc_exit.rs` — deleting an arm makes tsc emit TS2345 on the generated TS, and the error position maps through the sourcemap back to the original `match`

Phase 1 scope notes (locked by Zuri): arms are strictly `Variant { bindings } => expr` —
no bare variants, no guards. (Wildcard `_` and literal arms were added in Phase 5 with
Zuri's approval; the rest of the lock stands.) `await`/`yield` directly in an
arm body is a compile error until arms can lower to async IIFEs.

### Phase 2 — Toolchain ✅ (npm side under `packages/`)

- [x] napi-rs binding (`crates/zestty-napi` → `@zestty/native`; each compile on a
      64 MiB-stack thread so deep input can't SIGABRT the host; linux-x64 build
      via `npm run build:native`)
- [x] Vite plugin (`@zestty/vite-plugin`: `transform` filters `.zts`/`.ztsx`,
      native zts→TS, then vite's `transformWithEsbuild` TS→JS with `inMap` so
      the composed map reaches back to the `.zts`)
- [x] Svelte preprocessor (`@zestty/svelte-preprocess`: `<script lang="zts">`
      → compiled TS + `lang="ts"` attribute rewrite, chain before
      `vitePreprocess`)
- [x] Sourcemap proof — headless form: plugin test asserts a position in
      the final JS maps through both stages back to the originating `.zts`
      match arm. (Manual browser-devtools breakpoint check still worth one
      eyeball pass in a real app.)

### Consumer notes (learned from the first real integration)

- **Where to declare the Svelte preprocessor depends on where your Kit
  options live.** If you pass options inline to `sveltekit({...})` in
  `vite.config.ts`, SvelteKit IGNORES `svelte.config.js` entirely (it
  warns about this) — so the preprocessor (and `moduleExtensions`) must
  go inline there too. The better setup: move everything to
  `svelte.config.js`, which external tools (svelte-check, editors) read
  anyway. Pick ONE home for the options; a preprocessor declared in the
  file Kit isn't reading cost a real debugging round downstream.
- **svelte-check cannot type-check `lang="zts"` blocks itself** (v4.6): it
  checks the ORIGINAL source and keys the language off the original `lang`
  attribute. **`zts-check` closes the gap completely**: it checks `.zts`
  modules via twins AND runs svelte-check over a shadow tree where each
  zts component carries its compiled script as `lang="ts"` — so TEMPLATE
  bindings against zts script members are fully type-checked, with
  diagnostics remapped to original positions. Put `zts-check` in CI next
  to your build; `--no-svelte` skips the component pass if you need to.

### Phase 5 — Result ergonomics + identity safety (approved by Zuri, 2026-08-06)

Each repeats the Phase 1 loop (fork tests, snapshots, tsc exit test, review gate).

- [x] `_` wildcard match arm: `_ => expr`, LAST arm only, at most one, no
      binding — an explicit, greppable opt-out of the exhaustiveness
      keystone (the lowering replaces `__ztsAbsurd` with the wildcard body).
- [x] `match` on literal unions: `match (status) { "active" => ..., 404
=> ..., true => ... }` — string/number/boolean literal arms (negative
      numbers via `-1 => ...`), tsc proves exhaustiveness via the same
      never-narrowing. A match is either variant-mode or literal-mode,
      never mixed (`_` legal in both). Lowering shape (load-bearing):
      literal mode drops the `const __k = __m.kind` alias entirely — arms
      test `__m === <lit>` and the keystone receives `__m` itself, because
      (a) non-object discriminants have no `.kind` and (b) equality
      narrowing eliminates each tested literal from `__m`'s union, so an
      exhaustive literal match narrows `__m` to `never` and a missing arm
      makes tsc name the missing literal (TS2345, no `--strict` needed).
- [x] Newtypes: `newtype AccountId = string;` → branded type
      (`string & { readonly __ztsNewtype: "AccountId" }`) + factory.
      Kills the ID-confusion bug class; contextual parse mirrors the
      `type`-alias commit rule (same-line ident follows). See feature 5.
- [x] `?` try operator: postfix `?` propagates `Err` with an early return;
      tsc enforces error-type compatibility against the enclosing return
      type (no checker work on our side). Constraints (locked): `?` fires
      only where a ternary is impossible (before `;` `)` `,` `]`) — `?.`
      belongs to optional chaining, so `f()?.g` chaining is unavailable
      (bind first); banned inside match arms / if-expression blocks in v1
      (IIFE boundary would hijack the early return). Shipped with an extra
      v1 statement-shape lock — whole RHS of `const`/`let`/`return`/expr
      statement only, nested uses would silently reorder side effects. See
      feature 6.

Dispositions from the same review (recorded so they are not re-litigated):

- `Option<T>` — DEFERRED. Zuri's position: `null` and `undefined` should
  not exist as separate concepts; until that unification design exists,
  a naked Option fights `T | undefined` idiom. Revisit with `?`-operator
  interop and boundary adapters.
- Match guards (`if` in arms) — DEFERRED, design doc first: guards cannot
  count toward exhaustiveness (tsc cannot reason about predicates), so
  they need Rust's discipline (guarded arms do not discharge a variant).
- `let`/`let mut` — REJECTED: changing what vanilla `let` means breaks
  the superset promise; the itch belongs to a zts-check lint.
- no-untracked-throws — REJECTED: needs call-graph analysis tsc cannot be
  shaped into; the ZesTTY answer to exceptions is `Result` + `?`.
- Traits / move checking — horizon items, unchanged.

### Phase 3 — DX

- [ ] TextMate grammar for syntax highlighting
- [ ] LSP proxy: run tsserver over generated TS, map diagnostics back through sourcemaps (Civet's approach)
- [ ] `zts-check` (issue #3): the CI twin of the LSP proxy — compile all
      `.zts` sources + `lang="zts"` blocks into a shadow tree, run
      tsc/svelte-check there, remap diagnostics through the sourcemaps back
      to the `.zts` origins. The sourcemap discipline was built for exactly
      this.

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
- [x] `@zestty/core` with `Result`, `map`, `map_err` (feature #2): plus
      `is_ok`/`is_err` guards and `unwrap`/`unwrap_or`, all on the `kind`
      convention. Exit test proves Result + expression-if + match compose and
      the keystone still fires when the `Err` arm is deleted.
- [ ] Then and only then: revisit the deferred list

**Discipline rule: nothing from Phase 4 ships before Phase 2 is green.**
Language projects die with five features parsed and zero usable in an editor.

### Known limitation (open decision for the Engineer)

`MAX_EXPR_DEPTH = 2048` assumes ≥8 MiB of stack for the compiler's recursive
passes in debug builds. Shipping shapes are covered (napi binding: 64 MiB
thread; CLI: default main-thread stack), but a small-stack host embedding
`zestty` as a library and feeding it a _legitimate_ ~2000-deep expression could
still abort. Options if this ever matters: lower the constant, or run
`compile()` on a sized thread like the napi binding does. Pre-existing since
Phase 1 (twice-gated there); flagged again by the Phase 4 verification round.

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
