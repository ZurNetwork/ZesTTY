//! Lowering: zts enums-with-data → tagged union type alias + factories.
//!
//! ```ts
//! // zts source
//! enum Shape {
//!   Circle { radius: number },
//!   Square { side: number },
//! }
//!
//! // generated TS
//! type Shape =
//!   | { kind: "Circle"; radius: number }
//!   | { kind: "Square"; side: number };
//! const Shape = {
//!   Circle: (radius: number): Shape => ({ kind: "Circle", radius }),
//!   Square: (side: number): Shape => ({ kind: "Square", side }),
//! };
//! ```
//!
//! Never a TypeScript `enum` — tagged unions + factory functions only.
//!
//! Also lowers newtypes (Phase 5), which share the one-decl-becomes-two
//! shape and the same hoisting/export rules:
//!
//! ```ts
//! // zts source
//! newtype AccountId = string;
//!
//! // generated TS
//! type AccountId = string & { readonly __ztsNewtype: "AccountId" };
//! const AccountId = (value: string): AccountId => value as AccountId;
//! ```
//!
//! The brand property exists only at the type level (the factory is an
//! identity cast — zero runtime cost), so two newtypes over the same
//! underlying type are mutually unassignable and the raw type is not
//! assignable to either: the ID-confusion bug class becomes TS2345.
//!
//! This pass runs BEFORE the resolver, unlike match lowering: it emits
//! ordinary user-named declarations (no `__zts` glue), so letting the
//! resolver scope them like hand-written code is both simpler and safer
//! than teaching hygiene about them after the fact. One enum becomes two
//! declarations (type + const, legal TS declaration merging), so expansion
//! happens at the statement-list level.

use swc_atoms::atom;
use swc_common::{Spanned, SyntaxContext};
use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith, visit_mut_pass};

pub fn lower_enums() -> impl Pass {
    visit_mut_pass(LowerEnums)
}

struct LowerEnums;

/// `{ kind: "Variant"; field: Type; ... }`
fn variant_type_lit(variant: &ZtsEnumVariant) -> TsType {
    let mut members = Vec::with_capacity(variant.fields.len() + 1);

    // readonly kind: "Variant" — the discriminant is NEVER mutable (a
    // kind write would let a value lie about its own variant).
    members.push(TsTypeElement::TsPropertySignature(TsPropertySignature {
        span: variant.name.span,
        readonly: true,
        key: Box::new(Expr::Ident(Ident::new_no_ctxt(
            atom!("kind"),
            variant.name.span,
        ))),
        computed: false,
        optional: false,
        type_ann: Some(Box::new(TsTypeAnn {
            span: variant.name.span,
            type_ann: Box::new(TsType::TsLitType(TsLitType {
                span: variant.name.span,
                lit: TsLit::Str(Str {
                    span: variant.name.span,
                    value: variant.name.sym.clone().into(),
                    raw: None,
                }),
            })),
        })),
    }));

    // Payload fields are readonly by default (Phase 7 — THE 0.4.0
    // breaking change); `mut field: T` opts out per field. Erased,
    // shallow, direct-write protection only — recorded in README feat 3.
    for field in &variant.fields {
        members.push(TsTypeElement::TsPropertySignature(TsPropertySignature {
            span: field.span,
            readonly: !field.is_mut,
            key: Box::new(Expr::Ident(Ident::new_no_ctxt(
                field.name.sym.clone(),
                field.name.span,
            ))),
            computed: false,
            optional: false,
            type_ann: Some(Box::new(TsTypeAnn {
                span: field.span,
                type_ann: field.type_ann.clone(),
            })),
        }));
    }

    TsType::TsTypeLit(TsTypeLit {
        span: variant.span,
        members,
    })
}

/// `Variant: (field: Type, ...): Enum => ({ kind: "Variant", field, ... })`
fn variant_factory(enum_ident: &Ident, variant: &ZtsEnumVariant) -> PropOrSpread {
    let params: Vec<Pat> = variant
        .fields
        .iter()
        .map(|field| {
            Pat::Ident(BindingIdent {
                id: Ident::new_no_ctxt(field.name.sym.clone(), field.name.span),
                type_ann: Some(Box::new(TsTypeAnn {
                    span: field.span,
                    type_ann: field.type_ann.clone(),
                })),
            })
        })
        .collect();

    let mut props: Vec<PropOrSpread> = Vec::with_capacity(variant.fields.len() + 1);
    props.push(PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
        key: PropName::Ident(IdentName::new(atom!("kind"), variant.name.span)),
        value: Box::new(Expr::Lit(Lit::Str(Str {
            span: variant.name.span,
            value: variant.name.sym.clone().into(),
            raw: None,
        }))),
    }))));
    for field in &variant.fields {
        props.push(PropOrSpread::Prop(Box::new(Prop::Shorthand(
            Ident::new_no_ctxt(field.name.sym.clone(), field.name.span),
        ))));
    }

    let arrow = ArrowExpr {
        span: variant.span,
        ctxt: SyntaxContext::empty(),
        params,
        body: Box::new(BlockStmtOrExpr::Expr(Box::new(Expr::Paren(ParenExpr {
            span: variant.span,
            expr: Box::new(Expr::Object(ObjectLit {
                span: variant.span,
                props,
            })),
        })))),
        is_async: false,
        is_generator: false,
        type_params: None,
        return_type: Some(Box::new(TsTypeAnn {
            span: variant.name.span,
            type_ann: Box::new(TsType::TsTypeRef(TsTypeRef {
                span: variant.name.span,
                type_name: TsEntityName::Ident(enum_ident.clone()),
                type_params: None,
            })),
        })),
    };

    PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
        key: PropName::Ident(IdentName::new(variant.name.sym.clone(), variant.name.span)),
        value: Box::new(Expr::Arrow(arrow)),
    })))
}

/// `name(self: Enum, ...): Ret { body }` — one impl method as a factory
/// object member (Phase 6 traits). The bare `self` receiver gets the enum
/// type HERE — the parser guarantees it carried none — so method bodies
/// see a fully typed value while the source stays annotation-free.
fn impl_method_prop(enum_ident: &Ident, mut method: ZtsImplMethod) -> PropOrSpread {
    if let Some(Param {
        pat: Pat::Ident(self_pat),
        ..
    }) = method.function.params.first_mut()
    {
        self_pat.type_ann = Some(Box::new(TsTypeAnn {
            span: self_pat.id.span,
            type_ann: Box::new(TsType::TsTypeRef(TsTypeRef {
                span: self_pat.id.span,
                type_name: TsEntityName::Ident(enum_ident.clone()),
                type_params: None,
            })),
        }));
    }
    PropOrSpread::Prop(Box::new(Prop::Method(MethodProp {
        key: PropName::Ident(IdentName::new(method.name.sym.clone(), method.name.span)),
        function: method.function,
    })))
}

/// `{ [key: string]: unknown } & Display<Shape> & ...` — the `satisfies`
/// type for a factory const carrying impls.
///
/// The index-signature member is load-bearing: `satisfies` runs
/// excess-property checking on fresh object literals, so without it the
/// variant factories themselves would be rejected as excess against the
/// trait type. It is written as an inline type literal, NOT `Record` — a
/// user shadow of `Record` would silently change what conformance means
/// (hygiene is not TS-type-aware; same reasoning as the globalThis rule).
/// Each trait ref is the conformance obligation, checked entirely by tsc
/// and erased by emit.
fn impl_satisfies_type(enum_ident: &Ident, impls: &[ZtsImplDecl]) -> Box<TsType> {
    let span = enum_ident.span;
    let string_ann = Box::new(TsTypeAnn {
        span,
        type_ann: Box::new(TsType::TsKeywordType(TsKeywordType {
            span,
            kind: TsKeywordTypeKind::TsStringKeyword,
        })),
    });
    let unknown_ann = Box::new(TsTypeAnn {
        span,
        type_ann: Box::new(TsType::TsKeywordType(TsKeywordType {
            span,
            kind: TsKeywordTypeKind::TsUnknownKeyword,
        })),
    });
    let absorber = Box::new(TsType::TsTypeLit(TsTypeLit {
        span,
        members: vec![TsTypeElement::TsIndexSignature(TsIndexSignature {
            span,
            params: vec![TsFnParam::Ident(BindingIdent {
                id: Ident::new_no_ctxt(atom!("key"), span),
                type_ann: Some(string_ann),
            })],
            type_ann: Some(unknown_ann),
            readonly: false,
            is_static: false,
        })],
    }));

    let mut types: Vec<Box<TsType>> = Vec::with_capacity(impls.len() + 1);
    types.push(absorber);
    for i in impls {
        types.push(Box::new(TsType::TsTypeRef(TsTypeRef {
            span: i.trait_ident.span,
            type_name: TsEntityName::Ident(i.trait_ident.clone()),
            type_params: Some(Box::new(TsTypeParamInstantiation {
                span: i.for_ident.span,
                params: vec![Box::new(TsType::TsTypeRef(TsTypeRef {
                    span: i.for_ident.span,
                    type_name: TsEntityName::Ident(enum_ident.clone()),
                    type_params: None,
                }))],
            })),
        })));
    }

    Box::new(TsType::TsUnionOrIntersectionType(
        TsUnionOrIntersectionType::TsIntersectionType(TsIntersectionType { span, types }),
    ))
}

/// One zts enum → (type alias, factory const). Any impls for it merge
/// into the factory: their methods become object members and the const's
/// initializer gains `satisfies <absorber> & Trait<Enum> & ...`.
fn lower_enum(e: &ZtsEnumDecl, impls: Vec<ZtsImplDecl>) -> (Decl, Decl) {
    let union_ty: Box<TsType> = match e.variants.len() {
        // `enum Never {}` — the empty union.
        0 => Box::new(TsType::TsKeywordType(TsKeywordType {
            span: e.span,
            kind: TsKeywordTypeKind::TsNeverKeyword,
        })),
        1 => Box::new(variant_type_lit(&e.variants[0])),
        _ => Box::new(TsType::TsUnionOrIntersectionType(
            TsUnionOrIntersectionType::TsUnionType(TsUnionType {
                span: e.span,
                types: e
                    .variants
                    .iter()
                    .map(|v| Box::new(variant_type_lit(v)))
                    .collect(),
            }),
        )),
    };

    let type_alias = Decl::TsTypeAlias(Box::new(TsTypeAliasDecl {
        span: e.span,
        declare: false,
        id: e.ident.clone(),
        type_params: None,
        type_ann: union_ty,
    }));

    let mut props: Vec<PropOrSpread> = e
        .variants
        .iter()
        .map(|v| variant_factory(&e.ident, v))
        .collect();
    let satisfies = if impls.is_empty() {
        None
    } else {
        Some(impl_satisfies_type(&e.ident, &impls))
    };
    for i in impls {
        let ZtsImplDecl { methods, .. } = i;
        for method in methods {
            props.push(impl_method_prop(&e.ident, method));
        }
    }

    let obj = Expr::Object(ObjectLit {
        span: e.span,
        props,
    });
    let init = match satisfies {
        None => obj,
        Some(type_ann) => Expr::TsSatisfies(TsSatisfiesExpr {
            span: e.span,
            expr: Box::new(obj),
            type_ann,
        }),
    };

    let factories = Decl::Var(Box::new(VarDecl {
        span: e.span,
        ctxt: SyntaxContext::empty(),
        kind: VarDeclKind::Const,
        declare: false,
        decls: vec![VarDeclarator {
            span: e.span,
            name: Pat::Ident(e.ident.clone().into()),
            init: Some(Box::new(init)),
            definite: false,
        }],
    }));

    (type_alias, factories)
}

/// One zts newtype → (branded type alias, factory const).
fn lower_newtype(n: &ZtsNewtypeDecl) -> (Decl, Decl) {
    // { readonly __ztsNewtype: "Name" }
    let brand = TsType::TsTypeLit(TsTypeLit {
        span: n.ident.span,
        members: vec![TsTypeElement::TsPropertySignature(TsPropertySignature {
            span: n.ident.span,
            readonly: true,
            key: Box::new(Expr::Ident(Ident::new_no_ctxt(
                atom!("__ztsNewtype"),
                n.ident.span,
            ))),
            computed: false,
            optional: false,
            type_ann: Some(Box::new(TsTypeAnn {
                span: n.ident.span,
                type_ann: Box::new(TsType::TsLitType(TsLitType {
                    span: n.ident.span,
                    lit: TsLit::Str(Str {
                        span: n.ident.span,
                        value: n.ident.sym.clone().into(),
                        raw: None,
                    }),
                })),
            })),
        })],
    });

    // type Name = (<underlying>) & { readonly __ztsNewtype: "Name" };
    //
    // The parens are load-bearing (review-gated): stock codegen has no
    // type-level fixer, and `&` binds tighter than `|`, so an unwrapped
    // union underlying type would brand only its LAST member — silently
    // reopening the ID-confusion bug class. Same for `=>` return types.
    let underlying = Box::new(TsType::TsParenthesizedType(TsParenthesizedType {
        span: n.type_ann.span(),
        type_ann: n.type_ann.clone(),
    }));
    let type_alias = Decl::TsTypeAlias(Box::new(TsTypeAliasDecl {
        span: n.span,
        declare: false,
        id: n.ident.clone(),
        type_params: None,
        type_ann: Box::new(TsType::TsUnionOrIntersectionType(
            TsUnionOrIntersectionType::TsIntersectionType(TsIntersectionType {
                span: n.span,
                types: vec![underlying, Box::new(brand)],
            }),
        )),
    }));

    let name_ty = |span| {
        Box::new(TsType::TsTypeRef(TsTypeRef {
            span,
            type_name: TsEntityName::Ident(n.ident.clone()),
            type_params: None,
        }))
    };

    // const Name = (__ztsValue: <underlying>): Name => __ztsValue as Name;
    // (a `value`-named param would be captured by `typeof value` in the
    // underlying type — generated idents keep the __zts prefix)
    let value_ident = Ident::new_no_ctxt(atom!("__ztsValue"), n.span);
    let arrow = ArrowExpr {
        span: n.span,
        ctxt: SyntaxContext::empty(),
        params: vec![Pat::Ident(BindingIdent {
            id: value_ident.clone(),
            type_ann: Some(Box::new(TsTypeAnn {
                span: n.span,
                type_ann: n.type_ann.clone(),
            })),
        })],
        body: Box::new(BlockStmtOrExpr::Expr(Box::new(Expr::TsAs(TsAsExpr {
            span: n.span,
            expr: Box::new(Expr::Ident(value_ident)),
            type_ann: name_ty(n.span),
        })))),
        is_async: false,
        is_generator: false,
        type_params: None,
        return_type: Some(Box::new(TsTypeAnn {
            span: n.ident.span,
            type_ann: name_ty(n.ident.span),
        })),
    };

    let factory = Decl::Var(Box::new(VarDecl {
        span: n.span,
        ctxt: SyntaxContext::empty(),
        kind: VarDeclKind::Const,
        declare: false,
        decls: vec![VarDeclarator {
            span: n.span,
            name: Pat::Ident(n.ident.clone().into()),
            init: Some(Box::new(Expr::Arrow(arrow))),
            definite: false,
        }],
    }));

    (type_alias, factory)
}

/// One zts union → (literal-union type alias, values/has const).
///
/// ```ts
/// type Level = 'info' | 'warn';
/// const Level = {
///     values: ['info', 'warn'] as const,
///     has: (__ztsRaw: string): __ztsRaw is Level =>
///         Level.values.indexOf(__ztsRaw as Level) !== -1,
/// };
/// ```
fn lower_union(u: &ZtsUnionDecl) -> (Decl, Decl) {
    let str_kw = || {
        Box::new(TsType::TsKeywordType(TsKeywordType {
            span: u.span,
            kind: TsKeywordTypeKind::TsStringKeyword,
        }))
    };
    let name_ty = || {
        Box::new(TsType::TsTypeRef(TsTypeRef {
            span: u.ident.span,
            type_name: TsEntityName::Ident(u.ident.clone()),
            type_params: None,
        }))
    };

    // type Name = 'a' | 'b';
    let union_ty: Box<TsType> = match u.members.len() {
        1 => Box::new(TsType::TsLitType(TsLitType {
            span: u.members[0].span,
            lit: TsLit::Str(u.members[0].clone()),
        })),
        _ => Box::new(TsType::TsUnionOrIntersectionType(
            TsUnionOrIntersectionType::TsUnionType(TsUnionType {
                span: u.span,
                types: u
                    .members
                    .iter()
                    .map(|m| {
                        Box::new(TsType::TsLitType(TsLitType {
                            span: m.span,
                            lit: TsLit::Str(m.clone()),
                        }))
                    })
                    .collect(),
            }),
        )),
    };
    let type_alias = Decl::TsTypeAlias(Box::new(TsTypeAliasDecl {
        span: u.span,
        declare: false,
        id: u.ident.clone(),
        type_params: None,
        type_ann: union_ty,
    }));

    // ['a', 'b'] as const
    let values_array = Expr::TsConstAssertion(TsConstAssertion {
        span: u.span,
        expr: Box::new(Expr::Array(ArrayLit {
            span: u.span,
            elems: u
                .members
                .iter()
                .map(|m| {
                    Some(ExprOrSpread {
                        spread: None,
                        expr: Box::new(Expr::Lit(Lit::Str(m.clone()))),
                    })
                })
                .collect(),
        })),
    });

    // Name.values.indexOf(__ztsRaw as Name) !== -1
    //
    // indexOf, not includes: includes is ES2016 and would make has() the
    // only lowering that raises the emitted-TS lib floor (review finding
    // 3); indexOf is ES5-clean. The cast rides on the ARGUMENT, not the
    // receiver: a receiver cast (`(values as ...).indexOf`) needs parens
    // that the paren-stripping fixer removes (miscompile), and indexOf on
    // the as-const tuple wants the member union anyway.
    let raw_ident = Ident::new_no_ctxt(atom!("__ztsRaw"), u.span);
    let index_of_call = Expr::Call(CallExpr {
        span: u.span,
        ctxt: SyntaxContext::empty(),
        callee: Callee::Expr(Box::new(Expr::Member(MemberExpr {
            span: u.span,
            obj: Box::new(Expr::Member(MemberExpr {
                span: u.span,
                obj: Box::new(Expr::Ident(u.ident.clone())),
                prop: MemberProp::Ident(IdentName::new(atom!("values"), u.span)),
            })),
            prop: MemberProp::Ident(IdentName::new(atom!("indexOf"), u.span)),
        }))),
        args: vec![ExprOrSpread {
            spread: None,
            expr: Box::new(Expr::TsAs(TsAsExpr {
                span: u.span,
                expr: Box::new(Expr::Ident(raw_ident.clone())),
                type_ann: name_ty(),
            })),
        }],
        type_args: None,
    });
    let includes_call = Expr::Bin(BinExpr {
        span: u.span,
        op: BinaryOp::NotEqEq,
        left: Box::new(index_of_call),
        right: Box::new(Expr::Unary(UnaryExpr {
            span: u.span,
            op: UnaryOp::Minus,
            arg: Box::new(Expr::Lit(Lit::Num(Number {
                span: u.span,
                value: 1.0,
                raw: None,
            }))),
        })),
    });

    // (__ztsRaw: string): __ztsRaw is Name => ...
    let has_arrow = ArrowExpr {
        span: u.span,
        ctxt: SyntaxContext::empty(),
        params: vec![Pat::Ident(BindingIdent {
            id: raw_ident.clone(),
            type_ann: Some(Box::new(TsTypeAnn {
                span: u.span,
                type_ann: str_kw(),
            })),
        })],
        body: Box::new(BlockStmtOrExpr::Expr(Box::new(includes_call))),
        is_async: false,
        is_generator: false,
        type_params: None,
        return_type: Some(Box::new(TsTypeAnn {
            span: u.span,
            type_ann: Box::new(TsType::TsTypePredicate(TsTypePredicate {
                span: u.span,
                asserts: false,
                param_name: TsThisTypeOrIdent::Ident(raw_ident),
                type_ann: Some(Box::new(TsTypeAnn {
                    span: u.span,
                    type_ann: name_ty(),
                })),
            })),
        })),
    };

    let obj = Decl::Var(Box::new(VarDecl {
        span: u.span,
        ctxt: SyntaxContext::empty(),
        kind: VarDeclKind::Const,
        declare: false,
        decls: vec![VarDeclarator {
            span: u.span,
            name: Pat::Ident(u.ident.clone().into()),
            init: Some(Box::new(Expr::Object(ObjectLit {
                span: u.span,
                props: vec![
                    PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                        key: PropName::Ident(IdentName::new(atom!("values"), u.span)),
                        value: Box::new(values_array),
                    }))),
                    PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                        key: PropName::Ident(IdentName::new(atom!("has"), u.span)),
                        value: Box::new(Expr::Arrow(has_arrow)),
                    }))),
                ],
            }))),
            definite: false,
        }],
    }));

    (type_alias, obj)
}

/// First index past the directive prologue (and, for modules, past leading
/// imports): the hoist target. Enum expansions are pure declarations
/// (a type alias + an object literal of arrows), so evaluation order is
/// unaffected — and hoisting restores the order-independence both TS
/// `enum` (hoisted var) and Rust enums have. Without it, a use-before-decl
/// resolves the (hoisted) TYPE but not the factory const: a confusing
/// TS2448 on the generated file.
fn module_hoist_index(items: &[ModuleItem]) -> usize {
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

fn stmts_hoist_index(stmts: &[Stmt]) -> usize {
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

/// If `decl` is a zts construct that expands to two declarations, lower it.
/// An enum takes (and consumes) its impls from `impls`.
fn lower_zts_decl(decl: Decl, impls: &mut Vec<ZtsImplDecl>) -> Result<(Decl, Decl), Decl> {
    match decl {
        Decl::ZtsEnum(e) => {
            let mine = take_impls_for(impls, &e.ident);
            Ok(lower_enum(&e, mine))
        }
        Decl::ZtsNewtype(n) => Ok(lower_newtype(&n)),
        Decl::ZtsUnion(u) => Ok(lower_union(&u)),
        other => Err(other),
    }
}

/// Drain the impls targeting `ident` out of the pool, preserving source
/// order (method merge order is the impls' order in the file).
fn take_impls_for(impls: &mut Vec<ZtsImplDecl>, ident: &Ident) -> Vec<ZtsImplDecl> {
    let mut mine = Vec::new();
    let mut rest = Vec::new();
    for i in impls.drain(..) {
        if i.for_ident.sym == ident.sym {
            mine.push(i);
        } else {
            rest.push(i);
        }
    }
    *impls = rest;
    mine
}

fn is_zts_decl(decl: &Decl) -> bool {
    matches!(
        decl,
        Decl::ZtsEnum(..) | Decl::ZtsNewtype(..) | Decl::ZtsUnion(..)
    )
}

impl VisitMut for LowerEnums {
    fn visit_mut_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        items.visit_mut_children_with(self);

        if !items.iter().any(|item| {
            matches!(
                item,
                ModuleItem::Stmt(Stmt::Decl(d)) if is_zts_decl(d)
            ) || matches!(
                item,
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl { decl, .. }))
                    if is_zts_decl(decl)
            )
        }) {
            return;
        }

        // Pool the impls first: they may precede or follow their enum in
        // the file, and each merges into its enum's factory. Semantic has
        // already guaranteed every impl matches an enum in this list — a
        // leftover would reach codegen's unreachable!, the designed
        // tripwire for embedders that skip the semantic pass.
        let mut impls: Vec<ZtsImplDecl> = Vec::new();
        let mut hoisted: Vec<ModuleItem> = Vec::new();
        let mut out: Vec<ModuleItem> = Vec::with_capacity(items.len());
        for item in items.drain(..) {
            match item {
                ModuleItem::Stmt(Stmt::Decl(Decl::ZtsImpl(i))) => impls.push(*i),
                other => out.push(other),
            }
        }
        let mut rest: Vec<ModuleItem> = Vec::with_capacity(out.len());
        for item in out.drain(..) {
            match item {
                ModuleItem::Stmt(Stmt::Decl(d)) if is_zts_decl(&d) => {
                    let (ty, factories) = lower_zts_decl(d, &mut impls).unwrap();
                    hoisted.push(ModuleItem::Stmt(Stmt::Decl(ty)));
                    hoisted.push(ModuleItem::Stmt(Stmt::Decl(factories)));
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl { span, decl }))
                    if is_zts_decl(&decl) =>
                {
                    let (ty, factories) = lower_zts_decl(decl, &mut impls).unwrap();
                    let export = |decl: Decl| {
                        ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl { span, decl }))
                    };
                    hoisted.push(export(ty));
                    hoisted.push(export(factories));
                }
                other => rest.push(other),
            }
        }
        // Leftover impls (no matching enum) go back as-is: loud, not lost.
        rest.extend(
            impls
                .into_iter()
                .map(|i| ModuleItem::Stmt(Stmt::Decl(Decl::ZtsImpl(Box::new(i))))),
        );
        let idx = module_hoist_index(&rest);
        rest.splice(idx..idx, hoisted);
        *items = rest;
    }

    fn visit_mut_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        stmts.visit_mut_children_with(self);

        if !stmts
            .iter()
            .any(|s| matches!(s, Stmt::Decl(d) if is_zts_decl(d)))
        {
            return;
        }

        let mut impls: Vec<ZtsImplDecl> = Vec::new();
        let mut hoisted: Vec<Stmt> = Vec::new();
        let mut out: Vec<Stmt> = Vec::with_capacity(stmts.len());
        for stmt in stmts.drain(..) {
            match stmt {
                Stmt::Decl(Decl::ZtsImpl(i)) => impls.push(*i),
                other => out.push(other),
            }
        }
        let mut rest: Vec<Stmt> = Vec::with_capacity(out.len());
        for stmt in out.drain(..) {
            match stmt {
                Stmt::Decl(d) if is_zts_decl(&d) => {
                    let (ty, factories) = lower_zts_decl(d, &mut impls).unwrap();
                    hoisted.push(Stmt::Decl(ty));
                    hoisted.push(Stmt::Decl(factories));
                }
                other => rest.push(other),
            }
        }
        rest.extend(
            impls
                .into_iter()
                .map(|i| Stmt::Decl(Decl::ZtsImpl(Box::new(i)))),
        );
        let idx = stmts_hoist_index(&rest);
        rest.splice(idx..idx, hoisted);
        *stmts = rest;
    }

    // Defense in depth for single-statement positions (`if (c) enum E {}`
    // style): the parser rejects these under zts, but `Decl::ZtsEnum` /
    // `Decl::ZtsNewtype` are public API — never let one reach codegen's
    // `unreachable!`. These are the only Stmt slots that are not part of a
    // `Vec<Stmt>`.

    fn visit_mut_if_stmt(&mut self, s: &mut IfStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_enum(&mut s.cons);
        if let Some(alt) = &mut s.alt {
            wrap_single_stmt_enum(alt);
        }
    }

    fn visit_mut_while_stmt(&mut self, s: &mut WhileStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_enum(&mut s.body);
    }

    fn visit_mut_do_while_stmt(&mut self, s: &mut DoWhileStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_enum(&mut s.body);
    }

    fn visit_mut_for_stmt(&mut self, s: &mut ForStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_enum(&mut s.body);
    }

    fn visit_mut_for_in_stmt(&mut self, s: &mut ForInStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_enum(&mut s.body);
    }

    fn visit_mut_for_of_stmt(&mut self, s: &mut ForOfStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_enum(&mut s.body);
    }

    fn visit_mut_labeled_stmt(&mut self, s: &mut LabeledStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_enum(&mut s.body);
    }

    fn visit_mut_with_stmt(&mut self, s: &mut WithStmt) {
        s.visit_mut_children_with(self);
        wrap_single_stmt_enum(&mut s.body);
    }
}

/// If `stmt` is a bare zts enum/newtype declaration, expand it in a block.
fn wrap_single_stmt_enum(stmt: &mut Stmt) {
    if matches!(stmt, Stmt::Decl(d) if is_zts_decl(d)) {
        let Stmt::Decl(decl) = std::mem::replace(
            stmt,
            Stmt::Empty(EmptyStmt {
                span: swc_common::DUMMY_SP,
            }),
        ) else {
            unreachable!()
        };
        let span = match &decl {
            Decl::ZtsEnum(e) => e.span,
            Decl::ZtsNewtype(n) => n.span,
            Decl::ZtsUnion(u) => u.span,
            _ => unreachable!(),
        };
        // No impl pool here: an impl cannot legally target an enum in a
        // single-statement slot (semantic enforces same-list pairing).
        let (ty, factories) = lower_zts_decl(decl, &mut Vec::new()).unwrap();
        *stmt = Stmt::Block(BlockStmt {
            span,
            ctxt: SyntaxContext::empty(),
            stmts: vec![Stmt::Decl(ty), Stmt::Decl(factories)],
        });
    }
}
