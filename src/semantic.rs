//! Semantic checks that must run before lowering.
//!
//! Scope is deliberately small: everything that can be enforced by shaping
//! the generated TS is left to tsc (exhaustiveness above all). Here we only
//! reject zts constructs that are syntactically valid but outside the
//! locked Phase 1 grammar, with spans pointing at the original `.zts`.

use anyhow::{Result, bail};
use swc_common::{Spanned, errors::Handler};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

pub fn check(module: &Module, handler: &Handler) -> Result<()> {
    let mut checker = Checker { handler, errors: 0 };
    module.visit_with(&mut checker);
    if checker.errors > 0 {
        bail!("{} semantic error(s)", checker.errors);
    }
    Ok(())
}

struct Checker<'a> {
    handler: &'a Handler,
    errors: usize,
}

impl Checker<'_> {
    fn err(&mut self, span: swc_common::Span, msg: &str) {
        self.handler.struct_span_err(span, msg).emit();
        self.errors += 1;
    }

    fn check_match(&mut self, m: &MatchExpr) {
        let mut seen: Vec<&swc_atoms::Atom> = Vec::new();
        for arm in &m.arms {
            if seen.contains(&&arm.variant.sym) {
                self.err(
                    arm.variant.span,
                    &format!("duplicate match arm for variant `{}`", arm.variant.sym),
                );
            } else {
                seen.push(&arm.variant.sym);
            }

            if let Some(binding) = &arm.binding {
                self.check_binding(binding);
            }

            let mut suspender = SuspenderCheck { checker: self };
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

impl Visit for Checker<'_> {
    fn visit_match_expr(&mut self, m: &MatchExpr) {
        self.check_match(m);
        m.visit_children_with(self);
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

/// Rejects `await` / `yield` directly inside a match arm body: arms lower
/// into a synchronous IIFE, which would silently detach them from the
/// enclosing async/generator function. Nested functions reset the check.
struct SuspenderCheck<'a, 'b> {
    checker: &'a mut Checker<'b>,
}

impl Visit for SuspenderCheck<'_, '_> {
    fn visit_await_expr(&mut self, e: &AwaitExpr) {
        self.checker.err(
            e.span,
            "`await` inside a match arm is not supported yet (arms lower to a synchronous IIFE)",
        );
        e.visit_children_with(self);
    }

    fn visit_yield_expr(&mut self, e: &YieldExpr) {
        self.checker.err(
            e.span,
            "`yield` inside a match arm is not supported yet (arms lower to a synchronous IIFE)",
        );
        e.visit_children_with(self);
    }

    // Function boundaries make await/yield belong to the inner function;
    // stop descending.
    fn visit_function(&mut self, _: &Function) {}
    fn visit_arrow_expr(&mut self, _: &ArrowExpr) {}
}
