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
        self.parse_or_expr()
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
            Expr::Block(stmts, Box::new(final_expr), span)
        }
    }

    pub(crate) fn parse_or_expr(&mut self) -> Expr {
        let mut left = self.parse_and_expr();
        loop {
            self.skip_continuation_newline(TokenKind::Or);
            if !self.check(TokenKind::Or) { break; }
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
                // break is invisible as a token (ADR-004), so we rely on `at_line_start`. This
                // mirrors the post-Dedent suppression for top-level blocks (ADR-011).
                TokenKind::LBracket if !after_block && !self.at_line_start() => {
                    let span = self.current_span();
                    self.advance(); // [
                    let key = self.parse_expr();
                    self.expect(TokenKind::RBracket);
                    if self.check(TokenKind::Eq) && !self.check_ahead(TokenKind::Eq, 1) {
                        self.advance(); // =
                        self.skip_newlines();
                        let value = self.parse_expr_or_block();
                        expr = Expr::IndexAssign { object: Box::new(expr), key: Box::new(key), value: Box::new(value), span };
                        break;
                    }
                    expr = Expr::Index { object: Box::new(expr), key: Box::new(key), span };
                }
                TokenKind::LParen if !after_block && !self.at_line_start() => {
                    let span = self.current_span();
                    self.advance(); // (
                    let (args, partial) = self.parse_call_args();
                    self.expect(TokenKind::RParen);
                    expr = Expr::Call { func: Box::new(expr), args, partial, span };
                }
                TokenKind::Dot => {
                    after_block = false;
                    let span = self.current_span();
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
                    expr = Expr::DotCall { receiver: Box::new(expr), method, args, partial, span };
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
            TokenKind::LBrace => self.parse_object_expr(),
            TokenKind::LBracket => self.parse_array_expr(),
            TokenKind::LParen => self.parse_paren_or_function(),
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
        }
        self.expect(TokenKind::RBrace);
        Expr::Object(fields, span)
    }

    pub(crate) fn parse_array_expr(&mut self) -> Expr {
        let span = self.current_span();
        self.advance(); // [
        self.skip_newlines();
        let mut elements = Vec::new();
        while !self.check(TokenKind::RBracket) && !self.is_at_end() {
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
        }
        self.expect(TokenKind::RBracket);
        Expr::Array(elements, span)
    }

    pub(crate) fn parse_if_expr(&mut self) -> Expr {
        // The offside floor for an inline (no-Indent) branch is the column of the `if`
        // keyword that opens the chain. An `else if` continuation reuses the SAME floor as
        // the `if` it continues (so a trailing `else` aligned with the outer `if` still
        // belongs to the chain, not to the inner branch) — see `parse_if_expr_with_col`.
        let branch_col = self.current_column();
        self.parse_if_expr_with_col(branch_col)
    }

    /// Parse an `if` expression using `branch_col` as the exclusive offside floor for any
    /// inline (no-Indent) then/else branch — i.e. inside `()`/`[]`/`{}` where ADR-004
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
            // multi-statement then-branch isn't truncated to its first statement (the ADR-004
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

        Expr::If {
            condition: Box::new(condition),
            then_branch: Box::new(then_branch),
            else_branch: Box::new(else_branch),
            span,
        }
    }

    pub(crate) fn parse_match_expr(&mut self) -> Expr {
        let span = self.current_span();
        self.advance(); // match
        let scrutinee = self.parse_expr();
        self.skip_newlines();

        let mut arms = Vec::new();
        if self.check(TokenKind::Indent) {
            self.advance();
            loop {
                self.skip_newlines();
                if self.check(TokenKind::Dedent) || self.is_at_end() {
                    break;
                }
                arms.push(self.parse_match_arm());
            }
            if self.check(TokenKind::Dedent) {
                self.advance();
            }
        }

        Expr::Match { scrutinee: Box::new(scrutinee), arms, span }
    }
}
