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
    /// Identity of the one sanctioned `?` of the statement being visited
    /// (whole-RHS forms only). Pointer identity — a flag would be burned
    /// by unrelated statements nested in destructuring defaults, and would
    /// blame the wrong `?` in `const { a = h()? } = g()?;`.
    allow_try: Option<*const ZtsTryExpr>,
}

impl Checker<'_> {
    fn err(&mut self, span: Span, msg: &str) {
        self.handler.struct_span_err(span, msg).emit();
        self.errors += 1;
    }

    fn note_global_this_shadow(&mut self, ident: &Ident) {
        if ident.sym == "globalThis" {
            self.global_this_decls.push(ident.span);
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
                            format!("bi:{}{}", if l.neg { "-" } else { "" }, b.value)
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
                _ => continue,
            };
            if !seen.insert(&ident.sym) {
                self.err(
                    ident.span,
                    &format!(
                        "duplicate zts declaration `{}` in this scope (each enum/newtype \
                         expands to a type alias AND a const of that name)",
                        ident.sym
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
        self.check_zts_decl_names(items.iter().filter_map(|item| match item {
            ModuleItem::Stmt(Stmt::Decl(d)) => Some(d),
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl { decl, .. })) => Some(decl),
            _ => None,
        }));
        items.visit_children_with(self);
    }

    fn visit_stmts(&mut self, stmts: &[Stmt]) {
        self.check_zts_decl_names(stmts.iter().filter_map(|s| match s {
            Stmt::Decl(d) => Some(d),
            _ => None,
        }));
        stmts.visit_children_with(self);
    }

    fn visit_stmt(&mut self, s: &Stmt) {
        let saved = self.allow_try;
        if let Some(t) = stmt_top_try(s) {
            if self.fn_depth == 0 {
                self.err(
                    t.span,
                    "`?` needs an enclosing function to return the `Err` from; it cannot be \
                     used at module top level",
                );
            } else if self.zts_iife_depth > 0 {
                self.err(
                    t.span,
                    "`?` inside a match arm or if-expression block is not supported in v1: \
                     the construct lowers to an IIFE, which would hijack the early return",
                );
            }
            // Permit this exact node either way — the context error above
            // already covers it; the generic shape error would be noise.
            self.allow_try = Some(t as *const ZtsTryExpr);
        }
        s.visit_children_with(self);
        self.allow_try = saved;
    }

    fn visit_zts_try_expr(&mut self, t: &ZtsTryExpr) {
        if self
            .allow_try
            .is_some_and(|allowed| std::ptr::eq(t, allowed))
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
        f.visit_children_with(self);
        self.zts_iife_depth = saved;
        self.fn_depth -= 1;
    }

    fn visit_arrow_expr(&mut self, a: &ArrowExpr) {
        self.fn_depth += 1;
        let saved = std::mem::replace(&mut self.zts_iife_depth, 0);
        a.visit_children_with(self);
        self.zts_iife_depth = saved;
        self.fn_depth -= 1;
    }

    fn visit_static_block(&mut self, b: &StaticBlock) {
        // A static block is not a function: `return` is illegal inside it,
        // so `?` has nothing to return from. Reset the context so the
        // "needs an enclosing function" error fires.
        let saved_fn = std::mem::replace(&mut self.fn_depth, 0);
        let saved_iife = std::mem::replace(&mut self.zts_iife_depth, 0);
        b.visit_children_with(self);
        self.fn_depth = saved_fn;
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
