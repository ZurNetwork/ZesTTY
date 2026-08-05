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
//! This pass runs BEFORE the resolver, unlike match lowering: it emits
//! ordinary user-named declarations (no `__zts` glue), so letting the
//! resolver scope them like hand-written code is both simpler and safer
//! than teaching hygiene about them after the fact. One enum becomes two
//! declarations (type + const, legal TS declaration merging), so expansion
//! happens at the statement-list level.

use swc_atoms::atom;
use swc_common::SyntaxContext;
use swc_ecma_ast::*;
use swc_ecma_visit::{VisitMut, VisitMutWith, visit_mut_pass};

pub fn lower_enums() -> impl Pass {
    visit_mut_pass(LowerEnums)
}

struct LowerEnums;

/// `{ kind: "Variant"; field: Type; ... }`
fn variant_type_lit(variant: &ZtsEnumVariant) -> TsType {
    let mut members = Vec::with_capacity(variant.fields.len() + 1);

    // kind: "Variant"
    members.push(TsTypeElement::TsPropertySignature(TsPropertySignature {
        span: variant.name.span,
        readonly: false,
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

    for field in &variant.fields {
        members.push(TsTypeElement::TsPropertySignature(TsPropertySignature {
            span: field.span,
            readonly: false,
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

/// One zts enum → (type alias, factory const).
fn lower_enum(e: &ZtsEnumDecl) -> (Decl, Decl) {
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

    let factories = Decl::Var(Box::new(VarDecl {
        span: e.span,
        ctxt: SyntaxContext::empty(),
        kind: VarDeclKind::Const,
        declare: false,
        decls: vec![VarDeclarator {
            span: e.span,
            name: Pat::Ident(e.ident.clone().into()),
            init: Some(Box::new(Expr::Object(ObjectLit {
                span: e.span,
                props: e
                    .variants
                    .iter()
                    .map(|v| variant_factory(&e.ident, v))
                    .collect(),
            }))),
            definite: false,
        }],
    }));

    (type_alias, factories)
}

impl VisitMut for LowerEnums {
    fn visit_mut_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        items.visit_mut_children_with(self);

        if !items.iter().any(|item| {
            matches!(
                item,
                ModuleItem::Stmt(Stmt::Decl(Decl::ZtsEnum(..)))
                    | ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl {
                        decl: Decl::ZtsEnum(..),
                        ..
                    }))
            )
        }) {
            return;
        }

        let mut out: Vec<ModuleItem> = Vec::with_capacity(items.len() + 4);
        for item in items.drain(..) {
            match item {
                ModuleItem::Stmt(Stmt::Decl(Decl::ZtsEnum(e))) => {
                    let (ty, factories) = lower_enum(&e);
                    out.push(ModuleItem::Stmt(Stmt::Decl(ty)));
                    out.push(ModuleItem::Stmt(Stmt::Decl(factories)));
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl {
                    span,
                    decl: Decl::ZtsEnum(e),
                })) => {
                    let (ty, factories) = lower_enum(&e);
                    let export = |decl: Decl| {
                        ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl { span, decl }))
                    };
                    out.push(export(ty));
                    out.push(export(factories));
                }
                other => out.push(other),
            }
        }
        *items = out;
    }

    fn visit_mut_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        stmts.visit_mut_children_with(self);

        if !stmts
            .iter()
            .any(|s| matches!(s, Stmt::Decl(Decl::ZtsEnum(..))))
        {
            return;
        }

        let mut out: Vec<Stmt> = Vec::with_capacity(stmts.len() + 2);
        for stmt in stmts.drain(..) {
            match stmt {
                Stmt::Decl(Decl::ZtsEnum(e)) => {
                    let (ty, factories) = lower_enum(&e);
                    out.push(Stmt::Decl(ty));
                    out.push(Stmt::Decl(factories));
                }
                other => out.push(other),
            }
        }
        *stmts = out;
    }
}
