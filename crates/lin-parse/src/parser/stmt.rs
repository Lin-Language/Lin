use lin_lex::TokenKind;
use crate::ast::*;
use super::Parser;

impl Parser {
    pub(crate) fn parse_statement(&mut self) -> Option<Stmt> {
        self.skip_newlines();
        if self.is_at_end() {
            return None;
        }

        match self.peek_kind() {
            TokenKind::Export => self.parse_export(),
            TokenKind::Val => Some(self.parse_val(false)),
            TokenKind::Var => Some(self.parse_var(false)),
            TokenKind::Type => Some(self.parse_type_decl(false)),
            TokenKind::Import => {
                // Peek ahead to check for `import foreign`
                if self.peek_ahead_is_foreign() {
                    Some(self.parse_foreign_import())
                } else {
                    Some(self.parse_import())
                }
            }
            // `replace` is a CONTEXTUAL keyword (not reserved — `std/string` exports a
            // `replace` fn): recognise it only in the statement-leading shape
            // `replace <ident> = <expr>` (a single `=`, not `==`). Anything else lexed as
            // `replace` stays a normal identifier expression.
            TokenKind::Ident(w) if w == "replace" && self.is_replace_stmt() => {
                Some(self.parse_replace())
            }
            _ => {
                let expr = self.parse_expr();
                Some(Stmt::Expr(expr))
            }
        }
    }

    /// Lookahead for the `replace <ident> = ...` (NOT `==`) statement shape.
    fn is_replace_stmt(&self) -> bool {
        // pos: `replace`, pos+1: an identifier, pos+2: a single `=`.
        self.check_ahead(TokenKind::Ident(String::new()), 1)
            && self.check_ahead(TokenKind::Eq, 2)
    }

    /// Parse `replace <name> = <expr>` (a test-only mock — ADR-071).
    pub(crate) fn parse_replace(&mut self) -> Stmt {
        let span = self.current_span();
        self.advance(); // `replace`
        let name = self.expect_ident();
        self.expect(TokenKind::Eq);
        self.skip_newlines();
        let value = self.parse_expr_or_block();
        Stmt::Replace { name, value, span }
    }

    pub(crate) fn parse_export(&mut self) -> Option<Stmt> {
        self.advance(); // skip 'export'
        self.skip_newlines();
        match self.peek_kind() {
            TokenKind::Val => Some(self.parse_val(true)),
            TokenKind::Var => Some(self.parse_var(true)),
            TokenKind::Type => Some(self.parse_type_decl(true)),
            _ => None,
        }
    }

    pub(crate) fn parse_val(&mut self, exported: bool) -> Stmt {
        let span_start = self.current_span();
        self.advance(); // skip 'val'
        let pattern = self.parse_binding_pattern();
        let type_ann = if self.check(TokenKind::Colon) {
            self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };
        self.expect(TokenKind::Eq);
        self.skip_newlines();
        let value = self.parse_expr_or_block();
        Stmt::Val { pattern, type_ann, value, exported, span: span_start }
    }

    pub(crate) fn parse_var(&mut self, exported: bool) -> Stmt {
        let span_start = self.current_span();
        self.advance(); // skip 'var'
        let name = self.expect_ident();
        let type_ann = if self.check(TokenKind::Colon) {
            self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };
        self.expect(TokenKind::Eq);
        self.skip_newlines();
        let value = self.parse_expr_or_block();
        Stmt::Var { name, type_ann, value, exported, span: span_start }
    }

    pub(crate) fn parse_type_decl(&mut self, exported: bool) -> Stmt {
        let span = self.current_span();
        self.advance(); // skip 'type'
        let name = self.expect_ident();
        let params = if self.check(TokenKind::Lt) {
            self.advance();
            let mut params = Vec::new();
            loop {
                params.push(self.expect_ident());
                if !self.check(TokenKind::Comma) {
                    break;
                }
                self.advance();
            }
            self.expect(TokenKind::Gt);
            params
        } else {
            Vec::new()
        };
        self.expect(TokenKind::Eq);
        // A type body may continue on indented lines (spec §19 tagged-union form:
        // `type R =⏎  | { .. }⏎  | { .. }`). The lexer emits `Newline Indent` after `=`,
        // so skip the Indent too — otherwise the leading `|` is unreachable and parsing
        // fails with `unexpected token Pipe`. Track whether we opened an indented block so
        // its matching Dedent can be consumed after the body, keeping the statement boundary
        // clean for the next top-level item.
        self.skip_newlines();
        let indented = self.check(TokenKind::Indent);
        if indented {
            self.advance();
            self.skip_newlines();
        }
        let body = self.parse_type_expr_with_leading_pipe();
        if indented {
            // Consume the trailing Newline(s)/Dedent that close the indented body.
            while matches!(self.peek_kind(), TokenKind::Newline | TokenKind::Dedent) {
                let was_dedent = self.check(TokenKind::Dedent);
                self.advance();
                if was_dedent {
                    break;
                }
            }
        }
        Stmt::TypeDecl { name, params, body, exported, span }
    }

    pub(crate) fn parse_import(&mut self) -> Stmt {
        let span = self.current_span();
        self.advance(); // skip 'import'
        self.expect(TokenKind::LBrace);
        self.skip_newlines();
        let mut bindings = Vec::new();
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            let name = self.expect_ident();
            let alias = if self.check(TokenKind::As) {
                self.advance();
                Some(self.expect_ident())
            } else {
                None
            };
            bindings.push(ImportBinding { name, alias });
            if self.check(TokenKind::Comma) {
                self.advance();
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RBrace);
        self.skip_newlines();
        self.expect_keyword(TokenKind::From);
        let path = self.expect_string();
        Stmt::Import { bindings, path, span }
    }

    pub(crate) fn peek_ahead_is_foreign(&self) -> bool {
        // Check if the token after 'import' is 'foreign'
        if self.pos + 1 < self.tokens.len() {
            matches!(self.tokens[self.pos + 1].kind, TokenKind::Foreign)
        } else {
            false
        }
    }

    pub(crate) fn parse_foreign_import(&mut self) -> Stmt {
        let span = self.current_span();
        self.advance(); // skip 'import'
        self.advance(); // skip 'foreign'
        let path = self.expect_string();
        // Parse indented block of `val name: Type` declarations
        self.skip_newlines();
        let mut bindings = Vec::new();
        if self.check(TokenKind::Indent) {
            self.advance(); // consume Indent
            loop {
                self.skip_newlines();
                if self.check(TokenKind::Dedent) || self.is_at_end() {
                    break;
                }
                let binding_span = self.current_span();
                self.expect_keyword(TokenKind::Val);
                let name = self.expect_ident();
                self.expect(TokenKind::Colon);
                let type_ann = self.parse_type_expr();
                bindings.push(ForeignBinding { name, type_ann, span: binding_span });
                self.skip_newlines();
            }
            if self.check(TokenKind::Dedent) {
                self.advance(); // consume Dedent
            }
        }
        Stmt::ForeignImport { path, bindings, span }
    }
}
