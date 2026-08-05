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
//! Original spans are preserved on every node that has a source
//! counterpart; only glue (the IIFE scaffolding identifiers) is synthetic.
//! Generated identifiers get a fresh `Mark`, and the `hygiene()` pass that
//! runs after lowering renames on collision.

use swc_atoms::atom;
use swc_common::{DUMMY_SP, Spanned, SyntaxContext, util::take::Take};
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
        // distinct.
        let m_ident = private_ident!("__m");
        let k_ident = private_ident!("__k");

        let mut stmts: Vec<Stmt> = Vec::with_capacity(arms.len() + 2);

        // const __k = __m.kind;
        let disc_span = discriminant.span();
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

        for arm in arms {
            stmts.push(self.lower_arm(&m_ident, &k_ident, arm));
        }

        // return __ztsAbsurd(__k);
        let absurd = self.absurd_ident();
        stmts.push(Stmt::Return(ReturnStmt {
            span,
            arg: Some(Box::new(Expr::Call(CallExpr {
                span,
                ctxt: SyntaxContext::empty(),
                callee: Callee::Expr(Box::new(Expr::Ident(absurd))),
                args: vec![ExprOrSpread {
                    spread: None,
                    expr: Box::new(Expr::Ident(Ident {
                        span,
                        ..k_ident.clone()
                    })),
                }],
                type_args: None,
            }))),
        }));

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

    /// One arm: `if (__k === "Variant") { const { ... } = __m; return body; }`
    fn lower_arm(&mut self, m_ident: &Ident, k_ident: &Ident, arm: MatchArm) -> Stmt {
        let MatchArm {
            span,
            variant,
            binding,
            body,
        } = arm;

        // __k === "Variant"
        let test = Expr::Bin(BinExpr {
            span: variant.span,
            op: BinaryOp::EqEqEq,
            left: Box::new(Expr::Ident(Ident {
                span: variant.span,
                ..k_ident.clone()
            })),
            right: Box::new(Expr::Lit(Lit::Str(Str {
                span: variant.span,
                value: variant.sym.clone().into(),
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

        let body_span = body.span();
        cons_stmts.push(Stmt::Return(ReturnStmt {
            span: body_span,
            arg: Some(body),
        }));

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

impl VisitMut for Lower {
    fn visit_mut_expr(&mut self, e: &mut Expr) {
        // Children first: nested matches lower bottom-up.
        e.visit_mut_children_with(self);

        if e.is_match_expr() {
            let Expr::Match(m) = e.take() else {
                unreachable!()
            };
            *e = self.lower_match(m);
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
