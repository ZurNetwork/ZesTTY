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

pub fn lower(preamble_import: bool) -> impl Pass {
    visit_mut_pass(Lower {
        absurd: None,
        in_range: None,
        constrict: None,
        constrict_count: 0,
        range_aliases: Vec::new(),
        range_count: 0,
        preamble_import,
    })
}

struct Lower {
    /// The `__ztsAbsurd` identifier, created on first use; its presence
    /// also signals that the helper declaration must be injected.
    absurd: Option<Ident>,
    /// The `__ztsInRange` identifier, created on the first `lo..=hi` range
    /// arm; same injection story as `absurd` (import in preamble mode, an
    /// inline const otherwise).
    in_range: Option<Ident>,
    /// Hoisted `type __ztsRangeN = lo | … | hi;` aliases — one per range
    /// arm, fully erased, emitted next to the other generated preamble.
    range_aliases: Vec<Stmt>,
    /// Per-module range counter: like `constrict`, the alias names must
    /// self-uniquify, because hygiene is not TS-type-aware and will NOT
    /// rename duplicate TYPE aliases.
    range_count: usize,
    /// The constrict type helpers (`__ztsExpect`, `__ztsEqual`,
    /// `__ztsNot`), created on first `constrict` (Phase 7); their
    /// presence signals the type-only import (or inline aliases) must
    /// be injected.
    constrict: Option<(Ident, Ident, Ident)>,
    /// Per-module constrict counter: alias names must self-uniquify
    /// (`__ztsConstrict0`, `__ztsConstrict1`, ...) because hygiene is
    /// not TS-type-aware and will NOT rename duplicate TYPE aliases.
    constrict_count: usize,
    /// Import the helper from @zestty/core instead of emitting it inline
    /// (committed-twins mode). Scripts always inline — they cannot import.
    preamble_import: bool,
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

    fn in_range_ident(&mut self) -> Ident {
        self.in_range
            .get_or_insert_with(|| private_ident!("__ztsInRange"))
            .clone()
    }

    fn constrict_helpers(&mut self) -> (Ident, Ident, Ident) {
        self.constrict
            .get_or_insert_with(|| {
                (
                    private_ident!("__ztsExpect"),
                    private_ident!("__ztsEqual"),
                    private_ident!("__ztsNot"),
                )
            })
            .clone()
    }

    /// `constrict A == B;` → `type __ztsConstrict = <claim>;` (Phase 7).
    ///
    /// The claim is a generic-constraint violation when false:
    /// - `==` → `__ztsExpect<__ztsEqual<A, B>>`
    /// - `!=` → `__ztsExpect<__ztsNot<__ztsEqual<A, B>>>`
    /// - `extends` → `__ztsExpect<A extends B ? true : false>`
    ///
    /// The alias id carries the constrict's ORIGINAL span, so the TS2344
    /// lands on the assert's own line in the `.zts`. Fully erased; the
    /// alias name is a fresh-marked `__ztsConstrict` (hygiene dedupes
    /// multiple asserts per module).
    fn lower_constrict(&mut self, c: ZtsConstrictDecl) -> Decl {
        let (expect, equal, not) = self.constrict_helpers();
        let span = c.span;
        let type_ref = |ident: &Ident, params: Vec<Box<TsType>>| {
            Box::new(TsType::TsTypeRef(TsTypeRef {
                span,
                type_name: TsEntityName::Ident(ident.clone()),
                type_params: Some(Box::new(TsTypeParamInstantiation { span, params })),
            }))
        };
        let claim = match c.op {
            ZtsConstrictOp::Eq => type_ref(&equal, vec![c.left, c.right]),
            ZtsConstrictOp::NotEq => {
                let eq = type_ref(&equal, vec![c.left, c.right]);
                type_ref(&not, vec![eq])
            }
            ZtsConstrictOp::Extends => {
                let bool_lit = |value: bool| {
                    Box::new(TsType::TsLitType(TsLitType {
                        span,
                        lit: TsLit::Bool(Bool { span, value }),
                    }))
                };
                Box::new(TsType::TsConditionalType(TsConditionalType {
                    span,
                    check_type: c.left,
                    extends_type: c.right,
                    true_type: bool_lit(true),
                    false_type: bool_lit(false),
                }))
            }
        };

        let n = self.constrict_count;
        self.constrict_count += 1;
        let mut alias_id = private_ident!(format!("__ztsConstrict{n}"));
        alias_id.span = span;
        Decl::TsTypeAlias(Box::new(TsTypeAliasDecl {
            span,
            declare: false,
            id: alias_id,
            type_params: None,
            type_ann: type_ref(&expect, vec![claim]),
        }))
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
                MatchPat::Range(r) => {
                    stmts.push(self.lower_range_arm(&m_ident, arm_span, r, *body));
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

        // const { bindings } = __m; — skipped entirely for a bindingless
        // arm (`Ok {} =>`): `const {} = __m;` is an eslint error
        // (no-empty-pattern) in committed twins (issue #38).
        if let Some(binding) = binding.filter(|b| !b.props.is_empty()) {
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

    /// One range arm:
    /// `if (__ztsInRange<__ztsRangeN>(__m, lo, hi)) { return body; }`
    ///
    /// Shape 5 (measured against tsc 5.9 / 6.0 / 7.0 in the design round;
    /// do not substitute another shape). The three rejected alternatives
    /// and why:
    /// - relational comparisons (`__m >= lo && __m <= hi`) narrow NOTHING
    ///   anywhere in TypeScript — it is a guard by another name;
    /// - expanding to `switch` fallthrough or an `===` chain DOES narrow,
    ///   but tsc then emits TS2678/TS2367 for every enumerated value that
    ///   is not in the scrutinee's type, and a syntax-only compiler cannot
    ///   know which those are;
    /// - a type-level `Enumerate<lo, hi>` hits TS2589 above an ABSOLUTE
    ///   bound near 1000 (`Enumerate<4000, 4010>` fails at width 10), and
    ///   HTTP 4xx is already there.
    ///
    /// A type PREDICATE narrows without comparing, so none of that can
    /// happen: the true branch gets `__ztsRangeN & typeof __m`, the false
    /// branch has the covered members removed, and an exhaustive ranged
    /// match over a closed numeric literal union still runs `__m` to
    /// `never` for the keystone. The tail is UNCHANGED — whether a `_` is
    /// required is tsc's verdict, never the compiler's.
    ///
    /// `__ztsRangeN` is hoisted and fully erased; the `if` carries the
    /// arm's ORIGINAL span, and the bound literals are the ones the author
    /// wrote (raw text and all), so `0x1F..=0x2F` stays hexadecimal.
    fn lower_range_arm(
        &mut self,
        m_ident: &Ident,
        span: Span,
        pat: MatchRangePat,
        body: Expr,
    ) -> Stmt {
        let MatchRangePat {
            span: pat_span,
            lo,
            lo_neg,
            hi,
            hi_neg,
        } = pat;

        let alias = self.range_alias(pat_span, &lo, lo_neg, &hi, hi_neg);
        let in_range = self.in_range_ident();

        let bound = |n: Number, neg: bool| -> Box<Expr> {
            let lit = Expr::Lit(Lit::Num(n));
            Box::new(if neg {
                Expr::Unary(UnaryExpr {
                    span: pat_span,
                    op: UnaryOp::Minus,
                    arg: Box::new(lit),
                })
            } else {
                lit
            })
        };

        // __ztsInRange<__ztsRangeN>(__m, lo, hi)
        let test = Expr::Call(CallExpr {
            span: pat_span,
            ctxt: SyntaxContext::empty(),
            callee: Callee::Expr(Box::new(Expr::Ident(Ident {
                span: pat_span,
                ..in_range
            }))),
            args: vec![
                ExprOrSpread {
                    spread: None,
                    expr: Box::new(Expr::Ident(Ident {
                        span: pat_span,
                        ..m_ident.clone()
                    })),
                },
                ExprOrSpread {
                    spread: None,
                    expr: bound(lo, lo_neg),
                },
                ExprOrSpread {
                    spread: None,
                    expr: bound(hi, hi_neg),
                },
            ],
            type_args: Some(Box::new(TsTypeParamInstantiation {
                span: pat_span,
                params: vec![Box::new(TsType::TsTypeRef(TsTypeRef {
                    span: pat_span,
                    type_name: TsEntityName::Ident(alias),
                    type_params: None,
                }))],
            })),
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

    /// Hoists `type __ztsRangeN = lo | lo+1 | … | hi;` and returns its
    /// identifier.
    ///
    /// Defense in depth: the semantic pass has already rejected non-integer
    /// bounds, `lo > hi`, and widths over `MAX_RANGE_WIDTH` — but
    /// `MatchRangePat` is public API (napi, tests), and an embedder that
    /// skips the semantic pass must not be able to make the compiler
    /// allocate billions of nodes. Anything out of contract collapses to
    /// `never`, which fails closed: the arm can then never match.
    fn range_alias(
        &mut self,
        span: Span,
        lo: &Number,
        lo_neg: bool,
        hi: &Number,
        hi_neg: bool,
    ) -> Ident {
        let value = |n: &Number, neg: bool| if neg { -n.value } else { n.value };
        let (lo_v, hi_v) = (value(lo, lo_neg), value(hi, hi_neg));

        let members: Vec<Box<TsType>> = if lo_v.is_finite()
            && hi_v.is_finite()
            && lo_v.fract() == 0.0
            && hi_v.fract() == 0.0
            && lo_v <= hi_v
            && hi_v - lo_v < crate::semantic::MAX_RANGE_WIDTH as f64
        {
            let (lo_i, hi_i) = (lo_v as i64, hi_v as i64);
            (lo_i..=hi_i)
                .map(|v| {
                    Box::new(TsType::TsLitType(TsLitType {
                        span,
                        lit: TsLit::Number(Number {
                            span,
                            value: v as f64,
                            raw: None,
                        }),
                    }))
                })
                .collect()
        } else {
            vec![Box::new(TsType::TsKeywordType(TsKeywordType {
                span,
                kind: TsKeywordTypeKind::TsNeverKeyword,
            }))]
        };

        let n = self.range_count;
        self.range_count += 1;
        let mut alias_id = private_ident!(format!("__ztsRange{n}"));
        alias_id.span = span;

        let type_ann = match members.len() {
            1 => members.into_iter().next().expect("one member"),
            _ => Box::new(TsType::TsUnionOrIntersectionType(
                TsUnionOrIntersectionType::TsUnionType(TsUnionType {
                    span,
                    types: members,
                }),
            )),
        };

        self.range_aliases
            .push(Stmt::Decl(Decl::TsTypeAlias(Box::new(TsTypeAliasDecl {
                span,
                declare: false,
                id: alias_id.clone(),
                type_params: None,
                type_ann,
            }))));

        alias_id
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

        let keyword_ann = |kind: TsKeywordTypeKind| {
            Box::new(TsTypeAnn {
                span: DUMMY_SP,
                type_ann: Box::new(TsType::TsKeywordType(TsKeywordType {
                    span: DUMMY_SP,
                    kind,
                })),
            })
        };

        // new globalThis.Error("zts: non-exhaustive match")
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
        // throw globalThis.Object.assign(new globalThis.Error(...), { ztsTag: x });
        //
        // Object.assign keeps the thrown value a real Error while attaching
        // the tag WITHOUT an `any` (issue #36: the old `const e: any` was
        // the emitted code's only `any`, forcing downstream lint exemptions
        // over every committed twin). globalThis.Object, never bare Object
        // — hygiene is not TS-type-aware.
        let assign_call = Expr::Call(CallExpr {
            span: DUMMY_SP,
            ctxt: SyntaxContext::empty(),
            callee: Callee::Expr(Box::new(Expr::Member(MemberExpr {
                span: DUMMY_SP,
                obj: Box::new(Expr::Member(MemberExpr {
                    span: DUMMY_SP,
                    obj: Box::new(Expr::Ident(Ident::new_no_ctxt(
                        atom!("globalThis"),
                        DUMMY_SP,
                    ))),
                    prop: MemberProp::Ident(IdentName::new(atom!("Object"), DUMMY_SP)),
                })),
                prop: MemberProp::Ident(IdentName::new(atom!("assign"), DUMMY_SP)),
            }))),
            args: vec![
                ExprOrSpread {
                    spread: None,
                    expr: Box::new(new_error),
                },
                ExprOrSpread {
                    spread: None,
                    expr: Box::new(Expr::Object(ObjectLit {
                        span: DUMMY_SP,
                        props: vec![PropOrSpread::Prop(Box::new(Prop::KeyValue(KeyValueProp {
                            key: PropName::Ident(IdentName::new(atom!("ztsTag"), DUMMY_SP)),
                            value: Box::new(Expr::Ident(x.clone())),
                        })))],
                    })),
                },
            ],
            type_args: None,
        });

        let throw = Stmt::Throw(ThrowStmt {
            span: DUMMY_SP,
            arg: Box::new(assign_call),
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
                    stmts: vec![throw],
                }),
                is_generator: false,
                is_async: false,
                type_params: None,
                return_type: Some(keyword_ann(TsKeywordTypeKind::TsNeverKeyword)),
            }),
        }))
    }

    /// The inline (no @zestty/core) form of the range predicate:
    ///
    /// ```ts
    /// const __ztsInRange = <T extends number>(
    ///     __ztsV: unknown, __ztsLo: number, __ztsHi: number,
    /// ): __ztsV is T =>
    ///     typeof __ztsV === "number" && __ztsV % 1 === 0
    ///         && __ztsV >= __ztsLo && __ztsV <= __ztsHi;
    /// ```
    ///
    /// Byte-for-byte the same behaviour as the @zestty/core export, and
    /// deliberately the same two load-bearing details: the value parameter
    /// is `unknown` (a mixed scrutinee union must be able to reach it), and
    /// the integer gate is `% 1 === 0`, not `Number.isInteger`, which is
    /// ES2015 and would raise the emitted-TS lib floor.
    ///
    /// The parameter names carry the `__zts` prefix rather than the core
    /// package's plainer `__v`: user identifiers starting with `__zts` are
    /// rejected by the semantic pass, so nothing can collide and hygiene
    /// never has to rename them — which matters here because the return
    /// type is a type PREDICATE naming the first parameter, and hygiene is
    /// not TS-type-aware.
    fn in_range_decl(&self, in_range: Ident) -> Stmt {
        let t = private_ident!("T");
        let v = private_ident!("__ztsV");
        let lo = private_ident!("__ztsLo");
        let hi = private_ident!("__ztsHi");

        let keyword = |kind: TsKeywordTypeKind| {
            Box::new(TsType::TsKeywordType(TsKeywordType {
                span: DUMMY_SP,
                kind,
            }))
        };
        let ann = |ty: Box<TsType>| {
            Box::new(TsTypeAnn {
                span: DUMMY_SP,
                type_ann: ty,
            })
        };
        let param = |id: &Ident, ty: Box<TsType>| {
            Pat::Ident(BindingIdent {
                id: id.clone(),
                type_ann: Some(ann(ty)),
            })
        };
        let v_ref = || Box::new(Expr::Ident(v.clone()));
        let and = |left: Expr, right: Expr| {
            Expr::Bin(BinExpr {
                span: DUMMY_SP,
                op: op!("&&"),
                left: Box::new(left),
                right: Box::new(right),
            })
        };
        let num = |value: f64| {
            Box::new(Expr::Lit(Lit::Num(Number {
                span: DUMMY_SP,
                value,
                raw: None,
            })))
        };

        // typeof __ztsV === "number"
        let typeof_check = Expr::Bin(BinExpr {
            span: DUMMY_SP,
            op: op!("==="),
            left: Box::new(Expr::Unary(UnaryExpr {
                span: DUMMY_SP,
                op: op!("typeof"),
                arg: v_ref(),
            })),
            right: Box::new(Expr::Lit(Lit::Str(Str {
                span: DUMMY_SP,
                value: atom!("number").into(),
                raw: None,
            }))),
        });
        // __ztsV % 1 === 0
        let integer_check = Expr::Bin(BinExpr {
            span: DUMMY_SP,
            op: op!("==="),
            left: Box::new(Expr::Bin(BinExpr {
                span: DUMMY_SP,
                op: op!("%"),
                left: v_ref(),
                right: num(1.0),
            })),
            right: num(0.0),
        });
        let bound_check = |op: BinaryOp, bound: &Ident| {
            Expr::Bin(BinExpr {
                span: DUMMY_SP,
                op,
                left: v_ref(),
                right: Box::new(Expr::Ident(bound.clone())),
            })
        };

        let body = and(
            and(
                and(typeof_check, integer_check),
                bound_check(op!(">="), &lo),
            ),
            bound_check(op!("<="), &hi),
        );

        let arrow = ArrowExpr {
            span: DUMMY_SP,
            ctxt: SyntaxContext::empty(),
            params: vec![
                param(&v, keyword(TsKeywordTypeKind::TsUnknownKeyword)),
                param(&lo, keyword(TsKeywordTypeKind::TsNumberKeyword)),
                param(&hi, keyword(TsKeywordTypeKind::TsNumberKeyword)),
            ],
            body: Box::new(BlockStmtOrExpr::Expr(Box::new(body))),
            is_async: false,
            is_generator: false,
            // `<T extends number>`, never a bare `<T>`: the constraint is
            // what keeps the arrow unambiguous in a `.tsx` twin, where a
            // bare type parameter list would parse as a JSX element.
            type_params: Some(Box::new(TsTypeParamDecl {
                span: DUMMY_SP,
                params: vec![TsTypeParam {
                    span: DUMMY_SP,
                    name: t.clone(),
                    is_in: false,
                    is_out: false,
                    is_const: false,
                    constraint: Some(keyword(TsKeywordTypeKind::TsNumberKeyword)),
                    default: None,
                }],
            })),
            return_type: Some(ann(Box::new(TsType::TsTypePredicate(TsTypePredicate {
                span: DUMMY_SP,
                asserts: false,
                param_name: TsThisTypeOrIdent::Ident(v.clone()),
                type_ann: Some(ann(Box::new(TsType::TsTypeRef(TsTypeRef {
                    span: DUMMY_SP,
                    type_name: TsEntityName::Ident(t),
                    type_params: None,
                })))),
            })))),
        };

        VarDecl {
            span: DUMMY_SP,
            ctxt: SyntaxContext::empty(),
            kind: VarDeclKind::Const,
            declare: false,
            decls: vec![VarDeclarator {
                span: DUMMY_SP,
                name: Pat::Ident(in_range.into()),
                init: Some(Box::new(Expr::Arrow(arrow))),
                definite: false,
            }],
        }
        .into()
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
    fn visit_mut_ts_type(&mut self, t: &mut TsType) {
        t.visit_mut_children_with(self);

        // `T[+]` (Phase 7) → `[T, ...T[]]` — pure type plane: callers
        // must prove non-emptiness, `xs[0]` is `T`, fully erased.
        // Children first, so nested sugar (`string[+][+]`) lowers
        // inside-out; spans stay original.
        if let TsType::ZtsNonEmptyArray(ne) = t {
            let span = ne.span;
            let elem = ne.elem_type.clone();
            let rest_elem = elem.clone();
            *t = TsType::TsTupleType(TsTupleType {
                span,
                elem_types: vec![
                    TsTupleElement {
                        span: elem.span(),
                        label: None,
                        ty: elem,
                    },
                    TsTupleElement {
                        span,
                        label: None,
                        ty: Box::new(TsType::TsRestType(TsRestType {
                            span,
                            type_ann: Box::new(TsType::TsArrayType(TsArrayType {
                                span,
                                elem_type: rest_elem,
                            })),
                        })),
                    },
                ],
            });
        }
    }

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
            Expr::ZtsNot(..) => {
                // `not expr` → `!expr` (0.4.0: `not` is a real node so
                // formatters round-trip it; the compiler still erases it
                // to plain negation here).
                let Expr::ZtsNot(n) = e.take() else {
                    unreachable!()
                };
                *e = Expr::Unary(UnaryExpr {
                    span: n.span,
                    op: op!("!"),
                    arg: n.arg,
                });
            }
            _ => {}
        }
    }

    fn visit_mut_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        // Defense in depth: semantic rejects module-level `?`, but ZtsTry
        // is public API — never let one reach codegen's unreachable!.
        items.visit_mut_children_with(self);
        if !items
            .iter()
            .any(|item| matches!(item, ModuleItem::Stmt(s) if stmt_has_top_try(s)))
        {
            return;
        }
        let mut out: Vec<ModuleItem> = Vec::with_capacity(items.len() + 2);
        for item in items.drain(..) {
            match item {
                ModuleItem::Stmt(s) if stmt_has_top_try(&s) => {
                    let mut expanded = Vec::with_capacity(3);
                    expand_try_stmt(s, &mut expanded);
                    out.extend(expanded.into_iter().map(ModuleItem::Stmt));
                }
                other => out.push(other),
            }
        }
        *items = out;
    }

    fn visit_mut_module(&mut self, module: &mut Module) {
        module.visit_mut_children_with(self);

        // Hoisted range aliases. Spliced BEFORE the value helpers so the
        // helpers end up above them in the output: the aliases are pure
        // types (order is irrelevant to tsc), but a fixed position keeps
        // the emitted preamble stable and readable.
        if !self.range_aliases.is_empty() {
            let idx = helper_insert_index(&module.body);
            let aliases = std::mem::take(&mut self.range_aliases);
            module
                .body
                .splice(idx..idx, aliases.into_iter().map(ModuleItem::Stmt));
        }

        // The two VALUE helpers (`__ztsAbsurd`, `__ztsInRange`) share one
        // injection: one import in preamble mode, one splice of inline
        // declarations otherwise. Order is fixed so output is
        // deterministic; a module that needs only `__ztsAbsurd` emits
        // exactly what it emitted before ranges existed.
        let absurd = self.absurd.take();
        let in_range = self.in_range.take();
        if absurd.is_some() || in_range.is_some() {
            let idx = helper_insert_index(&module.body);
            if self.preamble_import {
                // import { __ztsAbsurd, __ztsInRange } from "@zestty/core";
                // — same idents (same marks), so hygiene treats them
                // exactly like the declarations they replace.
                let spec = |local: Ident| {
                    ImportSpecifier::Named(ImportNamedSpecifier {
                        span: DUMMY_SP,
                        local,
                        // No explicit `imported`: the local IS the
                        // exported name, and `Some` would emit a
                        // redundant `as` alias. (User `__zts*` idents
                        // are rejected by semantic F9, so hygiene never
                        // needs to rename these locals.)
                        imported: None,
                        is_type_only: false,
                    })
                };
                let specifiers = absurd
                    .into_iter()
                    .chain(in_range)
                    .map(spec)
                    .collect::<Vec<_>>();
                module.body.insert(
                    idx,
                    ModuleItem::ModuleDecl(ModuleDecl::Import(ImportDecl {
                        span: DUMMY_SP,
                        specifiers,
                        src: Box::new(Str {
                            span: DUMMY_SP,
                            value: atom!("@zestty/core").into(),
                            raw: None,
                        }),
                        type_only: false,
                        with: None,
                        phase: Default::default(),
                    })),
                );
            } else {
                let decls: Vec<Stmt> = absurd
                    .map(|a| self.absurd_decl(a))
                    .into_iter()
                    .chain(in_range.map(|r| self.in_range_decl(r)))
                    .collect();
                module
                    .body
                    .splice(idx..idx, decls.into_iter().map(ModuleItem::Stmt));
            }
        }

        if let Some(helpers) = self.constrict.take() {
            if self.preamble_import {
                // import type { __ztsExpect, __ztsEqual, __ztsNot } from
                // "@zestty/core"; — type-only, fully erased, same
                // hygiene story as the absurd import (locals ARE the
                // exported names; user `__zts*` idents are rejected by
                // semantic F9).
                let idx = helper_insert_index(&module.body);
                let (expect, equal, not) = helpers;
                let spec = |local: Ident| {
                    ImportSpecifier::Named(ImportNamedSpecifier {
                        span: DUMMY_SP,
                        local,
                        imported: None,
                        is_type_only: false,
                    })
                };
                module.body.insert(
                    idx,
                    ModuleItem::ModuleDecl(ModuleDecl::Import(ImportDecl {
                        span: DUMMY_SP,
                        specifiers: vec![spec(expect), spec(equal), spec(not)],
                        src: Box::new(Str {
                            span: DUMMY_SP,
                            value: atom!("@zestty/core").into(),
                            raw: None,
                        }),
                        type_only: true,
                        with: None,
                        phase: Default::default(),
                    })),
                );
            } else {
                let idx = helper_insert_index(&module.body);
                let decls = constrict_helper_decls(helpers);
                module
                    .body
                    .splice(idx..idx, decls.into_iter().map(ModuleItem::Stmt));
            }
        }
    }

    fn visit_mut_script(&mut self, script: &mut Script) {
        script.visit_mut_children_with(self);

        if !self.range_aliases.is_empty() {
            let idx = script_insert_index(&script.body);
            let aliases = std::mem::take(&mut self.range_aliases);
            script.body.splice(idx..idx, aliases);
        }

        // Scripts cannot import: the value helpers are always inline.
        let absurd = self.absurd.take();
        let in_range = self.in_range.take();
        if absurd.is_some() || in_range.is_some() {
            let idx = script_insert_index(&script.body);
            let decls: Vec<Stmt> = absurd
                .map(|a| self.absurd_decl(a))
                .into_iter()
                .chain(in_range.map(|r| self.in_range_decl(r)))
                .collect();
            script.body.splice(idx..idx, decls);
        }

        if let Some(helpers) = self.constrict.take() {
            // Scripts cannot import: always inline the erased helpers.
            let idx = script_insert_index(&script.body);
            script
                .body
                .splice(idx..idx, constrict_helper_decls(helpers));
        }
    }

    fn visit_mut_decl(&mut self, d: &mut Decl) {
        d.visit_mut_children_with(self);
        if matches!(d, Decl::ZtsConstrict(..)) {
            let Decl::ZtsConstrict(c) = d.take() else {
                unreachable!()
            };
            *d = self.lower_constrict(*c);
        }
    }
}

/// The inline (no @zestty/core) forms of the constrict helpers — type
/// aliases only, erased:
///
/// ```ts
/// type __ztsExpect<T extends true> = T;
/// type __ztsEqual<X, Y> =
///     (<T>() => T extends X ? 1 : 2) extends (<T>() => T extends Y ? 1 : 2)
///         ? true : false;
/// type __ztsNot<B extends boolean> = B extends true ? false : true;
/// ```
fn constrict_helper_decls((expect, equal, not): (Ident, Ident, Ident)) -> Vec<Stmt> {
    let bool_lit = |value: bool| {
        Box::new(TsType::TsLitType(TsLitType {
            span: DUMMY_SP,
            lit: TsLit::Bool(Bool {
                span: DUMMY_SP,
                value,
            }),
        }))
    };
    let param_ref = |name: &Ident| {
        Box::new(TsType::TsTypeRef(TsTypeRef {
            span: DUMMY_SP,
            type_name: TsEntityName::Ident(name.clone()),
            type_params: None,
        }))
    };
    let type_param = |name: &Ident, constraint: Option<Box<TsType>>| TsTypeParam {
        span: DUMMY_SP,
        name: name.clone(),
        is_in: false,
        is_out: false,
        is_const: false,
        constraint,
        default: None,
    };
    let alias = |id: Ident, params: Vec<TsTypeParam>, ann: Box<TsType>| {
        Stmt::Decl(Decl::TsTypeAlias(Box::new(TsTypeAliasDecl {
            span: DUMMY_SP,
            declare: false,
            id,
            type_params: Some(Box::new(TsTypeParamDecl {
                span: DUMMY_SP,
                params,
            })),
            type_ann: ann,
        })))
    };

    // type __ztsExpect<T extends true> = T;
    let t = private_ident!("T");
    let expect_decl = alias(
        expect,
        vec![type_param(&t, Some(bool_lit(true)))],
        param_ref(&t),
    );

    // (<T>() => T extends SIDE ? 1 : 2)
    let probe = |side: &Ident| {
        let t = private_ident!("T");
        let num_lit = |value: f64| {
            Box::new(TsType::TsLitType(TsLitType {
                span: DUMMY_SP,
                lit: TsLit::Number(Number {
                    span: DUMMY_SP,
                    value,
                    raw: None,
                }),
            }))
        };
        // The parens are load-bearing (same lesson as the newtype brand):
        // stock codegen has no type-level fixer, and an unwrapped fn-type
        // return swallows the outer `extends ... ? ... : ...` into its
        // own conditional.
        Box::new(TsType::TsParenthesizedType(TsParenthesizedType {
            span: DUMMY_SP,
            type_ann: Box::new(TsType::TsFnOrConstructorType(
                TsFnOrConstructorType::TsFnType(TsFnType {
                    span: DUMMY_SP,
                    params: vec![],
                    type_params: Some(Box::new(TsTypeParamDecl {
                        span: DUMMY_SP,
                        params: vec![type_param(&t, None)],
                    })),
                    type_ann: Box::new(TsTypeAnn {
                        span: DUMMY_SP,
                        type_ann: Box::new(TsType::TsConditionalType(TsConditionalType {
                            span: DUMMY_SP,
                            check_type: param_ref(&t),
                            extends_type: param_ref(side),
                            true_type: num_lit(1.0),
                            false_type: num_lit(2.0),
                        })),
                    }),
                }),
            )),
        }))
    };

    // type __ztsEqual<X, Y> = probe(X) extends probe(Y) ? true : false;
    let x = private_ident!("X");
    let y = private_ident!("Y");
    let equal_decl = alias(
        equal,
        vec![type_param(&x, None), type_param(&y, None)],
        Box::new(TsType::TsConditionalType(TsConditionalType {
            span: DUMMY_SP,
            check_type: probe(&x),
            extends_type: probe(&y),
            true_type: bool_lit(true),
            false_type: bool_lit(false),
        })),
    );

    // type __ztsNot<B extends boolean> = B extends true ? false : true;
    let b = private_ident!("B");
    let bool_kw = Box::new(TsType::TsKeywordType(TsKeywordType {
        span: DUMMY_SP,
        kind: TsKeywordTypeKind::TsBooleanKeyword,
    }));
    let not_decl = alias(
        not,
        vec![type_param(&b, Some(bool_kw))],
        Box::new(TsType::TsConditionalType(TsConditionalType {
            span: DUMMY_SP,
            check_type: param_ref(&b),
            extends_type: bool_lit(true),
            true_type: bool_lit(false),
            false_type: bool_lit(true),
        })),
    );

    vec![expect_decl, equal_decl, not_decl]
}
