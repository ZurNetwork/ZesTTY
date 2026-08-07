//! Semantic checks that must run before lowering.
//!
//! Scope is deliberately small: everything that can be enforced by shaping
//! the generated TS is left to tsc (exhaustiveness above all). Here we only
//! reject zts constructs that are syntactically valid but outside the
//! locked Phase 1 grammar, with spans pointing at the original `.zts`.

use std::collections::HashSet;

use swc_common::{Span, Spanned, errors::Handler};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

/// Deeper than this and the recursive passes (lowering, hygiene, codegen)
/// risk a stack overflow — which is an uncatchable SIGABRT, unacceptable
/// once the compiler runs inside a Node/Vite process. Reject early with a
/// real diagnostic instead.
const MAX_EXPR_DEPTH: usize = 2048;

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

    fn note_global_this_shadow(&mut self, ident: &Ident) {
        if ident.sym == "globalThis" {
            self.global_this_decls.push(ident.span);
        }
        // The `__zts` prefix is documented as generated-code-only; a user
        // binding named `__ztsValue` etc. would collide with pre-resolver
        // lowerings that hygiene cannot protect (security-gate F9).
        if ident.sym.starts_with("__zts") {
            self.err(
                ident.span,
                "identifiers starting with `__zts` are reserved for zts-generated code",
            );
        }
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
        let mut seen_variants: HashSet<&swc_atoms::Atom> = HashSet::new();
        let mut seen_lits: HashSet<String> = HashSet::new();
        let mut mode: Option<&'static str> = None;
        let mut wildcard_seen = false;

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
    /// The trait rules that need the whole statement list (Phase 6):
    /// - orphan rule: `impl X for T` requires a zts enum `T` in the SAME
    ///   list (v1: enum impls only);
    /// - `export impl` is meaningless (the factory const owns the export);
    /// - every impl seen here is sanctioned — impls anywhere else are in a
    ///   single-statement slot and get rejected by `visit_zts_impl_decl`.
    fn check_zts_impls(&mut self, decls: &[(&Decl, bool)]) {
        let enums: HashSet<&swc_atoms::Atom> = decls
            .iter()
            .filter_map(|(d, _)| match d {
                Decl::ZtsEnum(e) => Some(&e.ident.sym),
                _ => None,
            })
            .collect();
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
            if !enums.contains(&i.for_ident.sym) {
                self.err(
                    i.for_ident.span,
                    &format!(
                        "impl target `{}` is not a zts enum declared in this scope (the orphan \
                         rule: an impl lives in the module that declares its type; v1 supports \
                         enum impls only)",
                        i.for_ident.sym
                    ),
                );
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
        self.check_zts_impls(&decls);
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
        self.check_zts_impls(&decls);
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
