use lin_common::{Span, NumSuffix};

/// A `//` line comment captured on the lexer's side channel (not part of the token
/// stream — see `Lexer::comments()`). `own_line` is true when nothing but whitespace
/// precedes the comment on its source line; false for a trailing comment after code.
/// `text` is right-trimmed of trailing whitespace at capture for formatter idempotency.
#[derive(Debug, Clone)]
pub struct Comment {
    pub span: Span,
    pub text: String,
    pub own_line: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// True when a source newline appears between the previous token and this one — even when
    /// that newline was suppressed for block purposes because it falls inside `()`/`[]`/`{}`
    /// (ADR-003). The parser uses this to stop a postfix `[`/`(` on a fresh line from being
    /// glued to the previous expression as an index/call inside an inline lambda body, so a
    /// line-leading array literal reads as its own statement. Defaults to false.
    pub newline_before: bool,
    /// 1-based column of this token's first char on its source line (number of chars from the
    /// line start, +1). Computed in the same post-tokenize pass that sets `newline_before`.
    /// Used ONLY by the inline-block / control-flow-branch parsers to apply the offside rule
    /// inside `()`/`[]`/`{}` where ADR-003 suppresses Indent/Dedent/Newline — it does not add
    /// any tokens, so all ADR-003-dependent behaviour is unchanged. Defaults to 0.
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals. Numeric literals carry an optional explicit type suffix (e.g. `42i8`,
    // `3.14f32`); `None` means no suffix (type comes from context/default — spec §2.6, §21).
    StringLit(String),
    IntLit(i64, Option<NumSuffix>),
    FloatLit(f64, Option<NumSuffix>),
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
    QuestionQuestion, // ?? (null-coalescing — coalesces Null only, never Error; ADR-065)
    Eq,
    Arrow,    // =>
    Dot,
    DotDotDot, // ...
    Pipe,     // |
    Amp,      // & (bitwise and)
    Caret,    // ^ (bitwise xor)
    Tilde,    // ~ (bitwise not)
    Bang,     // ! (logical not)

    // Delimiters
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    // `;` — Lin has no semicolons (spec §1.2). Lexed as a distinct token (rather than
    // falling into the `Ident` catch-all) so the parser can emit an actionable diagnostic
    // pointing the user at newline-separated statements instead of "Undefined variable ';'".
    Semicolon,

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
        Self { kind, span, newline_before: false, column: 0 }
    }
}
