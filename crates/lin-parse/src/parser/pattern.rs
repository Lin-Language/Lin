use lin_lex::TokenKind;
use crate::ast::*;
use super::Parser;

impl Parser {
    /// Parse one match arm. `arm_col`, when set, is the offside column of the match's arms
    /// (inline / inside-parens mode, where ADR-004 suppresses Indent/Dedent). The arm body is
    /// then parsed as a column-delimited block whose exclusive floor is `arm_col - 1`: a
    /// multi-statement arm body stays together, but the next arm — aligned at `arm_col` — ends
    /// it, and a statement dedented below the arm column ends the whole match. `None` means the
    /// arms are an Indent/Dedent block (top-level), where the body uses the existing logic.
    pub(crate) fn parse_match_arm(&mut self, arm_col: Option<u32>) -> MatchArm {
        let span = self.current_span();
        let pattern = match self.peek_kind() {
            TokenKind::Is => {
                self.advance();
                MatchPattern::Is(self.parse_pattern())
            }
            TokenKind::Has => {
                self.advance();
                MatchPattern::Has(self.parse_pattern())
            }
            TokenKind::Else => {
                self.advance();
                MatchPattern::Else
            }
            _ => {
                self.advance();
                MatchPattern::Else
            }
        };

        let guard = if self.check(TokenKind::When) {
            self.advance();
            Some(self.parse_expr())
        } else {
            None
        };

        self.expect(TokenKind::Arrow);
        self.skip_newlines();
        let body = if self.check(TokenKind::Indent) {
            self.parse_block()
        } else if let Some(ac) = arm_col {
            // Inline / inside-parens arm: column-delimited block whose EXCLUSIVE floor is the arm
            // column itself. Continuation statements of a multi-statement body are indented past
            // `ac` (kept); the next arm (aligned at `ac`) and any dedent to/below `ac` end this
            // body. The arm head (`has 1 => ...`) shares the arm column, so it correctly ends the
            // previous arm's body rather than being swallowed into it. `match_arm_floor` makes a
            // following `is`/`has` at <= `ac` read as the next arm, not an infix test on the body.
            let prev_floor = self.match_arm_floor;
            self.match_arm_floor = Some(ac);
            let body = self.parse_branch_block(ac);
            self.match_arm_floor = prev_floor;
            body
        } else {
            self.parse_expr()
        };
        self.skip_newlines();

        MatchArm { pattern, guard, body, span }
    }

    pub(crate) fn parse_pattern(&mut self) -> Pattern {
        match self.peek_kind() {
            TokenKind::LBrace => self.parse_object_pattern(),
            TokenKind::LBracket => self.parse_array_pattern(),
            TokenKind::StringLit(_) => {
                let span = self.current_span();
                if let TokenKind::StringLit(s) = self.advance_kind() {
                    Pattern::Literal(Box::new(Expr::StringLit(s, span)))
                } else {
                    unreachable!()
                }
            }
            TokenKind::IntLit(..) => {
                let span = self.current_span();
                if let TokenKind::IntLit(v, suffix) = self.advance_kind() {
                    Pattern::Literal(Box::new(Expr::IntLit(v, suffix, span)))
                } else {
                    unreachable!()
                }
            }
            TokenKind::FloatLit(..) => {
                let span = self.current_span();
                if let TokenKind::FloatLit(v, suffix) = self.advance_kind() {
                    Pattern::Literal(Box::new(Expr::FloatLit(v, suffix, span)))
                } else {
                    unreachable!()
                }
            }
            TokenKind::True => {
                let span = self.current_span();
                self.advance();
                Pattern::Literal(Box::new(Expr::BoolLit(true, span)))
            }
            TokenKind::False => {
                let span = self.current_span();
                self.advance();
                Pattern::Literal(Box::new(Expr::BoolLit(false, span)))
            }
            TokenKind::Null => {
                let span = self.current_span();
                self.advance();
                Pattern::TypeName("Null".to_string(), span)
            }
            TokenKind::Ident(_) => {
                let span = self.current_span();
                let name = self.expect_ident();
                // If starts with uppercase, it's a type name
                if name.chars().next().is_some_and(|c| c.is_uppercase()) {
                    Pattern::TypeName(name, span)
                } else {
                    Pattern::Ident(name, span)
                }
            }
            _ => {
                let span = self.current_span();
                self.advance();
                Pattern::Wildcard(span)
            }
        }
    }

    pub(crate) fn parse_object_pattern(&mut self) -> Pattern {
        let span = self.current_span();
        self.advance(); // {
        self.skip_newlines();
        let mut fields = Vec::new();
        let mut rest = None;

        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            if self.check(TokenKind::DotDotDot) {
                self.advance();
                rest = Some(self.expect_ident());
                if self.check(TokenKind::Comma) {
                    self.advance();
                }
                self.skip_newlines();
                continue;
            }

            // Could be "key": pattern or just name (shorthand)
            if let TokenKind::StringLit(_) = self.peek_kind() {
                let key_span = self.current_span();
                let key = if let TokenKind::StringLit(s) = self.advance_kind() { s } else { String::new() };
                if self.check(TokenKind::Colon) {
                    self.advance();
                    self.skip_newlines();
                    // Check if there's a literal value pattern (for tagged unions like "type": "success")
                    let value_pat = match self.peek_kind() {
                        TokenKind::StringLit(_) => {
                            let vs = self.current_span();
                            if let TokenKind::StringLit(s) = self.advance_kind() {
                                // Check if next is comma or } - then it's a value literal pattern
                                Some(Expr::StringLit(s.clone(), vs))
                            } else {
                                None
                            }
                        }
                        _ => {
                            let pat = self.parse_binding_pattern();
                            fields.push(ObjectPatternField { key: Some(key), pattern: pat, value_pattern: None });
                            if self.check(TokenKind::Comma) { self.advance(); }
                            self.skip_newlines();
                            continue;
                        }
                    };
                    if let Some(vp) = value_pat {
                        fields.push(ObjectPatternField {
                            key: Some(key.clone()),
                            pattern: Pattern::Ident(key, key_span),
                            value_pattern: Some(vp),
                        });
                    }
                } else {
                    fields.push(ObjectPatternField {
                        key: Some(key.clone()),
                        pattern: Pattern::Ident(key, key_span),
                        value_pattern: None,
                    });
                }
            } else {
                // Shorthand: name
                let name_span = self.current_span();
                let name = self.expect_ident();
                fields.push(ObjectPatternField {
                    key: Some(name.clone()),
                    pattern: Pattern::Ident(name, name_span),
                    value_pattern: None,
                });
            }

            if self.check(TokenKind::Comma) {
                self.advance();
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RBrace);
        Pattern::Object(fields, rest, span)
    }

    pub(crate) fn parse_array_pattern(&mut self) -> Pattern {
        let span = self.current_span();
        self.advance(); // [
        self.skip_newlines();
        let mut elements = Vec::new();
        let mut rest = None;

        while !self.check(TokenKind::RBracket) && !self.is_at_end() {
            if self.check(TokenKind::DotDotDot) {
                self.advance();
                rest = Some(self.expect_ident());
            } else {
                elements.push(self.parse_binding_pattern());
            }
            if self.check(TokenKind::Comma) {
                self.advance();
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RBracket);
        Pattern::Array(elements, rest, span)
    }

    pub(crate) fn parse_binding_pattern(&mut self) -> Pattern {
        match self.peek_kind() {
            TokenKind::LBrace => self.parse_object_pattern(),
            TokenKind::LBracket => self.parse_array_pattern(),
            TokenKind::Ident(_) => {
                let span = self.current_span();
                let name = self.expect_ident();
                Pattern::Ident(name, span)
            }
            _ => {
                let span = self.current_span();
                self.advance();
                Pattern::Wildcard(span)
            }
        }
    }
}
