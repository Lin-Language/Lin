use lin_common::{Span, NumSuffix};

#[derive(Debug, Clone)]
pub struct Module {
    pub statements: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Val {
        pattern: Pattern,
        type_ann: Option<TypeExpr>,
        value: Expr,
        exported: bool,
        span: Span,
    },
    Var {
        name: String,
        type_ann: Option<TypeExpr>,
        value: Expr,
        exported: bool,
        span: Span,
    },
    TypeDecl {
        name: String,
        params: Vec<String>,
        body: TypeExpr,
        exported: bool,
        span: Span,
    },
    Import {
        bindings: Vec<ImportBinding>,
        path: String,
        span: Span,
    },
    ForeignImport {
        path: String,
        bindings: Vec<ForeignBinding>,
        span: Span,
    },
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub struct ForeignBinding {
    pub name: String,
    pub type_ann: TypeExpr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ImportBinding {
    pub name: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Expr {
    /// Integer literal with an optional explicit type suffix (e.g. `42i8`). The suffix, when
    /// present, pins the literal's type in the checker, overriding context/default (spec §3.6).
    IntLit(i64, Option<NumSuffix>, Span),
    /// Float literal with an optional explicit type suffix (e.g. `3.14f32`).
    FloatLit(f64, Option<NumSuffix>, Span),
    StringLit(String, Span),
    BoolLit(bool, Span),
    NullLit(Span),
    Ident(String, Span),
    StringInterp(Vec<StringPart>, Span),
    BinaryOp {
        left: Box<Expr>,
        op: BinOp,
        right: Box<Expr>,
        span: Span,
    },
    UnaryOp {
        op: UnaryOp,
        operand: Box<Expr>,
        span: Span,
    },
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        /// True when the argument list ended with an explicit trailing comma
        /// (`f(x,)`), requesting partial application rather than default-fill.
        partial: bool,
        span: Span,
    },
    DotCall {
        receiver: Box<Expr>,
        method: String,
        args: Option<Vec<Expr>>,
        /// True when the argument list ended with an explicit trailing comma.
        partial: bool,
        span: Span,
    },
    Index {
        object: Box<Expr>,
        key: Box<Expr>,
        span: Span,
    },
    If {
        condition: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
        span: Span,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
    },
    Block(Vec<Stmt>, Box<Expr>, Span),
    Function {
        /// Generic type parameters introduced by a leading `<T, ...>` (Phase 0: single-module
        /// monomorphized generics). Empty for ordinary (non-generic) functions, which keeps the
        /// monomorphization pass a no-op.
        type_params: Vec<String>,
        params: Vec<Param>,
        return_type: Option<TypeExpr>,
        body: Box<Expr>,
        span: Span,
    },
    Object(Vec<ObjectField>, Span),
    Array(Vec<Expr>, Span),
    Assign {
        target: String,
        value: Box<Expr>,
        span: Span,
    },
    IndexAssign {
        object: Box<Expr>,
        key: Box<Expr>,
        value: Box<Expr>,
        span: Span,
    },
    Is {
        expr: Box<Expr>,
        pattern: Box<Pattern>,
        span: Span,
    },
    Has {
        expr: Box<Expr>,
        pattern: Box<Pattern>,
        span: Span,
    },
    TupleArgs(Vec<Expr>, Span),
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::IntLit(_, _, s) => *s,
            Expr::FloatLit(_, _, s) => *s,
            Expr::StringLit(_, s) => *s,
            Expr::BoolLit(_, s) => *s,
            Expr::NullLit(s) => *s,
            Expr::Ident(_, s) => *s,
            Expr::StringInterp(_, s) => *s,
            Expr::BinaryOp { span, .. } => *span,
            Expr::UnaryOp { span, .. } => *span,
            Expr::Call { span, .. } => *span,
            Expr::DotCall { span, .. } => *span,
            Expr::Index { span, .. } => *span,
            Expr::If { span, .. } => *span,
            Expr::Match { span, .. } => *span,
            Expr::Block(_, _, s) => *s,
            Expr::Function { span, .. } => *span,
            Expr::Object(_, s) => *s,
            Expr::Array(_, s) => *s,
            Expr::Assign { span, .. } => *span,
            Expr::IndexAssign { span, .. } => *span,
            Expr::Is { span, .. } => *span,
            Expr::Has { span, .. } => *span,
            Expr::TupleArgs(_, s) => *s,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ObjectField {
    Pair(Expr, Expr),
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub enum StringPart {
    Literal(String),
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
    BAnd,
    BOr,
    BXor,
    Shl,
    Shr,
}

/// Unary operators: `~` (bitwise not) and `!` (logical not). Both prefix,
/// right-associative, at the same precedence (tighter than `*`, looser than postfix).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum UnaryOp {
    BNot,
    Not,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: MatchPattern,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum MatchPattern {
    Is(Pattern),
    Has(Pattern),
    Else,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Ident(String, Span),
    TypeName(String, Span),
    Literal(Box<Expr>),
    Object(Vec<ObjectPatternField>, Option<String>, Span),
    Array(Vec<Pattern>, Option<String>, Span),
    Wildcard(Span),
}

#[derive(Debug, Clone)]
pub struct ObjectPatternField {
    pub key: Option<String>,
    pub pattern: Pattern,
    pub value_pattern: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub pattern: Pattern,
    pub type_ann: Option<TypeExpr>,
    /// Default value expression: `(a: Int32, b: Int32 = a + 1)`. When present, the
    /// parameter is optional at call sites. Optional params must be last (enforced
    /// in lin-check). A default may reference parameters declared before it.
    pub default: Option<Box<Expr>>,
}

#[derive(Debug, Clone)]
pub enum TypeExpr {
    Named(String, Span),
    Generic(String, Vec<TypeExpr>, Span),
    Array(Box<TypeExpr>, Span),
    FixedArray(Vec<TypeExpr>, Span),
    Union(Vec<TypeExpr>, Span),
    Function(Vec<TypeExpr>, Box<TypeExpr>, Span),
    Object(Vec<(String, TypeExpr)>, Span),
    TaggedUnion(Vec<TypeExpr>, Span),
    /// A string-literal singleton type, e.g. `"success"` in type position.
    StringLit(String, Span),
}
