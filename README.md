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
  zts-fmt-forks/ ← the zts-fmt engine (Phase 7): ZurNetwork forks of
                 dprint-plugin-typescript + deno_ast + dprint-swc-ext,
                 zts branches, repointed at ../swc_rustify. Print rules
                 for every zts node; 669 specs + 1144 idempotence fixed
                 points live THERE. crates/zts-fmt path-depends on the
                 plugin fork.
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
// generated TS (universal absurd, issue #47: the ONE shared helper is
// imported from @zestty/core — injected once per module, after
// directives+imports. `--inline-preamble` (CLI/zts-check) and
// `inlinePreamble` (vite/svelte options) restore the standalone
// per-module declaration for consumers without the core dependency;
// virtual twins that never ship — plain zts-check, the language-server —
// stay inline so dep-less workspaces keep working.)
import { __ztsAbsurd } from "@zestty/core";

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

Plus `Ok()`, `Err()` constructors and combinators: `map`, `map_err`,
`and_then`, `is_ok`/`is_err` guards, `unwrap`/`unwrap_or`. Must use the same
`kind` discriminant convention as everything else so it composes with
`match` and `?`. Zero fork changes.

Results are plain tagged objects — combinators are FREE FUNCTIONS, never
methods, so a Result survives JSON/structuredClone/network boundaries. For
Rust-style left-to-right chaining, `ResultPipe` (approved by Zuri,
2026-08-06) wraps them ephemerally:

```ts
const out = ResultPipe(parsePort(raw))
  .map((p) => p + 1)
  .map_err((e) => `boot failed: ${e}`)
  .done(); // plain Result back out — the pipe itself is never stored/sent
```

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
// generated TS (fields readonly by default since 0.4.0 — Phase 7's
// breaking change; `mut field: T` opts out per field, `kind` never can)
type Shape =
  | { readonly kind: "Circle"; readonly radius: number }
  | { readonly kind: "Square"; readonly side: number };

const Shape = {
  Circle: (radius: number): Shape => ({ kind: "Circle", radius }),
  Square: (side: number): Shape => ({ kind: "Square", side }),
};
```

**Never emit TypeScript `enum`.** Not ever. Tagged unions only.

**Readonly payloads (Phase 7, 0.4.0 — BREAKING).** Writing a payload
field is a TS2540 unless the field is declared `mut` (`Cell { mut count:
number }`); `kind` is always readonly with no opt-out (a kind write
would let a value lie about its own variant). Migration is mechanical —
one TS2540 per mutation site, fix = add `mut`. `mut` is contextual: a
field literally named `mut` keeps working (`mut: number`, and
`mut mut: number` is a mutable field named mut). Honest limits
(recorded): TS `readonly` is shallow (an array-typed field's contents
stay mutable) and not part of structural assignability — the guarantee
fires on direct writes through the typed view, where the aliased-
mutation bug class actually lives.

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
scopes/modules are the same type to tsc — and the brand is structurally
forgeable without a cast (`Object.assign("raw", { __ztsNewtype:
"AccountId" as const })` type-checks). Options if this bites: qualify the
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

tsc enforces the contract on the generated shape: `.kind` on a non-Result
is TS2339, and `return __t` fails (TS2322) unless the enclosing return type
accepts the `Err` side. For that check to be REAL, the enclosing function
must carry an explicit return type annotation — zts requires it (in a
void-contextual callback like `xs.forEach(x => ...)` TypeScript accepts any
returned value, so an inferred return type would let the Err vanish
silently). `?` is also banned in generators (the early return would become
TReturn) and setters (cannot return a value); both get dedicated
diagnostics. Boundary: an annotation of `any` (or a return type absorbing
the Err some other way) satisfies the rule syntactically but voids the
check — `any` is outside every zts guarantee, not just this one.

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
real function with an annotated return type resets the rule). In
single-statement slots (`if (c) g()?;`,
loop bodies, labels) the expansion is wrapped in a block, so the early
return keeps its meaning.

### 7. `union` — closed string vocabularies (Zuri-approved, 2026-08-06)

A literal-union type WITH a runtime side: the values list and the
membership guard that plain `type` aliases can't give you.

```ts
// zts source
union DeleteOutcome = 'soft' | 'hard' | 'unknown';

// generated TS
type DeleteOutcome = 'soft' | 'hard' | 'unknown';
const DeleteOutcome = {
  values: ['soft', 'hard', 'unknown'] as const,
  has: (__ztsRaw: string): __ztsRaw is DeleteOutcome =>
    DeleteOutcome.values.indexOf(__ztsRaw as DeleteOutcome) !== -1,
};
```

The wire-normalizer stops hand-listing literals — `DeleteOutcome.has(raw)`
narrows, so `has(raw) ? raw : 'unknown'` is the whole R8 fail-closed
mapping — while the closed side keeps exhaustive `match` (a missing member
is a TS2345 naming it). Members are string literals only in v1 (numbers
would widen the guard's parameter type); duplicates are a compile error;
leading `|` allowed; same contextual-keyword commit rule, hoisting, and
export behavior as `newtype`. The guard uses `indexOf` (ES5-clean —
`includes` would raise the emitted-TS lib floor to ES2016), and its cast
rides on the argument — a receiver cast would need parens the fixer strips
(load-bearing).

### 8. Traits (Phase 6, Zuri-approved 2026-08-06)

TS-flavored trait impls: ONE new construct, everything else is plain
TypeScript on both ends.

```ts
// zts source — the trait is a vanilla TS interface (zero zts grammar)
interface Display<Self> {
  fmt(self: Self): string;
}
enum Shape {
  Circle { r: number },
}
impl Display for Shape {
  fmt(self): string {
    return match (self) { Circle { r } => `circle r=${r}` };
  }
}

// generated TS — methods merge into the factory const
const Shape = {
  Circle: (r: number): Shape => ({ kind: "Circle", r }),
  fmt(self: Shape): string { /* lowered match */ },
} satisfies { [key: string]: unknown } & Display<Shape>;
```

Calls are plain TS: `Shape.fmt(s)` (static-method style), and generics
take the dictionary as an ordinary parameter — `describe(s, Shape)` works
because the factory structurally satisfies `Display<Shape>`; there is
**no call-site lowering at all**. Vanilla `.ts` consumers inherit the
whole system as objects + interfaces.

**Traits v2 (Phase 7 — SHIPPED):**

```ts
impl From<string>, From<number> for Id {
  from(value: string | number): Id {           // no self: associated fn
    return typeof value === "string" ? Id.Str(value) : Id.Num(value);
  }
}
// → Id.from("x") / Id.from(4), and
//   satisfies { ... } & From<Id, string> & From<Id, number>
```

- **Associated functions**: `self` is optional — without it the method
  merges as `Id.from(...)` with fully user-annotated params. Bare first
  `self` still marks a receiver method (annotated in the lowering);
  ANNOTATED self and non-first self stay errors.
- **Trait type-args**: `impl From<string>` → Self is the FIRST type
  argument, header args appended after: `satisfies From<Id, string>`.
- **Comma-header multi-instantiation**: each listed trait is a separate
  satisfies obligation over ONE union-typed body (TS overload semantics
  via intersection — the signature-level guarantee; the body's own
  dispatch is the author's, deliberately NOT `From<string | number>`
  which is a weaker single claim).
- **Early semantic checks (original spans, before tsc)**: unknown trait
  name at the header (declared/imported in-module, module level);
  method-vs-variant collisions; CROSS-impl duplicate methods ("`x` is
  defined by both `Human` and `Machine`") — superseding the v1 "left to
  tsc" coherence disposition; same-file no-`extends` interfaces get a
  syntactic member-NAME comparison (missing/extra methods) — imported
  traits still defer to `satisfies`, names from syntax only, never
  types.

Load-bearing details:

- **`fn` DROPPED (Zuri, 2026-08-07 — lands in 0.4.0, breaking):** impl
  members are bare TS-style methods, `fmt(self): string { ... }` —
  exactly class/object-method syntax. An impl block only contains
  methods, so `fn` was never structurally necessary; v1 (0.3.1) shipped
  with it, 0.4.0 removes it (a member starting with the word `fn` gets
  a dedicated migration diagnostic). Return types are TS-style `:` (no
  `->` token exists). The bare `self` receiver is required first and
  gets its type annotation in the LOWERING (annotating it in source is
  an error).
- The `{ [key: string]: unknown }` intersection member absorbs the
  variant factories from `satisfies`' excess-property check. Written
  inline, never `Record` — a user shadow of `Record` would silently
  change what conformance means (same reasoning as the globalThis rule).
- Orphan rule (semantic): `impl X for T` requires zts enum `T` in the
  SAME statement list; v1 is enum-impls only. `export impl` is an error
  (the factory const owns the export). Single-statement slots
  (`if (c) impl ...`) are an error.
- Safety, all tsc-enforced (exit-tested): non-conforming method →
  TS2322 on the method span; colliding methods across impls → TS2300
  duplicate identifier; non-exhaustive match inside a method → the
  existing keystone.
- Within-impl duplicate methods and `__zts`-prefixed method names are
  semantic errors (better spans than the tsc equivalents).

#### Traits: the permanent boundary (recorded 2026-08-06 — do not re-litigate)

Traits are the one adopted feature whose Rust home is inside the type
checker, so every extension request meets the same wall: **if resolving a
call requires reading a type, it is out — permanently.** The type plane
is write-only (Conventions). This table is the lookup answer to every
future "can traits do X":

| Inside (syntactic — shipped or Phase 7)                                                            | Outside (type-directed — never)                                                                                                                                                                      |
| -------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| impl blocks, factory merge, auto-`satisfies`                                                       | `x.fmt()` method-call syntax                                                                                                                                                                         |
| receiver methods; associated functions (no `self`)                                                 | merging separate impl bodies under one name (needs call-site types or runtime type tests that erased types don't have — and we cannot even _detect_ the safe primitive subset without reading types) |
| trait type-args (`impl From<string> for Status`)                                                   | blanket impls (`impl<T: Display> Show for T`)                                                                                                                                                        |
| comma-header multi-instantiation (one union-typed body, `satisfies A & B` — TS overload semantics) | specialization                                                                                                                                                                                       |
| dictionary passing (`describe(s, Shape)`)                                                          | implicit dictionary selection                                                                                                                                                                        |
| orphan rule, coherence, early semantic checks                                                      | —                                                                                                                                                                                                    |

Where Rust spends a type checker, zts spends a method name (e.g.
`from_string`/`from_number` instead of two merged `From` impls). That is
the trade the whole language is built on.

### 9. `constrict` — erased type assertions (Phase 7, shipped)

```rust
newtype UserId = string;
newtype OrderId = string;

constrict UserId != OrderId;                  // brands really are distinct
constrict keyof Config == "host" | "port";   // shape pinned against drift
constrict ApiResult extends Result<User, ApiError>;
```

Lowers to erased type aliases whose generic constraint fails when the
claim is false — a TS2344 at the assert's own line:

```ts
import type { __ztsExpect, __ztsEqual, __ztsNot } from "@zestty/core";
type __ztsConstrict0 = __ztsExpect<__ztsNot<__ztsEqual<UserId, OrderId>>>;
```

Operators: `==` (EXACT equality via the conditional-fn identity trick —
distinguishes brands, `any` vs `unknown`, optionality; mutual `extends`
would not), `!=`, `extends` (one-way assignability). Load-bearing
details: alias names self-uniquify with a counter (hygiene is not
TS-type-aware and will not rename duplicate TYPE aliases); the LHS
parses as a NON-conditional type or `A extends B` would be swallowed as
a conditional-type head; inline mode (`--inline-preamble` / scripts)
emits the three helper aliases locally with the probe fn-types
explicitly parenthesized (no type-level fixer exists — the newtype
parens lesson). Renamed from `static_assert` with parens dropped
(recorded in Phase 7 item 2: the paren form is legal TS and would steal
meaning). Contextual: `const constrict = 1` and `constrict(x)` stay
vanilla; commit = same-line word.

### Deliberately deferred (do not implement yet)

`Option<T>`, `let`/`let mut`, no-untracked-throws, move checking.
(Newtypes and `?` shipped in Phase 5 — see features 5 and 6 above;
traits shipped in Phase 6 — feature 8.)

**Shipped 2026-08-06 (Zuri-approved): `not` as a prefix operator** —
pure sugar, `not <unary-expr>` → `!expr`, same precedence as `!` (so
`not a === b` is `(!a) === b`). Rationale: `!expr` is visually easy to
skip when reading, unlike `||`/`&&`; a loud negation keyword reduces
misread-logic bugs. Disambiguation is a deterministic one-token rule, no
speculation: negation only when the operand token can never legally
follow an identifier (a word or literal, on the same line). Everything
else keeps vanilla meaning: `not(x)` calls, `not.foo`, `not => x`,
`not instanceof F`, and ASI (`not⏎x` is two statements).

**RE-DECIDED (Zuri, 2026-08-07 — lands in 0.4.0, breaking): `not` is a
RESERVED WORD.** The contextual rule above is retired: `not` can no
longer be a user identifier (`const not = 1`, `not(x)` as a call,
`not.foo` become errors). Two reasons: (a) the formatter cannot
round-trip `not` today — the parser desugars it to `!` at parse time
with no AST marker, so zts-fmt would silently rewrite `not ready` to
`!ready`; reserving the word lets the parser keep a real `ZtsNot` node
(appended Expr variant) that the compiler lowers to `!` in lower.rs
like every other zts construct and the formatter prints verbatim;
(b) it removes the ambiguity carve-outs entirely.

Considered and REJECTED (Zuri, 2026-08-05): paren-less `if` conditions.
Statement `if` must stay vanilla TS (superset promise), and the `) {`
boundary is load-bearing for the ASI guards; Rust only affords this by
banning struct literals in conditions. Not worth re-opening that ambiguity
class for cosmetics.

These are on the horizon but
**nothing gets built until Phase 2 below is green.**

---

## Conventions (locked decisions)

### The type-plane rule (Zuri-approved, 2026-08-06)

**Type-plane first.** Every guarantee ZesTTY adds must live in the _types_
of the generated TS wherever possible — enforced by tsc (the gate we
already trust) and erased by emit (the discipline we already follow).
Runtime code in a lowering is justified only when a value must exist at
runtime anyway (enum factories, `Result` objects). If a guarantee can
neither be expressed as emitted types nor piggyback on values that
already exist, it needs a checker we'd have to write ourselves — and
that is an automatic reject.

The type plane is **write-only** for us. The compiler is purely
syntactic (parse → lower → emit; no inference, no checker), so a feature
must be decidable from **syntax alone** at lowering time:

1. Lowers to **pure types**? Best case — zero runtime cost, zero hygiene
   risk, fully erased.
2. Needs runtime values that **would exist anyway**? Acceptable — this
   is `match`/enums/`Result` today: the shape is runtime, the guarantee
   is still the type plane.
3. Needs branching on a type, or analysis tsc cannot be shaped into?
   Reject — the same reasoning that killed no-untracked-throws.

We can still _pose questions_ to the type plane: emit types that encode
a proof obligation and let tsc be the oracle — its failure surfaces as
our diagnostic (the `__ztsAbsurd` keystone is exactly this), and TS's
own type-level operators (`keyof`, mapped, conditional types) are
computation we may emit without ever reading the answer. Corollaries in
shipped code: `match` picks literal-mode vs variant-mode from the arm
_shapes_, never from the matched expression's type; a bindingless
variant arm emits no destructure at all (issue #38) — lowering decides
shape from syntax, tsc owns meaning.

### Other locked conventions

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
- **Go-to-definition from consumers lands in the `.zts`, not the twin**
  (issue #45). In committed-twins mode tsserver answers definition
  queries with the generated `.ts`; `typescript-zestty-plugin` (a TS
  Language Service plugin, same pattern as `typescript-svelte-plugin`)
  intercepts them and remaps the span to the sibling `.zts` through the
  `.ts.map` that `zts-check --twins` now emits next to each twin
  (whole-word symbol search when a twin has no map). Enable it per repo
  via `tsconfig.json` → `compilerOptions.plugins: [{ "name":
"typescript-zestty-plugin" }]` (plus `npm i -D typescript-zestty-plugin`);
  the VS Code extension bundles it automatically. The committed `.ts.map`
  is machine-independent (sibling-relative `sources`, no
  `sourcesContent`) and inert to staleness checks and orphan scans.

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
- Traits — promoted to Phase 6 (Zuri-approved, 2026-08-06); move
  checking stays a horizon item.

### Phase 6 — Traits + universal absurd (Zuri-approved, 2026-08-06; NEXT)

The next implementation. Repeats the Phase 1 loop (fork parser tests,
snapshots happy + error, tsc exit test per safety property, review gate).

- [x] **Traits — TS-flavored, one new construct only** — SHIPPED, see
      feature 8. Design decided
      in conversation with Zuri (2026-08-06); passes the type-plane rule
      at levels 1–2. Deliberately NOT Rust cosplay: everything except
      the `impl` block is plain TypeScript on both ends.
  - **Trait declaration = a vanilla TS `interface`** with a `Self` type
    parameter (`interface Display<Self> { fmt(self: Self): string }`).
    Zero new grammar; erased.
  - **`impl Display for Shape { fn fmt(self): string { ... } }`** —
    (return types are TS-style `:` — there is no `->` token in the
    lexer, and the TS-flavored direction prefers it anyway) —
    the one zts construct (`for` header locked: it reads as a sentence).
    Lowers to methods **merged into the factory const the enum already
    emits**, with `satisfies ... & Display<Shape>` appended — the
    dictionary is a value that must exist anyway (level 2), and
    conformance is tsc's verdict (level 1).
  - **Calls are plain TS**: direct `Shape.fmt(s)` (static-method style);
    generic bounds are ordinary dictionary parameters —
    `function describe<T>(x: T, impl: Display<T>)` called as
    `describe(s, Shape)`, because the factory namespace structurally
    satisfies `Display<Shape>`. **No call-site lowering exists at all**
    (a Rust-style turbofish would have required cross-module knowledge
    of callee bounds — non-syntactic, rejected). Vanilla `.ts` consumers
    get the whole system for free; it is just objects and interfaces.
  - **Safety properties (each gets a tsc exit test):** missing/wrong
    method → `satisfies Display<Shape>` fails on the impl; deleted impl
    → every call site fails; non-exhaustive `match` inside an impl → the
    existing `__ztsAbsurd` keystone fires; two impls colliding on a
    method name → duplicate identifier (TS2300) — coherence enforced by
    the emit shape, never checked by us.
  - **Orphan rule (locked):** `impl ... for T` only in the module that
    declares `T` — locally checkable at parse time, and what makes the
    factory-merge lowering possible.
  - **v1 scope forks (open):** enum-only impls vs also newtypes/plain
    types; default trait methods (lean: defer to v2); multi-trait impls
    on one type (falls out of the merge — keep).
- [x] **Universal absurd** (issue #47) — SHIPPED. The default emit now
      imports the ONE shared `__ztsAbsurd` from `@zestty/core` (as
      `--twins` mode already did, issue #37) instead of declaring the
      helper per module. Rationale (Zuri): the per-module copy was
      re-generated on top of functions — a waste of memory. Opt-outs:
      CLI `--inline-preamble`, `inlinePreamble` option on the vite
      plugin and svelte preprocessor (both now carry an optional peer on
      `@zestty/core >= 0.4.0`). Virtual twins that never ship stay
      inline deliberately: plain `zts-check` temp twins and the
      language-server, so dep-less workspaces keep working and editor
      diagnostics can't invent a missing-module error. Scripts
      (non-modules) always inline — they cannot import. Pinned by an
      inline-mode snapshot + a self-contained tsc exit test.

### Phase 7 — 0.4.0, "the type-plane minor" (Zuri-approved, 2026-08-06)

Each language item repeats the Phase 1 loop (fork parser tests,
snapshots happy + error, tsc exit test per safety property, review
gate). Pre-1.0 semver: the 0.x minor slot carries breaking changes —
item 1 is THE headline break of this release, loudly documented.

Language:

- [x] 1. **Readonly enum payloads + `mut` opt-out** — SHIPPED (issue #54), BREAKING. Variant
      fields emit `readonly` in the generated tagged union;
      `mut field: T` opts out per field; `kind` is always readonly with
      no opt-out.
      Migration is mechanical: every break is a TS2540 at the exact
      mutation site, fix = add `mut`. Lives on zts-owned constructs
      only, so the superset promise is untouched (unlike the rejected
      `let`/`let mut`). Honest limits (recorded): TS `readonly` is
      shallow, and readonly is not part of structural assignability —
      the guarantee fires on direct writes through the typed view.
- [x] 2. **`constrict A == B;`** — SHIPPED, see feature 9 (renamed from `static_assert`,
      parens DROPPED — both re-decided with Zuri 2026-08-07: the paren
      form `static_assert(a == b)` is already legal TS — a call with a
      comparison — so it would steal meaning from valid programs, and
      no speculation rule can distinguish the two since both parse.
      Paren-free commits on the same-line-ident rule like `union`, and
      two identifiers in a row is never valid TS). Erased type-level
      assertion; operators `==` (mutual, Equal-trick), `!=`, `extends`.
      Lowers to a type alias whose constraint fails when the claim is
      false (TS2344 remapped to the assert line). `Equal`/`Expect`
      helper types ship as type-only exports from @zestty/core.
- [x] 3. **Non-empty array sugar `T[+]`** — SHIPPED. Lowers to
      `[T, ...T[]]` (post-resolver type rewrite; `ZtsNonEmptyArray`
      appended to TsType — a real node, not a parse-time desugar, so
      zts-fmt round-trips it). Callers must prove non-emptiness;
      `xs[0]` is `T` even under noUncheckedIndexedAccess (exit-tested
      both directions: [] and plain T[] are TS2345). `isNonEmpty` guard
      shipped in @zestty/core (a `.length` check does not narrow —
      recorded). Read-shape contract (`.pop()` does not un-narrow),
      like all TS tuples. Suffix composes: `string[+][]`,
      `readonly T[+]`.
- [x] 4. **Traits v2** — SHIPPED, see feature 8. Associated functions (methods without `self` →
      merge as `Status.from(...)`; params are user-annotated so the
      receiver-typing step is skipped); trait type-arguments in the
      header (`impl From<string> for Status` →
      `satisfies From<Status, string>`, Self first then header args in
      order); comma-header multi-instantiation
      (`impl From<string>, From<number> for Status` with ONE
      union-typed body — each listed trait is a separate satisfies
      obligation; deliberately NOT
      `From<string | number>`, a weaker single claim); early semantic
      checks with original spans (trait ident must be declared/imported
      in-module; method-vs-variant and cross-impl collisions named
      "`x` is defined by both A and B"; same-file no-extends trait
      interfaces get syntactic member-name comparison — imported traits
      still defer to `satisfies`; NOTE this supersedes the v1
      "collisions left to tsc by design" disposition, re-decided with
      Zuri 2026-08-06). See "the permanent boundary" table in feature 8
      for everything deliberately NOT here.
- [x] 4b. **Syntax re-decisions, 2026-08-07 — SHIPPED (issue #60), both
      breaking:** drop `fn` from impl blocks — members are bare TS-style
      methods (`fmt(self): string {}`; the word `fn` starting a member
      gets a migration diagnostic); reserve `not` and keep it as a
      `ZtsNot` AST node lowered in lower.rs (formatter round-trip —
      see the `not` section above; `const not = 1` / `not(x)` calls
      become errors).
- [x] 5. **Impls for newtypes and unions** — SHIPPED. The orphan rule
      widens to all three zts nominal types. Union factories merge like
      enums (methods + satisfies; `values`/`has` name collisions are a
      semantic error at the original span). Newtype factories are
      ARROWS, so impls attach via
      `globalThis.Object.assign(factory, { methods } satisfies ...)` —
      Object.assign's return type is the intersection, keeping the
      const callable AND carrying the methods; dictionary passing
      (`describe(u, UserId)`) works unchanged. No syntax change — the
      impl grammar was already target-agnostic (grammar/formatter
      dispositions: none needed).

`zts-fmt` (no formatter can parse zts; prettier-plugin route rejected —
bidirectional TS↔zts nesting makes `embed` delegation impractical):

- [x] 6. DONE (verdict GREEN, then productionized into the three forks). **Feasibility spike (the risk gate)**: fork
      dprint-plugin-typescript (Rust, prettier-style output, built on
      swc's AST), repoint its swc deps at `../swc_rustify@zts`
      path-deps, confirm the version pin aligns and a zts-flag parse
      flows through its pipeline. A day to learn what a month would
      otherwise cost.
- [x] 7. DONE, all 19 zts nodes. Print rules for the zts nodes (match, enums-with-data,
      expression-if chains, newtype, union, impl/fn, not, postfix `?`)
      — donor-node discipline, mirroring existing dprint patterns.
- [x] 8. DONE: 21 zts spec files / 143 specs; 1144 idempotence fixed points (4 configs, 2 starting points, triple-pass). Idempotence suite over
      the existing fixtures; comment preservation.
- [x] 9. DONE: crates/zts-fmt (bin `zts-fmt [--check]`, zts/ztsx only, line width 80) + `format()` on @zestty/native, served through
      @zestty/language-server `textDocument/formatting` so VS Code and
      nvim get format-on-save with zero new editor wiring; defaults
      tuned to this repo's prettier style.
- [x] 10. AMENDED + DONE: fixtures are deliberately NOT format-gated —
      several encode load-bearing layout (the ASI regression fixture
      REQUIRES `match(1)` + newline + block) and reformatting them
      would destroy what they test. The gate is instead the fork's
      1144 idempotence fixed points plus crates/zts-fmt's smoke corpus
      (canonical constructs format, are idempotent, and every construct
      round-trips). CI clones the three forks as siblings.

- [x] 11. Release 0.4.0.

Open inputs from Zuri before the affected items start: RESOLVED — the
impl-block indent reset was nvim (tree-sitter indent override, issue
#57, shipped in this phase). (`fn`: DROPPED; `static_assert`: renamed
`constrict`, paren-free; `not`: reserved — all decided 2026-08-07, see
item 4b and the feature sections.)

Standing rule (Zuri, 2026-08-07 — also in CLAUDE.md): every syntax
change ships WITH its VS Code + nvim highlighting updates and its
zts-fmt print-rule updates in ONE patch-version commit.

### Post-0.4.0 — the DX & speed slate (0.4.x patches, Zuri-approved 2026-08-08)

Ships as 0.4.x PATCHES ("keep it at 0.4 for now — none of these
break"), each its own patch with the normal PR flow and tests. Order
is load-bearing — no optimization lands without a before/after number
from the harness:

- [ ] 1. **Benchmark harness** — compile-time suite over the fixtures +
      a synthetic large module, LS keystroke latency, zts-check
      wall-clock. Lands FIRST; every later item cites its numbers.
- [ ] 2. **Toolchain optimization round** — napi reusable sized worker
      thread (replaces the per-call 64 MiB spawn; preserves the
      stack-safety property), LS incremental/debounced recompile,
      zts-check content-hash skip-cache, watch-mode per-file twin
      regen.
- [ ] 3. **LSP hover granularity** — sharper position mapping in match
      arms + synthesized hovers for zts-only constructs, answered from
      the enum decl.
- [ ] 4. **LS semantic tokens** — real nvim highlighting; retires the
      parity-rule nvim caveat (update the CLAUDE.md parity note when it
      lands — confirm with Zuri then).

Disposition (Zuri): toolchain speed first; generated-output
optimizations explicitly deferred — do not re-litigate emitted shapes
without profiling evidence. Parked: prebuilds/marketplace publishing.

**0.5.0 is unassigned** — reserved for language-feature work. Shelf
candidates: match guards (design round first), trait default methods,
Option/null-unification (likely 1.0-territory breaking).

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
