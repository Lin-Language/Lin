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
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0, diagnostics: Vec::new(), error_count_at_stmt_start: 0 }
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
