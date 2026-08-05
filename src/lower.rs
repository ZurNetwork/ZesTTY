//! Lowering: zts custom nodes → vanilla TS AST.
//!
//! `match` lowers to a call of the generic helper `__ztsMatch`:
//!
//! ```ts
//! function __ztsMatch<T, R>(v: T, f: (v: T) => R): R { return f(v); }
//!
//! __ztsMatch(shape, (__m) => {
//!     const __k = __m.kind;
//!     if (__k === "Circle") { const { radius } = __m; return PI * radius ** 2; }
//!     if (__k === "Square") { const { side } = __m; return side ** 2; }
//!     return __ztsAbsurd(__k);
//! })
//! ```
//!
//! The `__k` alias is load-bearing: testing `__k` still narrows `__m`
//! (aliased discriminant narrowing, TS 4.4), and passing `__k` — not
//! `__m` — to the keystone works for BOTH shapes: in an exhaustive union
//! match `__m` itself narrows to `never`, so `__m.kind` would be a TS2339;
//! and a single-variant (non-union) type never narrows `__m` at all, while
//! its literal `kind` still narrows to `never`.
//!
//! Why not a plain IIFE? Three constraints meet here:
//! - the discriminant must be evaluated in the *enclosing* context, so
//!   `await`/`yield`/`this` inside it keep working → it must be an
//!   argument, not a statement inside the arrow;
//! - `__m` must get the discriminant's type for narrowing to work, and an
//!   IIFE parameter is implicitly `any` → the arrow needs a contextual
//!   type, which the generic helper provides;
//! - the whole thing must stay an expression.
//!
//! `__ztsAbsurd(x: never): never` is the exhaustiveness keystone: a missing
//! arm means `__m.kind` does not narrow to `never` and tsc rejects the
//! generated code. Passing `__m.kind` (not `__m`) matters twice over:
//! single-variant types never narrow the whole object to `never`, and the
//! tsc error message then names the missing variant(s). The absurd call
//! carries the span of the original `match`, so the tsc error maps back to
//! the `.zts` source.
//!
//! The helper deliberately throws a plain object, not `new Error(...)`: a
//! reference to the global `Error` would make `hygiene()` rename user
//! bindings that shadow `Error`, and hygiene is not TS-type-aware, so type
//! annotations naming the shadowed class would silently change meaning.
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
    visit_mut_pass(Lower {
        absurd: None,
        match_helper: None,
    })
}

struct Lower {
    /// The `__ztsAbsurd` identifier, created on first use; its presence
    /// also signals that the helper declaration must be injected.
    absurd: Option<Ident>,
    /// Same for `__ztsMatch`.
    match_helper: Option<Ident>,
}

impl Lower {
    fn absurd_ident(&mut self) -> Ident {
        self.absurd
            .get_or_insert_with(|| private_ident!("__ztsAbsurd"))
            .clone()
    }

    fn match_helper_ident(&mut self) -> Ident {
        self.match_helper
            .get_or_insert_with(|| private_ident!("__ztsMatch"))
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

        // __ztsMatch(<discriminant>, (__m) => { ... })
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

        let helper = self.match_helper_ident();
        Expr::Call(CallExpr {
            span,
            ctxt: SyntaxContext::empty(),
            callee: Callee::Expr(Box::new(Expr::Ident(helper))),
            args: vec![
                ExprOrSpread {
                    spread: None,
                    expr: discriminant,
                },
                ExprOrSpread {
                    spread: None,
                    expr: Box::new(Expr::Arrow(arrow)),
                },
            ],
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

    /// `function __ztsAbsurd(x: never): never { throw {...}; }`
    ///
    /// Throws a plain object on purpose — see the module docs.
    fn absurd_decl(&self, absurd: Ident) -> Stmt {
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

        let str_prop = |key: &str, value: swc_atoms::Atom| {
            PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                key: PropName::Ident(IdentName::new(key.into(), DUMMY_SP)),
                value: Box::new(Expr::Lit(Lit::Str(Str {
                    span: DUMMY_SP,
                    value: value.into(),
                    raw: None,
                }))),
            })))
        };

        // throw { name: "...", message: "...", kind: x };
        let throw = Stmt::Throw(ThrowStmt {
            span: DUMMY_SP,
            arg: Box::new(Expr::Object(ObjectLit {
                span: DUMMY_SP,
                props: vec![
                    str_prop("name", atom!("ZtsNonExhaustiveMatch")),
                    str_prop("message", atom!("zts: non-exhaustive match")),
                    PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                        key: PropName::Ident(IdentName::new(atom!("kind"), DUMMY_SP)),
                        value: Box::new(Expr::Ident(x.clone())),
                    }))),
                ],
            })),
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
        }))
    }
}

impl Lower {
    /// `function __ztsMatch<T, R>(v: T, f: (v: T) => R): R { return f(v); }`
    ///
    /// The generic signature is what makes narrowing work: the arrow passed
    /// as `f` gets its parameter contextually typed to the discriminant's
    /// type, which a bare IIFE parameter (implicitly `any`) would not.
    fn match_helper_decl(&self, helper: Ident) -> Stmt {
        let t = Ident::new_no_ctxt(atom!("T"), DUMMY_SP);
        let r = Ident::new_no_ctxt(atom!("R"), DUMMY_SP);
        let v = private_ident!("v");
        let f = private_ident!("f");

        let type_ref = |name: &Ident| {
            Box::new(TsType::TsTypeRef(TsTypeRef {
                span: DUMMY_SP,
                type_name: TsEntityName::Ident(name.clone()),
                type_params: None,
            }))
        };
        let type_ann = |ty: Box<TsType>| {
            Some(Box::new(TsTypeAnn {
                span: DUMMY_SP,
                type_ann: ty,
            }))
        };
        let type_param = |name: &Ident| TsTypeParam {
            span: DUMMY_SP,
            name: name.clone(),
            is_in: false,
            is_out: false,
            is_const: false,
            constraint: None,
            default: None,
        };

        // (v: T) => R
        let f_type = Box::new(TsType::TsFnOrConstructorType(
            TsFnOrConstructorType::TsFnType(TsFnType {
                span: DUMMY_SP,
                params: vec![TsFnParam::Ident(BindingIdent {
                    id: Ident::new_no_ctxt(atom!("v"), DUMMY_SP),
                    type_ann: type_ann(type_ref(&t)),
                })],
                type_params: None,
                type_ann: Box::new(TsTypeAnn {
                    span: DUMMY_SP,
                    type_ann: type_ref(&r),
                }),
            }),
        ));

        // return f(v);
        let body = Stmt::Return(ReturnStmt {
            span: DUMMY_SP,
            arg: Some(Box::new(Expr::Call(CallExpr {
                span: DUMMY_SP,
                ctxt: SyntaxContext::empty(),
                callee: Callee::Expr(Box::new(Expr::Ident(f.clone()))),
                args: vec![ExprOrSpread {
                    spread: None,
                    expr: Box::new(Expr::Ident(v.clone())),
                }],
                type_args: None,
            }))),
        });

        let param = |id: Ident, ty: Box<TsType>| Param {
            span: DUMMY_SP,
            decorators: Vec::new(),
            pat: Pat::Ident(BindingIdent {
                id,
                type_ann: type_ann(ty),
            }),
        };

        Stmt::Decl(Decl::Fn(FnDecl {
            ident: helper,
            declare: false,
            function: Box::new(Function {
                params: vec![param(v, type_ref(&t)), param(f, f_type)],
                decorators: Vec::new(),
                span: DUMMY_SP,
                ctxt: SyntaxContext::empty(),
                body: Some(BlockStmt {
                    span: DUMMY_SP,
                    ctxt: SyntaxContext::empty(),
                    stmts: vec![body],
                }),
                is_generator: false,
                is_async: false,
                type_params: Some(Box::new(TsTypeParamDecl {
                    span: DUMMY_SP,
                    params: vec![type_param(&t), type_param(&r)],
                })),
                return_type: Some(Box::new(TsTypeAnn {
                    span: DUMMY_SP,
                    type_ann: type_ref(&r),
                })),
            }),
        }))
    }

    /// Both helper declarations, in injection order, draining the state.
    fn take_helper_decls(&mut self) -> Vec<Stmt> {
        let mut decls = Vec::new();
        if let Some(absurd) = self.absurd.take() {
            decls.push(self.absurd_decl(absurd));
        }
        if let Some(helper) = self.match_helper.take() {
            decls.push(self.match_helper_decl(helper));
        }
        decls
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

        let mut idx = helper_insert_index(&module.body);
        for decl in self.take_helper_decls() {
            module.body.insert(idx, ModuleItem::Stmt(decl));
            idx += 1;
        }
    }

    fn visit_mut_script(&mut self, script: &mut Script) {
        script.visit_mut_children_with(self);

        let mut idx = script_insert_index(&script.body);
        for decl in self.take_helper_decls() {
            script.body.insert(idx, decl);
            idx += 1;
        }
    }
}
