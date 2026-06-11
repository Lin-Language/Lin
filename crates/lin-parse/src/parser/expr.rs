use lin_common::Diagnostic;
use lin_lex::TokenKind;
use crate::ast::*;
use super::Parser;

/// True when `LIN_FMT_ALLOW_TRAILING_COMMA` is set — an escape hatch so `lin fmt` can parse
/// legacy files that still contain literal trailing commas and rewrite them away. Normal
/// compilation leaves it unset, so trailing commas in array/object literals stay an error.
fn allow_trailing_comma() -> bool {
    std::env::var_os("LIN_FMT_ALLOW_TRAILING_COMMA").is_some()
}

impl Parser {
    pub(crate) fn parse_expr(&mut self) -> Expr {
        self.parse_coalesce_expr()
    }

    /// Null-coalescing `left ?? right` — the LOWEST binary rung (rung 13, below `||`; ADR-065).
    /// Left-associative, short-circuiting (semantics fixed in lin-check/lowering: `left` once,
    /// `right` only when `left` is Null). To match JS, an UNPARENTHESISED mix of `??` directly with
    /// `&&`/`||` is a parse error in BOTH directions (`a || b ?? c` and `a ?? b || c`) — wrap the
    /// logical sub-expression in parens. Continuation lines mirror `||`/`&&` (ADR-005).
    pub(crate) fn parse_coalesce_expr(&mut self) -> Expr {
        // Transient signal: did the operand we are about to parse consume a TOP-LEVEL `||`/`&&`?
        // We reset before each operand and read it right after, so a parenthesised group (which is
        // parsed by a NESTED parse_coalesce_expr that restores the flag on exit) does not leak.
        let saved_logical = self.produced_unparenthesized_logical;
        self.produced_unparenthesized_logical = false;
        let mut left = self.parse_or_expr();
        let mut left_was_logical = self.produced_unparenthesized_logical;

        // No `??` follows: nothing to coalesce, nothing to check. Restore the caller's flag (so a
        // parenthesised `(a || b)` does not leak a top-level-logical signal upward) and return.
        self.skip_continuation_newline(TokenKind::QuestionQuestion);
        if !self.check(TokenKind::QuestionQuestion) {
            self.produced_unparenthesized_logical = saved_logical;
            return left;
        }

        loop {
            self.skip_continuation_newline(TokenKind::QuestionQuestion);
            if !self.check(TokenKind::QuestionQuestion) { break; }
            let span = self.current_span();
            // `a || b ?? c` / `a && b ?? c`: the left operand mixed `??` with `||`/`&&` unparen'd.
            if left_was_logical {
                self.error_mixed_coalesce(span);
            }
            self.advance();
            self.skip_newlines();
            self.produced_unparenthesized_logical = false;
            let right = self.parse_or_expr();
            // `a ?? b || c` / `a ?? b && c`: the right operand mixed unparenthesised.
            if self.produced_unparenthesized_logical {
                self.error_mixed_coalesce(span);
            }
            left = Expr::Coalesce {
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
            // The Coalesce node itself is not a `||`/`&&`, and we have already validated the chain,
            // so subsequent `??` rungs see a non-logical left.
            left_was_logical = false;
        }
        self.produced_unparenthesized_logical = saved_logical;
        left
    }

    fn error_mixed_coalesce(&mut self, span: lin_common::Span) {
        self.diagnostics.push(
            Diagnostic::error(span, "cannot mix `||`/`&&` and `??` without parentheses")
                .with_help(
                    "wrap the `||`/`&&` sub-expression in parentheses, e.g. `(a || b) ?? c` or `a ?? (b || c)`",
                ),
        );
    }

    pub(crate) fn parse_expr_or_block(&mut self) -> Expr {
        if self.check(TokenKind::Indent) {
            self.parse_block()
        } else {
            self.parse_expr()
        }
    }

    pub(crate) fn parse_block(&mut self) -> Expr {
        let span = self.current_span();
        self.advance(); // consume Indent
        let mut stmts = Vec::new();
        let mut last_expr: Option<Expr> = None;

        loop {
            self.skip_newlines();
            if self.check(TokenKind::Dedent) || self.is_at_end() {
                break;
            }

            // Try to parse a statement
            match self.peek_kind() {
                TokenKind::Val => {
                    if let Some(e) = last_expr.take() {
                        stmts.push(Stmt::Expr(e));
                    }
                    stmts.push(self.parse_val(false));
                }
                TokenKind::Var => {
                    if let Some(e) = last_expr.take() {
                        stmts.push(Stmt::Expr(e));
                    }
                    stmts.push(self.parse_var(false));
                }
                _ => {
                    if let Some(e) = last_expr.take() {
                        stmts.push(Stmt::Expr(e));
                    }
                    last_expr = Some(self.parse_expr());
                }
            }
        }

        if self.check(TokenKind::Dedent) {
            self.advance();
        }

        let final_expr = last_expr.unwrap_or(Expr::NullLit(span));
        if stmts.is_empty() {
            final_expr
        } else {
            // Full extent: opening Indent .. end of the tail expr (the last thing in the block).
            let full_span = span.to(final_expr.full_span());
            Expr::Block(stmts, Box::new(final_expr), span, full_span)
        }
    }

    pub(crate) fn parse_or_expr(&mut self) -> Expr {
        let mut left = self.parse_and_expr();
        loop {
            self.skip_continuation_newline(TokenKind::Or);
            if !self.check(TokenKind::Or) { break; }
            self.produced_unparenthesized_logical = true;
            let span = self.current_span();
            self.advance();
            self.skip_newlines();
            let right = self.parse_and_expr();
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinOp::Or,
                right: Box::new(right),
                span,
            };
        }
        left
    }

    pub(crate) fn parse_and_expr(&mut self) -> Expr {
        let mut left = self.parse_bitor_expr();
        loop {
            self.skip_continuation_newline(TokenKind::And);
            if !self.check(TokenKind::And) { break; }
            self.produced_unparenthesized_logical = true;
            let span = self.current_span();
            self.advance();
            self.skip_newlines();
            let right = self.parse_bitor_expr();
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinOp::And,
                right: Box::new(right),
                span,
            };
        }
        left
    }

    // Bitwise OR `|` (value position only; type-expression `|` is parsed separately).
    pub(crate) fn parse_bitor_expr(&mut self) -> Expr {
        let mut left = self.parse_bitxor_expr();
        loop {
            self.skip_continuation_newline(TokenKind::Pipe);
            if !self.check(TokenKind::Pipe) { break; }
            let span = self.current_span();
            self.advance();
            self.skip_newlines();
            let right = self.parse_bitxor_expr();
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinOp::BOr,
                right: Box::new(right),
                span,
            };
        }
        left
    }

    // Bitwise XOR `^`.
    pub(crate) fn parse_bitxor_expr(&mut self) -> Expr {
        let mut left = self.parse_bitand_expr();
        loop {
            self.skip_continuation_newline(TokenKind::Caret);
            if !self.check(TokenKind::Caret) { break; }
            let span = self.current_span();
            self.advance();
            self.skip_newlines();
            let right = self.parse_bitand_expr();
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinOp::BXor,
                right: Box::new(right),
                span,
            };
        }
        left
    }

    // Bitwise AND `&`.
    pub(crate) fn parse_bitand_expr(&mut self) -> Expr {
        let mut left = self.parse_equality_expr();
        loop {
            self.skip_continuation_newline(TokenKind::Amp);
            if !self.check(TokenKind::Amp) { break; }
            let span = self.current_span();
            self.advance();
            self.skip_newlines();
            let right = self.parse_equality_expr();
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinOp::BAnd,
                right: Box::new(right),
                span,
            };
        }
        left
    }

    pub(crate) fn parse_equality_expr(&mut self) -> Expr {
        let mut left = self.parse_comparison_expr();
        loop {
            let op = match self.peek_kind() {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::NotEq => BinOp::NotEq,
                // Bare `=` in expression context is almost always `==` — suggest the fix.
                TokenKind::Eq => {
                    let span = self.current_span();
                    self.diagnostics.push(
                        Diagnostic::error(span, "unexpected `=` in expression")
                            .with_help("did you mean `==` for equality comparison?")
                    );
                    self.advance();
                    self.skip_newlines();
                    let right = self.parse_comparison_expr();
                    left = Expr::BinaryOp { left: Box::new(left), op: BinOp::Eq, right: Box::new(right), span };
                    continue;
                }
                _ => break,
            };
            let span = self.current_span();
            self.advance();
            self.skip_newlines();
            let right = self.parse_comparison_expr();
            left = Expr::BinaryOp { left: Box::new(left), op, right: Box::new(right), span };
        }
        left
    }

    pub(crate) fn parse_comparison_expr(&mut self) -> Expr {
        let mut left = self.parse_is_has_expr();
        loop {
            let op = match self.peek_kind() {
                TokenKind::Lt => BinOp::Lt,
                TokenKind::LtEq => BinOp::LtEq,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::GtEq => BinOp::GtEq,
                _ => break,
            };
            let span = self.current_span();
            self.advance();
            self.skip_newlines();
            let right = self.parse_shift_expr();
            left = Expr::BinaryOp { left: Box::new(left), op, right: Box::new(right), span };
        }
        left
    }

    pub(crate) fn parse_is_has_expr(&mut self) -> Expr {
        let left = self.parse_shift_expr();
        // Inside an inline match scrutinee (no Newline to bound it, ADR-003), a following
        // `is`/`has` begins the FIRST arm, not an infix type-test on the scrutinee — so skip it.
        if self.suppress_is_has {
            return left;
        }
        // Within an inline match arm body, an `is`/`has` at or left of the arm column is the
        // next arm's head, not an infix test on this body. (Inline tests like `x is Foo` sit at
        // a strictly greater column and are still consumed.)
        if self.check(TokenKind::Is) || self.check(TokenKind::Has) {
            if let Some(floor) = self.match_arm_floor {
                if self.current_column() <= floor {
                    return left;
                }
            }
        }
        if self.check(TokenKind::Is) {
            let span = self.current_span();
            self.advance();
            let pattern = self.parse_pattern();
            return Expr::Is { expr: Box::new(left), pattern: Box::new(pattern), span };
        }
        if self.check(TokenKind::Has) {
            let span = self.current_span();
            self.advance();
            let pattern = self.parse_pattern();
            return Expr::Has { expr: Box::new(left), pattern: Box::new(pattern), span };
        }
        left
    }

    // Bitwise shift `<<` `>>`. The lexer emits single `Lt`/`Gt` tokens so that nested
    // generic types (`Promise<Promise<Int32>>`) keep closing with `expect(Gt)`. We detect a
    // shift here, in value position only, by checking for two ADJACENT `Lt`/`Gt` tokens
    // (the first token's span.end == the second's span.start, same file). Type expressions
    // are parsed by a separate path, so generics are unaffected.
    pub(crate) fn parse_shift_expr(&mut self) -> Expr {
        let mut left = self.parse_additive_expr();
        loop {
            let op = if self.adjacent_pair(TokenKind::Lt, TokenKind::Lt) {
                BinOp::Shl
            } else if self.adjacent_pair(TokenKind::Gt, TokenKind::Gt) {
                BinOp::Shr
            } else {
                break;
            };
            let span = self.current_span();
            self.advance(); // first < or >
            self.advance(); // second < or >
            self.skip_newlines();
            let right = self.parse_additive_expr();
            left = Expr::BinaryOp { left: Box::new(left), op, right: Box::new(right), span };
        }
        left
    }

    pub(crate) fn parse_additive_expr(&mut self) -> Expr {
        let mut left = self.parse_multiplicative_expr();
        loop {
            let op = match self.peek_kind() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            let span = self.current_span();
            self.advance();
            self.skip_newlines();
            let right = self.parse_multiplicative_expr();
            left = Expr::BinaryOp { left: Box::new(left), op, right: Box::new(right), span };
        }
        left
    }

    pub(crate) fn parse_multiplicative_expr(&mut self) -> Expr {
        let mut left = self.parse_unary_expr();
        loop {
            let op = match self.peek_kind() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Mod,
                _ => break,
            };
            let span = self.current_span();
            self.advance();
            self.skip_newlines();
            let right = self.parse_unary_expr();
            left = Expr::BinaryOp { left: Box::new(left), op, right: Box::new(right), span };
        }
        left
    }

    // Unary `~` (bitwise not) and `!` (logical not). Both bind tighter than `*`, looser
    // than postfix. Right-associative so `~~x` parses as `~(~x)` and `!!x` as `!(!x)`.
    pub(crate) fn parse_unary_expr(&mut self) -> Expr {
        if self.check(TokenKind::Tilde) {
            let span = self.current_span();
            self.advance();
            self.skip_newlines();
            let operand = self.parse_unary_expr();
            return Expr::UnaryOp {
                op: UnaryOp::BNot,
                operand: Box::new(operand),
                span,
            };
        }
        if self.check(TokenKind::Bang) {
            let span = self.current_span();
            self.advance();
            self.skip_newlines();
            let operand = self.parse_unary_expr();
            return Expr::UnaryOp {
                op: UnaryOp::Not,
                operand: Box::new(operand),
                span,
            };
        }
        self.parse_postfix_expr()
    }

    pub(crate) fn parse_postfix_expr(&mut self) -> Expr {
        let mut expr = self.parse_primary_expr();
        let mut after_block = self.prev_was_dedent();
        loop {
            match self.peek_kind() {
                // A `[`/`(` that opens a new source line is NOT a postfix index/call on the
                // previous expression — it starts a new statement (e.g. a line-leading array
                // literal returned from an inline lambda body). Inside `()`/`[]`/`{}` the line
                // break is invisible as a token (ADR-003), so we rely on `at_line_start`. This
                // mirrors the post-Dedent suppression for top-level blocks (ADR-010).
                TokenKind::LBracket if !after_block && !self.at_line_start() => {
                    let span = self.current_span();
                    let obj_start = expr.full_span();
                    self.advance(); // [
                    let key = self.parse_expr();
                    self.expect(TokenKind::RBracket);
                    if self.check(TokenKind::Eq) && !self.check_ahead(TokenKind::Eq, 1) {
                        self.advance(); // =
                        self.skip_newlines();
                        let value = self.parse_expr_or_block();
                        // object start .. end of the assigned value.
                        let full_span = obj_start.to(value.full_span());
                        expr = Expr::IndexAssign { object: Box::new(expr), key: Box::new(key), value: Box::new(value), span, full_span };
                        break;
                    }
                    // object start .. closing `]` (just consumed).
                    let full_span = obj_start.to(self.prev_span());
                    expr = Expr::Index { object: Box::new(expr), key: Box::new(key), span, full_span };
                }
                TokenKind::LParen if !after_block && !self.at_line_start() => {
                    let span = self.current_span();
                    let callee_start = expr.full_span();
                    self.advance(); // (
                    let (args, partial) = self.parse_call_args();
                    self.expect(TokenKind::RParen);
                    // callee start .. closing `)` (just consumed).
                    let full_span = callee_start.to(self.prev_span());
                    expr = Expr::Call { func: Box::new(expr), args, partial, span, full_span };
                }
                TokenKind::Dot => {
                    after_block = false;
                    let span = self.current_span();
                    let recv_start = expr.full_span();
                    self.advance(); // .
                    self.skip_newlines();
                    let method = self.expect_ident();
                    let (args, partial) = if self.check(TokenKind::LParen) {
                        self.advance();
                        let (a, p) = self.parse_call_args();
                        self.expect(TokenKind::RParen);
                        (Some(a), p)
                    } else {
                        (None, false)
                    };
                    // receiver start .. closing `)` of the arg list, or the method ident when
                    // there is no parenthesised call (`x.foo`). Either way `prev_span` is the
                    // last token consumed for this dot-call.
                    let full_span = recv_start.to(self.prev_span());
                    expr = Expr::DotCall { receiver: Box::new(expr), method, args, partial, span, full_span };
                }
                TokenKind::Newline => {
                    // Look ahead past newlines/indent for dot-chaining
                    let saved = self.pos;
                    self.skip_newlines_and_indent();
                    if self.check(TokenKind::Dot) {
                        after_block = false;
                        continue; // The Dot case above will handle it
                    } else {
                        self.pos = saved;
                        break;
                    }
                }
                _ => break,
            }
        }
        expr
    }

    /// Parses a call argument list. Returns the args and whether the list ended
    /// with an explicit trailing comma (`f(x,)`), which requests partial
    /// application rather than default-fill.
    pub(crate) fn parse_call_args(&mut self) -> (Vec<Expr>, bool) {
        let mut args = Vec::new();
        let mut trailing_comma = false;
        self.skip_newlines();
        if self.check(TokenKind::RParen) {
            return (args, false);
        }
        args.push(self.parse_arg_expr());
        while self.check(TokenKind::Comma) {
            self.advance();
            self.skip_newlines();
            if self.check(TokenKind::RParen) {
                trailing_comma = true;
                break;
            }
            args.push(self.parse_arg_expr());
        }
        self.skip_newlines();
        (args, trailing_comma)
    }

    pub(crate) fn parse_arg_expr(&mut self) -> Expr {
        self.skip_newlines();
        // An argument can be a function expression or a regular expression
        if self.is_function_start() || self.is_generic_function_start() {
            return self.parse_function_expr();
        }
        // Check for bare identifier lambda: name => body
        if self.is_bare_lambda() {
            return self.parse_bare_lambda();
        }
        self.parse_expr()
    }

    pub(crate) fn parse_primary_expr(&mut self) -> Expr {
        match self.peek_kind() {
            TokenKind::IntLit(..) => {
                let span = self.current_span();
                if let TokenKind::IntLit(v, suffix) = self.advance_kind() {
                    Expr::IntLit(v, suffix, span)
                } else {
                    unreachable!()
                }
            }
            TokenKind::FloatLit(..) => {
                let span = self.current_span();
                if let TokenKind::FloatLit(v, suffix) = self.advance_kind() {
                    Expr::FloatLit(v, suffix, span)
                } else {
                    unreachable!()
                }
            }
            TokenKind::StringLit(_) => {
                let span = self.current_span();
                if let TokenKind::StringLit(s) = self.advance_kind() {
                    Expr::StringLit(s, span)
                } else {
                    unreachable!()
                }
            }
            TokenKind::InterpString(_) => self.parse_interp_string(),
            TokenKind::True => {
                let span = self.current_span();
                self.advance();
                Expr::BoolLit(true, span)
            }
            TokenKind::False => {
                let span = self.current_span();
                self.advance();
                Expr::BoolLit(false, span)
            }
            TokenKind::Null => {
                let span = self.current_span();
                self.advance();
                Expr::NullLit(span)
            }
            TokenKind::Ident(_) => {
                let span = self.current_span();
                let name = self.expect_ident();
                // Check for assignment
                if self.check(TokenKind::Eq) && !self.check_ahead(TokenKind::Eq, 1) {
                    self.advance(); // =
                    self.skip_newlines();
                    let value = self.parse_expr_or_block();
                    return Expr::Assign { target: name, value: Box::new(value), span };
                }
                Expr::Ident(name, span)
            }
            // Delimited groups open a fresh balanced span; the `suppress_is_has` scrutinee guard
            // does not apply inside them, so `match (x is Foo) ...` parses its inner `is` test.
            TokenKind::LBrace => self.without_is_has_suppression(Self::parse_object_expr),
            TokenKind::LBracket => self.without_is_has_suppression(Self::parse_array_expr),
            TokenKind::LParen => self.without_is_has_suppression(Self::parse_paren_or_function),
            // Generic function literal `<T, ...>(...) => ...`. A primary expression never
            // otherwise begins with `<` (comparison `<` is only reached after a left operand).
            TokenKind::Lt if self.is_generic_function_start() => self.parse_function_expr(),
            TokenKind::If => self.parse_if_expr(),
            TokenKind::Match => self.parse_match_expr(),
            TokenKind::Minus => {
                let span = self.current_span();
                self.advance();
                let right = self.parse_postfix_expr();
                Expr::BinaryOp {
                    left: Box::new(Expr::IntLit(0, None, span)),
                    op: BinOp::Sub,
                    right: Box::new(right),
                    span,
                }
            }
            // Lin has no semicolons (spec §1.2). A `;` here is almost always someone
            // separating statements C-style (e.g. inside an inline closure body
            // `c => idx[c] = i; i = i + 1`). Emit an actionable diagnostic rather than the
            // misleading "Undefined variable ';'" that the old Ident-catch-all produced.
            TokenKind::Semicolon => {
                let span = self.current_span();
                self.diagnostics.push(
                    Diagnostic::error(
                        span,
                        "unexpected ';' — Lin has no semicolons",
                    )
                    .with_help("separate statements with newlines, not ';'"),
                );
                self.advance();
                Expr::NullLit(span)
            }
            _ => {
                let span = self.current_span();
                let got = self.peek_kind();
                // Layout tokens (Indent/Dedent/Newline) can appear here during
                // error recovery; don't treat them as parse errors themselves.
                if !matches!(got, TokenKind::Indent | TokenKind::Dedent | TokenKind::Newline) {
                    self.diagnostics.push(Diagnostic::error(
                        span,
                        format!("unexpected token {:?}", got),
                    ));
                }
                self.advance();
                Expr::NullLit(span)
            }
        }
    }

    pub(crate) fn parse_interp_string(&mut self) -> Expr {
        let span = self.current_span();
        let interp_parts = if let TokenKind::InterpString(parts) = self.advance_kind() {
            parts
        } else {
            unreachable!()
        };

        let mut string_parts = Vec::new();
        for part in interp_parts {
            match part {
                lin_lex::InterpPart::Literal(s) => {
                    string_parts.push(StringPart::Literal(s));
                }
                lin_lex::InterpPart::Expr(tokens) => {
                    let mut sub_parser = Parser::new(tokens);
                    let expr = sub_parser.parse_expr();
                    string_parts.push(StringPart::Expr(expr));
                }
            }
        }

        Expr::StringInterp(string_parts, span)
    }

    pub(crate) fn parse_object_expr(&mut self) -> Expr {
        let span = self.current_span();
        self.advance(); // {
        self.skip_newlines();
        let mut fields = Vec::new();
        while !self.check(TokenKind::RBrace) && !self.is_at_end() {
            let loop_start = self.pos;
            if self.check(TokenKind::DotDotDot) {
                self.advance();
                let expr = self.parse_expr();
                fields.push(ObjectField::Spread(expr));
            } else if let TokenKind::Ident(ref ident_name) = self.peek_kind() {
                if self.check_ahead(TokenKind::Colon, 1) {
                    // Unquoted key with colon: { name: ... } — error, must use quoted key.
                    let key_span = self.current_span();
                    let name = ident_name.clone();
                    self.diagnostics.push(
                        Diagnostic::error(key_span, "object keys must be quoted strings".to_string())
                            .with_help(format!("use a quoted key: \"{}\"", name))
                    );
                    let key = self.parse_expr();
                    self.expect(TokenKind::Colon);
                    self.skip_newlines();
                    let value = self.parse_expr();
                    fields.push(ObjectField::Pair(key, value));
                } else {
                    // Shorthand field: { name } → { "name": name }
                    let field_span = self.current_span();
                    let name = ident_name.clone();
                    self.advance();
                    fields.push(ObjectField::Pair(
                        Expr::StringLit(name.clone(), field_span),
                        Expr::Ident(name, field_span),
                    ));
                }
            } else {
                let key = self.parse_expr();
                self.expect(TokenKind::Colon);
                self.skip_newlines();
                let value = self.parse_expr();
                fields.push(ObjectField::Pair(key, value));
            }
            if self.check(TokenKind::Comma) {
                let comma_span = self.current_span();
                self.advance();
                self.skip_newlines();
                // A comma immediately before the closing `}` is a trailing comma, which is
                // not allowed in object literals (the formatter never emits one). Function
                // calls still accept `f(x,)` for partial application — that's parse_call_args.
                // LIN_FMT_ALLOW_TRAILING_COMMA lets `lin fmt` read legacy files that still
                // contain trailing commas so it can strip them; normal compilation rejects.
                if self.check(TokenKind::RBrace) && !allow_trailing_comma() {
                    self.diagnostics.push(Diagnostic::error(
                        comma_span,
                        "trailing comma is not allowed in object literals".to_string(),
                    ));
                }
            }
            self.skip_newlines();
            if self.ensure_progress(loop_start) {
                continue;
            }
        }
        self.expect(TokenKind::RBrace);
        // opening `{` .. closing `}` (just consumed).
        let full_span = span.to(self.prev_span());
        Expr::Object(fields, span, full_span)
    }

    pub(crate) fn parse_array_expr(&mut self) -> Expr {
        let span = self.current_span();
        self.advance(); // [
        self.skip_newlines();
        let mut elements = Vec::new();
        while !self.check(TokenKind::RBracket) && !self.is_at_end() {
            let loop_start = self.pos;
            elements.push(self.parse_expr());
            if self.check(TokenKind::Comma) {
                let comma_span = self.current_span();
                self.advance();
                self.skip_newlines();
                // A comma immediately before the closing `]` is a trailing comma, which is
                // not allowed in array literals (the formatter never emits one).
                if self.check(TokenKind::RBracket) && !allow_trailing_comma() {
                    self.diagnostics.push(Diagnostic::error(
                        comma_span,
                        "trailing comma is not allowed in array literals".to_string(),
                    ));
                }
            }
            self.skip_newlines();
            if self.ensure_progress(loop_start) {
                continue;
            }
        }
        self.expect(TokenKind::RBracket);
        // opening `[` .. closing `]` (just consumed).
        let full_span = span.to(self.prev_span());
        Expr::Array(elements, span, full_span)
    }

    pub(crate) fn parse_if_expr(&mut self) -> Expr {
        // The offside floor for an inline (no-Indent) branch is the indentation of the LINE the
        // `if` sits on — not the `if` keyword's own column. When `if` starts its statement the
        // two coincide (unchanged behaviour); when `if` is a right-hand side
        // (`val x = if c then\n  A\nelse\n  B`) its wrapped branches indent relative to the
        // enclosing statement, which is left of the keyword, so anchoring on the keyword would
        // set an impossibly high floor and collapse the branch (orphaning the `else`). An
        // `else if` continuation reuses the SAME floor as the `if` it continues — see
        // `parse_if_expr_with_col`.
        let branch_col = self.line_start_column();
        self.parse_if_expr_with_col(branch_col)
    }

    /// Parse an `if` expression using `branch_col` as the exclusive offside floor for any
    /// inline (no-Indent) then/else branch — i.e. inside `()`/`[]`/`{}` where ADR-003
    /// suppresses Indent/Dedent. Statements indented past `branch_col` belong to the branch;
    /// the first statement at or before it ends the branch. An `else if` continuation is
    /// parsed with the SAME `branch_col` so the whole chain shares one offside anchor.
    fn parse_if_expr_with_col(&mut self, branch_col: u32) -> Expr {
        let span = self.current_span();
        self.advance(); // if
        self.skip_newlines();
        let condition = self.parse_expr();

        self.skip_newlines();
        if self.check(TokenKind::Indent) {
            let span = self.current_span();
            self.diagnostics.push(Diagnostic::error(
                span,
                "`then` must appear on the same line as the condition: `if cond then ...`".to_string(),
            ));
        }
        self.expect_keyword(TokenKind::Then);
        self.skip_newlines();
        let then_branch = if self.check(TokenKind::Indent) {
            self.parse_block()   // consumes INDENT … DEDENT
        } else {
            // No Indent (inline / inside parens): collect the column-delimited then-block so a
            // multi-statement then-branch isn't truncated to its first statement (the ADR-003
            // newline-suppression bug). Single-line `if c then e` reads as a one-expr block.
            self.parse_branch_block(branch_col)
        };

        self.skip_newlines();
        let else_branch = if self.check(TokenKind::Else) {
            self.advance();
            self.skip_newlines();
            if self.check(TokenKind::Indent) {
                self.parse_block()
            } else if self.check(TokenKind::If) {
                // `else if` continues the chain: reuse the opening `if`'s offside floor.
                self.parse_if_expr_with_col(branch_col)
            } else {
                self.parse_branch_block(branch_col)
            }
        } else {
            Expr::NullLit(span)
        };

        // `if` keyword .. end of the last branch. When there is no `else`, the synthesized
        // NullLit sits at the `if` span, so fold to whichever branch ends later.
        let full_span = span
            .to(then_branch.full_span())
            .to(else_branch.full_span());
        Expr::If {
            condition: Box::new(condition),
            then_branch: Box::new(then_branch),
            else_branch: Box::new(else_branch),
            span,
            full_span,
        }
    }

    pub(crate) fn parse_match_expr(&mut self) -> Expr {
        let span = self.current_span();
        self.advance(); // match
        // Suppress the `is`/`has` infix at the scrutinee's top level so an inline (inside-parens)
        // `match i has 0 => ...` reads `i` as the scrutinee and `has 0 => ...` as the first arm,
        // not `(i has 0)` as a type-test. The flag is reset on entry to any `(`/`[`/`{` group, so
        // a parenthesised `match (x is Foo)` scrutinee still parses its inner `is` test.
        let prev_suppress = self.suppress_is_has;
        self.suppress_is_has = true;
        let scrutinee = self.parse_expr();
        self.suppress_is_has = prev_suppress;
        self.skip_newlines();

        let mut arms = Vec::new();
        if self.check(TokenKind::Indent) {
            // Top-level / block-statement match: arms are an indented block (ADR-003 emits
            // Indent/Dedent here). Unchanged from the original behaviour.
            self.advance();
            loop {
                self.skip_newlines();
                if self.check(TokenKind::Dedent) || self.is_at_end() {
                    break;
                }
                let loop_start = self.pos;
                arms.push(self.parse_match_arm(None));
                if self.ensure_progress(loop_start) {
                    continue;
                }
            }
            if self.check(TokenKind::Dedent) {
                self.advance();
            }
        } else {
            // Inline / inside parens (ADR-003 suppresses Indent/Dedent/Newline). Use the
            // offside rule: the arms all line up at one column (`arm_col`, taken from the first
            // arm's `is`/`has`/`else`/`when` keyword). An arm-starting keyword at exactly that
            // column is another arm; a token at a column < arm_col, or any non-arm token, ends
            // the match (so a statement dedented to the lambda-body level after the match — e.g.
            // `print(label)` — is NOT swallowed). Each arm body is parsed with `arm_col` as its
            // offside floor so a multi-statement arm body stays together but the next arm (same
            // column) terminates it.
            let mut arm_col: Option<u32> = None;
            loop {
                if self.is_at_end()
                    || self.check(TokenKind::RParen)
                    || self.check(TokenKind::RBracket)
                    || self.check(TokenKind::RBrace)
                    || self.check(TokenKind::Comma)
                    || self.check(TokenKind::Dedent)
                {
                    break;
                }
                if !self.at_match_arm_start() {
                    break;
                }
                let col = self.current_column();
                match arm_col {
                    None => arm_col = Some(col),
                    Some(ac) => {
                        // A subsequent arm must align with the first. A dedent below the arm
                        // column ends the match; a column past it is treated as out-of-block too
                        // (it cannot belong to this match's arm list).
                        if col != ac {
                            break;
                        }
                    }
                }
                arms.push(self.parse_match_arm(arm_col));
            }
        }

        // `match` keyword .. end of the last arm's body (or the scrutinee if there are no arms).
        let full_span = match arms.last() {
            Some(arm) => span.to(arm.body.full_span()),
            None => span.to(scrutinee.full_span()),
        };
        Expr::Match { scrutinee: Box::new(scrutinee), arms, span, full_span }
    }

    /// Run `f` with the `suppress_is_has` scrutinee guard cleared, restoring it afterwards.
    /// Used at delimited-group entry so an inner `is`/`has` type-test parses normally.
    fn without_is_has_suppression(&mut self, f: fn(&mut Self) -> Expr) -> Expr {
        let prev = self.suppress_is_has;
        let prev_floor = self.match_arm_floor;
        self.suppress_is_has = false;
        self.match_arm_floor = None;
        let e = f(self);
        self.suppress_is_has = prev;
        self.match_arm_floor = prev_floor;
        e
    }

    /// True when the cursor is on a token that can begin a match arm (`is`/`has`/`else`/`when`).
    fn at_match_arm_start(&self) -> bool {
        matches!(
            self.peek_kind(),
            TokenKind::Is | TokenKind::Has | TokenKind::Else | TokenKind::When
        )
    }
}
