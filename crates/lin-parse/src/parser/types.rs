use lin_common::{Diagnostic, Span};
use lin_lex::TokenKind;
use crate::ast::*;
use super::Parser;

impl Parser {
    pub(crate) fn parse_type_expr(&mut self) -> TypeExpr {
        let first = self.parse_type_intersection();
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
                types.push(self.parse_type_intersection());
            }
            TypeExpr::Union(types, Span::dummy())
        } else {
            first
        }
    }

    /// Record intersection `A & B [& C …]` (ADR-061). Binds TIGHTER than union `|` (TS-style), so
    /// `A & B | C` parses as `(A & B) | C`. Left-associative. Sits between `parse_type_expr` (which
    /// handles `|`) and `parse_type_primary` (the leaves). A single operand passes straight through,
    /// so non-intersection types are unaffected.
    pub(crate) fn parse_type_intersection(&mut self) -> TypeExpr {
        let first = self.parse_type_primary();
        if !self.check(TokenKind::Amp) {
            return first;
        }
        let mut types = vec![first];
        while self.check(TokenKind::Amp) {
            self.advance();
            self.skip_newlines();
            types.push(self.parse_type_primary());
        }
        TypeExpr::Intersection(types, Span::dummy())
    }

    pub(crate) fn parse_type_expr_with_leading_pipe(&mut self) -> TypeExpr {
        if self.check(TokenKind::Pipe) {
            let mut types = Vec::new();
            while self.check(TokenKind::Pipe) {
                self.advance();
                self.skip_newlines();
                types.push(self.parse_type_intersection());
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
        // Read-and-clear the return-type context: it applies only to the OUTERMOST type primary,
        // so the parameter types / inner return type / object & array element types all parse with
        // normal rules. (Token-free recursion from `parse_type_expr`→`parse_type_intersection`
        // →`parse_type_primary` carries the flag straight here for the outermost type only.)
        let in_return_type = self.in_return_type;
        self.in_return_type = false;
        let base = match self.peek_kind() {
            TokenKind::LParen => {
                // Either a function type `(T1, T2) => U` OR a parenthesized (grouped) type
                // `(T)` — the latter is needed so a union/intersection can take a postfix `[]`
                // array suffix, e.g. `(String | Null)[]`. We can't tell which until after the
                // `)`: a single parenthesized type followed by `=>` is a function with one param;
                // the same type NOT followed by `=>` is a grouped type. So parse the comma-list,
                // consume `)`, then branch on whether `=>` follows.
                self.advance();
                let mut params = Vec::new();
                let mut saw_comma = false;
                while !self.check(TokenKind::RParen) && !self.is_at_end() {
                    let loop_start = self.pos;
                    params.push(self.parse_type_expr());
                    if self.check(TokenKind::Comma) {
                        saw_comma = true;
                        self.advance();
                    }
                    if self.ensure_progress(loop_start) {
                        continue;
                    }
                }
                self.expect(TokenKind::RParen);
                // In return-type position, a single parenthesised type that is ITSELF a complete
                // function type is a GROUPED return type (`(h): ((Json) => Json) => body`): the
                // outer parens already closed the type, so a trailing `=>` is the function-body
                // arrow, not a type-level arrow. Returning the inner function type here leaves the
                // body `=>` for `parse_function_expr` to consume. (A genuine higher-order type
                // alias `((Json) => Json) => Json` is never parsed in return position, so it is
                // unaffected.)
                let grouped_fn_return = in_return_type
                    && !saw_comma
                    && params.len() == 1
                    && matches!(params[0], TypeExpr::Function(..));
                // A `=>` makes it a function type. A single grouped type with no `=>` is just
                // that type (and may take a postfix `[]` below). A comma-list / zero-arg with no
                // `=>` is malformed as a grouped type, so still expect the arrow (preserving the
                // original error) and treat it as a function type.
                if !grouped_fn_return && (self.check(TokenKind::Arrow) || saw_comma || params.len() != 1) {
                    self.expect(TokenKind::Arrow);
                    // The return parses with FULL type-expression precedence so a `|`/`&`
                    // continuation binds to the RETURN, e.g. `(Json) => Int64 | Error` is
                    // `(Json) => (Int64 | Error)` (a callable returning a union), not the
                    // non-callable `((Json) => Int64) | Error` that `parse_type_primary`
                    // (single-leaf) produced. The grouped-type path below is unaffected: it
                    // only fires when NO `=>` follows, so `(Int32 | Null)[]` still parses as a
                    // grouped union with an array suffix.
                    let ret = self.parse_type_expr();
                    TypeExpr::Function(params, Box::new(ret), Span::dummy())
                } else {
                    // `(T)` — grouped type. Unwrap to the inner type so the postfix `[]` loop
                    // (and `&`/`|` continuations handled by callers) apply to it directly.
                    params.into_iter().next().unwrap()
                }
            }
            TokenKind::LBrace => {
                // Object type
                let span = self.current_span();
                self.advance();
                self.skip_newlines();
                // Index-signature form `{ Key: T }` (ADR-055): a bare identifier key (an Ident,
                // not a string literal) followed by `:`. The key may be any type expression — it
                // must resolve to `String` (possibly via a type alias such as `StopID = String`),
                // which is validated at type-resolution time, not here. Records use string-literal
                // keys, so a bare `Ident` + `:` is unambiguously the index-sig form.
                let is_index_sig = matches!(self.peek_kind(), TokenKind::Ident(_))
                    && self.check_ahead(TokenKind::Colon, 1);
                if is_index_sig {
                    let key_ty = self.parse_type_expr();
                    self.expect(TokenKind::Colon);
                    let val_ty = self.parse_type_expr();
                    self.skip_newlines();
                    // HINT B: if a comma follows the single `Key: Value` entry, this is almost
                    // certainly a TypeScript-style record with bare (unquoted) field keys, e.g.
                    // `{ bestArrivals: Arrivals, k: UInt8 }`.  Emit ONE clear diagnostic with a
                    // quoting hint, recover by skipping to the matching `}`, and return an empty
                    // placeholder record so downstream checking doesn't pile on more errors.
                    if self.check(TokenKind::Comma) {
                        let comma_span = self.current_span();
                        self.diagnostics.push(
                            Diagnostic::error(
                                comma_span,
                                "a `{ Key: Value }` index-signature type has exactly one entry; \
                                 multiple comma-separated entries look like a record",
                            )
                            .with_help(
                                "for a record type, quote each field key, e.g. \
                                 { \"field\": Type, \"other\": Type }. \
                                 A bare `name: T` is parsed as a { KeyType: ValueType } \
                                 map/index-signature, so `name` is read as a type name.",
                            ),
                        );
                        // Recovery: skip tokens until the matching closing `}`, tracking nesting.
                        let mut depth: usize = 1;
                        while !self.is_at_end() && depth > 0 {
                            match self.peek_kind() {
                                TokenKind::LBrace => { self.advance(); depth += 1; }
                                TokenKind::RBrace => {
                                    depth -= 1;
                                    if depth == 0 {
                                        self.advance(); // consume the closing `}`
                                        break;
                                    }
                                    self.advance();
                                }
                                _ => { self.advance(); }
                            }
                        }
                        return TypeExpr::Object(Vec::new(), span);
                    }
                    self.expect(TokenKind::RBrace);
                    TypeExpr::IndexSig(Box::new(key_ty), Box::new(val_ty), span)
                } else {
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
            TokenKind::IntLit(_, _) => {
                // A non-negative integer-literal singleton type, e.g. `0` or `42`.
                let span = self.current_span();
                let n = if let TokenKind::IntLit(n, _) = self.advance_kind() { n } else { 0 };
                TypeExpr::IntLit(n, span)
            }
            TokenKind::Minus => {
                // A negative integer-literal singleton type, e.g. `-1` in type position.
                let span = self.current_span();
                self.advance(); // consume `-`
                if let TokenKind::IntLit(_n, _) = self.peek_kind() {
                    let n = if let TokenKind::IntLit(n, _) = self.advance_kind() { n } else { 0 };
                    TypeExpr::IntLit(-n, span)
                } else {
                    // Not a negative literal — unknown type, advance past what we consumed.
                    TypeExpr::Named("Unknown".to_string(), span)
                }
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
