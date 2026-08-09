//! Semantic checks that must run before lowering.
//!
//! Scope is deliberately small: everything that can be enforced by shaping
//! the generated TS is left to tsc (exhaustiveness above all). Here we only
//! reject zts constructs that are syntactically valid but outside the
//! locked Phase 1 grammar, with spans pointing at the original `.zts`.

use std::collections::{BTreeMap, HashSet};

use swc_common::{Span, Spanned, errors::Handler};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

/// Deeper than this and the recursive passes (lowering, hygiene, codegen)
/// risk a stack overflow — which is an uncatchable SIGABRT, unacceptable
/// once the compiler runs inside a Node/Vite process. Reject early with a
/// real diagnostic instead.
const MAX_EXPR_DEPTH: usize = 2048;

/// Widest `lo..=hi` range arm zts will lower, in values (`hi - lo + 1`).
///
/// This is a DoS control, not a nicety, and it is enforced HERE — before
/// lowering allocates anything. A range arm expands to a type alias with
/// one literal member per value, so an unbounded range (`0..=2000000000`)
/// is billions of AST nodes: the language server would OOM on a keystroke.
/// Fail closed with a diagnostic naming the width instead.
///
/// 1024 covers every realistic literal-union vocabulary (the whole HTTP
/// 4xx/5xx space is 100 values each) with two orders of magnitude spare.
pub const MAX_RANGE_WIDTH: i64 = 1024;

/// Total enumerated range values allowed in ONE MODULE.
///
/// [`MAX_RANGE_WIDTH`] bounds a single arm; without a module-wide budget
/// the same DoS just needs more arms. Measured on the 0.5.0 security
/// round: 447 KB of *individually legal* disjoint range arms expanded to
/// ~160 MB of generated TypeScript and 2.7 GB RSS — and the language
/// server pays that on every keystroke, because it recompiles the whole
/// document. 65_536 is ~64x the single-arm cap and two orders of
/// magnitude above any real vocabulary; the diagnostic names both the
/// running total and the cap so the author can see what they spent it on.
pub const MAX_RANGE_TOTAL: i64 = 65_536;

/// Bounds must be exactly representable as integers, so the enumeration
/// `lo..=hi` is exact. `2^53 - 1`, JavaScript's `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INT: f64 = 9_007_199_254_740_991.0;

/// Semantic checking failed; diagnostics were emitted via the handler.
#[derive(Debug)]
pub struct SemanticFailure {
    pub errors: usize,
    /// The expression-nesting limit fired. The caller should `mem::forget`
    /// the module instead of dropping it: `Drop for Expr` recurses and is
    /// not stack-protected, so dropping an over-deep AST can SIGABRT
    /// *after* the diagnostic was printed.
    pub depth_exceeded: bool,
}

impl std::fmt::Display for SemanticFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} semantic error(s)", self.errors)
    }
}

impl std::error::Error for SemanticFailure {}

pub fn check(module: &Module, handler: &Handler) -> Result<(), SemanticFailure> {
    let mut checker = Checker {
        handler,
        errors: 0,
        expr_depth: 0,
        depth_reported: false,
        has_match: false,
        global_this_decls: Vec::new(),
        fn_depth: 0,
        zts_iife_depth: 0,
        allow_try: None,
        enclosing_fn: EnclosingFn::None,
        next_fn_is_setter: false,
        sanctioned_impls: HashSet::new(),
        range_values_total: 0,
        range_total_reported: false,
    };
    module.visit_with(&mut checker);

    // A module-scope binding named `globalThis` would capture the injected
    // helper's `globalThis.Error` reference — lexical scoping leaves no way
    // to make that reference shadow-proof, so reject the (pathological)
    // combination outright with a span the author can act on.
    if checker.has_match && !checker.global_this_decls.is_empty() {
        for span in std::mem::take(&mut checker.global_this_decls) {
            checker.err(
                span,
                "declaring a binding named `globalThis` is not supported in a module that uses \
                 `match` (the generated exhaustiveness helper references `globalThis.Error`)",
            );
        }
    }

    if checker.errors > 0 {
        return Err(SemanticFailure {
            errors: checker.errors,
            depth_exceeded: checker.depth_reported,
        });
    }
    Ok(())
}

struct Checker<'a> {
    handler: &'a Handler,
    errors: usize,
    expr_depth: usize,
    depth_reported: bool,
    has_match: bool,
    global_this_decls: Vec<Span>,
    /// Nesting depth of real functions (fn/arrow/method): `?` needs an
    /// enclosing function to return the `Err` from.
    fn_depth: usize,
    /// Inside a construct that lowers to a synchronous IIFE (match arm
    /// block, if-expression block): an early `return` there would return
    /// from the IIFE, not the user's function. Reset at function
    /// boundaries.
    zts_iife_depth: usize,
    /// Identity (address) of the one sanctioned `?` of the statement being
    /// visited (whole-RHS forms only). Node identity — a flag would be
    /// burned by unrelated statements nested in destructuring defaults, and
    /// would blame the wrong `?` in `const { a = h()? } = g()?;`. Stored as
    /// a usize so a future deref is impossible by construction.
    allow_try: Option<usize>,
    /// Return-shape of the nearest enclosing function (F1: `return __t` in
    /// a void-contextual callback or generator silently swallows the Err).
    enclosing_fn: EnclosingFn,
    /// Set by class-method visitors so the inner `visit_function` knows it
    /// is a setter (setters cannot return a value).
    next_fn_is_setter: bool,
    /// Node identities (addresses) of impls found in a legal position (a
    /// module/block statement list). `visit_zts_impl_decl` removes each on
    /// visit; an impl NOT in the set sits in a single-statement slot
    /// (`if (c) impl ...`), which the list-based orphan check never sees
    /// and the lowering cannot merge.
    sanctioned_impls: HashSet<usize>,
    /// Running total of values enumerated by range arms in this MODULE
    /// (0.5.0 review, code finding 3 / security finding 2). The per-arm
    /// width cap does not bound the aggregate; this does.
    range_values_total: i64,
    /// The module budget message is emitted once, not once per later arm.
    range_total_reported: bool,
}

/// Hard cap on RENDERED zts diagnostics: each one re-renders its source
/// line, so unbounded emission is a memory-amplification vector (see the
/// Buf cap in lib.rs — this is the other half of that fix).
const MAX_DIAGNOSTICS: usize = 100;

impl Checker<'_> {
    fn err(&mut self, span: Span, msg: &str) {
        self.errors += 1;
        match self.errors.cmp(&MAX_DIAGNOSTICS) {
            std::cmp::Ordering::Less => self.handler.struct_span_err(span, msg).emit(),
            std::cmp::Ordering::Equal => self
                .handler
                .struct_span_err(span, "too many errors; further zts diagnostics suppressed")
                .emit(),
            std::cmp::Ordering::Greater => {}
        }
    }

    /// The `__zts` namespace is generated-code-only, on BOTH planes.
    ///
    /// On the value plane this closes security-gate F9: a user binding
    /// named `__ztsValue` would collide with a pre-resolver lowering that
    /// hygiene cannot protect.
    ///
    /// On the TYPE plane it closes a keystone false-green (0.5.0 security
    /// round, finding 1). Hygiene is not TS-type-aware, so it will not
    /// rename a user type declaration that collides with a generated one —
    /// and a shadowed generated type is not a compile error, it is a
    /// SILENTLY DIFFERENT PROOF. Reproduced: with
    /// `type __ztsRange0 = number` in an inner scope, a range arm covering
    /// 2 of 5 union members re-points its predicate at `number`, narrows
    /// the scrutinee to `never`, and certifies the match exhaustive — tsc
    /// exits 0 and the program throws at runtime. The same shape would
    /// forge `__ztsExpect`/`__ztsEqual` for `constrict`.
    /// A reachability error, naming the EARLIER ARM THE AUTHOR WROTE and
    /// pointing a secondary span at it (0.5.0 review, code finding 4).
    /// Naming the merged interval instead sends the author hunting for an
    /// arm that does not exist in their source.
    fn err_covered(&mut self, arm: SourceArm, prior: &Coverage, prefix: &str) {
        self.errors += 1;
        if self.errors > MAX_DIAGNOSTICS {
            return;
        }
        if self.errors == MAX_DIAGNOSTICS {
            self.handler
                .struct_span_err(
                    arm.span,
                    "too many errors; further zts diagnostics suppressed",
                )
                .emit();
            return;
        }
        match prior.source_hit(arm.lo, arm.hi) {
            Some(earlier) => {
                let mut diag = self
                    .handler
                    .struct_span_err(arm.span, &format!("{prefix} the earlier arm `{earlier}`"));
                diag.span_note(earlier.span, "the earlier arm is here");
                diag.emit();
            }
            None => self
                .handler
                .struct_span_err(arm.span, &format!("{prefix} an earlier arm"))
                .emit(),
        }
    }

    fn note_zts_reserved_ident(&mut self, ident: &Ident) {
        if ident.sym.starts_with("__zts") {
            self.err(
                ident.span,
                "identifiers starting with `__zts` are reserved for zts-generated code",
            );
        }
    }

    fn note_global_this_shadow(&mut self, ident: &Ident) {
        if ident.sym == "globalThis" {
            self.global_this_decls.push(ident.span);
        }
        self.note_zts_reserved_ident(ident);
        // `not` is a reserved word since 0.4.0 (Zuri, 2026-08-07): the
        // parser owns expression positions; bindings are rejected here so
        // every binding path (const/let/fn/class/params/imports) gets one
        // consistent diagnostic. Property names stay legal, like ES
        // reserved words.
        if ident.sym == "not" {
            self.err(
                ident.span,
                "`not` is a reserved word in zts (the negation operator, 0.4.0); rename this \
                 binding",
            );
        }
    }

    fn guard_depth(&mut self, e: &Expr, descend: impl FnOnce(&mut Self)) {
        self.expr_depth += 1;
        if self.expr_depth > MAX_EXPR_DEPTH {
            if !self.depth_reported {
                self.depth_reported = true;
                self.err(
                    e.span(),
                    &format!("expression nesting exceeds the zts limit of {MAX_EXPR_DEPTH}"),
                );
            }
        } else {
            descend(self);
        }
        self.expr_depth -= 1;
    }

    fn check_match(&mut self, m: &MatchExpr) {
        // The parser cannot produce a zero-arm match, but MatchExpr is
        // public API (napi, tests) — defend here too.
        if m.arms.is_empty() {
            self.err(m.span, "match must have at least one arm");
        }

        // A match is either variant-mode or literal-mode, never mixed;
        // `_` is legal in both but must be the LAST arm and appear once.
        // Range arms (`400..=499`) are literal-mode arms.
        let mut seen_variants: HashSet<&swc_atoms::Atom> = HashSet::new();
        let mut seen_lits: HashSet<String> = HashSet::new();
        let mut mode: Option<&'static str> = None;
        let mut wildcard_seen = false;

        // Syntactic reachability over the integer number line (0.5.0). tsc
        // is SILENT about a range arm shadowed by an earlier one — nothing
        // in the generated TS is ill-typed — so this is the compile-error
        // class ranges bring with them. `covered` holds earlier range arms
        // AND earlier integer literal arms; `ranges` holds only the range
        // arms, because a literal written before a range that contains it
        // is the deliberate specific-case-first idiom
        // (`404 => …, 400..=499 => …`) and must keep working.
        let mut covered = Coverage::default();
        let mut ranges = Coverage::default();

        for arm in &m.arms {
            if wildcard_seen {
                self.err(
                    arm.span,
                    "unreachable arm: `_` matches everything and must be the last arm",
                );
            }
            match &arm.pattern {
                MatchPat::Wildcard(..) => {
                    wildcard_seen = true;
                }
                MatchPat::Variant(v) => {
                    if v.name.sym == "_" {
                        self.err(
                            v.name.span,
                            "`_` cannot be a variant name; write `_ => ...` for a wildcard arm",
                        );
                    }
                    match mode {
                        Some("lit") => self.err(
                            v.span,
                            "cannot mix variant arms and literal arms in one match",
                        ),
                        _ => mode = Some("variant"),
                    }
                    if !seen_variants.insert(&v.name.sym) {
                        self.err(
                            v.name.span,
                            &format!("duplicate match arm for variant `{}`", v.name.sym),
                        );
                    }
                    if let Some(binding) = &v.binding {
                        self.check_binding(binding);
                    }
                }
                MatchPat::Lit(l) => {
                    match mode {
                        Some("variant") => self.err(
                            l.span,
                            "cannot mix literal arms and variant arms in one match",
                        ),
                        _ => mode = Some("lit"),
                    }
                    // Span-free identity: Debug on Lit embeds spans, which
                    // would make every literal unique. Numbers key on the
                    // VALUE with `-0` collapsed to `0` (they are === in JS,
                    // so a `-0` arm after `0` is dead code).
                    let key = match &l.lit {
                        Lit::Str(s) => format!("s:{:?}", s.value),
                        Lit::Num(n) => {
                            let v = if l.neg { -n.value } else { n.value };
                            format!("n:{}", v + 0.0)
                        }
                        Lit::BigInt(b) => {
                            // Negate the VALUE (num_bigint never displays
                            // `-0`), not the text — `0n` and `-0n` are ===.
                            let v = if l.neg {
                                -(*b.value).clone()
                            } else {
                                (*b.value).clone()
                            };
                            format!("bi:{v}")
                        }
                        Lit::Bool(b) => format!("b:{}", b.value),
                        Lit::Null(..) => "null".to_string(),
                        other => format!("x:{:?}", other.span()),
                    };
                    if !seen_lits.insert(key) {
                        self.err(l.span, "duplicate literal match arm");
                        // Already reported; letting it fall through to the
                        // interval check would blame an earlier RANGE for
                        // what is plainly a duplicate literal.
                    } else if let Some(v) = integer_literal_value(&l.lit, l.neg) {
                        let arm = SourceArm {
                            lo: v,
                            hi: v,
                            span: l.span,
                            is_range: false,
                        };
                        if ranges.overlap(v, v).is_some() {
                            self.err_covered(
                                arm,
                                &ranges,
                                &format!("unreachable arm: `{v}` is already covered by"),
                            );
                        }
                        covered.insert(arm);
                    }
                }
                MatchPat::Range(r) => {
                    match mode {
                        Some("variant") => self.err(
                            r.span,
                            "cannot mix range arms and variant arms in one match (a range arm \
                             matches a number, a variant arm matches a `kind` tag)",
                        ),
                        _ => mode = Some("lit"),
                    }
                    if let Some((lo, hi)) = self.check_range_bounds(r) {
                        let arm = SourceArm {
                            lo,
                            hi,
                            span: r.span,
                            is_range: true,
                        };
                        if covered.covering(lo, hi).is_some() {
                            self.err_covered(
                                arm,
                                &covered,
                                &format!(
                                    "unreachable arm: every value of the range `{lo}..={hi}` is \
                                     already matched by"
                                ),
                            );
                        } else if ranges.overlap(lo, hi).is_some() {
                            self.err_covered(
                                arm,
                                &ranges,
                                &format!(
                                    "overlapping range arms: `{lo}..={hi}` shares values with"
                                ),
                            );
                        }
                        covered.insert(arm);
                        ranges.insert(arm);
                    }
                }
            }

            let mut suspender = SuspenderCheck {
                checker: self,
                what: "a match arm",
            };
            arm.body.visit_with(&mut suspender);
        }
    }

    /// Validates one `lo..=hi` range arm and returns its integer bounds.
    ///
    /// Everything here fails CLOSED: on any error the arm contributes no
    /// interval and no lowering runs (the compile already failed), so a
    /// pathological range can never reach the enumeration in `lower.rs`.
    fn check_range_bounds(&mut self, r: &MatchRangePat) -> Option<(i64, i64)> {
        let lo = self.range_bound(&r.lo, r.lo_neg, r.lo.span)?;
        let hi = self.range_bound(&r.hi, r.hi_neg, r.hi.span)?;

        if lo > hi {
            self.err(
                r.span,
                &format!(
                    "range lower bound `{lo}` is greater than its upper bound `{hi}`; zts ranges \
                     are inclusive and must be written low-to-high (`{hi}..={lo}`)"
                ),
            );
            return None;
        }

        // Cannot overflow: both bounds are safe integers, so the width fits
        // in i64 with room to spare.
        let width = hi - lo + 1;
        if width > MAX_RANGE_WIDTH {
            self.err(
                r.span,
                &format!(
                    "range `{lo}..={hi}` spans {width} values, over the zts limit of \
                     {MAX_RANGE_WIDTH}: a range arm expands to one literal type per value, so an \
                     unbounded range would exhaust memory in the compiler and the editor. Match \
                     on `number` with a `_` arm instead."
                ),
            );
            return None;
        }

        // Per-MODULE budget. The per-arm cap alone does not bound the
        // expansion: the same attack just uses more arms, each of them
        // individually legal (measured: 447 KB of source → ~160 MB of
        // generated TS, 2.7 GB RSS, and the LS pays it per keystroke).
        // Checked here, before any coverage bookkeeping or lowering
        // allocation happens.
        match self.range_values_total.checked_add(width) {
            Some(total) if total <= MAX_RANGE_TOTAL => self.range_values_total = total,
            _ => {
                let total = self.range_values_total.saturating_add(width);
                // Once per module: every later range arm would repeat the
                // same budget message with the same number.
                if !std::mem::replace(&mut self.range_total_reported, true) {
                    self.err(
                        r.span,
                        &format!(
                            "range arms in this module enumerate {total} values, over the zts \
                             limit of {MAX_RANGE_TOTAL}: each value becomes a literal type in the \
                             generated \
                         TypeScript, and the language server re-expands all of them on every \
                             keystroke. Narrow the ranges, or match on `number` with a `_` arm."
                        ),
                    );
                }
                return None;
            }
        }

        Some((lo, hi))
    }

    /// One range bound: an INTEGER number literal, optionally negated. The
    /// span is the BOUND's own (0.5.0 review, code finding 5) — pointing at
    /// the whole range for a problem with one end of it makes the author
    /// check both.
    fn range_bound(&mut self, n: &Number, neg: bool, span: Span) -> Option<i64> {
        // Decimal-point and exponent forms are rejected on the literal's
        // RAW TEXT, not its value: `4e2` is integral (400) but is not an
        // integer literal, and letting it through would mean `4e2..=4e2`
        // silently enumerates `400`. Radix-prefixed literals are integer
        // literals by construction — and `0x1E` contains an `E` that is a
        // hex digit, not an exponent, so they must be excluded from the
        // text scan.
        //
        // ASYMMETRY, deliberate (0.5.0 review, code finding 9): a `union`
        // MEMBER is checked by VALUE (`fract() != 0`), a range BOUND by
        // RAW TEXT. They answer different questions. A range bound is the
        // start of an ENUMERATION, so its written form has to be one the
        // reader can count from — `4e2..=4e3` reads like "4 to 4000" and
        // enumerates 2601 values. A union member is just a literal type,
        // never enumerated, and the value rule is what makes `1` and `1.0`
        // collapse into the one duplicate-member error TypeScript would
        // otherwise apply silently. See `visit_zts_union_decl` for the
        // other half.
        if let Some(raw) = n.raw.as_deref() {
            let bytes = raw.as_bytes();
            let radix_prefixed = bytes.len() > 1
                && bytes[0] == b'0'
                && matches!(bytes[1], b'x' | b'X' | b'o' | b'O' | b'b' | b'B');
            if !radix_prefixed && raw.contains(['.', 'e', 'E']) {
                self.err(
                    span,
                    &format!(
                        "range bounds must be integer literals; `{raw}` is a decimal or exponent \
                         form (a range enumerates whole numbers, so a fractional bound has no \
                         meaning)"
                    ),
                );
                return None;
            }
        }

        if !n.value.is_finite() || n.value.fract() != 0.0 {
            self.err(span, "range bounds must be integer literals");
            return None;
        }
        if n.value.abs() > MAX_SAFE_INT {
            self.err(
                span,
                "range bound is outside JavaScript's safe integer range, so the values it spans \
                 cannot be enumerated exactly",
            );
            return None;
        }

        // `-0` and `0` are the same value in JS; `-(0.0) as i64` is 0.
        let v = if neg { -n.value } else { n.value };
        Some(v as i64)
    }

    fn check_binding(&mut self, binding: &ObjectPat) {
        if binding.optional {
            self.err(binding.span, "zts match bindings cannot be optional");
        }
        if let Some(ann) = &binding.type_ann {
            self.err(
                ann.span,
                "zts match bindings cannot carry type annotations; the variant type is inferred",
            );
        }
        for prop in &binding.props {
            match prop {
                ObjectPatProp::Assign(AssignPatProp { value: None, .. }) => {}
                ObjectPatProp::Assign(AssignPatProp {
                    span,
                    value: Some(..),
                    ..
                }) => {
                    self.err(
                        *span,
                        "zts match bindings cannot have defaults; bind the field and handle it in the arm body",
                    );
                }
                ObjectPatProp::KeyValue(kv) => {
                    self.err(
                        kv.key.span(),
                        "zts match bindings must be shorthand identifiers (`{ field }`, not `{ field: alias }`)",
                    );
                }
                ObjectPatProp::Rest(rest) => {
                    self.err(rest.span, "zts match bindings cannot use rest patterns");
                }
            }
        }
    }
}

/// The integer value of a numeric literal arm pattern, if it has one.
/// `-0` collapses to `0` (they are `===` in JS — the same collapse the
/// duplicate-arm key does).
fn integer_literal_value(lit: &Lit, neg: bool) -> Option<i64> {
    let Lit::Num(n) = lit else { return None };
    let v = if neg { -n.value } else { n.value };
    if !v.is_finite() || v.fract() != 0.0 || v.abs() > MAX_SAFE_INT {
        return None;
    }
    Some(v as i64)
}

/// One literal-mode arm as the author wrote it, for diagnostics.
#[derive(Clone, Copy)]
struct SourceArm {
    lo: i64,
    hi: i64,
    span: Span,
    is_range: bool,
}

impl std::fmt::Display for SourceArm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_range {
            write!(f, "{}..={}", self.lo, self.hi)
        } else {
            write!(f, "{}", self.lo)
        }
    }
}

/// Merged, disjoint, non-adjacent integer intervals — the coverage the
/// arms seen so far have accumulated. Adjacency counts as overlap when
/// merging (`[1,2] + [3,4] = [1,4]`) because the values are integers:
/// nothing can sit between them.
///
/// Keyed by lower bound in a `BTreeMap` so every operation is O(log n)
/// (0.5.0 security round, finding 3). The first cut rebuilt a `Vec` per
/// insert, which is O(n²) over disjoint arms and REGRESSED pre-0.5.0
/// code: a 50k plain-literal-arm match went from 0.11s to 2.3s, and the
/// language server pays that per keystroke.
///
/// `merged` decides; `sources` only supplies the arm text and span for a
/// diagnostic. They are separate because a merged interval is usually
/// several arms, and naming the merge ("already covered by `0..=9`") sends
/// the author looking for an arm they never wrote.
#[derive(Default)]
struct Coverage {
    merged: BTreeMap<i64, i64>,
    sources: BTreeMap<i64, SourceArm>,
}

impl Coverage {
    /// The merged interval that fully contains `lo..=hi`, if any. Exact:
    /// the intervals are merged, so a covered range lies inside exactly
    /// one of them, and it is the last one starting at or before `lo`.
    fn covering(&self, lo: i64, hi: i64) -> Option<(i64, i64)> {
        let (&a, &b) = self.merged.range(..=lo).next_back()?;
        (hi <= b).then_some((a, b))
    }

    /// A merged interval sharing any value with `lo..=hi`. Exact: if any
    /// overlaps, so does the last one starting at or before `hi`.
    fn overlap(&self, lo: i64, hi: i64) -> Option<(i64, i64)> {
        let (&a, &b) = self.merged.range(..=hi).next_back()?;
        (b >= lo).then_some((a, b))
    }

    /// An EARLIER source arm intersecting `lo..=hi`, for the message.
    /// Best-effort by design: the verdict already came from `merged`, and
    /// source arms can themselves overlap once an error has been reported,
    /// in which case naming any intersecting one is still right.
    fn source_hit(&self, lo: i64, hi: i64) -> Option<SourceArm> {
        let (_, arm) = self.sources.range(..=hi).next_back()?;
        (arm.hi >= lo).then_some(*arm)
    }

    /// Record `arm`, merging its interval into the coverage. Amortized
    /// O(log n): every interval is removed at most once over the life of
    /// the map.
    fn insert(&mut self, arm: SourceArm) {
        self.sources.entry(arm.lo).or_insert(arm);

        let (mut start, mut end) = (arm.lo, arm.hi);
        // Absorb a left neighbour that touches (adjacent counts).
        if let Some((&a, &b)) = self.merged.range(..start).next_back() {
            if b.saturating_add(1) >= start {
                start = a;
                end = end.max(b);
            }
        }
        // Absorb every interval starting inside (or adjacent to) the run.
        // Only the LAST removal can extend `end`, and the next interval
        // after it starts at least two past its own end, so one pass is
        // enough.
        let keys: Vec<i64> = self
            .merged
            .range(start..=end.saturating_add(1))
            .map(|(&a, _)| a)
            .collect();
        for k in keys {
            if let Some(b) = self.merged.remove(&k) {
                end = end.max(b);
            }
        }
        self.merged.insert(start, end);
    }
}

/// Return-shape of the nearest enclosing function, for the `?` operator.
/// TypeScript accepts ANY returned value from a void-contextual callback
/// (`xs.forEach(x => ...)`) and folds generator returns into TReturn — in
/// both, the generated `return __t` would silently swallow the Err. `?`
/// therefore requires an explicit return type annotation.
#[derive(Clone, Copy, PartialEq)]
enum EnclosingFn {
    None,
    Annotated,
    Unannotated,
    Generator,
    Setter,
}

/// The `?` operator's sanctioned v1 shapes: the try is the WHOLE top-level
/// expression of one of these statements. Anything deeper would hoist the
/// operand's evaluation past sibling subexpressions — a silent side-effect
/// reorder — so v1 rejects it.
fn stmt_top_try(s: &Stmt) -> Option<&ZtsTryExpr> {
    match s {
        Stmt::Decl(Decl::Var(v)) if v.decls.len() == 1 && !v.declare => {
            match v.decls[0].init.as_deref() {
                Some(Expr::ZtsTry(t)) => Some(t),
                _ => None,
            }
        }
        Stmt::Return(r) => match r.arg.as_deref() {
            Some(Expr::ZtsTry(t)) => Some(t),
            _ => None,
        },
        Stmt::Expr(e) => match &*e.expr {
            Expr::ZtsTry(t) => Some(t),
            _ => None,
        },
        _ => None,
    }
}

impl Checker<'_> {
    /// One `enum`/`newtype` expands to a type alias + a const; two zts
    /// declarations sharing a name in one scope would otherwise surface as
    /// four confusing TS2451s on GENERATED code. Catch it here with one
    /// error on the original span.
    fn check_zts_decl_names<'x>(&mut self, decls: impl Iterator<Item = &'x Decl>) {
        let mut seen: HashSet<&swc_atoms::Atom> = HashSet::new();
        for decl in decls {
            let ident = match decl {
                Decl::ZtsEnum(e) => &e.ident,
                Decl::ZtsNewtype(n) => &n.ident,
                Decl::ZtsUnion(u) => &u.ident,
                _ => continue,
            };
            if !seen.insert(&ident.sym) {
                self.err(
                    ident.span,
                    &format!(
                        "duplicate zts declaration `{}` in this scope (each enum/newtype/union \
                         expands to a type alias AND a const of that name)",
                        ident.sym
                    ),
                );
            }
        }
    }
}

impl Checker<'_> {
    /// The trait rules that need the whole statement list (Phase 6, and
    /// the traits-v2 early checks of Phase 7):
    /// - orphan rule: `impl X for T` requires a zts enum `T` in the SAME
    ///   list (v1: enum impls only);
    /// - `export impl` is meaningless (the factory const owns the export);
    /// - every impl seen here is sanctioned — impls anywhere else are in a
    ///   single-statement slot and get rejected by `visit_zts_impl_decl`;
    /// - (module level only, `module_items` is Some) each trait name must
    ///   be declared or imported in the module — typos die at the header
    ///   instead of as a remapped TS2304 on generated code;
    /// - method-vs-variant collisions and CROSS-impl duplicate methods
    ///   per target, named at the original spans (supersedes the v1
    ///   "left to tsc" disposition — re-decided with Zuri 2026-08-06);
    /// - same-file no-`extends` trait interfaces get a syntactic
    ///   member-NAME comparison (missing/extra methods before tsc runs);
    ///   imported traits still defer to the generated `satisfies` — we
    ///   read local syntax, never types (the type-plane rule).
    fn check_zts_impls(&mut self, decls: &[(&Decl, bool)], module_items: Option<&[ModuleItem]>) {
        // Phase 7 item 5: impls target enums, newtypes, and unions.
        #[derive(Clone, Copy, PartialEq)]
        enum ImplTarget {
            Enum,
            Newtype,
            Union,
        }
        let targets: std::collections::HashMap<&swc_atoms::Atom, ImplTarget> = decls
            .iter()
            .filter_map(|(d, _)| match d {
                Decl::ZtsEnum(e) => Some((&e.ident.sym, ImplTarget::Enum)),
                Decl::ZtsNewtype(n) => Some((&n.ident.sym, ImplTarget::Newtype)),
                Decl::ZtsUnion(u) => Some((&u.ident.sym, ImplTarget::Union)),
                _ => None,
            })
            .collect();

        // Module-scope type-ish names + same-file plain interfaces.
        let mut type_names: Option<HashSet<&swc_atoms::Atom>> = None;
        let mut interfaces: std::collections::HashMap<&swc_atoms::Atom, &TsInterfaceDecl> =
            std::collections::HashMap::new();
        if let Some(items) = module_items {
            let mut names: HashSet<&swc_atoms::Atom> = HashSet::new();
            for item in items {
                match item {
                    ModuleItem::Stmt(Stmt::Decl(d))
                    | ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl {
                        decl: d, ..
                    })) => match d {
                        Decl::TsInterface(iface) => {
                            names.insert(&iface.id.sym);
                            if iface.extends.is_empty() {
                                interfaces.insert(&iface.id.sym, iface);
                            }
                        }
                        Decl::TsTypeAlias(a) => {
                            names.insert(&a.id.sym);
                        }
                        Decl::Class(c) => {
                            names.insert(&c.ident.sym);
                        }
                        Decl::ZtsEnum(e) => {
                            names.insert(&e.ident.sym);
                        }
                        Decl::ZtsNewtype(n) => {
                            names.insert(&n.ident.sym);
                        }
                        Decl::ZtsUnion(u) => {
                            names.insert(&u.ident.sym);
                        }
                        _ => {}
                    },
                    ModuleItem::ModuleDecl(ModuleDecl::Import(imp)) => {
                        for s in &imp.specifiers {
                            match s {
                                ImportSpecifier::Named(n) => {
                                    names.insert(&n.local.sym);
                                }
                                ImportSpecifier::Default(d) => {
                                    names.insert(&d.local.sym);
                                }
                                ImportSpecifier::Namespace(ns) => {
                                    names.insert(&ns.local.sym);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            type_names = Some(names);
        }

        // Variant names per enum target in this list.
        let variants_of = |target: &swc_atoms::Atom| -> HashSet<&swc_atoms::Atom> {
            decls
                .iter()
                .filter_map(|(d, _)| match d {
                    Decl::ZtsEnum(e) if e.ident.sym == *target => Some(e),
                    _ => None,
                })
                .flat_map(|e| e.variants.iter().map(|v| &v.name.sym))
                .collect()
        };

        // Cross-impl method registry per target: name -> (trait label, span).
        let mut seen_methods: std::collections::HashMap<
            (&swc_atoms::Atom, &swc_atoms::Atom),
            String,
        > = std::collections::HashMap::new();
        let trait_label = |i: &ZtsImplDecl| -> String {
            i.traits
                .iter()
                .map(|t| t.ident.sym.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };

        for (d, exported) in decls {
            let Decl::ZtsImpl(i) = d else { continue };
            self.sanctioned_impls
                .insert(&**i as *const ZtsImplDecl as usize);
            if *exported {
                self.err(
                    i.span,
                    "`export impl` is not supported: the impl merges into the type's factory \
                     const — exporting the enum exports its methods",
                );
            }
            let target_kind = targets.get(&i.for_ident.sym).copied();
            if target_kind.is_none() {
                self.err(
                    i.for_ident.span,
                    &format!(
                        "impl target `{}` is not a zts enum, newtype, or union declared in this \
                         scope (the orphan rule: an impl lives in the module that declares its \
                         type)",
                        i.for_ident.sym
                    ),
                );
            }
            // Union factories already carry `values` and `has`; a method
            // by either name would be a duplicate key in generated code —
            // reject with the original span instead.
            if target_kind == Some(ImplTarget::Union) {
                for m in &i.methods {
                    if m.name.sym == "values" || m.name.sym == "has" {
                        self.err(
                            m.name.span,
                            &format!(
                                "method `{}` collides with the built-in `{}` member of union \
                                 `{}`",
                                m.name.sym, m.name.sym, i.for_ident.sym
                            ),
                        );
                    }
                }
            }

            // (a) trait name must exist in the module (module level only).
            if let Some(names) = &type_names {
                for tr in &i.traits {
                    if !names.contains(&tr.ident.sym) {
                        self.err(
                            tr.ident.span,
                            &format!(
                                "unknown trait `{}`: declare or import it in this module",
                                tr.ident.sym
                            ),
                        );
                    }
                }
            }

            // (c) same-file no-extends interfaces: member-NAME comparison.
            let mut required: std::collections::HashMap<&swc_atoms::Atom, &swc_atoms::Atom> =
                std::collections::HashMap::new();
            let mut all_traits_local = !i.traits.is_empty();
            for tr in &i.traits {
                match interfaces.get(&tr.ident.sym) {
                    Some(iface) => {
                        for member in &iface.body.body {
                            let key = match member {
                                TsTypeElement::TsMethodSignature(m) => m.key.as_ident(),
                                TsTypeElement::TsPropertySignature(p) => p.key.as_ident(),
                                _ => None,
                            };
                            if let Some(k) = key {
                                required.entry(&k.sym).or_insert(&tr.ident.sym);
                            }
                        }
                    }
                    None => all_traits_local = false,
                }
            }
            let method_names: HashSet<&swc_atoms::Atom> =
                i.methods.iter().map(|m| &m.name.sym).collect();
            for (req, owner) in &required {
                if !method_names.contains(*req) {
                    self.err(
                        i.span,
                        &format!(
                            "impl for `{}` is missing method `{req}` required by trait \
                             `{owner}` (declared in this file)",
                            i.for_ident.sym
                        ),
                    );
                }
            }
            if all_traits_local {
                for m in &i.methods {
                    if !required.contains_key(&m.name.sym) {
                        self.err(
                            m.name.span,
                            &format!(
                                "`{}` is not a member of {} (traits declared in this file)",
                                m.name.sym,
                                trait_label(i)
                            ),
                        );
                    }
                }
            }

            // (b) variant collisions + cross-impl duplicates, original spans.
            // Within-impl duplicates are visit_zts_impl_decl's diagnostic;
            // skip repeats here so one mistake gets one error.
            let mut in_this_impl: HashSet<&swc_atoms::Atom> = HashSet::new();
            let variants = variants_of(&i.for_ident.sym);
            let label = trait_label(i);
            for m in &i.methods {
                if !in_this_impl.insert(&m.name.sym) {
                    continue;
                }
                if variants.contains(&m.name.sym) {
                    self.err(
                        m.name.span,
                        &format!(
                            "method `{}` collides with variant `{}` of enum `{}` (both become \
                             factory members)",
                            m.name.sym, m.name.sym, i.for_ident.sym
                        ),
                    );
                }
                match seen_methods.entry((&i.for_ident.sym, &m.name.sym)) {
                    std::collections::hash_map::Entry::Occupied(prev) => {
                        self.err(
                            m.name.span,
                            &format!(
                                "`{}` is defined by both `{}` and `{}` for `{}` (one method \
                                 name per type — the factory merge cannot hold two)",
                                m.name.sym,
                                prev.get(),
                                label,
                                i.for_ident.sym
                            ),
                        );
                    }
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert(label.clone());
                    }
                }
            }
        }
    }
}

impl Visit for Checker<'_> {
    fn visit_expr(&mut self, e: &Expr) {
        self.guard_depth(e, |c| e.visit_children_with(c));
    }

    fn visit_module_items(&mut self, items: &[ModuleItem]) {
        let decls: Vec<(&Decl, bool)> = items
            .iter()
            .filter_map(|item| match item {
                ModuleItem::Stmt(Stmt::Decl(d)) => Some((d, false)),
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl { decl, .. })) => {
                    Some((decl, true))
                }
                _ => None,
            })
            .collect();
        self.check_zts_decl_names(decls.iter().map(|(d, _)| *d));
        self.check_zts_impls(&decls, Some(items));
        items.visit_children_with(self);
    }

    fn visit_stmts(&mut self, stmts: &[Stmt]) {
        let decls: Vec<(&Decl, bool)> = stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Decl(d) => Some((d, false)),
                _ => None,
            })
            .collect();
        self.check_zts_decl_names(decls.iter().map(|(d, _)| *d));
        // Block scope: no module-item view, so the trait-name-exists and
        // same-file-interface checks are skipped (imports/interfaces live
        // at module level); orphan, collision, and duplicate checks run.
        self.check_zts_impls(&decls, None);
        stmts.visit_children_with(self);
    }

    fn visit_zts_impl_decl(&mut self, i: &ZtsImplDecl) {
        if !self
            .sanctioned_impls
            .remove(&(i as *const ZtsImplDecl as usize))
        {
            self.err(
                i.span,
                "an impl must be a top-level statement of the module or block that declares \
                 its enum",
            );
        }
        let mut seen: HashSet<&swc_atoms::Atom> = HashSet::with_capacity(i.methods.len());
        for m in &i.methods {
            // Method names become factory-object keys; the __zts namespace
            // is generated-code-only (F9) and duplicates WITHIN one impl
            // are certainly author error — catch them here with the
            // original span. Collisions ACROSS impls or with variant names
            // are left to tsc (duplicate object key, TS1117) by design.
            if m.name.sym.starts_with("__zts") {
                self.err(
                    m.name.span,
                    "identifiers starting with `__zts` are reserved for zts-generated code",
                );
            }
            if !seen.insert(&m.name.sym) {
                self.err(
                    m.name.span,
                    &format!("duplicate method `{}` in this impl", m.name.sym),
                );
            }
        }
        i.visit_children_with(self);
    }

    fn visit_ts_type(&mut self, t: &TsType) {
        // Types recurse through lowering (newtype clones its underlying
        // type), hygiene, codegen and Drop just like expressions — charge
        // them against the same budget (security-gate F3).
        self.expr_depth += 1;
        if self.expr_depth > MAX_EXPR_DEPTH {
            if !self.depth_reported {
                self.depth_reported = true;
                self.err(
                    t.span(),
                    &format!("type nesting exceeds the zts limit of {MAX_EXPR_DEPTH}"),
                );
            }
        } else {
            t.visit_children_with(self);
        }
        self.expr_depth -= 1;
    }

    fn visit_pat(&mut self, p: &Pat) {
        // Binding patterns recurse through the post-parse passes like
        // everything else (security verification V1); same budget.
        self.expr_depth += 1;
        if self.expr_depth > MAX_EXPR_DEPTH {
            if !self.depth_reported {
                self.depth_reported = true;
                self.err(
                    p.span(),
                    &format!("pattern nesting exceeds the zts limit of {MAX_EXPR_DEPTH}"),
                );
            }
        } else {
            p.visit_children_with(self);
        }
        self.expr_depth -= 1;
    }

    fn visit_stmt(&mut self, s: &Stmt) {
        // Statement nesting (`if (c) if (c) ...`) recurses through the
        // post-parse passes too (security-gate F4); same budget.
        self.expr_depth += 1;
        if self.expr_depth > MAX_EXPR_DEPTH {
            if !self.depth_reported {
                self.depth_reported = true;
                self.err(
                    s.span(),
                    &format!("statement nesting exceeds the zts limit of {MAX_EXPR_DEPTH}"),
                );
            }
            self.expr_depth -= 1;
            return;
        }
        let saved = self.allow_try;
        if let Some(t) = stmt_top_try(s) {
            if self.fn_depth == 0 {
                self.err(
                    t.span,
                    "`?` needs an enclosing function to return the `Err` from; it cannot be \
                     used at module top level or in a class static block",
                );
            } else if self.zts_iife_depth > 0 {
                self.err(
                    t.span,
                    "`?` inside a match arm or if-expression block is not supported in v1: \
                     the construct lowers to an IIFE, which would hijack the early return",
                );
            } else {
                match self.enclosing_fn {
                    EnclosingFn::Setter => self.err(
                        t.span,
                        "`?` cannot be used in a setter: setters cannot return a value, so \
                         there is no way to propagate the `Err`",
                    ),
                    EnclosingFn::Generator => self.err(
                        t.span,
                        "`?` inside a generator is not supported: the early return would \
                         silently become the generator's TReturn instead of propagating \
                         the `Err`",
                    ),
                    EnclosingFn::Unannotated => self.err(
                        t.span,
                        "`?` requires the enclosing function to have an explicit return \
                         type annotation so tsc can verify the propagated `Err` (in a \
                         void-contextual callback like `xs.forEach(x => ...)` an inferred \
                         return type lets the Err vanish silently)",
                    ),
                    EnclosingFn::Annotated | EnclosingFn::None => {}
                }
            }
            // Permit this exact node either way — the context error above
            // already covers it; the generic shape error would be noise.
            self.allow_try = Some(t as *const ZtsTryExpr as usize);
        }
        s.visit_children_with(self);
        self.allow_try = saved;
        self.expr_depth -= 1;
    }

    fn visit_zts_try_expr(&mut self, t: &ZtsTryExpr) {
        if self
            .allow_try
            .is_some_and(|allowed| allowed == t as *const ZtsTryExpr as usize)
        {
            self.allow_try = None;
        } else {
            self.err(
                t.span,
                "`?` is not allowed here in v1: it must be the whole right-hand side of a \
                 `const`/`let` declaration, a `return`, or an expression statement inside a \
                 function body (nested `?` would silently reorder side effects)",
            );
        }
        t.visit_children_with(self);
    }

    fn visit_function(&mut self, f: &Function) {
        self.fn_depth += 1;
        let saved = std::mem::replace(&mut self.zts_iife_depth, 0);
        let shape = if std::mem::take(&mut self.next_fn_is_setter) {
            EnclosingFn::Setter
        } else if f.is_generator {
            EnclosingFn::Generator
        } else if f.return_type.is_some() {
            EnclosingFn::Annotated
        } else {
            EnclosingFn::Unannotated
        };
        let saved_fn = std::mem::replace(&mut self.enclosing_fn, shape);
        f.visit_children_with(self);
        self.enclosing_fn = saved_fn;
        self.zts_iife_depth = saved;
        self.fn_depth -= 1;
    }

    fn visit_arrow_expr(&mut self, a: &ArrowExpr) {
        self.fn_depth += 1;
        let saved = std::mem::replace(&mut self.zts_iife_depth, 0);
        let shape = if a.return_type.is_some() {
            EnclosingFn::Annotated
        } else {
            EnclosingFn::Unannotated
        };
        let saved_fn = std::mem::replace(&mut self.enclosing_fn, shape);
        a.visit_children_with(self);
        self.enclosing_fn = saved_fn;
        self.zts_iife_depth = saved;
        self.fn_depth -= 1;
    }

    fn visit_class_method(&mut self, m: &ClassMethod) {
        // Visit the key FIRST with the flag clear — a computed key's
        // function expression must not consume the setter marker
        // (security verification V2).
        m.key.visit_with(self);
        self.next_fn_is_setter = m.kind == MethodKind::Setter;
        m.function.visit_with(self);
        self.next_fn_is_setter = false;
    }

    fn visit_private_method(&mut self, m: &PrivateMethod) {
        self.next_fn_is_setter = m.kind == MethodKind::Setter;
        m.function.visit_with(self);
        self.next_fn_is_setter = false;
    }

    fn visit_constructor(&mut self, c: &Constructor) {
        // A constructor cannot return a value; `?` inside one must get the
        // "needs an enclosing function" error even when the class sits
        // inside a function.
        let saved_fn = std::mem::replace(&mut self.fn_depth, 0);
        let saved_shape = std::mem::replace(&mut self.enclosing_fn, EnclosingFn::None);
        let saved_iife = std::mem::replace(&mut self.zts_iife_depth, 0);
        c.visit_children_with(self);
        self.fn_depth = saved_fn;
        self.enclosing_fn = saved_shape;
        self.zts_iife_depth = saved_iife;
    }

    fn visit_getter_prop(&mut self, g: &GetterProp) {
        // Object-literal accessors carry a BlockStmt, not a Function —
        // give them real fn context so an annotated getter can use `?`
        // and a setter gets the setter diagnostic (security verification
        // V3).
        g.key.visit_with(self);
        self.fn_depth += 1;
        let saved_iife = std::mem::replace(&mut self.zts_iife_depth, 0);
        let shape = if g.type_ann.is_some() {
            EnclosingFn::Annotated
        } else {
            EnclosingFn::Unannotated
        };
        let saved_fn = std::mem::replace(&mut self.enclosing_fn, shape);
        g.body.visit_with(self);
        self.enclosing_fn = saved_fn;
        self.zts_iife_depth = saved_iife;
        self.fn_depth -= 1;
    }

    fn visit_setter_prop(&mut self, st: &SetterProp) {
        st.key.visit_with(self);
        st.param.visit_with(self);
        self.fn_depth += 1;
        let saved_iife = std::mem::replace(&mut self.zts_iife_depth, 0);
        let saved_fn = std::mem::replace(&mut self.enclosing_fn, EnclosingFn::Setter);
        st.body.visit_with(self);
        self.enclosing_fn = saved_fn;
        self.zts_iife_depth = saved_iife;
        self.fn_depth -= 1;
    }

    fn visit_static_block(&mut self, b: &StaticBlock) {
        // A static block is not a function: `return` is illegal inside it,
        // so `?` has nothing to return from. Reset the context so the
        // "needs an enclosing function" error fires.
        let saved_fn = std::mem::replace(&mut self.fn_depth, 0);
        let saved_shape = std::mem::replace(&mut self.enclosing_fn, EnclosingFn::None);
        let saved_iife = std::mem::replace(&mut self.zts_iife_depth, 0);
        b.visit_children_with(self);
        self.fn_depth = saved_fn;
        self.enclosing_fn = saved_shape;
        self.zts_iife_depth = saved_iife;
    }

    fn visit_zts_expr_block(&mut self, b: &ZtsExprBlock) {
        self.zts_iife_depth += 1;
        b.visit_children_with(self);
        self.zts_iife_depth -= 1;
    }

    fn visit_match_expr(&mut self, m: &MatchExpr) {
        self.has_match = true;
        self.check_match(m);
        m.visit_children_with(self);
    }

    fn visit_binding_ident(&mut self, b: &BindingIdent) {
        self.note_global_this_shadow(&b.id);
        b.visit_children_with(self);
    }

    fn visit_fn_decl(&mut self, d: &FnDecl) {
        self.note_global_this_shadow(&d.ident);
        d.visit_children_with(self);
    }

    fn visit_class_decl(&mut self, d: &ClassDecl) {
        self.note_global_this_shadow(&d.ident);
        d.visit_children_with(self);
    }

    fn visit_import_default_specifier(&mut self, s: &ImportDefaultSpecifier) {
        self.note_global_this_shadow(&s.local);
        s.visit_children_with(self);
    }

    fn visit_import_named_specifier(&mut self, s: &ImportNamedSpecifier) {
        self.note_global_this_shadow(&s.local);
        s.visit_children_with(self);
    }

    fn visit_import_star_as_specifier(&mut self, s: &ImportStarAsSpecifier) {
        self.note_global_this_shadow(&s.local);
        s.visit_children_with(self);
    }

    fn visit_zts_if_expr(&mut self, i: &ZtsIfExpr) {
        // Statement-free chains lower to ternaries, where await/yield stay
        // legal. Chains with statements lower to a synchronous IIFE, so
        // suspension anywhere in the chain (tests included — they evaluate
        // inside the IIFE) must be rejected. Run the suspender ONCE for the
        // whole chain, from its root.
        if crate::lower::if_chain_has_stmts(i) {
            let mut suspender = SuspenderCheck {
                checker: self,
                what: "a multi-statement if-expression",
            };
            i.visit_with(&mut suspender);
        }

        // Walk the chain ITERATIVELY, charging one depth unit per link:
        // `ZtsIfAlt::If` links are not `Expr`s, so the visit_expr guard
        // never sees them — without this, a long else-if chain sails past
        // MAX_EXPR_DEPTH and stack-overflows the recursive passes.
        let mut link = i;
        let mut charged = 0usize;
        loop {
            charged += 1;
            self.expr_depth += 1;
            if self.expr_depth > MAX_EXPR_DEPTH {
                if !self.depth_reported {
                    self.depth_reported = true;
                    self.err(
                        link.span,
                        &format!("expression nesting exceeds the zts limit of {MAX_EXPR_DEPTH}"),
                    );
                }
                break;
            }
            link.test.visit_with(self);
            link.cons.visit_with(self);
            match &link.alt {
                ZtsIfAlt::Block(b) => {
                    b.visit_with(self);
                    break;
                }
                ZtsIfAlt::If(next) => link = next,
            }
        }
        self.expr_depth -= charged;
    }

    fn visit_zts_enum_decl(&mut self, e: &ZtsEnumDecl) {
        self.note_global_this_shadow(&e.ident);
        let mut seen: HashSet<&swc_atoms::Atom> = HashSet::with_capacity(e.variants.len());
        for variant in &e.variants {
            if !seen.insert(&variant.name.sym) {
                self.err(
                    variant.name.span,
                    &format!("duplicate enum variant `{}`", variant.name.sym),
                );
            }

            let mut fields: HashSet<&swc_atoms::Atom> =
                HashSet::with_capacity(variant.fields.len());
            for field in &variant.fields {
                if field.name.sym == "kind" {
                    self.err(
                        field.name.span,
                        "`kind` is reserved: it is the discriminant field every zts tagged \
                         union carries",
                    );
                }
                if !fields.insert(&field.name.sym) {
                    self.err(
                        field.name.span,
                        &format!("duplicate field `{}` in enum variant", field.name.sym),
                    );
                }
            }
        }
        e.visit_children_with(self);
    }

    fn visit_zts_union_decl(&mut self, u: &ZtsUnionDecl) {
        // Union names are bindings like any other (globalThis shadow ban +
        // __zts prefix reservation apply) — review finding 1/2.
        self.note_global_this_shadow(&u.ident);
        // The parser cannot produce a zero-member union, but ZtsUnionDecl
        // is public API — defend here too (finding 7).
        if u.members.is_empty() {
            self.err(u.span, "union must have at least one member");
        }
        let mut seen: HashSet<&swc_atoms::Wtf8Atom> = HashSet::with_capacity(u.members.len());
        for m in &u.members {
            if !seen.insert(&m.value) {
                self.err(m.span, "duplicate union member");
            }
        }
        u.visit_children_with(self);
    }

    fn visit_zts_newtype_decl(&mut self, n: &ZtsNewtypeDecl) {
        self.note_global_this_shadow(&n.ident);
        n.visit_children_with(self);
    }

    // The TYPE plane of the `__zts` reservation. Deliberately only the
    // `__zts` check and not the whole of `note_global_this_shadow`:
    // `type globalThis = X` cannot shadow the VALUE the absurd helper
    // reaches through, and `not` is reserved in expression positions only
    // (a type or namespace named `not` parses today and rejecting it would
    // be an unrelated break). Both would be false positives here.

    fn visit_ts_type_alias_decl(&mut self, d: &TsTypeAliasDecl) {
        self.note_zts_reserved_ident(&d.id);
        d.visit_children_with(self);
    }

    fn visit_ts_interface_decl(&mut self, d: &TsInterfaceDecl) {
        self.note_zts_reserved_ident(&d.id);
        d.visit_children_with(self);
    }

    fn visit_ts_type_param(&mut self, p: &TsTypeParam) {
        // `<__ztsRange0 extends number>` shadows inside the whole
        // signature and body — same forgery, different scope.
        self.note_zts_reserved_ident(&p.name);
        p.visit_children_with(self);
    }

    fn visit_ts_module_decl(&mut self, d: &TsModuleDecl) {
        // `namespace __ztsX { export type ... }` plus a `__ztsX.Y`
        // reference is the same shadow one indirection away.
        if let TsModuleName::Ident(id) = &d.id {
            self.note_zts_reserved_ident(id);
        }
        d.visit_children_with(self);
    }

    fn visit_ts_enum_decl(&mut self, e: &TsEnumDecl) {
        self.err(
            e.span,
            "TypeScript `enum` is not allowed in zts; zts `enum` (tagged unions) replaces it. \
             This will become zts enums-with-data in a later phase.",
        );
        e.visit_children_with(self);
    }
}

/// Rejects `await` / `yield` directly inside constructs that lower into a
/// synchronous IIFE (match arms, multi-statement if-expressions), which
/// would silently detach them from the enclosing async/generator function.
/// Nested functions reset the check.
struct SuspenderCheck<'a, 'b> {
    checker: &'a mut Checker<'b>,
    what: &'static str,
}

impl Visit for SuspenderCheck<'_, '_> {
    // Shares the Checker's depth budget: arm bodies are walked here before
    // the main guarded walk reaches them, so an unguarded traversal would
    // reintroduce the stack-overflow window this pass exists to close.
    fn visit_expr(&mut self, e: &Expr) {
        self.checker.expr_depth += 1;
        if self.checker.expr_depth > MAX_EXPR_DEPTH {
            if !self.checker.depth_reported {
                self.checker.depth_reported = true;
                self.checker.err(
                    e.span(),
                    &format!("expression nesting exceeds the zts limit of {MAX_EXPR_DEPTH}"),
                );
            }
        } else {
            e.visit_children_with(self);
        }
        self.checker.expr_depth -= 1;
    }

    fn visit_await_expr(&mut self, e: &AwaitExpr) {
        let msg = format!(
            "`await` inside {} is not supported yet (it lowers to a synchronous IIFE)",
            self.what
        );
        self.checker.err(e.span, &msg);
        e.visit_children_with(self);
    }

    fn visit_yield_expr(&mut self, e: &YieldExpr) {
        let msg = format!(
            "`yield` inside {} is not supported yet (it lowers to a synchronous IIFE)",
            self.what
        );
        self.checker.err(e.span, &msg);
        e.visit_children_with(self);
    }

    // Function boundaries make await/yield belong to the inner function;
    // stop descending.
    fn visit_function(&mut self, _: &Function) {}
    fn visit_arrow_expr(&mut self, _: &ArrowExpr) {}
}
