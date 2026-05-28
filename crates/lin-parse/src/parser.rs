use lin_common::{Diagnostic, Span};
use lin_lex::{Token, TokenKind};
use crate::ast::*;

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

    fn parse_statement(&mut self) -> Option<Stmt> {
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
            _ => {
                let expr = self.parse_expr();
                Some(Stmt::Expr(expr))
            }
        }
    }

    fn parse_export(&mut self) -> Option<Stmt> {
        self.advance(); // skip 'export'
        self.skip_newlines();
        match self.peek_kind() {
            TokenKind::Val => Some(self.parse_val(true)),
            TokenKind::Var => Some(self.parse_var(true)),
            TokenKind::Type => Some(self.parse_type_decl(true)),
            _ => None,
        }
    }

    fn parse_val(&mut self, exported: bool) -> Stmt {
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

    fn parse_var(&mut self, exported: bool) -> Stmt {
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

    fn parse_type_decl(&mut self, exported: bool) -> Stmt {
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
        self.skip_newlines();
        let body = self.parse_type_expr_with_leading_pipe();
        Stmt::TypeDecl { name, params, body, exported, span }
    }

    fn parse_import(&mut self) -> Stmt {
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

    fn peek_ahead_is_foreign(&self) -> bool {
        // Check if the token after 'import' is 'foreign'
        if self.pos + 1 < self.tokens.len() {
            matches!(self.tokens[self.pos + 1].kind, TokenKind::Foreign)
        } else {
            false
        }
    }

    fn parse_foreign_import(&mut self) -> Stmt {
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

    fn parse_expr(&mut self) -> Expr {
        self.parse_or_expr()
    }

    fn parse_expr_or_block(&mut self) -> Expr {
        if self.check(TokenKind::Indent) {
            self.parse_block()
        } else {
            self.parse_expr()
        }
    }

    fn parse_block(&mut self) -> Expr {
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

    fn parse_or_expr(&mut self) -> Expr {
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

    fn parse_and_expr(&mut self) -> Expr {
        let mut left = self.parse_equality_expr();
        loop {
            self.skip_continuation_newline(TokenKind::And);
            if !self.check(TokenKind::And) { break; }
            let span = self.current_span();
            self.advance();
            self.skip_newlines();
            let right = self.parse_equality_expr();
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinOp::And,
                right: Box::new(right),
                span,
            };
        }
        left
    }

    fn skip_continuation_newline(&mut self, expected: TokenKind) {
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

    fn parse_equality_expr(&mut self) -> Expr {
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

    fn parse_comparison_expr(&mut self) -> Expr {
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
            let right = self.parse_additive_expr();
            left = Expr::BinaryOp { left: Box::new(left), op, right: Box::new(right), span };
        }
        left
    }

    fn parse_is_has_expr(&mut self) -> Expr {
        let left = self.parse_additive_expr();
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

    fn parse_additive_expr(&mut self) -> Expr {
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

    fn parse_multiplicative_expr(&mut self) -> Expr {
        let mut left = self.parse_postfix_expr();
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
            let right = self.parse_postfix_expr();
            left = Expr::BinaryOp { left: Box::new(left), op, right: Box::new(right), span };
        }
        left
    }

    fn parse_postfix_expr(&mut self) -> Expr {
        let mut expr = self.parse_primary_expr();
        let mut after_block = self.prev_was_dedent();
        loop {
            match self.peek_kind() {
                TokenKind::LBracket if !after_block => {
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
                TokenKind::LParen if !after_block => {
                    let span = self.current_span();
                    self.advance(); // (
                    let args = self.parse_call_args();
                    self.expect(TokenKind::RParen);
                    expr = Expr::Call { func: Box::new(expr), args, span };
                }
                TokenKind::Dot => {
                    after_block = false;
                    let span = self.current_span();
                    self.advance(); // .
                    self.skip_newlines();
                    let method = self.expect_ident();
                    let args = if self.check(TokenKind::LParen) {
                        self.advance();
                        let a = self.parse_call_args();
                        self.expect(TokenKind::RParen);
                        Some(a)
                    } else {
                        None
                    };
                    expr = Expr::DotCall { receiver: Box::new(expr), method, args, span };
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

    fn parse_call_args(&mut self) -> Vec<Expr> {
        let mut args = Vec::new();
        self.skip_newlines();
        if self.check(TokenKind::RParen) {
            return args;
        }
        args.push(self.parse_arg_expr());
        while self.check(TokenKind::Comma) {
            self.advance();
            self.skip_newlines();
            if self.check(TokenKind::RParen) {
                break;
            }
            args.push(self.parse_arg_expr());
        }
        self.skip_newlines();
        args
    }

    fn parse_arg_expr(&mut self) -> Expr {
        self.skip_newlines();
        // An argument can be a function expression or a regular expression
        if self.is_function_start() {
            return self.parse_function_expr();
        }
        // Check for bare identifier lambda: name => body
        if self.is_bare_lambda() {
            return self.parse_bare_lambda();
        }
        self.parse_expr()
    }

    fn parse_primary_expr(&mut self) -> Expr {
        match self.peek_kind() {
            TokenKind::IntLit(_) => {
                let span = self.current_span();
                if let TokenKind::IntLit(v) = self.advance_kind() {
                    Expr::IntLit(v, span)
                } else {
                    unreachable!()
                }
            }
            TokenKind::FloatLit(_) => {
                let span = self.current_span();
                if let TokenKind::FloatLit(v) = self.advance_kind() {
                    Expr::FloatLit(v, span)
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
            TokenKind::If => self.parse_if_expr(),
            TokenKind::Match => self.parse_match_expr(),
            TokenKind::Minus => {
                let span = self.current_span();
                self.advance();
                let right = self.parse_postfix_expr();
                Expr::BinaryOp {
                    left: Box::new(Expr::IntLit(0, span)),
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

    fn parse_interp_string(&mut self) -> Expr {
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

    fn parse_object_expr(&mut self) -> Expr {
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
                        Diagnostic::error(key_span, format!("object keys must be quoted strings"))
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
                self.advance();
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RBrace);
        Expr::Object(fields, span)
    }

    fn parse_array_expr(&mut self) -> Expr {
        let span = self.current_span();
        self.advance(); // [
        self.skip_newlines();
        let mut elements = Vec::new();
        while !self.check(TokenKind::RBracket) && !self.is_at_end() {
            elements.push(self.parse_expr());
            if self.check(TokenKind::Comma) {
                self.advance();
            }
            self.skip_newlines();
        }
        self.expect(TokenKind::RBracket);
        Expr::Array(elements, span)
    }

    fn parse_paren_or_function(&mut self) -> Expr {
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
                let call_args = if self.check(TokenKind::LParen) {
                    self.advance();
                    let a = self.parse_call_args();
                    self.expect(TokenKind::RParen);
                    Some(a)
                } else {
                    None
                };
                return Expr::DotCall {
                    receiver: Box::new(Expr::TupleArgs(args, span)),
                    method,
                    args: call_args,
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
            let call_args = if self.check(TokenKind::LParen) {
                self.advance();
                let a = self.parse_call_args();
                self.expect(TokenKind::RParen);
                Some(a)
            } else {
                None
            };
            return Expr::DotCall {
                receiver: Box::new(first),
                method,
                args: call_args,
                span: dot_span,
            };
        }
        first
    }

    fn is_bare_lambda(&self) -> bool {
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

    fn parse_bare_lambda(&mut self) -> Expr {
        let span = self.current_span();
        let name = self.expect_ident();
        let param = Param {
            pattern: Pattern::Ident(name, span),
            type_ann: None,
        };
        self.expect(TokenKind::Arrow);
        self.skip_newlines();
        let body = self.parse_function_body();
        Expr::Function {
            params: vec![param],
            return_type: None,
            body: Box::new(body),
            span,
        }
    }

    fn parse_function_body(&mut self) -> Expr {
        if self.check(TokenKind::Indent) {
            return self.parse_block();
        }
        self.parse_inline_block()
    }

    fn parse_inline_block(&mut self) -> Expr {
        let span = self.current_span();
        let mut stmts = Vec::new();
        let mut last_expr: Option<Expr> = None;

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

    fn is_function_start(&self) -> bool {
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

    fn parse_function_expr(&mut self) -> Expr {
        let span = self.current_span();
        self.advance(); // (
        self.skip_newlines();
        let mut params = Vec::new();
        while !self.check(TokenKind::RParen) && !self.is_at_end() {
            let param = self.parse_param();
            params.push(param);
            if self.check(TokenKind::Comma) {
                self.advance();
            }
            self.skip_newlines();
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
        Expr::Function { params, return_type, body: Box::new(body), span }
    }

    fn parse_param(&mut self) -> Param {
        // Could be destructured: { name, age }: Person or a simple: name: Type
        let pattern = self.parse_binding_pattern();
        let type_ann = if self.check(TokenKind::Colon) {
            self.advance();
            Some(self.parse_type_expr())
        } else {
            None
        };
        Param { pattern, type_ann }
    }

    fn parse_if_expr(&mut self) -> Expr {
        let span = self.current_span();
        self.advance(); // if
        self.skip_newlines();
        let condition = self.parse_expr();

        self.skip_newlines();
        self.expect_keyword(TokenKind::Then);
        self.skip_newlines();
        let then_branch = if self.check(TokenKind::Indent) {
            self.parse_block()   // consumes INDENT … DEDENT
        } else {
            self.parse_expr()
        };

        self.skip_newlines();
        let else_branch = if self.check(TokenKind::Else) {
            self.advance();
            self.skip_newlines();
            if self.check(TokenKind::Indent) {
                self.parse_block()
            } else if self.check(TokenKind::If) {
                self.parse_if_expr()
            } else {
                self.parse_expr()
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

    fn parse_match_expr(&mut self) -> Expr {
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

    fn parse_match_arm(&mut self) -> MatchArm {
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
        } else {
            self.parse_expr()
        };
        self.skip_newlines();

        MatchArm { pattern, guard, body, span }
    }

    fn parse_pattern(&mut self) -> Pattern {
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
            TokenKind::IntLit(_) => {
                let span = self.current_span();
                if let TokenKind::IntLit(v) = self.advance_kind() {
                    Pattern::Literal(Box::new(Expr::IntLit(v, span)))
                } else {
                    unreachable!()
                }
            }
            TokenKind::FloatLit(_) => {
                let span = self.current_span();
                if let TokenKind::FloatLit(v) = self.advance_kind() {
                    Pattern::Literal(Box::new(Expr::FloatLit(v, span)))
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
                if name.chars().next().map_or(false, |c| c.is_uppercase()) {
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

    fn parse_object_pattern(&mut self) -> Pattern {
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

    fn parse_array_pattern(&mut self) -> Pattern {
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

    fn parse_binding_pattern(&mut self) -> Pattern {
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

    fn parse_type_expr(&mut self) -> TypeExpr {
        let first = self.parse_type_primary();
        if self.check(TokenKind::Pipe) {
            let mut types = vec![first];
            while self.check(TokenKind::Pipe) {
                self.advance();
                self.skip_newlines();
                types.push(self.parse_type_primary());
            }
            TypeExpr::Union(types, Span::dummy())
        } else {
            first
        }
    }

    fn parse_type_expr_with_leading_pipe(&mut self) -> TypeExpr {
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

    fn parse_type_primary(&mut self) -> TypeExpr {
        let base = match self.peek_kind() {
            TokenKind::LParen => {
                // Function type: (T1, T2) => U
                self.advance();
                let mut params = Vec::new();
                while !self.check(TokenKind::RParen) && !self.is_at_end() {
                    params.push(self.parse_type_expr());
                    if self.check(TokenKind::Comma) {
                        self.advance();
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
                    types.push(self.parse_type_expr());
                    if self.check(TokenKind::Comma) {
                        self.advance();
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
            _ => {
                let span = self.current_span();
                self.advance();
                TypeExpr::Named("Unknown".to_string(), span)
            }
        };

        // Check for postfix [] (array type)
        if self.check(TokenKind::LBracket) && self.check_ahead(TokenKind::RBracket, 1) {
            self.advance(); // [
            self.advance(); // ]
            TypeExpr::Array(Box::new(base), Span::dummy())
        } else {
            base
        }
    }

    // --- Helpers ---

    fn prev_was_dedent(&self) -> bool {
        if self.pos == 0 { return false; }
        matches!(self.tokens[self.pos - 1].kind, TokenKind::Dedent)
    }

    fn peek_kind(&self) -> TokenKind {
        if self.pos < self.tokens.len() {
            self.tokens[self.pos].kind.clone()
        } else {
            TokenKind::Eof
        }
    }

    fn check(&self, kind: TokenKind) -> bool {
        std::mem::discriminant(&self.peek_kind()) == std::mem::discriminant(&kind)
    }

    fn check_ahead(&self, kind: TokenKind, offset: usize) -> bool {
        let idx = self.pos + offset;
        if idx < self.tokens.len() {
            std::mem::discriminant(&self.tokens[idx].kind) == std::mem::discriminant(&kind)
        } else {
            false
        }
    }

    fn advance(&mut self) -> &Token {
        let tok = &self.tokens[self.pos.min(self.tokens.len() - 1)];
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn advance_kind(&mut self) -> TokenKind {
        let kind = self.peek_kind();
        self.advance();
        kind
    }

    fn current_span(&self) -> Span {
        if self.pos < self.tokens.len() {
            self.tokens[self.pos].span
        } else {
            Span::dummy()
        }
    }

    fn expect(&mut self, kind: TokenKind) {
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


    fn expect_keyword(&mut self, kind: TokenKind) {
        self.expect(kind);
    }

    fn expect_ident(&mut self) -> String {
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

    fn expect_string(&mut self) -> String {
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

    fn skip_newlines(&mut self) {
        while self.check(TokenKind::Newline) {
            self.advance();
        }
    }

    fn skip_newlines_and_indent(&mut self) {
        while matches!(self.peek_kind(), TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent) {
            self.advance();
        }
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.tokens.len() || self.tokens[self.pos].kind == TokenKind::Eof
    }

    /// Advance past tokens until we reach a statement boundary:
    /// a Newline/Dedent at the top level, or EOF.
    /// This lets parse_module continue reporting errors for later statements.
    fn synchronize(&mut self) {
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
