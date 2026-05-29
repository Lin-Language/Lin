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
    /// True if this is a legal FFI type (see spec §34.3).
    pub valid: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum TypedExpr {
    IntLit(i64, Type, Span),
    FloatLit(f64, Type, Span),
    StringLit(String, Span),
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
        span: Span,
    },
    If {
        cond: Box<TypedExpr>,
        then_br: Box<TypedExpr>,
        else_br: Box<TypedExpr>,
        result_type: Type,
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
            TypedExpr::StringLit(_, _) => Type::Str,
            TypedExpr::BoolLit(_, _) => Type::Bool,
            TypedExpr::NullLit(_) => Type::Null,
            TypedExpr::LocalGet { ty, .. } => ty.clone(),
            TypedExpr::LocalSet { ty, .. } => ty.clone(),
            TypedExpr::BinaryOp { result_type, .. } => result_type.clone(),
            TypedExpr::UnaryOp { result_type, .. } => result_type.clone(),
            TypedExpr::Coerce { to, .. } => to.clone(),
            TypedExpr::Call { result_type, .. } => result_type.clone(),
            TypedExpr::If { result_type, .. } => result_type.clone(),
            TypedExpr::Match { result_type, .. } => result_type.clone(),
            TypedExpr::Block { ty, .. } => ty.clone(),
            TypedExpr::Function {
                params, ret_type, ..
            } => Type::Function {
                params: params.iter().map(|p| p.ty.clone()).collect(),
                ret: Box::new(ret_type.clone()),
            },
            TypedExpr::MakeObject { ty, .. } => ty.clone(),
            TypedExpr::MakeArray { ty, .. } => ty.clone(),
            TypedExpr::Index { result_type, .. } => result_type.clone(),
            TypedExpr::FieldGet { result_type, .. } => result_type.clone(),
            TypedExpr::IndexSet { .. } => Type::Null,
            TypedExpr::StringInterp { .. } => Type::Str,
            TypedExpr::Is { .. } => Type::Bool,
            TypedExpr::Has { .. } => Type::Bool,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            TypedExpr::IntLit(_, _, s) => *s,
            TypedExpr::FloatLit(_, _, s) => *s,
            TypedExpr::StringLit(_, s) => *s,
            TypedExpr::BoolLit(_, s) => *s,
            TypedExpr::NullLit(s) => *s,
            TypedExpr::LocalGet { span, .. } => *span,
            TypedExpr::LocalSet { span, .. } => *span,
            TypedExpr::BinaryOp { span, .. } => *span,
            TypedExpr::UnaryOp { span, .. } => *span,
            TypedExpr::Coerce { span, .. } => *span,
            TypedExpr::Call { span, .. } => *span,
            TypedExpr::If { span, .. } => *span,
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
