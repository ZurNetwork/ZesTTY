//! Lowering: zts custom nodes → vanilla TS AST.
//!
//! `match` lowers to an IIFE so it stays an expression:
//!
//! ```ts
//! (() => {
//!     const __m = shape;
//!     if (__m.kind === "Circle") { const { radius } = __m; return PI * radius ** 2; }
//!     if (__m.kind === "Square") { const { side } = __m; return side ** 2; }
//!     return __ztsAbsurd(__m);
//! })()
//! ```
//!
//! `__ztsAbsurd(x: never): never` is the exhaustiveness keystone: a missing
//! arm means `__m` does not narrow to `never` and tsc rejects the generated
//! code. The absurd call carries the span of the original `match`, so the
//! tsc error maps back to the `.zts` source.
//!
//! Original spans are preserved on every node that has a source
//! counterpart; only glue (the IIFE scaffolding identifiers) is synthetic.
//! Generated identifiers get a fresh `Mark`, and the `hygiene()` pass that
//! runs after lowering renames them if user code collides.

use swc_atoms::atom;
use swc_common::{DUMMY_SP, Mark, Spanned, SyntaxContext, util::take::Take};
use swc_ecma_ast::*;
use swc_ecma_utils::private_ident;
use swc_ecma_visit::{VisitMut, VisitMutWith, visit_mut_pass};

/// `unresolved_mark` must be the mark the `resolver` pass ran with, so
/// generated references to globals (`Error`) resolve as unresolved.
pub fn lower(unresolved_mark: Mark) -> impl Pass {
    visit_mut_pass(Lower {
        unresolved_mark,
        absurd: None,
    })
}

struct Lower {
    unresolved_mark: Mark,
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

        // `__m`, one fresh mark per match so nested matches stay distinct.
        let m_ident = private_ident!("__m");

        let mut stmts: Vec<Stmt> = Vec::with_capacity(arms.len() + 2);

        // const __m = <discriminant>;
        stmts.push(
            VarDecl {
                span: discriminant.span(),
                ctxt: SyntaxContext::empty(),
                kind: VarDeclKind::Const,
                declare: false,
                decls: vec![VarDeclarator {
                    span: discriminant.span(),
                    name: Pat::Ident(m_ident.clone().into()),
                    init: Some(discriminant),
                    definite: false,
                }],
            }
            .into(),
        );

        for arm in arms {
            stmts.push(self.lower_arm(&m_ident, arm));
        }

        // return __ztsAbsurd(__m);
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
                        ..m_ident.clone()
                    })),
                }],
                type_args: None,
            }))),
        }));

        // (() => { ... })()
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

    /// One arm: `if (__m.kind === "Variant") { const { ... } = __m; return body; }`
    fn lower_arm(&mut self, m_ident: &Ident, arm: MatchArm) -> Stmt {
        let MatchArm {
            span,
            variant,
            binding,
            body,
        } = arm;

        // __m.kind === "Variant"
        let test = Expr::Bin(BinExpr {
            span: variant.span,
            op: BinaryOp::EqEqEq,
            left: Box::new(Expr::Member(MemberExpr {
                span: variant.span,
                obj: Box::new(Expr::Ident(Ident {
                    span: variant.span,
                    ..m_ident.clone()
                })),
                prop: MemberProp::Ident(IdentName::new(atom!("kind"), variant.span)),
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

    /// `function __ztsAbsurd(x: never): never { throw new Error(...); }`
    fn absurd_decl(&self, absurd: Ident) -> ModuleItem {
        let x = private_ident!("x");

        let never_ann = || {
            Box::new(TsTypeAnn {
                span: DUMMY_SP,
                type_ann: Box::new(TsType::TsKeywordType(TsKeywordType {
                    span: DUMMY_SP,
                    kind: TsKeywordTypeKind::TsNeverKeyword,
                })),
            })
        };

        let throw = Stmt::Throw(ThrowStmt {
            span: DUMMY_SP,
            arg: Box::new(Expr::New(NewExpr {
                span: DUMMY_SP,
                ctxt: SyntaxContext::empty(),
                callee: Box::new(Expr::Ident(Ident::new(
                    atom!("Error"),
                    DUMMY_SP,
                    SyntaxContext::empty().apply_mark(self.unresolved_mark),
                ))),
                args: Some(vec![ExprOrSpread {
                    spread: None,
                    expr: Box::new(Expr::Lit(Lit::Str(Str {
                        span: DUMMY_SP,
                        value: atom!("zts: non-exhaustive match").into(),
                        raw: None,
                    }))),
                }]),
                type_args: None,
            })),
        });

        ModuleItem::Stmt(Stmt::Decl(Decl::Fn(FnDecl {
            ident: absurd,
            declare: false,
            function: Box::new(Function {
                params: vec![Param {
                    span: DUMMY_SP,
                    decorators: Vec::new(),
                    pat: Pat::Ident(BindingIdent {
                        id: x,
                        type_ann: Some(never_ann()),
                    }),
                }],
                decorators: Vec::new(),
                span: DUMMY_SP,
                ctxt: SyntaxContext::empty(),
                body: Some(BlockStmt {
                    span: DUMMY_SP,
                    ctxt: SyntaxContext::empty(),
                    stmts: vec![throw],
                }),
                is_generator: false,
                is_async: false,
                type_params: None,
                return_type: Some(never_ann()),
            }),
        })))
    }
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
            module.body.insert(0, self.absurd_decl(absurd));
        }
    }
}
