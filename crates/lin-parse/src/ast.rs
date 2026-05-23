use lin_common::Span;

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
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub struct ImportBinding {
    pub name: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Expr {
    IntLit(i64, Span),
    FloatLit(f64, Span),
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
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        span: Span,
    },
    DotCall {
        receiver: Box<Expr>,
        method: String,
        args: Option<Vec<Expr>>,
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
            Expr::IntLit(_, s) => *s,
            Expr::FloatLit(_, s) => *s,
            Expr::StringLit(_, s) => *s,
            Expr::BoolLit(_, s) => *s,
            Expr::NullLit(s) => *s,
            Expr::Ident(_, s) => *s,
            Expr::StringInterp(_, s) => *s,
            Expr::BinaryOp { span, .. } => *span,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}
