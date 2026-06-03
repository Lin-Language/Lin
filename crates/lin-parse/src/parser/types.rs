use lin_common::Span;
use lin_lex::TokenKind;
use crate::ast::*;
use super::Parser;

impl Parser {
    pub(crate) fn parse_type_expr(&mut self) -> TypeExpr {
        let first = self.parse_type_primary();
        // A `|` continuation may sit on the next (indented) line, e.g.
        // `type R =⏎  { .. }⏎  | { .. }` (first variant without a leading pipe). Peek past
        // newlines via save/restore: only treat them as a continuation when a `|` follows,
        // so a real statement boundary (newline not followed by `|`) is left intact.
        if self.check(TokenKind::Pipe) || self.newline_precedes_pipe() {
            let mut types = vec![first];
            loop {
                self.skip_newlines();
                if !self.check(TokenKind::Pipe) {
                    break;
                }
                self.advance();
                self.skip_newlines();
                types.push(self.parse_type_primary());
            }
            TypeExpr::Union(types, Span::dummy())
        } else {
            first
        }
    }

    pub(crate) fn parse_type_expr_with_leading_pipe(&mut self) -> TypeExpr {
        if self.check(TokenKind::Pipe) {
            let mut types = Vec::new();
            while self.check(TokenKind::Pipe) {
                self.advance();
                self.skip_newlines();
                types.push(self.parse_type_primary());
                self.skip_newlines();
            }
            if types.len() == 1 {
                types.into_iter().next().unwrap()
            } else {
                TypeExpr::Union(types, Span::dummy())
            }
        } else {
            self.parse_type_expr()
        }
    }

    pub(crate) fn parse_type_primary(&mut self) -> TypeExpr {
        let base = match self.peek_kind() {
            TokenKind::LParen => {
                // Function type: (T1, T2) => U
                self.advance();
                let mut params = Vec::new();
                while !self.check(TokenKind::RParen) && !self.is_at_end() {
                    let loop_start = self.pos;
                    params.push(self.parse_type_expr());
                    if self.check(TokenKind::Comma) {
                        self.advance();
                    }
                    if self.ensure_progress(loop_start) {
                        continue;
                    }
                }
                self.expect(TokenKind::RParen);
                self.expect(TokenKind::Arrow);
                let ret = self.parse_type_primary();
                TypeExpr::Function(params, Box::new(ret), Span::dummy())
            }
            TokenKind::LBrace => {
                // Object type
                let span = self.current_span();
                self.advance();
                self.skip_newlines();
                let mut fields = Vec::new();
                while !self.check(TokenKind::RBrace) && !self.is_at_end() {
                    if let TokenKind::StringLit(_) = self.peek_kind() {
                        let key = if let TokenKind::StringLit(s) = self.advance_kind() { s } else { String::new() };
                        self.expect(TokenKind::Colon);
                        let ty = self.parse_type_expr();
                        fields.push((key, ty));
                    } else {
                        break;
                    }
                    if self.check(TokenKind::Comma) {
                        self.advance();
                    }
                    self.skip_newlines();
                }
                self.expect(TokenKind::RBrace);
                TypeExpr::Object(fields, span)
            }
            TokenKind::LBracket => {
                // Fixed-length array type
                let span = self.current_span();
                self.advance();
                let mut types = Vec::new();
                while !self.check(TokenKind::RBracket) && !self.is_at_end() {
                    let loop_start = self.pos;
                    types.push(self.parse_type_expr());
                    if self.check(TokenKind::Comma) {
                        self.advance();
                    }
                    if self.ensure_progress(loop_start) {
                        continue;
                    }
                }
                self.expect(TokenKind::RBracket);
                TypeExpr::FixedArray(types, span)
            }
            TokenKind::Ident(_) => {
                let span = self.current_span();
                let name = self.expect_ident();
                if self.check(TokenKind::Lt) {
                    self.advance();
                    let mut args = Vec::new();
                    loop {
                        args.push(self.parse_type_expr());
                        if !self.check(TokenKind::Comma) {
                            break;
                        }
                        self.advance();
                    }
                    self.expect(TokenKind::Gt);
                    TypeExpr::Generic(name, args, span)
                } else {
                    TypeExpr::Named(name, span)
                }
            }
            TokenKind::StringLit(_) => {
                // A string-literal singleton type, e.g. `"success"`.
                let span = self.current_span();
                let s = if let TokenKind::StringLit(s) = self.advance_kind() { s } else { String::new() };
                TypeExpr::StringLit(s, span)
            }
            _ => {
                let span = self.current_span();
                self.advance();
                TypeExpr::Named("Unknown".to_string(), span)
            }
        };

        // Check for postfix `[]` (array type), repeated for nested arrays: `T[][]` is
        // `Array(Array(T))`. A single `if` only matched one `[]`, so `Int32[][]` / `UInt8[][]`
        // failed to parse (the second `[` was left dangling → "expected Eq, got LBracket").
        let mut ty = base;
        while self.check(TokenKind::LBracket) && self.check_ahead(TokenKind::RBracket, 1) {
            self.advance(); // [
            self.advance(); // ]
            ty = TypeExpr::Array(Box::new(ty), Span::dummy());
        }
        ty
    }
}
