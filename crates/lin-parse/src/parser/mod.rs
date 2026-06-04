use lin_common::{Diagnostic, Span};
use lin_lex::{Token, TokenKind};
use crate::ast::*;

mod stmt;
mod expr;
mod function;
mod pattern;
mod types;

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    pub diagnostics: Vec<Diagnostic>,
    /// Number of diagnostics at the start of the current statement parse.
    /// Used to detect whether an error occurred during a statement so we can synchronize.
    error_count_at_stmt_start: usize,
    /// When true, `parse_is_has_expr` will NOT consume a trailing `is`/`has` infix operator.
    /// Set only while parsing an inline (inside-parens) match scrutinee, where ADR-004
    /// suppresses the Newline that would otherwise terminate the scrutinee before the first
    /// `has`/`is` arm. Reset to false on entry to any delimited group (`(`/`[`/`{`) so a
    /// parenthesised `match (x is Foo) ...` scrutinee still parses the inner `is` test.
    suppress_is_has: bool,
    /// When set, the arm column of an inline (inside-parens) match currently being parsed. An
    /// `is`/`has` token at a column <= this floor begins the NEXT arm, not an infix type-test
    /// on the current arm body, so `parse_is_has_expr` declines to consume it. An `is`/`has`
    /// written inline within a body (`has 0 => x is Foo`) is at a strictly greater column and so
    /// is still parsed as an infix test. Reset on entry to any delimited group.
    match_arm_floor: Option<u32>,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0, diagnostics: Vec::new(), error_count_at_stmt_start: 0, suppress_is_has: false, match_arm_floor: None }
    }

    pub fn parse_module(&mut self) -> Module {
        let mut statements = Vec::new();
        self.skip_newlines();
        while !self.is_at_end() {
            self.error_count_at_stmt_start = self.diagnostics.len();
            if let Some(stmt) = self.parse_statement() {
                statements.push(stmt);
            }
            // If the statement parse produced a new error, synchronize to the
            // next statement boundary so subsequent statements still parse cleanly.
            if self.diagnostics.len() > self.error_count_at_stmt_start {
                self.synchronize();
            }
            self.skip_newlines();
        }
        Module {
            span: Span::dummy(),
            statements,
        }
    }

    pub(crate) fn skip_continuation_newline(&mut self, expected: TokenKind) {
        if self.check(TokenKind::Newline) {
            let saved = self.pos;
            self.skip_newlines();
            if std::mem::discriminant(&self.peek_kind()) == std::mem::discriminant(&expected) {
                // Continuation line — stay at new position
            } else {
                self.pos = saved;
            }
        }
    }

    /// True when the next two tokens have the given kinds AND are adjacent in the source
    /// (no whitespace between them), so `> >` (generic close) is not mistaken for `>>`.
    pub(crate) fn adjacent_pair(&self, first: TokenKind, second: TokenKind) -> bool {
        if self.pos + 1 >= self.tokens.len() {
            return false;
        }
        let a = &self.tokens[self.pos];
        let b = &self.tokens[self.pos + 1];
        std::mem::discriminant(&a.kind) == std::mem::discriminant(&first)
            && std::mem::discriminant(&b.kind) == std::mem::discriminant(&second)
            && a.span.file_id == b.span.file_id
            && a.span.end == b.span.start
    }

    // --- Helpers ---

    pub(crate) fn prev_was_dedent(&self) -> bool {
        if self.pos == 0 { return false; }
        matches!(self.tokens[self.pos - 1].kind, TokenKind::Dedent)
    }

    /// True when the current token begins a new source line (a newline precedes it), even one
    /// suppressed inside `()`/`[]`/`{}` (ADR-004). Used to stop a line-leading postfix `[`/`(`
    /// from gluing onto the previous expression as an index/call inside an inline lambda body.
    pub(crate) fn at_line_start(&self) -> bool {
        self.pos < self.tokens.len() && self.tokens[self.pos].newline_before
    }

    pub(crate) fn peek_kind(&self) -> TokenKind {
        if self.pos < self.tokens.len() {
            self.tokens[self.pos].kind.clone()
        } else {
            TokenKind::Eof
        }
    }

    pub(crate) fn check(&self, kind: TokenKind) -> bool {
        std::mem::discriminant(&self.peek_kind()) == std::mem::discriminant(&kind)
    }

    pub(crate) fn check_ahead(&self, kind: TokenKind, offset: usize) -> bool {
        let idx = self.pos + offset;
        if idx < self.tokens.len() {
            std::mem::discriminant(&self.tokens[idx].kind) == std::mem::discriminant(&kind)
        } else {
            false
        }
    }

    pub(crate) fn advance(&mut self) -> &Token {
        let tok = &self.tokens[self.pos.min(self.tokens.len() - 1)];
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    pub(crate) fn advance_kind(&mut self) -> TokenKind {
        let kind = self.peek_kind();
        self.advance();
        kind
    }

    pub(crate) fn current_span(&self) -> Span {
        if self.pos < self.tokens.len() {
            self.tokens[self.pos].span
        } else {
            Span::dummy()
        }
    }

    /// 1-based source column of the current token (0 at end of stream). Used ONLY by the
    /// inline-block / control-flow-branch parsers to apply the offside rule inside `()`/`[]`/`{}`
    /// where ADR-004 suppresses Indent/Dedent/Newline. Does not consult or alter the token
    /// stream shape.
    pub(crate) fn current_column(&self) -> u32 {
        if self.pos < self.tokens.len() {
            self.tokens[self.pos].column
        } else {
            0
        }
    }

    /// 1-based column of the FIRST token on the current token's source line — i.e. the
    /// indentation of the statement/line the current token sits on, found by scanning back
    /// over tokens with no source newline before them (`newline_before == false`).
    ///
    /// This is the right offside anchor for a control-flow branch whose keyword is NOT at the
    /// start of its line — e.g. `val x = if cond then\n  A\nelse\n  B`: the `if` keyword is far
    /// to the right, but its wrapped branches are indented relative to the enclosing statement
    /// (`val`), not the keyword. Anchoring on the keyword column would set an impossibly high
    /// floor and collapse the branch to empty (then orphan the `else`). Inside `()`/`[]`/`{}`
    /// the lexer still records real columns (ADR-004 suppresses Indent/Dedent, not columns).
    pub(crate) fn line_start_column(&self) -> u32 {
        if self.pos >= self.tokens.len() {
            return 0;
        }
        let mut i = self.pos;
        while i > 0 && !self.tokens[i].newline_before {
            i -= 1;
        }
        self.tokens[i].column
    }

    pub(crate) fn expect(&mut self, kind: TokenKind) {
        if self.check(kind.clone()) {
            self.advance();
        } else {
            let span = self.current_span();
            let got = self.peek_kind();
            self.diagnostics.push(Diagnostic::error(
                span,
                format!("expected {:?}, got {:?}", kind, got),
            ));
        }
    }


    pub(crate) fn expect_keyword(&mut self, kind: TokenKind) {
        self.expect(kind);
    }

    pub(crate) fn expect_ident(&mut self) -> String {
        if let TokenKind::Ident(name) = self.peek_kind() {
            self.advance();
            name
        } else {
            let span = self.current_span();
            let got = self.peek_kind();
            self.diagnostics.push(Diagnostic::error(
                span,
                format!("expected identifier, got {:?}", got),
            ));
            String::new()
        }
    }

    pub(crate) fn expect_string(&mut self) -> String {
        if let TokenKind::StringLit(s) = self.peek_kind() {
            self.advance();
            s
        } else {
            let span = self.current_span();
            let got = self.peek_kind();
            self.diagnostics.push(Diagnostic::error(
                span,
                format!("expected string literal, got {:?}", got),
            ));
            String::new()
        }
    }

    pub(crate) fn skip_newlines(&mut self) {
        while self.check(TokenKind::Newline) {
            self.advance();
        }
    }

    /// No-progress backstop for delimiter-bounded loops. Call at the bottom of a loop
    /// body with the cursor position recorded at the top. If the cursor did not advance,
    /// the loop would spin forever on an unexpected token whose handler (e.g. a
    /// non-advancing `expect_*`) consumed nothing — so emit a diagnostic, skip one token
    /// to guarantee termination, and return `true` to signal the caller may continue.
    /// Returns `false` when progress was made (the common case). A parser must always
    /// make progress and emit diagnostics — it must never hang.
    pub(crate) fn ensure_progress(&mut self, start_pos: usize) -> bool {
        if self.pos == start_pos {
            let span = self.current_span();
            let got = self.peek_kind();
            self.diagnostics.push(Diagnostic::error(
                span,
                format!("unexpected {:?}", got),
            ));
            self.advance();
            true
        } else {
            false
        }
    }

    /// True when the upcoming token(s) are one or more Newlines followed by a `|`.
    /// Pure lookahead — does not advance. Used to recognise a union-variant `|` that
    /// continues onto the next line when the first variant had no leading pipe.
    pub(crate) fn newline_precedes_pipe(&self) -> bool {
        if !self.check(TokenKind::Newline) {
            return false;
        }
        let mut i = self.pos;
        while matches!(self.tokens.get(i).map(|t| &t.kind), Some(TokenKind::Newline)) {
            i += 1;
        }
        matches!(self.tokens.get(i).map(|t| &t.kind), Some(TokenKind::Pipe))
    }

    pub(crate) fn skip_newlines_and_indent(&mut self) {
        while matches!(self.peek_kind(), TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent) {
            self.advance();
        }
    }

    pub(crate) fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len() || self.tokens[self.pos].kind == TokenKind::Eof
    }

    /// Advance past tokens until we reach a statement boundary:
    /// a Newline/Dedent at the top level, or EOF.
    /// This lets parse_module continue reporting errors for later statements.
    pub(crate) fn synchronize(&mut self) {
        // Skip until a Newline, Dedent, or EOF that looks like a statement boundary.
        // Also stop if we see a statement-starting keyword — it means we've recovered.
        loop {
            match self.peek_kind() {
                TokenKind::Eof => break,
                TokenKind::Newline | TokenKind::Dedent => {
                    self.advance();
                    break;
                }
                // Stop before statement-starting keywords so the next loop
                // iteration in parse_module picks them up cleanly.
                TokenKind::Val
                | TokenKind::Var
                | TokenKind::Type
                | TokenKind::Import
                | TokenKind::Export => break,
                _ => { self.advance(); }
            }
        }
    }
}

#[cfg(test)]
mod hang_regression_tests {
    use super::*;
    use lin_lex::Lexer;

    /// Parse `source` and return the diagnostics. The mere fact that this function
    /// RETURNS is the regression assertion: on the pre-fix parser each of these inputs
    /// spun forever in a delimiter-bounded loop whose unexpected token was neither the
    /// closing delimiter nor a comma, and whose handler (a non-advancing `expect_*`)
    /// made no progress. A parser must always terminate and emit diagnostics.
    fn diagnostics_for(source: &str) -> Vec<Diagnostic> {
        let mut lexer = Lexer::new(source, 0);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let _module = parser.parse_module();
        parser.diagnostics
    }

    #[test]
    fn object_pattern_with_non_ident_shorthand_terminates_with_error() {
        // `parse_object_pattern` shorthand branch called the non-advancing `expect_ident`
        // on an IntLit → infinite loop. Must terminate and report an error.
        let diags = diagnostics_for("val { 1 } = x\n");
        assert!(!diags.is_empty(), "expected a diagnostic for `val {{ 1 }} = x`");
    }

    #[test]
    fn import_with_non_ident_binding_terminates_with_error() {
        let diags = diagnostics_for("import { 1 } from \"x\"\n");
        assert!(!diags.is_empty(), "expected a diagnostic for `import {{ 1 }} from`");
    }

    #[test]
    fn foreign_import_with_non_val_line_terminates_with_error() {
        // `parse_foreign_import` expected `val` to open each binding; a non-`val` first
        // line left `expect_keyword(Val)` non-advancing → infinite loop.
        let src = "import foreign \"x\"\n  notval y: Int32\n";
        let diags = diagnostics_for(src);
        assert!(!diags.is_empty(), "expected a diagnostic for foreign import non-val line");
    }

    #[test]
    fn param_object_pattern_with_non_ident_terminates_with_error() {
        // Reached via `parse_param` → `parse_object_pattern`: `({ 1 }) => 0`.
        let diags = diagnostics_for("val f = ({ 1 }) => 0\n");
        assert!(!diags.is_empty(), "expected a diagnostic for `({{ 1 }}) =>`");
    }

    #[test]
    fn match_arm_object_pattern_with_non_ident_terminates_with_error() {
        // Reached via a match arm pattern: `has { 1 } => ...`.
        let src = "val r = match x\n  has { 1 } => 0\n  else => 1\n";
        let diags = diagnostics_for(src);
        assert!(!diags.is_empty(), "expected a diagnostic for `has {{ 1 }} =>`");
    }
}
