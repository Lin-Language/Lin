use lin_lex::TokenKind;
use crate::ast::*;
use super::Parser;

impl Parser {
    pub(crate) fn parse_paren_or_function(&mut self) -> Expr {
        // Could be: grouped expr, function params, or tuple-args for dot application
        let span = self.current_span();

        // Look ahead to determine if this is a function
        if self.is_function_start() {
            return self.parse_function_expr();
        }

        self.advance(); // (
        self.skip_newlines();

        if self.check(TokenKind::RParen) {
            self.advance();
            // Empty parens - could be () => body (no-arg function)
            if self.check(TokenKind::Arrow) {
                self.advance();
                self.skip_newlines();
                let body = self.parse_expr_or_block();
                return Expr::Function {
                    type_params: Vec::new(),
                    params: Vec::new(),
                    return_type: None,
                    body: Box::new(body),
                    span,
                };
            }
            return Expr::TupleArgs(Vec::new(), span);
        }

        let first = self.parse_expr();

        if self.check(TokenKind::Comma) {
            // Multiple expressions in parens - tuple args for dot application
            let mut args = vec![first];
            while self.check(TokenKind::Comma) {
                self.advance();
                self.skip_newlines();
                if self.check(TokenKind::RParen) {
                    break;
                }
                args.push(self.parse_expr());
            }
            self.skip_newlines();
            self.expect(TokenKind::RParen);

            // Check for .method
            if self.check(TokenKind::Dot) {
                let dot_span = self.current_span();
                self.advance();
                let method = self.expect_ident();
                let (call_args, partial) = if self.check(TokenKind::LParen) {
                    self.advance();
                    let (a, p) = self.parse_call_args();
                    self.expect(TokenKind::RParen);
                    (Some(a), p)
                } else {
                    (None, false)
                };
                return Expr::DotCall {
                    receiver: Box::new(Expr::TupleArgs(args, span)),
                    method,
                    args: call_args,
                    partial,
                    span: dot_span,
                };
            }

            return Expr::TupleArgs(args, span);
        }

        self.skip_newlines();
        self.expect(TokenKind::RParen);
        // Grouped expression - check for .method
        if self.check(TokenKind::Dot) {
            let dot_span = self.current_span();
            self.advance();
            self.skip_newlines();
            let method = self.expect_ident();
            let (call_args, partial) = if self.check(TokenKind::LParen) {
                self.advance();
                let (a, p) = self.parse_call_args();
                self.expect(TokenKind::RParen);
                (Some(a), p)
            } else {
                (None, false)
            };
            return Expr::DotCall {
                receiver: Box::new(first),
                method,
                args: call_args,
                partial,
                span: dot_span,
            };
        }
        first
    }

    pub(crate) fn is_bare_lambda(&self) -> bool {
        if let TokenKind::Ident(_) = self.peek_kind() {
            // Check if next non-newline token after the ident is =>
            let mut i = self.pos + 1;
            while i < self.tokens.len() && matches!(self.tokens[i].kind, TokenKind::Newline) {
                i += 1;
            }
            if i < self.tokens.len() && self.tokens[i].kind == TokenKind::Arrow {
                return true;
            }
            // Also check for (ident, ident) => pattern (multi-param bare lambda)
            false
        } else {
            false
        }
    }

    pub(crate) fn parse_bare_lambda(&mut self) -> Expr {
        let span = self.current_span();
        let name = self.expect_ident();
        let param = Param {
            pattern: Pattern::Ident(name, span),
            type_ann: None,
            default: None,
        };
        self.expect(TokenKind::Arrow);
        self.skip_newlines();
        let body = self.parse_function_body();
        Expr::Function {
            type_params: Vec::new(),
            params: vec![param],
            return_type: None,
            body: Box::new(body),
            span,
        }
    }

    pub(crate) fn parse_function_body(&mut self) -> Expr {
        if self.check(TokenKind::Indent) {
            return self.parse_block();
        }
        self.parse_inline_block()
    }

    pub(crate) fn parse_inline_block(&mut self) -> Expr {
        // The offside column is anchored on the FIRST statement of the block. Subsequent
        // statements that dedent below it belong to an enclosing block. Using 0 as the base
        // until the first statement is read means the very first statement is always accepted.
        self.parse_inline_block_inner(None)
    }

    /// Parse a column-delimited block of an `if`/`else` branch inside parens. `if_col` is the
    /// 1-based column of the controlling `if` keyword; statements with column STRICTLY GREATER
    /// than `if_col` belong to the branch, and the first statement at column <= `if_col` ends it.
    /// Also stops at `else`, the closing delimiters, comma and EOF (handled in the inner loop).
    pub(crate) fn parse_branch_block(&mut self, if_col: u32) -> Expr {
        self.parse_inline_block_inner(Some(if_col))
    }

    /// Shared inline-block loop. `min_col`, when set, is an EXCLUSIVE column floor: a statement
    /// whose column is <= `min_col` ends the block (offside rule for `if`/`else` branches).
    /// When `None`, the floor is taken from the first statement's column and applied to
    /// subsequent statements (so an inline lambda body's own statements stay together but a
    /// statement that dedents below the body's start belongs to the enclosing block).
    ///
    /// IMPORTANT: the column is only checked BETWEEN statements, never within a single
    /// `parse_expr()` call. Continuation lines of one expression (`x\n  + y`) and dot-chains
    /// across newlines (`foo\n  .bar()`, ADR-005) are consumed whole by one `parse_expr()` and
    /// are therefore never split by this guard.
    fn parse_inline_block_inner(&mut self, min_col: Option<u32>) -> Expr {
        let span = self.current_span();
        let mut stmts = Vec::new();
        let mut last_expr: Option<Expr> = None;
        // The exclusive column floor. For a branch block this is the `if` column. For a plain
        // inline lambda body it is (first-statement-column - 1), established lazily below.
        let mut floor: Option<u32> = min_col;

        loop {
            if self.check(TokenKind::Newline)
                || self.check(TokenKind::RParen)
                || self.check(TokenKind::RBracket)
                || self.check(TokenKind::RBrace)
                || self.check(TokenKind::Comma)
                || self.check(TokenKind::Dedent)
                || self.is_at_end()
            {
                break;
            }
            // A branch block also ends at `else` (it belongs to the enclosing `if`).
            if min_col.is_some() && self.check(TokenKind::Else) {
                break;
            }
            // Offside guard: a statement that dedents to or below the floor ends this block.
            // Only consulted between statements (the leading token of each statement).
            if let Some(f) = floor {
                if self.current_column() <= f {
                    break;
                }
            } else {
                // First statement of a plain inline body: anchor the floor just below its
                // column so equal-column siblings continue the block but dedents break out.
                floor = Some(self.current_column().saturating_sub(1));
            }

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

        let final_expr = last_expr.unwrap_or(Expr::NullLit(span));
        if stmts.is_empty() {
            final_expr
        } else {
            Expr::Block(stmts, Box::new(final_expr), span)
        }
    }

    pub(crate) fn is_function_start(&self) -> bool {
        if !self.check(TokenKind::LParen) {
            return false;
        }
        // Scan forward to find matching ) and check for => or : after params
        let mut depth = 0;
        let mut i = self.pos;
        while i < self.tokens.len() {
            match &self.tokens[i].kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        // Skip newlines
                        while i < self.tokens.len() && matches!(self.tokens[i].kind, TokenKind::Newline) {
                            i += 1;
                        }
                        // Check for => or : (return type)
                        if i < self.tokens.len() {
                            return matches!(self.tokens[i].kind, TokenKind::Arrow | TokenKind::Colon);
                        }
                        return false;
                    }
                }
                TokenKind::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// Detects a generic function literal `<T, ...>(...)` at the current position.
    /// Only matches a `<` immediately followed by an ident-list closed by `>` and then a `(`.
    /// This is unambiguous in primary/argument position because a bare expression never begins
    /// with `<`. (Comparison `<` is only ever reached from the binary-operator ladder, after a
    /// left operand has been parsed — never here.)
    pub(crate) fn is_generic_function_start(&self) -> bool {
        if !self.check(TokenKind::Lt) {
            return false;
        }
        let mut i = self.pos + 1;
        // Require at least one type-param ident.
        if !matches!(self.tokens.get(i).map(|t| &t.kind), Some(TokenKind::Ident(_))) {
            return false;
        }
        loop {
            // ident
            if !matches!(self.tokens.get(i).map(|t| &t.kind), Some(TokenKind::Ident(_))) {
                return false;
            }
            i += 1;
            match self.tokens.get(i).map(|t| &t.kind) {
                Some(TokenKind::Comma) => {
                    i += 1;
                    continue;
                }
                Some(TokenKind::Gt) => {
                    i += 1;
                    break;
                }
                _ => return false,
            }
        }
        // After `>` must come `(` to begin the parameter list.
        matches!(self.tokens.get(i).map(|t| &t.kind), Some(TokenKind::LParen))
    }

    /// Parse a `<T, ...>` type-parameter list (the leading `<` is at the cursor).
    fn parse_type_params(&mut self) -> Vec<String> {
        self.advance(); // <
        let mut params = Vec::new();
        loop {
            params.push(self.expect_ident());
            if self.check(TokenKind::Comma) {
                self.advance();
                continue;
            }
            break;
        }
        self.expect(TokenKind::Gt);
        params
    }

    pub(crate) fn parse_function_expr(&mut self) -> Expr {
        let span = self.current_span();
        // Optional generic type parameters: `<T, ...>(...) => ...`
        let type_params = if self.check(TokenKind::Lt) {
            self.parse_type_params()
        } else {
            Vec::new()
        };
        self.advance(); // (
        self.skip_newlines();
        let mut params = Vec::new();
        while !self.check(TokenKind::RParen) && !self.is_at_end() {
            let loop_start = self.pos;
            let param = self.parse_param();
            params.push(param);
            if self.check(TokenKind::Comma) {
                self.advance();
            }
            self.skip_newlines();
            if self.ensure_progress(loop_start) {
                continue;
            }
        }
        self.expect(TokenKind::RParen);

        let return_type = if self.check(TokenKind::Colon) {
            self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };

        self.expect(TokenKind::Arrow);
        self.skip_newlines();
        let body = self.parse_function_body();
        Expr::Function { type_params, params, return_type, body: Box::new(body), span }
    }

    pub(crate) fn parse_param(&mut self) -> Param {
        // Could be destructured: { name, age }: Person or a simple: name: Type
        let pattern = self.parse_binding_pattern();
        let type_ann = if self.check(TokenKind::Colon) {
            self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };
        // Default value: `name: Type = expr` (or `name = expr`). Guard against `==`
        // so a malformed comparison isn't silently consumed as a default.
        let default = if self.check(TokenKind::Eq) && !self.check_ahead(TokenKind::Eq, 1) {
            self.advance(); // =
            self.skip_newlines();
            Some(Box::new(self.parse_arg_expr()))
        } else {
            None
        };
        Param { pattern, type_ann, default }
    }
}
