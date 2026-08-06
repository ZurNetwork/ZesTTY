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

    // type Name = <underlying> & { readonly __ztsNewtype: "Name" };
    let type_alias = Decl::TsTypeAlias(Box::new(TsTypeAliasDecl {
        span: n.span,
        declare: false,
        id: n.ident.clone(),
        type_params: None,
        type_ann: Box::new(TsType::TsUnionOrIntersectionType(
            TsUnionOrIntersectionType::TsIntersectionType(TsIntersectionType {
                span: n.span,
                types: vec![n.type_ann.clone(), Box::new(brand)],
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

    // const Name = (value: <underlying>): Name => value as Name;
    let value_ident = Ident::new_no_ctxt(atom!("value"), n.span);
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
fn lower_zts_decl(decl: Decl) -> Result<(Decl, Decl), Decl> {
    match decl {
        Decl::ZtsEnum(e) => Ok(lower_enum(&e)),
        Decl::ZtsNewtype(n) => Ok(lower_newtype(&n)),
        other => Err(other),
    }
}

fn is_zts_decl(decl: &Decl) -> bool {
    matches!(decl, Decl::ZtsEnum(..) | Decl::ZtsNewtype(..))
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

        let mut hoisted: Vec<ModuleItem> = Vec::new();
        let mut out: Vec<ModuleItem> = Vec::with_capacity(items.len());
        for item in items.drain(..) {
            match item {
                ModuleItem::Stmt(Stmt::Decl(d)) if is_zts_decl(&d) => {
                    let (ty, factories) = lower_zts_decl(d).unwrap();
                    hoisted.push(ModuleItem::Stmt(Stmt::Decl(ty)));
                    hoisted.push(ModuleItem::Stmt(Stmt::Decl(factories)));
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl { span, decl }))
                    if is_zts_decl(&decl) =>
                {
                    let (ty, factories) = lower_zts_decl(decl).unwrap();
                    let export = |decl: Decl| {
                        ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl { span, decl }))
                    };
                    hoisted.push(export(ty));
                    hoisted.push(export(factories));
                }
                other => out.push(other),
            }
        }
        let idx = module_hoist_index(&out);
        out.splice(idx..idx, hoisted);
        *items = out;
    }

    fn visit_mut_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        stmts.visit_mut_children_with(self);

        if !stmts
            .iter()
            .any(|s| matches!(s, Stmt::Decl(d) if is_zts_decl(d)))
        {
            return;
        }

        let mut hoisted: Vec<Stmt> = Vec::new();
        let mut out: Vec<Stmt> = Vec::with_capacity(stmts.len());
        for stmt in stmts.drain(..) {
            match stmt {
                Stmt::Decl(d) if is_zts_decl(&d) => {
                    let (ty, factories) = lower_zts_decl(d).unwrap();
                    hoisted.push(Stmt::Decl(ty));
                    hoisted.push(Stmt::Decl(factories));
                }
                other => out.push(other),
            }
        }
        let idx = stmts_hoist_index(&out);
        out.splice(idx..idx, hoisted);
        *stmts = out;
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
            _ => unreachable!(),
        };
        let (ty, factories) = lower_zts_decl(decl).unwrap();
        *stmt = Stmt::Block(BlockStmt {
            span,
            ctxt: SyntaxContext::empty(),
            stmts: vec![Stmt::Decl(ty), Stmt::Decl(factories)],
        });
    }
}
