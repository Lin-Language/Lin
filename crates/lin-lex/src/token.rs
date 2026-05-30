use lin_common::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// True when a source newline appears between the previous token and this one — even when
    /// that newline was suppressed for block purposes because it falls inside `()`/`[]`/`{}`
    /// (ADR-004). The parser uses this to stop a postfix `[`/`(` on a fresh line from being
    /// glued to the previous expression as an index/call inside an inline lambda body, so a
    /// line-leading array literal reads as its own statement. Defaults to false.
    pub newline_before: bool,
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
        Self { kind, span, newline_before: false }
    }
}
