use lin_common::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    StringLit(String),
    IntLit(i64),
    FloatLit(f64),
    True,
    False,
    Null,

    // Identifier
    Ident(String),

    // Keywords
    Val,
    Var,
    Type,
    Export,
    If,
    Then,
    Else,
    Match,
    Is,
    Has,
    When,
    Import,
    From,
    As,
    Foreign,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
    Eq,
    Arrow,    // =>
    Dot,
    DotDotDot, // ...
    Pipe,     // |
    Amp,      // & (bitwise and)
    Caret,    // ^ (bitwise xor)
    Tilde,    // ~ (bitwise not)

    // Delimiters
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,

    // String interpolation
    InterpolStart, // ${
    InterpolEnd,   // } closing interpolation
    InterpString(Vec<InterpPart>),

    // Indentation
    Newline,
    Indent,
    Dedent,

    // End of file
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InterpPart {
    Literal(String),
    Expr(Vec<Token>),
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}
