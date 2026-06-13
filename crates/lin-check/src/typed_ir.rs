use std::collections::HashMap;
use lin_common::Span;
use lin_parse::ast::{BinOp, UnaryOp};
use crate::types::Type;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TypedModule {
    pub statements: Vec<TypedStmt>,
    pub span: Span,
    /// Maps slot index to intrinsic name (e.g. 0 => "print").
    /// Populated by the checker when it registers intrinsics.
    pub intrinsics: HashMap<usize, String>,
    /// Exported `type` declarations (`export type Foo = ...`), as name → (params, resolved body).
    /// Type decls produce no runtime code, so they are recorded here as module metadata (like
    /// `intrinsics`) rather than as `TypedStmt`s — lowering/codegen ignore this field. A dependent
    /// module's checker re-registers these into its type env so `import { Foo }` can be used in
    /// type position (the value-import mechanism via `ModuleSignature::exports` is the analogue).
    #[serde(default)]
    pub exported_types: HashMap<String, (Vec<String>, Type)>,
    /// Test-only mock overrides (`replace <name> = <expr>`, ADR-046). Each entry records the
    /// imported export's CANONICAL mangled symbol (`{module_key}_{name}` for functions, the same
    /// base for vals — the `__val` suffix is added at lowering) and the type-checked replacement
    /// body. Lowering emits each body under that symbol and suppresses the original module's
    /// emission of it, so every reference — however the import path is spelled — resolves to the
    /// mock (it's one LLVM symbol). Only ever non-empty in a `.test.lin` (a hard error otherwise).
    /// Side-channel metadata like `intrinsics`/`exported_types`; statement lowering ignores it.
    #[serde(default)]
    pub replacements: Vec<Replacement>,
    /// Coverage attribution for CROSS-MODULE monomorphized specializations (`name$Int32`): maps the
    /// specialization's top-level `val` slot → the origin module path its generic body was cloned
    /// from. Populated by the monomorphizer (`spec.origin.is_some()`). Lowering uses it to set each
    /// specialization `LinFunction.coverage_origin`, so codegen attributes the spec's coverage
    /// regions to the origin source file (the generic definition's lines) instead of the importing
    /// module — otherwise a generic exported from another module reports 0% coverage even when its
    /// monomorphized instances are exercised by tests. Side-channel metadata; lowering of the
    /// statement values themselves ignores it.
    #[serde(default)]
    pub spec_origins: HashMap<usize, String>,
}

/// One `replace <name> = <expr>` override (ADR-046). See `TypedModule::replacements`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Replacement {
    /// The export's canonical mangled symbol base, `{module_key}_{name}` (e.g. `std_fs_readFile`).
    /// Function mocks are emitted under this exact symbol; val mocks under `{sym}__val`.
    pub sym: String,
    /// The export name as seen in the source module (e.g. `readFile`), used to identify which of
    /// an imported module's exports to suppress during that module's lowering.
    pub export_name: String,
    /// The resolved import path (e.g. `std/fs`), so lowering can match the owning module.
    pub module_path: String,
    /// True when the replacement body is a function (emitted as a named function); false for a
    /// non-function val (emitted as a zero-arg `{sym}__val` wrapper).
    pub is_function: bool,
    /// The type-checked replacement body.
    pub value: TypedExpr,
    /// The export's declared type (the body was checked against this).
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum TypedStmt {
    Val {
        slot: usize,
        name: Option<String>,
        value: TypedExpr,
        ty: Type,
        span: Span,
    },
    Var {
        slot: usize,
        /// Source binding name (for DWARF local-variable emission under `--debug`; Phase 3 of the
        /// debugger). Carried purely as metadata — it does not affect type checking or codegen
        /// semantics. Mirrors `Val.name` (always `Some` for a plain `var x = …`).
        name: Option<String>,
        value: TypedExpr,
        ty: Type,
        span: Span,
    },
    Import {
        path: String,
        bindings: Vec<ImportSlot>,
        span: Span,
    },
    /// FFI: extern functions imported from a compiled library.
    ForeignImport {
        path: String,
        bindings: Vec<ForeignSlot>,
        span: Span,
    },
    /// Object destructuring: evaluate value, store in obj_slot, then extract fields.
    Destructure {
        obj_slot: usize,
        value: TypedExpr,
        obj_ty: Type,
        /// (field_name, binding_slot, field_ty)
        fields: Vec<(String, usize, Type)>,
        /// rest binding slot (captures remaining fields as a new object)
        rest: Option<usize>,
        span: Span,
    },
    /// Array destructuring: evaluate value, store in arr_slot, then extract elements by index.
    ArrayDestructure {
        arr_slot: usize,
        value: TypedExpr,
        elem_ty: Type,
        /// (index, binding_slot, element_ty)
        elements: Vec<(usize, usize, Type)>,
        /// rest binding slot and type, if any
        rest: Option<(usize, Type)>,
        span: Span,
    },
    Expr(TypedExpr),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImportSlot {
    pub name: String,
    pub slot: usize,
    pub ty: Type,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ForeignSlot {
    pub name: String,
    pub slot: usize,
    pub ty: Type,
    /// True if this is a legal FFI type (see spec §26.3).
    pub valid: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum TypedExpr {
    IntLit(i64, Type, Span),
    FloatLit(f64, Type, Span),
    /// A string literal. The `Type` is normally `Str`, but bidirectional refinement
    /// (ADR-034) may narrow it to a `StrLit` singleton when checked against an expected
    /// literal type. The runtime representation is identical either way.
    StringLit(String, Type, Span),
    BoolLit(bool, Span),
    NullLit(Span),
    LocalGet {
        slot: usize,
        ty: Type,
        span: Span,
    },
    LocalSet {
        slot: usize,
        value: Box<TypedExpr>,
        ty: Type,
        span: Span,
    },
    BinaryOp {
        left: Box<TypedExpr>,
        op: BinOp,
        right: Box<TypedExpr>,
        result_type: Type,
        span: Span,
    },
    UnaryOp {
        op: UnaryOp,
        operand: Box<TypedExpr>,
        result_type: Type,
        span: Span,
    },
    Coerce {
        expr: Box<TypedExpr>,
        from: Type,
        to: Type,
        span: Span,
    },
    Call {
        func: Box<TypedExpr>,
        args: Vec<TypedExpr>,
        result_type: Type,
        is_tail: bool,
        /// True when this call is an explicit partial application (`f(x,)`).
        /// When false and fewer args than the callee's arity are supplied, the
        /// missing trailing arguments are filled from their defaults.
        partial: bool,
        span: Span,
    },
    If {
        cond: Box<TypedExpr>,
        then_br: Box<TypedExpr>,
        else_br: Box<TypedExpr>,
        result_type: Type,
        span: Span,
    },
    /// `T.fromJson(value)` / `fromJson(T, value)` — type-directed decode (ADR-031).
    /// `target` is the resolved concrete `Type` T (drives the runtime schema descriptor);
    /// `value` is the Json input; `result_type` is `T | Error` and flows to the surrounding
    /// assignment/return check.
    FromJson {
        target: Type,
        value: Box<TypedExpr>,
        result_type: Type,
        /// Resolved bodies of every `Named` type reachable from `target`, so codegen can
        /// build the recursive schema descriptor without a type environment. Recursion points
        /// in `target`/these bodies remain `Type::Named(n)` and are looked up here (ADR-031).
        named_defs: Vec<(String, Type)>,
        span: Span,
    },
    Match {
        scrutinee: Box<TypedExpr>,
        arms: Vec<TypedMatchArm>,
        result_type: Type,
        span: Span,
    },
    Block {
        stmts: Vec<TypedStmt>,
        expr: Box<TypedExpr>,
        ty: Type,
        span: Span,
    },
    Function {
        name: Option<String>,
        params: Vec<TypedParam>,
        body: Box<TypedExpr>,
        ret_type: Type,
        captures: Vec<Capture>,
        span: Span,
        /// Path-11 lambda-set identity: a unique id assigned to this syntactic lambda by the
        /// checker (`Checker::next_lambda_id`). The function type produced by `ty()` carries
        /// `LambdaSet::singleton(lambda_id)`. Inert metadata for shadow inference / measurement;
        /// `#[serde(default)]` => `0` for any cached `TypedExpr` written before this field existed
        /// (the cache stamp is bumped, so such entries are rejected, not mis-decoded).
        #[serde(default)]
        lambda_id: u32,
    },
    MakeObject {
        fields: Vec<(String, TypedExpr)>,
        spreads: Vec<TypedExpr>,
        ty: Type,
        span: Span,
    },
    MakeArray {
        elements: Vec<TypedExpr>,
        ty: Type,
        span: Span,
    },
    Index {
        object: Box<TypedExpr>,
        key: Box<TypedExpr>,
        result_type: Type,
        span: Span,
    },
    FieldGet {
        object: Box<TypedExpr>,
        field: String,
        result_type: Type,
        span: Span,
    },
    IndexSet {
        object: Box<TypedExpr>,
        key: Box<TypedExpr>,
        value: Box<TypedExpr>,
        obj_ty: Type,
        span: Span,
    },
    StringInterp {
        parts: Vec<TypedStringPart>,
        span: Span,
    },
    Is {
        expr: Box<TypedExpr>,
        pattern: TypedPattern,
        span: Span,
    },
    Has {
        expr: Box<TypedExpr>,
        pattern: TypedPattern,
        span: Span,
    },
}

impl TypedExpr {
    pub fn ty(&self) -> Type {
        match self {
            TypedExpr::IntLit(_, t, _) => t.clone(),
            TypedExpr::FloatLit(_, t, _) => t.clone(),
            TypedExpr::StringLit(_, ty, _) => ty.clone(),
            TypedExpr::BoolLit(_, _) => Type::Bool,
            TypedExpr::NullLit(_) => Type::Null,
            TypedExpr::LocalGet { ty, .. } => ty.clone(),
            TypedExpr::LocalSet { ty, .. } => ty.clone(),
            TypedExpr::BinaryOp { result_type, .. } => result_type.clone(),
            TypedExpr::UnaryOp { result_type, .. } => result_type.clone(),
            TypedExpr::Coerce { to, .. } => to.clone(),
            TypedExpr::Call { result_type, .. } => result_type.clone(),
            TypedExpr::If { result_type, .. } => result_type.clone(),
            TypedExpr::FromJson { result_type, .. } => result_type.clone(),
            TypedExpr::Match { result_type, .. } => result_type.clone(),
            TypedExpr::Block { ty, .. } => ty.clone(),
            TypedExpr::Function {
                params, ret_type, lambda_id, ..
            } => Type::Function {
                params: params.iter().map(|p| p.ty.clone()).collect(),
                ret: Box::new(ret_type.clone()),
                required: params.iter().filter(|p| p.default.is_none()).count(),
                // A syntactic lambda is the canonical singleton inhabitant of its own function
                // type. `lambda_id == 0` means "unassigned" (cache-default or a synthesized
                // function the checker never tagged) — fall back to `Top` rather than aliasing
                // every untagged function to the id-0 set.
                lset: if *lambda_id == 0 {
                    crate::types::LambdaSet::Top
                } else {
                    crate::types::LambdaSet::singleton(*lambda_id)
                },
            },
            TypedExpr::MakeObject { ty, .. } => ty.clone(),
            TypedExpr::MakeArray { ty, .. } => ty.clone(),
            TypedExpr::Index { result_type, .. } => result_type.clone(),
            TypedExpr::FieldGet { result_type, .. } => result_type.clone(),
            // An assignment expression evaluates to the assigned value (spec §8 / §27 rule 8),
            // mirroring `LocalSet`. The stored value's type is the result type.
            TypedExpr::IndexSet { value, .. } => value.ty(),
            TypedExpr::StringInterp { .. } => Type::Str,
            TypedExpr::Is { .. } => Type::Bool,
            TypedExpr::Has { .. } => Type::Bool,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            TypedExpr::IntLit(_, _, s) => *s,
            TypedExpr::FloatLit(_, _, s) => *s,
            TypedExpr::StringLit(_, _, s) => *s,
            TypedExpr::BoolLit(_, s) => *s,
            TypedExpr::NullLit(s) => *s,
            TypedExpr::LocalGet { span, .. } => *span,
            TypedExpr::LocalSet { span, .. } => *span,
            TypedExpr::BinaryOp { span, .. } => *span,
            TypedExpr::UnaryOp { span, .. } => *span,
            TypedExpr::Coerce { span, .. } => *span,
            TypedExpr::Call { span, .. } => *span,
            TypedExpr::If { span, .. } => *span,
            TypedExpr::FromJson { span, .. } => *span,
            TypedExpr::Match { span, .. } => *span,
            TypedExpr::Block { span, .. } => *span,
            TypedExpr::Function { span, .. } => *span,
            TypedExpr::MakeObject { span, .. } => *span,
            TypedExpr::MakeArray { span, .. } => *span,
            TypedExpr::Index { span, .. } => *span,
            TypedExpr::FieldGet { span, .. } => *span,
            TypedExpr::StringInterp { span, .. } => *span,
            TypedExpr::Is { span, .. } => *span,
            TypedExpr::Has { span, .. } => *span,
            TypedExpr::IndexSet { span, .. } => *span,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TypedParam {
    pub slot: usize,
    pub name: String,
    pub ty: Type,
    /// Default value expression, type-checked against `ty`. Present only for
    /// optional (trailing) parameters. Lowered by the defining module into
    /// per-arity adapter functions.
    pub default: Option<Box<TypedExpr>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Capture {
    pub name: String,
    pub outer_slot: usize,
    pub is_mutable: bool,
    pub ty: Type,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TypedMatchArm {
    pub pattern: TypedMatchPattern,
    pub guard: Option<TypedExpr>,
    pub body: TypedExpr,
    pub span: Span,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum TypedMatchPattern {
    Is(TypedPattern),
    Has(TypedPattern),
    Else,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum TypedPattern {
    TypeCheck(Type, Span),
    /// `is <Name>` where the resolved type is a non-empty object (ADR-036). Behaves exactly
    /// like `TypeCheck(ty, span)` for narrowing/zonking/exhaustiveness, but lowers to a
    /// `MatchesSchema` deep type-validation instead of a bare tag/presence check. Carries the
    /// resolved bodies of every reachable `Named` type (`named_defs`) so IR lowering — which
    /// has no type environment — can build the recursive schema descriptor (mirrors
    /// `TypedExpr::FromJson`'s payload).
    TypeCheckDeep(Type, Vec<(String, Type)>, Span),
    Literal(Box<TypedExpr>),
    Object {
        fields: Vec<TypedPatternField>,
        rest: Option<usize>,
        span: Span,
    },
    Array {
        elements: Vec<TypedPattern>,
        rest: Option<usize>,
        span: Span,
    },
    Binding(usize, Type, Span),
    Wildcard(Span),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TypedPatternField {
    pub key: String,
    pub binding_slot: Option<usize>,
    pub value_pattern: Option<Box<TypedExpr>>,
    pub ty: Type,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum TypedStringPart {
    Literal(String),
    Expr(TypedExpr),
}
