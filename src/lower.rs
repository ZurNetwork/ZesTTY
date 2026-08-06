//! Lowering: zts custom nodes → vanilla TS AST.
//!
//! `match` lowers to a parameterized IIFE:
//!
//! ```ts
//! ((__m) => {
//!     const __k = __m.kind;
//!     if (__k === "Circle") { const { radius } = __m; return PI * radius ** 2; }
//!     if (__k === "Square") { const { side } = __m; return side ** 2; }
//!     return __ztsAbsurd(__k);
//! })(shape)
//! ```
//!
//! Shape rationale (all review-gated, all load-bearing):
//! - The discriminant is the IIFE *argument*, so it evaluates in the
//!   enclosing context — `await`/`yield`/`this` inside it keep working.
//! - It must be an IIFE specifically, not a named helper call: TypeScript
//!   contextually types IIFE parameters from the call arguments AND has a
//!   special case that preserves outer control-flow narrowing (of `let`s)
//!   inside immediately-invoked function expressions. A callback passed to
//!   a helper gets neither, which both breaks `__m`'s type and un-narrows
//!   every outer variable in arm bodies.
//! - Testing the alias `__k` still narrows `__m` (aliased discriminant
//!   narrowing, TS 4.4), and passing `__k` — not `__m` — to the keystone
//!   works for BOTH shapes: in an exhaustive union match `__m` itself
//!   narrows to `never` (so `__m.kind` would be TS2339), while a
//!   single-variant type never narrows `__m` at all but its literal `kind`
//!   still narrows to `never`.
//!
//! `__ztsAbsurd(x: never): never` is the exhaustiveness keystone: a missing
//! arm means `__k` does not narrow to `never` and tsc rejects the generated
//! code, naming the missing variant. The absurd call carries the span of
//! the original `match`, so the tsc error maps back to the `.zts` source.
//!
//! The helper throws `globalThis.Error` (never a bare `Error` reference):
//! hygiene is not TS-type-aware, so a bare global reference would make it
//! rename user bindings shadowing `Error` while their type annotations
//! keep pointing at the global — silent meaning change. `globalThis.X`
//! avoids binding-level collision entirely while keeping the thrown value
//! a real Error (stack, instanceof) for crash reporters. The unmatched
//! value rides on a `ztsTag` property — deliberately NOT `kind`, so the
//! thrown object can never impersonate a domain tagged union in a `catch`.
//!
//! Accepted limitation (review-gated, minor): a module-scope
//! `const globalThis = { Error: ... }` shadow makes the helper throw the
//! user's class instead of a real Error. "Real Error" and "shadow-proof
//! against every global" are fundamentally in tension; module code that
//! shadows `globalThis` itself is far outside anything we defend against.
//!
//! Phase 5 pattern arms extend the same shape:
//! - Literal mode (`match (x) { "a" => ..., 1 => ..., }`) drops the `__k`
//!   alias entirely — arms test `__m === <lit>` and the keystone receives
//!   `__m` itself. Equality narrowing eliminates each literal from `__m`'s
//!   union, so an exhaustive literal match narrows `__m` to `never`; there
//!   is no `.kind` access anywhere, so non-object discriminants type-check.
//! - A `_ =>` wildcard arm REPLACES the `return __ztsAbsurd(...)` tail (its
//!   body becomes the fall-through), which deliberately disables the
//!   exhaustiveness keystone — that is the semantics the author asked for.
//!
//! The `?` try operator lowers at STATEMENT level (semantic checking has
//! already confined it to whole-RHS forms inside real function bodies):
//!
//! ```ts
//! const x = f()?;          //  =>  const __t = f();
//!                          //      if (__t.kind === "Err") { return __t; }
//!                          //      const x = __t.value;
//! ```
//!
//! `return e?;` ends in `return __t.value;`; a bare `e?;` just drops the
//! value. tsc enforces the safety property on the shape alone: `.kind`
//! comparison fails on non-Results, and `return __t` fails unless the
//! enclosing return type accepts the `Err` side.
//!
//! Original spans are preserved on every node that has a source
//! counterpart; only glue (the IIFE scaffolding identifiers) is synthetic.
//! Generated identifiers get a fresh `Mark`, and the `hygiene()` pass that
//! runs after lowering renames on collision.

use swc_atoms::atom;
use swc_common::{DUMMY_SP, Span, Spanned, SyntaxContext, util::take::Take};
use swc_ecma_ast::*;
use swc_ecma_utils::private_ident;
use swc_ecma_visit::{VisitMut, VisitMutWith, visit_mut_pass};

pub fn lower() -> impl Pass {
    visit_mut_pass(Lower { absurd: None })
}

struct Lower {
    /// The `__ztsAbsurd` identifier, created on first use; its presence
    /// also signals that the helper declaration must be injected.
    absurd: Option<Ident>,
}

/// Append an arm body as trailing statements: a block body splices its
/// statements straight in (no extra IIFE) and returns the tail; any other
/// body becomes a single `return`.
fn push_body_as_return(stmts: &mut Vec<Stmt>, body: Expr) {
    match body {
        Expr::ZtsExprBlock(block) => {
            stmts.extend(block.stmts);
            let tail_span = block.tail.span();
            stmts.push(Stmt::Return(ReturnStmt {
                span: tail_span,
                arg: Some(block.tail),
            }));
        }
        body => {
            let body_span = body.span();
            stmts.push(Stmt::Return(ReturnStmt {
                span: body_span,
                arg: Some(Box::new(body)),
            }));
        }
    }
}

impl Lower {
    fn absurd_ident(&mut self) -> Ident {
        self.absurd
            .get_or_insert_with(|| private_ident!("__ztsAbsurd"))
            .clone()
    }

    fn lower_match(&mut self, m: MatchExpr) -> Expr {
        let MatchExpr {
            span,
            discriminant,
            arms,
        } = m;

        // `__m`/`__k`, one fresh mark per match so nested matches stay
        // distinct. `__k` exists only in variant mode: literal (and
        // wildcard-only) matches never touch `.kind`, so the alias would be
        // a type error on non-object discriminants.
        let m_ident = private_ident!("__m");
        let variant_mode = arms
            .iter()
            .any(|a| matches!(a.pattern, MatchPat::Variant(..)));
        let k_ident = variant_mode.then(|| private_ident!("__k"));

        let mut stmts: Vec<Stmt> = Vec::with_capacity(arms.len() + 2);

        // const __k = __m.kind;
        let disc_span = discriminant.span();
        if let Some(k_ident) = &k_ident {
            stmts.push(
                VarDecl {
                    span: disc_span,
                    ctxt: SyntaxContext::empty(),
                    kind: VarDeclKind::Const,
                    declare: false,
                    decls: vec![VarDeclarator {
                        span: disc_span,
                        name: Pat::Ident(k_ident.clone().into()),
                        init: Some(Box::new(Expr::Member(MemberExpr {
                            span: disc_span,
                            obj: Box::new(Expr::Ident(m_ident.clone())),
                            prop: MemberProp::Ident(IdentName::new(atom!("kind"), disc_span)),
                        }))),
                        definite: false,
                    }],
                }
                .into(),
            );
        }

        // Semantic checking guarantees a wildcard is the single last arm.
        let mut wildcard_body: Option<Box<Expr>> = None;
        for arm in arms {
            let MatchArm {
                span: arm_span,
                pattern,
                body,
            } = arm;
            match pattern {
                MatchPat::Wildcard(..) => wildcard_body = Some(body),
                MatchPat::Variant(v) => {
                    let k_ident = k_ident.as_ref().expect("variant arm implies variant mode");
                    stmts.push(self.lower_variant_arm(&m_ident, k_ident, arm_span, v, *body));
                }
                MatchPat::Lit(l) => {
                    stmts.push(self.lower_lit_arm(&m_ident, arm_span, l, *body));
                }
            }
        }

        if let Some(body) = wildcard_body {
            // `_ =>` body is the fall-through tail; no keystone.
            push_body_as_return(&mut stmts, *body);
        } else {
            // return __ztsAbsurd(__k); — or __ztsAbsurd(__m) in literal
            // mode, where equality narrowing has run __m itself to never.
            let absurd = self.absurd_ident();
            let keystone = match &k_ident {
                Some(k) => k.clone(),
                None => m_ident.clone(),
            };
            stmts.push(Stmt::Return(ReturnStmt {
                span,
                arg: Some(Box::new(Expr::Call(CallExpr {
                    span,
                    ctxt: SyntaxContext::empty(),
                    callee: Callee::Expr(Box::new(Expr::Ident(absurd))),
                    args: vec![ExprOrSpread {
                        spread: None,
                        expr: Box::new(Expr::Ident(Ident { span, ..keystone })),
                    }],
                    type_args: None,
                }))),
            }));
        }

        // ((__m) => { ... })(<discriminant>)
        let arrow = ArrowExpr {
            span,
            ctxt: SyntaxContext::empty(),
            params: vec![Pat::Ident(m_ident.into())],
            body: Box::new(BlockStmtOrExpr::BlockStmt(BlockStmt {
                span,
                ctxt: SyntaxContext::empty(),
                stmts,
            })),
            is_async: false,
            is_generator: false,
            type_params: None,
            return_type: None,
        };

        Expr::Call(CallExpr {
            span,
            ctxt: SyntaxContext::empty(),
            callee: Callee::Expr(Box::new(Expr::Paren(ParenExpr {
                span,
                expr: Box::new(Expr::Arrow(arrow)),
            }))),
            args: vec![ExprOrSpread {
                spread: None,
                expr: discriminant,
            }],
            type_args: None,
        })
    }

    /// One variant arm:
    /// `if (__k === "Variant") { const { ... } = __m; return body; }`
    fn lower_variant_arm(
        &mut self,
        m_ident: &Ident,
        k_ident: &Ident,
        span: Span,
        pat: MatchVariantPat,
        body: Expr,
    ) -> Stmt {
        let MatchVariantPat { name, binding, .. } = pat;

        // __k === "Variant"
        let test = Expr::Bin(BinExpr {
            span: name.span,
            op: BinaryOp::EqEqEq,
            left: Box::new(Expr::Ident(Ident {
                span: name.span,
                ..k_ident.clone()
            })),
            right: Box::new(Expr::Lit(Lit::Str(Str {
                span: name.span,
                value: name.sym.clone().into(),
                raw: None,
            }))),
        });

        let mut cons_stmts: Vec<Stmt> = Vec::with_capacity(2);

        // const { bindings } = __m;
        if let Some(binding) = binding {
            let binding_span = binding.span;
            cons_stmts.push(
                VarDecl {
                    span: binding_span,
                    ctxt: SyntaxContext::empty(),
                    kind: VarDeclKind::Const,
                    declare: false,
                    decls: vec![VarDeclarator {
                        span: binding_span,
                        name: Pat::Object(binding),
                        init: Some(Box::new(Expr::Ident(Ident {
                            span: binding_span,
                            ..m_ident.clone()
                        }))),
                        definite: false,
                    }],
                }
                .into(),
            );
        }

        push_body_as_return(&mut cons_stmts, body);

        Stmt::If(IfStmt {
            span,
            test: Box::new(test),
            cons: Box::new(Stmt::Block(BlockStmt {
                span,
                ctxt: SyntaxContext::empty(),
                stmts: cons_stmts,
            })),
            alt: None,
        })
    }

    /// One literal arm: `if (__m === <lit>) { return body; }`
    fn lower_lit_arm(&mut self, m_ident: &Ident, span: Span, pat: MatchLitPat, body: Expr) -> Stmt {
        let MatchLitPat {
            span: pat_span,
            lit,
            neg,
        } = pat;

        let lit_expr = if neg {
            Expr::Unary(UnaryExpr {
                span: pat_span,
                op: UnaryOp::Minus,
                arg: Box::new(Expr::Lit(lit)),
            })
        } else {
            Expr::Lit(lit)
        };

        // __m === <lit>
        let test = Expr::Bin(BinExpr {
            span: pat_span,
            op: BinaryOp::EqEqEq,
            left: Box::new(Expr::Ident(Ident {
                span: pat_span,
                ..m_ident.clone()
            })),
            right: Box::new(lit_expr),
        });

        let mut cons_stmts: Vec<Stmt> = Vec::with_capacity(1);
        push_body_as_return(&mut cons_stmts, body);

        Stmt::If(IfStmt {
            span,
            test: Box::new(test),
            cons: Box::new(Stmt::Block(BlockStmt {
                span,
                ctxt: SyntaxContext::empty(),
                stmts: cons_stmts,
            })),
            alt: None,
        })
    }

    /// ```ts
    /// function __ztsAbsurd(x: never): never {
    ///     const e: any = new globalThis.Error("zts: non-exhaustive match");
    ///     e.ztsTag = x;
    ///     throw e;
    /// }
    /// ```
    ///
    /// `globalThis.Error` and `ztsTag` are both deliberate — see module docs.
    fn absurd_decl(&self, absurd: Ident) -> Stmt {
        let x = private_ident!("x");
        let e = private_ident!("e");

        let keyword_ann = |kind: TsKeywordTypeKind| {
            Box::new(TsTypeAnn {
                span: DUMMY_SP,
                type_ann: Box::new(TsType::TsKeywordType(TsKeywordType {
                    span: DUMMY_SP,
                    kind,
                })),
            })
        };

        // const e: any = new globalThis.Error("zts: non-exhaustive match");
        let new_error = Expr::New(NewExpr {
            span: DUMMY_SP,
            ctxt: SyntaxContext::empty(),
            callee: Box::new(Expr::Member(MemberExpr {
                span: DUMMY_SP,
                obj: Box::new(Expr::Ident(Ident::new_no_ctxt(
                    atom!("globalThis"),
                    DUMMY_SP,
                ))),
                prop: MemberProp::Ident(IdentName::new(atom!("Error"), DUMMY_SP)),
            })),
            args: Some(vec![ExprOrSpread {
                spread: None,
                expr: Box::new(Expr::Lit(Lit::Str(Str {
                    span: DUMMY_SP,
                    value: atom!("zts: non-exhaustive match").into(),
                    raw: None,
                }))),
            }]),
            type_args: None,
        });
        let decl_e = Stmt::from(VarDecl {
            span: DUMMY_SP,
            ctxt: SyntaxContext::empty(),
            kind: VarDeclKind::Const,
            declare: false,
            decls: vec![VarDeclarator {
                span: DUMMY_SP,
                name: Pat::Ident(BindingIdent {
                    id: e.clone(),
                    type_ann: Some(keyword_ann(TsKeywordTypeKind::TsAnyKeyword)),
                }),
                init: Some(Box::new(new_error)),
                definite: false,
            }],
        });

        // e.ztsTag = x;
        let set_tag = Stmt::Expr(ExprStmt {
            span: DUMMY_SP,
            expr: Box::new(Expr::Assign(AssignExpr {
                span: DUMMY_SP,
                op: AssignOp::Assign,
                left: AssignTarget::Simple(SimpleAssignTarget::Member(MemberExpr {
                    span: DUMMY_SP,
                    obj: Box::new(Expr::Ident(e.clone())),
                    prop: MemberProp::Ident(IdentName::new(atom!("ztsTag"), DUMMY_SP)),
                })),
                right: Box::new(Expr::Ident(x.clone())),
            })),
        });

        let throw = Stmt::Throw(ThrowStmt {
            span: DUMMY_SP,
            arg: Box::new(Expr::Ident(e)),
        });

        Stmt::Decl(Decl::Fn(FnDecl {
            ident: absurd,
            declare: false,
            function: Box::new(Function {
                params: vec![Param {
                    span: DUMMY_SP,
                    decorators: Vec::new(),
                    pat: Pat::Ident(BindingIdent {
                        id: x,
                        type_ann: Some(keyword_ann(TsKeywordTypeKind::TsNeverKeyword)),
                    }),
                }],
                decorators: Vec::new(),
                span: DUMMY_SP,
                ctxt: SyntaxContext::empty(),
                body: Some(BlockStmt {
                    span: DUMMY_SP,
                    ctxt: SyntaxContext::empty(),
                    stmts: vec![decl_e, set_tag, throw],
                }),
                is_generator: false,
                is_async: false,
                type_params: None,
                return_type: Some(keyword_ann(TsKeywordTypeKind::TsNeverKeyword)),
            }),
        }))
    }
}

/// Index right after the directive prologue and any leading imports: the
/// helper must not displace `"use strict"` / `"use client"` / `"use server"`
/// (a directive is only a directive while it stays in the prologue), and
/// reads better below the imports.
fn helper_insert_index(items: &[ModuleItem]) -> usize {
    let mut idx = 0;
    for item in items {
        match item {
            ModuleItem::Stmt(Stmt::Expr(ExprStmt { expr, .. }))
                if matches!(&**expr, Expr::Lit(Lit::Str(..))) =>
            {
                idx += 1;
            }
            ModuleItem::ModuleDecl(ModuleDecl::Import(..)) => idx += 1,
            _ => break,
        }
    }
    idx
}

fn script_insert_index(stmts: &[Stmt]) -> usize {
    let mut idx = 0;
    for stmt in stmts {
        match stmt {
            Stmt::Expr(ExprStmt { expr, .. }) if matches!(&**expr, Expr::Lit(Lit::Str(..))) => {
                idx += 1;
            }
            _ => break,
        }
    }
    idx
}

/// Does any block in this if-expression chain carry statements? Statement-
/// free chains lower to ternaries; anything else needs the IIFE.
/// Iterative: chains can be long, and recursion here would undercut the
/// semantic depth guard.
pub(crate) fn if_chain_has_stmts(i: &ZtsIfExpr) -> bool {
    let mut link = i;
    loop {
        if !link.cons.stmts.is_empty() {
            return true;
        }
        match &link.alt {
            ZtsIfAlt::Block(b) => return !b.stmts.is_empty(),
            ZtsIfAlt::If(next) => link = next,
        }
    }
}

/// `test ? consTail : (…)` — for statement-free chains. Iterative: collect
/// the links, then fold the ternary from the tail end.
fn build_ternary(i: ZtsIfExpr) -> Expr {
    let mut links: Vec<(Span, Box<Expr>, Box<Expr>)> = Vec::new();
    let mut link = i;
    let final_alt = loop {
        links.push((link.span, link.test, link.cons.tail));
        match link.alt {
            ZtsIfAlt::Block(b) => break b.tail,
            ZtsIfAlt::If(next) => link = *next,
        }
    };

    let mut alt = final_alt;
    for (span, test, cons) in links.into_iter().rev() {
        alt = Box::new(Expr::Cond(CondExpr {
            span,
            test,
            cons,
            alt,
        }));
    }
    *alt
}

/// `{ …stmts; return tail; }`
fn block_with_return(b: ZtsExprBlock) -> BlockStmt {
    let mut stmts = b.stmts;
    let tail_span = b.tail.span();
    stmts.push(Stmt::Return(ReturnStmt {
        span: tail_span,
        arg: Some(b.tail),
    }));
    BlockStmt {
        span: b.span,
        ctxt: SyntaxContext::empty(),
        stmts,
    }
}

/// `(() => { if (t1) { …; return v1 } …; …else stmts; return elseTail; })()`
fn build_if_iife(i: ZtsIfExpr) -> Expr {
    let span = i.span;
    let mut stmts: Vec<Stmt> = Vec::new();

    let mut link = i;
    loop {
        stmts.push(Stmt::If(IfStmt {
            span: link.cons.span,
            test: link.test,
            cons: Box::new(Stmt::Block(block_with_return(link.cons))),
            alt: None,
        }));
        match link.alt {
            ZtsIfAlt::If(next) => link = *next,
            ZtsIfAlt::Block(b) => {
                let else_block = block_with_return(b);
                stmts.extend(else_block.stmts);
                break;
            }
        }
    }

    let arrow = ArrowExpr {
        span,
        ctxt: SyntaxContext::empty(),
        params: Vec::new(),
        body: Box::new(BlockStmtOrExpr::BlockStmt(BlockStmt {
            span,
            ctxt: SyntaxContext::empty(),
            stmts,
        })),
        is_async: false,
        is_generator: false,
        type_params: None,
        return_type: None,
    };

    Expr::Call(CallExpr {
        span,
        ctxt: SyntaxContext::empty(),
        callee: Callee::Expr(Box::new(Expr::Paren(ParenExpr {
            span,
            expr: Box::new(Expr::Arrow(arrow)),
        }))),
        args: Vec::new(),
        type_args: None,
    })
}

/// Does this statement carry a sanctioned top-level `?`? (Mirror of the
/// semantic pass's shape rule; anything else was already rejected.)
fn stmt_has_top_try(s: &Stmt) -> bool {
    match s {
        Stmt::Decl(Decl::Var(v)) if v.decls.len() == 1 => {
            matches!(v.decls[0].init.as_deref(), Some(Expr::ZtsTry(..)))
        }
        Stmt::Return(r) => matches!(r.arg.as_deref(), Some(Expr::ZtsTry(..))),
        Stmt::Expr(e) => matches!(&*e.expr, Expr::ZtsTry(..)),
        _ => false,
    }
}

/// `const __t = <operand>; if (__t.kind === "Err") { return __t; }`
/// Returns the prelude plus the `__t` ident (fresh mark per use).
fn try_prelude(t: ZtsTryExpr) -> (Vec<Stmt>, Ident) {
    let ZtsTryExpr { span, expr } = t;
    let t_ident = private_ident!("__t");
    let operand_span = expr.span();

    let decl: Stmt = VarDecl {
        span: operand_span,
        ctxt: SyntaxContext::empty(),
        kind: VarDeclKind::Const,
        declare: false,
        decls: vec![VarDeclarator {
            span: operand_span,
            name: Pat::Ident(t_ident.clone().into()),
            init: Some(expr),
            definite: false,
        }],
    }
    .into();

    let test = Expr::Bin(BinExpr {
        span,
        op: BinaryOp::EqEqEq,
        left: Box::new(Expr::Member(MemberExpr {
            span,
            obj: Box::new(Expr::Ident(t_ident.clone())),
            prop: MemberProp::Ident(IdentName::new(atom!("kind"), span)),
        })),
        right: Box::new(Expr::Lit(Lit::Str(Str {
            span,
            value: atom!("Err").into(),
            raw: None,
        }))),
    });
    let guard = Stmt::If(IfStmt {
        span,
        test: Box::new(test),
        cons: Box::new(Stmt::Block(BlockStmt {
            span,
            ctxt: SyntaxContext::empty(),
            stmts: vec![Stmt::Return(ReturnStmt {
                span,
                arg: Some(Box::new(Expr::Ident(t_ident.clone()))),
            })],
        })),
        alt: None,
    });

    (vec![decl, guard], t_ident)
}

/// `__t.value`
fn try_value(t_ident: &Ident, span: Span) -> Expr {
    Expr::Member(MemberExpr {
        span,
        obj: Box::new(Expr::Ident(t_ident.clone())),
        prop: MemberProp::Ident(IdentName::new(atom!("value"), span)),
    })
}

/// Defense + correctness for `?` in single-statement slots (`if (c) g()?;`,
/// loop bodies, labels): these `Stmt`s are not part of a `Vec<Stmt>`, so the
/// statement-list expansion never sees them. Expand inside a block — the
/// early return keeps its meaning. Mirrors lower_enums' single-statement
/// defense, which enumerates the same slots.
fn wrap_single_stmt_try(stmt: &mut Stmt) {
    if !stmt_has_top_try(stmt) {
        return;
    }
    let original = std::mem::replace(stmt, Stmt::Empty(EmptyStmt { span: DUMMY_SP }));
    let span = original.span();
    let mut stmts = Vec::with_capacity(3);
    expand_try_stmt(original, &mut stmts);
    *stmt = Stmt::Block(BlockStmt {
        span,
        ctxt: SyntaxContext::empty(),
        stmts,
    });
}

/// Expand one statement carrying a sanctioned top-level `?` into `out`.
/// Statements without one are passed through untouched.
fn expand_try_stmt(stmt: Stmt, out: &mut Vec<Stmt>) {
    match stmt {
        Stmt::Decl(Decl::Var(mut v))
            if v.decls.len() == 1
                && matches!(v.decls[0].init.as_deref(), Some(Expr::ZtsTry(..))) =>
        {
            let Some(Expr::ZtsTry(t)) = v.decls[0].init.take().map(|b| *b) else {
                unreachable!()
            };
            let try_span = t.span;
            let (prelude, t_ident) = try_prelude(t);
            out.extend(prelude);
            v.decls[0].init = Some(Box::new(try_value(&t_ident, try_span)));
            out.push(Stmt::Decl(Decl::Var(v)));
        }
        Stmt::Return(mut r) if matches!(r.arg.as_deref(), Some(Expr::ZtsTry(..))) => {
            let Some(Expr::ZtsTry(t)) = r.arg.take().map(|b| *b) else {
                unreachable!()
            };
            let try_span = t.span;
            let (prelude, t_ident) = try_prelude(t);
            out.extend(prelude);
            r.arg = Some(Box::new(try_value(&t_ident, try_span)));
            out.push(Stmt::Return(r));
        }
        Stmt::Expr(e) if matches!(&*e.expr, Expr::ZtsTry(..)) => {
            let Expr::ZtsTry(t) = *e.expr else {
                unreachable!()
            };
            let (prelude, _) = try_prelude(t);
            out.extend(prelude);
        }
        other => out.push(other),
    }
}

impl VisitMut for Lower {
    fn visit_mut_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        // Children first: matches/if-exprs inside try operands (and
        // everything else) are already vanilla TS by the time the
        // statement expands.
        stmts.visit_mut_children_with(self);

        if !stmts.iter().any(stmt_has_top_try) {
            return;
        }

        let mut out: Vec<Stmt> = Vec::with_capacity(stmts.len() + 2);
        for stmt in stmts.drain(..) {
            expand_try_stmt(stmt, &mut out);
        }
        *stmts = out;
    }

    // `?` in single-statement slots: same nine slots lower_enums defends.

    fn visit_mut_if_stmt(&mut self, s: &mut IfStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_try(&mut s.cons);
        if let Some(alt) = &mut s.alt {
            wrap_single_stmt_try(alt);
        }
    }

    fn visit_mut_while_stmt(&mut self, s: &mut WhileStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_try(&mut s.body);
    }

    fn visit_mut_do_while_stmt(&mut self, s: &mut DoWhileStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_try(&mut s.body);
    }

    fn visit_mut_for_stmt(&mut self, s: &mut ForStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_try(&mut s.body);
    }

    fn visit_mut_for_in_stmt(&mut self, s: &mut ForInStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_try(&mut s.body);
    }

    fn visit_mut_for_of_stmt(&mut self, s: &mut ForOfStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_try(&mut s.body);
    }

    fn visit_mut_labeled_stmt(&mut self, s: &mut LabeledStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_try(&mut s.body);
    }

    fn visit_mut_with_stmt(&mut self, s: &mut WithStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_try(&mut s.body);
    }

    fn visit_mut_expr(&mut self, e: &mut Expr) {
        // Children first: nested constructs lower bottom-up. Note that
        // else-if links and arm-body blocks are NOT `Expr` children — the
        // whole chain is consumed at once below, after its tests, tails,
        // and statements have been visited.
        e.visit_mut_children_with(self);

        match e {
            Expr::Match(..) => {
                let Expr::Match(m) = e.take() else {
                    unreachable!()
                };
                *e = self.lower_match(m);
            }
            Expr::ZtsIf(..) => {
                let Expr::ZtsIf(i) = e.take() else {
                    unreachable!()
                };
                *e = if if_chain_has_stmts(&i) {
                    build_if_iife(i)
                } else {
                    build_ternary(i)
                };
            }
            _ => {}
        }
    }

    fn visit_mut_module(&mut self, module: &mut Module) {
        module.visit_mut_children_with(self);

        if let Some(absurd) = self.absurd.take() {
            let idx = helper_insert_index(&module.body);
            module
                .body
                .insert(idx, ModuleItem::Stmt(self.absurd_decl(absurd)));
        }
    }

    fn visit_mut_script(&mut self, script: &mut Script) {
        script.visit_mut_children_with(self);

        if let Some(absurd) = self.absurd.take() {
            let idx = script_insert_index(&script.body);
            script.body.insert(idx, self.absurd_decl(absurd));
        }
    }
}
