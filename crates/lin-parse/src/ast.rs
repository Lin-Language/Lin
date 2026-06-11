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
        /// Span of the bound identifier itself (not the `var` keyword). Used by the checker to
        /// record a `def_span` so find-references/rename can group the binding's uses.
        name_span: Span,
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
    /// Test-only mock: `replace <name> = <expr>`. `name` must be a previously-imported
    /// export; `value` becomes the definition emitted for that export's symbol across the
    /// whole test program (ADR-046). Only valid in a `.test.lin`.
    Replace {
        name: String,
        value: Expr,
        span: Span,
    },
    Expr(Expr),
}

impl Stmt {
    /// The source span of this statement. Struct variants return their own `span` field;
    /// `Stmt::Expr` delegates to the wrapped expression. Mirrors `Expr::span()`.
    pub fn span(&self) -> Span {
        match self {
            Stmt::Val { span, .. } => *span,
            Stmt::Var { span, .. } => *span,
            Stmt::TypeDecl { span, .. } => *span,
            Stmt::Import { span, .. } => *span,
            Stmt::ForeignImport { span, .. } => *span,
            Stmt::Replace { span, .. } => *span,
            Stmt::Expr(e) => e.span(),
        }
    }
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
    /// present, pins the literal's type in the checker, overriding context/default (spec §2.6).
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
    /// Null-coalescing `left ?? right` (ADR-065). Kept as a dedicated form (not desugared in
    /// the parser) so the formatter round-trips `??` exactly as written. Semantics:
    /// `if left != null then left else right` — `left` evaluated once, `right` only when `left`
    /// is Null. Coalesces `Null` only; an `Error` value flows through unchanged.
    Coalesce {
        left: Box<Expr>,
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
        /// Full source extent (callee start .. closing `)`). Additive — see `Expr::full_span`.
        /// `span` is unchanged (the opening `(`), so formatter/coverage consumers are untouched.
        full_span: Span,
    },
    DotCall {
        receiver: Box<Expr>,
        method: String,
        args: Option<Vec<Expr>>,
        /// True when the argument list ended with an explicit trailing comma.
        partial: bool,
        span: Span,
        /// Full source extent (receiver start .. end of method/args). See `Expr::full_span`.
        full_span: Span,
    },
    Index {
        object: Box<Expr>,
        key: Box<Expr>,
        span: Span,
        /// Full source extent (object start .. closing `]`). See `Expr::full_span`.
        full_span: Span,
    },
    If {
        condition: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
        span: Span,
        /// Full source extent (`if` keyword .. end of the last branch). See `Expr::full_span`.
        full_span: Span,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
        /// Full source extent (`match` keyword .. end of the last arm). See `Expr::full_span`.
        full_span: Span,
    },
    /// `Block(stmts, tail, span, full_span)`. `span` is the opening Indent/inline-start token
    /// (unchanged); `full_span` covers the whole block (start .. end of the tail expr).
    Block(Vec<Stmt>, Box<Expr>, Span, Span),
    Function {
        /// Generic type parameters introduced by a leading `<T, ...>` (Phase 0: single-module
        /// monomorphized generics). Empty for ordinary (non-generic) functions, which keeps the
        /// monomorphization pass a no-op.
        type_params: Vec<String>,
        params: Vec<Param>,
        return_type: Option<TypeExpr>,
        body: Box<Expr>,
        span: Span,
        /// Full source extent (opening `(`/`<`/bare-param .. end of body). See `Expr::full_span`.
        full_span: Span,
    },
    /// `Object(fields, span, full_span)`. `span` is the opening `{` (unchanged); `full_span`
    /// covers `{` .. closing `}`.
    Object(Vec<ObjectField>, Span, Span),
    /// `Array(elements, span, full_span)`. `span` is the opening `[` (unchanged); `full_span`
    /// covers `[` .. closing `]`.
    Array(Vec<Expr>, Span, Span),
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
        /// Full source extent (object start .. end of the assigned value). See `Expr::full_span`.
        full_span: Span,
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
    /// The OPENING-token span, byte-identical to what it has always returned. Consumed by the
    /// formatter's comment-attachment (ADR-025), coverage-region mapping, and the LSP type map.
    /// Do NOT widen this; use `full_span()` for the full source extent.
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
            Expr::Coalesce { span, .. } => *span,
            Expr::UnaryOp { span, .. } => *span,
            Expr::Call { span, .. } => *span,
            Expr::DotCall { span, .. } => *span,
            Expr::Index { span, .. } => *span,
            Expr::If { span, .. } => *span,
            Expr::Match { span, .. } => *span,
            Expr::Block(_, _, s, _) => *s,
            Expr::Function { span, .. } => *span,
            Expr::Object(_, s, _) => *s,
            Expr::Array(_, s, _) => *s,
            Expr::Assign { span, .. } => *span,
            Expr::IndexAssign { span, .. } => *span,
            Expr::Is { span, .. } => *span,
            Expr::Has { span, .. } => *span,
            Expr::TupleArgs(_, s) => *s,
        }
    }

    /// The FULL source extent of this expression — opening token start .. closing token/last
    /// child end. Additive companion to `span()` (which stays the opening-token marker). For
    /// compound nodes that carry a recorded `full_span` (Call/DotCall/Index/IndexAssign/Object/
    /// Array/Block/If/Match/Function) it returns that field; for every leaf and any node without
    /// a wider extent it falls back to `span()`. Used by the LSP folding/selection-range
    /// providers to emit AST-precise ranges. NOT consumed by the formatter or codegen.
    pub fn full_span(&self) -> Span {
        match self {
            Expr::Call { full_span, .. } => *full_span,
            Expr::DotCall { full_span, .. } => *full_span,
            Expr::Index { full_span, .. } => *full_span,
            Expr::If { full_span, .. } => *full_span,
            Expr::Match { full_span, .. } => *full_span,
            Expr::Block(_, _, _, full_span) => *full_span,
            Expr::Function { full_span, .. } => *full_span,
            Expr::Object(_, _, full_span) => *full_span,
            Expr::Array(_, _, full_span) => *full_span,
            Expr::IndexAssign { full_span, .. } => *full_span,
            _ => self.span(),
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
    /// Record intersection `A & B` (ADR-061). Record-only: each operand must resolve to an
    /// object/record type; the result is the union of their fields (conflicting field types =
    /// error). Binds tighter than `|`. Resolved into a plain `Type::Object` at resolution time —
    /// no runtime/codegen representation of its own.
    Intersection(Vec<TypeExpr>, Span),
    Function(Vec<TypeExpr>, Box<TypeExpr>, Span),
    Object(Vec<(String, TypeExpr)>, Span),
    /// A typed index-signature object type `{ String: T }` (ADR-055): a dictionary keyed by
    /// arbitrary strings, each mapping to value type `T`. The first box is the KEY type-expr, the
    /// second is the VALUE type-expr. The key type-expr must resolve to `String` (it may be a type
    /// alias such as `StopID = String`); this is validated at type-resolution time. The key
    /// type-expr is preserved (rather than collapsed to `String`) so the formatter can round-trip
    /// the alias name the user wrote.
    IndexSig(Box<TypeExpr>, Box<TypeExpr>, Span),
    TaggedUnion(Vec<TypeExpr>, Span),
    /// A string-literal singleton type, e.g. `"success"` in type position.
    StringLit(String, Span),
}
