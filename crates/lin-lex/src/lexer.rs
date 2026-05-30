use lin_common::Span;
use crate::token::{Token, TokenKind};

pub struct Lexer {
    source: Vec<char>,
    pos: usize,
    file_id: u32,
    indent_stack: Vec<usize>,
    pending_tokens: Vec<Token>,
    at_line_start: bool,
    paren_depth: usize,
    bracket_depth: usize,
    brace_depth: usize,
    interp_depth: Vec<usize>,
}

impl Lexer {
    pub fn new(source: &str, file_id: u32) -> Self {
        Self {
            source: source.chars().collect(),
            pos: 0,
            file_id,
            indent_stack: vec![0],
            pending_tokens: Vec::new(),
            at_line_start: true,
            paren_depth: 0,
            bracket_depth: 0,
            brace_depth: 0,
            interp_depth: Vec::new(),
        }
    }

    pub fn tokenize(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token();
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        // Mark each token whose preceding gap (since the previous token ended) contains a
        // source newline. Explicit `Newline` tokens already signal line breaks at the top
        // level; this additionally surfaces breaks that were suppressed inside `()`/`[]`/`{}`
        // (ADR-004), which the parser needs to keep a line-leading `[`/`(` from gluing onto the
        // previous expression as an index/call. Spans are char offsets into `source`.
        let mut prev_end = 0usize;
        for tok in tokens.iter_mut() {
            let start = tok.span.start as usize;
            if start > prev_end
                && self.source[prev_end..start.min(self.source.len())]
                    .iter()
                    .any(|&c| c == '\n')
            {
                tok.newline_before = true;
            }
            prev_end = prev_end.max(tok.span.end as usize);
        }
        tokens
    }

    fn next_token(&mut self) -> Token {
        if let Some(tok) = self.pending_tokens.pop() {
            return tok;
        }

        if self.at_line_start {
            self.at_line_start = false;
            if !self.inside_balanced() {
                self.handle_indentation();
                if let Some(tok) = self.pending_tokens.pop() {
                    return tok;
                }
            }
        }

        self.skip_spaces();

        if self.pos >= self.source.len() {
            self.emit_dedents_to(0);
            if let Some(tok) = self.pending_tokens.pop() {
                return tok;
            }
            return Token::new(TokenKind::Eof, self.span(self.pos, self.pos));
        }

        let ch = self.source[self.pos];

        if ch == '/' && self.peek_at(1) == Some('/') {
            self.skip_line_comment();
            return self.next_token();
        }

        if ch == '\n' {
            self.pos += 1;
            self.at_line_start = true;
            if self.inside_balanced() {
                return self.next_token();
            }
            return Token::new(TokenKind::Newline, self.span(self.pos - 1, self.pos));
        }

        if ch == '\r' {
            self.pos += 1;
            return self.next_token();
        }

        if ch == '"' {
            return self.lex_string();
        }

        if ch.is_ascii_digit() || (ch == '0' && matches!(self.peek_at(1), Some('x') | Some('b') | Some('o'))) {
            return self.lex_number();
        }

        if ch == '-' && self.is_negative_literal() {
            return self.lex_number();
        }

        if ch.is_alphabetic() || ch == '_' {
            return self.lex_ident_or_keyword();
        }

        self.lex_punctuation()
    }

    fn inside_balanced(&self) -> bool {
        self.paren_depth > 0 || self.bracket_depth > 0 || self.brace_depth > 0 || !self.interp_depth.is_empty()
    }

    fn handle_indentation(&mut self) {
        let mut indent = 0;
        while self.pos < self.source.len() {
            match self.source[self.pos] {
                ' ' => { indent += 1; self.pos += 1; }
                '\n' => {
                    indent = 0;
                    self.pos += 1;
                }
                '\r' => { self.pos += 1; }
                '/' if self.peek_at(1) == Some('/') => {
                    self.skip_line_comment();
                    indent = 0;
                }
                _ => break,
            }
        }

        if self.pos >= self.source.len() {
            return;
        }

        let ch = self.source[self.pos];
        if ch == '&' && self.peek_at(1) == Some('&') {
            return;
        }
        if ch == '|' && self.peek_at(1) == Some('|') {
            return;
        }
        // A line beginning with `.method` is a dot-chain continuation of the previous
        // expression, not a new block (spec §3.2; mirrors the `&&`/`||` suppression above
        // and ADR-006/013). Suppressing INDENT/DEDENT here keeps the enclosing block's
        // indentation accounting balanced when the chain is bound to a `val` inside a
        // function body (the postfix loop still chains via skip_newlines_and_indent).
        // Restricted to `.` followed by an identifier char so the range/spread `...`
        // token is unaffected.
        if ch == '.' && self.peek_at(1).is_some_and(|c| c.is_alphabetic() || c == '_') {
            return;
        }

        let current = *self.indent_stack.last().unwrap();
        if indent > current {
            self.indent_stack.push(indent);
            self.pending_tokens.insert(0, Token::new(TokenKind::Indent, self.span(self.pos, self.pos)));
        } else if indent < current {
            self.emit_dedents_to(indent);
        }
    }

    fn emit_dedents_to(&mut self, target: usize) {
        while *self.indent_stack.last().unwrap() > target {
            self.indent_stack.pop();
            self.pending_tokens.insert(0, Token::new(TokenKind::Dedent, self.span(self.pos, self.pos)));
        }
    }

    fn skip_spaces(&mut self) {
        while self.pos < self.source.len() && self.source[self.pos] == ' ' {
            self.pos += 1;
        }
    }

    fn skip_line_comment(&mut self) {
        while self.pos < self.source.len() && self.source[self.pos] != '\n' {
            self.pos += 1;
        }
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.source.get(self.pos + offset).copied()
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.file_id, start as u32, end as u32)
    }

    fn is_negative_literal(&self) -> bool {
        if self.source[self.pos] != '-' {
            return false;
        }
        if self.pos + 1 >= self.source.len() || !self.source[self.pos + 1].is_ascii_digit() {
            return false;
        }
        if self.pos == 0 {
            return true;
        }
        let prev = self.source[self.pos - 1];
        // `-` begins a negative literal (not a binary subtraction) when it follows a token that
        // cannot end an expression: an opener `( [`, a separator `, =  :`, or whitespace. After
        // `[` specifically, `-1` is an array element, so `[-1, ...]` must lex like `[ -1, ...]`
        // (otherwise the `0 - 1` Sub it would become types as Int32 and can't narrow to e.g.
        // Int8[]).
        matches!(prev, '(' | '[' | ',' | '=' | ':' | ' ')
    }

    fn lex_string(&mut self) -> Token {
        let start = self.pos;
        self.pos += 1; // skip opening "
        let mut current_lit = String::new();
        let mut parts: Vec<crate::token::InterpPart> = Vec::new();
        let mut has_interp = false;

        while self.pos < self.source.len() && self.source[self.pos] != '"' {
            if self.source[self.pos] == '\\' {
                self.pos += 1;
                if self.pos < self.source.len() {
                    match self.source[self.pos] {
                        'n' => current_lit.push('\n'),
                        'r' => current_lit.push('\r'),
                        't' => current_lit.push('\t'),
                        '0' => current_lit.push('\0'),
                        '"' => current_lit.push('"'),
                        '\\' => current_lit.push('\\'),
                        '$' => current_lit.push('$'),
                        'u' => {
                            self.pos += 1; // skip u
                            if self.pos < self.source.len() && self.source[self.pos] == '{' {
                                self.pos += 1;
                                let mut hex = String::new();
                                while self.pos < self.source.len() && self.source[self.pos] != '}' {
                                    hex.push(self.source[self.pos]);
                                    self.pos += 1;
                                }
                                if self.pos < self.source.len() {
                                    self.pos += 1; // skip }
                                }
                                if let Ok(code) = u32::from_str_radix(&hex, 16) {
                                    if let Some(c) = char::from_u32(code) {
                                        current_lit.push(c);
                                    }
                                }
                            }
                            continue;
                        }
                        c => current_lit.push(c),
                    }
                    self.pos += 1;
                }
            } else if self.source[self.pos] == '$' && self.peek_at(1) == Some('{') {
                has_interp = true;
                if !current_lit.is_empty() {
                    parts.push(crate::token::InterpPart::Literal(std::mem::take(&mut current_lit)));
                }
                self.pos += 2; // skip ${

                // Lex tokens until matching }
                let mut expr_tokens = Vec::new();
                let mut depth = 1;
                while self.pos < self.source.len() && depth > 0 {
                    self.skip_spaces();
                    if self.pos >= self.source.len() {
                        break;
                    }
                    if self.source[self.pos] == '}' {
                        depth -= 1;
                        if depth == 0 {
                            self.pos += 1;
                            break;
                        }
                    }
                    if self.source[self.pos] == '{' {
                        depth += 1;
                    }
                    let inner_tok = self.lex_inner_token();
                    expr_tokens.push(inner_tok);
                }
                // Add EOF to expr tokens so parser knows when to stop
                expr_tokens.push(Token::new(TokenKind::Eof, self.span(self.pos, self.pos)));
                parts.push(crate::token::InterpPart::Expr(expr_tokens));
            } else {
                current_lit.push(self.source[self.pos]);
                self.pos += 1;
            }
        }

        if self.pos < self.source.len() {
            self.pos += 1; // skip closing "
        }

        if has_interp {
            if !current_lit.is_empty() {
                parts.push(crate::token::InterpPart::Literal(current_lit));
            }
            Token::new(TokenKind::InterpString(parts), self.span(start, self.pos))
        } else {
            Token::new(TokenKind::StringLit(current_lit), self.span(start, self.pos))
        }
    }

    fn lex_inner_token(&mut self) -> Token {
        self.skip_spaces();
        if self.pos >= self.source.len() {
            return Token::new(TokenKind::Eof, self.span(self.pos, self.pos));
        }

        let ch = self.source[self.pos];

        if ch == '"' {
            return self.lex_string();
        }
        if ch.is_ascii_digit() {
            return self.lex_number();
        }
        if ch.is_alphabetic() || ch == '_' {
            return self.lex_ident_or_keyword();
        }
        self.lex_punctuation()
    }

    fn lex_number(&mut self) -> Token {
        let start = self.pos;
        let negative = self.source[self.pos] == '-';
        if negative {
            self.pos += 1;
        }

        let mut is_float = false;

        if self.source[self.pos] == '0' && self.pos + 1 < self.source.len() {
            match self.source[self.pos + 1] {
                'x' | 'X' => return self.lex_hex(start, negative),
                'b' | 'B' => return self.lex_binary(start, negative),
                'o' | 'O' => return self.lex_octal(start, negative),
                _ => {}
            }
        }

        let mut num_str = String::new();
        while self.pos < self.source.len() && (self.source[self.pos].is_ascii_digit() || self.source[self.pos] == '_') {
            if self.source[self.pos] != '_' {
                num_str.push(self.source[self.pos]);
            }
            self.pos += 1;
        }

        if self.pos < self.source.len() && self.source[self.pos] == '.' && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            is_float = true;
            num_str.push('.');
            self.pos += 1;
            while self.pos < self.source.len() && (self.source[self.pos].is_ascii_digit() || self.source[self.pos] == '_') {
                if self.source[self.pos] != '_' {
                    num_str.push(self.source[self.pos]);
                }
                self.pos += 1;
            }
        }

        if self.pos < self.source.len() && (self.source[self.pos] == 'e' || self.source[self.pos] == 'E') {
            is_float = true;
            num_str.push('e');
            self.pos += 1;
            if self.pos < self.source.len() && (self.source[self.pos] == '+' || self.source[self.pos] == '-') {
                num_str.push(self.source[self.pos]);
                self.pos += 1;
            }
            while self.pos < self.source.len() && self.source[self.pos].is_ascii_digit() {
                num_str.push(self.source[self.pos]);
                self.pos += 1;
            }
        }

        // Skip type suffixes (i8, u32, f32, etc.) - we just consume them
        while self.pos < self.source.len() && (self.source[self.pos].is_alphabetic() || self.source[self.pos].is_ascii_digit()) {
            let c = self.source[self.pos];
            if c == 'i' || c == 'u' || c == 'f' {
                self.pos += 1;
                while self.pos < self.source.len() && self.source[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }

        let span = self.span(start, self.pos);
        if is_float {
            let val: f64 = num_str.parse().unwrap_or(0.0);
            let val = if negative { -val } else { val };
            Token::new(TokenKind::FloatLit(val), span)
        } else {
            // Parse as i64 when in range; otherwise fall back to u64 and store its bit
            // pattern (so UInt64 literals > i64::MAX, e.g. 18446744073709551615, survive into
            // codegen — they are re-read as u64 via the UInt64 tag).
            let val: i64 = match num_str.parse::<i64>() {
                Ok(v) => v,
                Err(_) => num_str.parse::<u64>().map(|u| u as i64).unwrap_or(0),
            };
            let val = if negative { -val } else { val };
            Token::new(TokenKind::IntLit(val), span)
        }
    }

    fn lex_hex(&mut self, start: usize, negative: bool) -> Token {
        self.pos += 2; // skip 0x
        let mut num_str = String::new();
        while self.pos < self.source.len() && (self.source[self.pos].is_ascii_hexdigit() || self.source[self.pos] == '_') {
            if self.source[self.pos] != '_' {
                num_str.push(self.source[self.pos]);
            }
            self.pos += 1;
        }
        let val = i64::from_str_radix(&num_str, 16).unwrap_or(0);
        let val = if negative { -val } else { val };
        Token::new(TokenKind::IntLit(val), self.span(start, self.pos))
    }

    fn lex_binary(&mut self, start: usize, negative: bool) -> Token {
        self.pos += 2; // skip 0b
        let mut num_str = String::new();
        while self.pos < self.source.len() && (self.source[self.pos] == '0' || self.source[self.pos] == '1' || self.source[self.pos] == '_') {
            if self.source[self.pos] != '_' {
                num_str.push(self.source[self.pos]);
            }
            self.pos += 1;
        }
        let val = i64::from_str_radix(&num_str, 2).unwrap_or(0);
        let val = if negative { -val } else { val };
        Token::new(TokenKind::IntLit(val), self.span(start, self.pos))
    }

    fn lex_octal(&mut self, start: usize, negative: bool) -> Token {
        self.pos += 2; // skip 0o
        let mut num_str = String::new();
        while self.pos < self.source.len() && ((self.source[self.pos] >= '0' && self.source[self.pos] <= '7') || self.source[self.pos] == '_') {
            if self.source[self.pos] != '_' {
                num_str.push(self.source[self.pos]);
            }
            self.pos += 1;
        }
        let val = i64::from_str_radix(&num_str, 8).unwrap_or(0);
        let val = if negative { -val } else { val };
        Token::new(TokenKind::IntLit(val), self.span(start, self.pos))
    }

    fn lex_ident_or_keyword(&mut self) -> Token {
        let start = self.pos;
        while self.pos < self.source.len() && (self.source[self.pos].is_alphanumeric() || self.source[self.pos] == '_') {
            self.pos += 1;
        }
        let word: String = self.source[start..self.pos].iter().collect();
        let span = self.span(start, self.pos);
        let kind = match word.as_str() {
            "val" => TokenKind::Val,
            "var" => TokenKind::Var,
            "type" => TokenKind::Type,
            "export" => TokenKind::Export,
            "if" => TokenKind::If,
            "then" => TokenKind::Then,
            "else" => TokenKind::Else,
            "match" => TokenKind::Match,
            "is" => TokenKind::Is,
            "has" => TokenKind::Has,
            "when" => TokenKind::When,
            "import" => TokenKind::Import,
            "from" => TokenKind::From,
            "as" => TokenKind::As,
            "foreign" => TokenKind::Foreign,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "null" => TokenKind::Null,
            _ => TokenKind::Ident(word),
        };
        Token::new(kind, span)
    }

    fn lex_punctuation(&mut self) -> Token {
        let start = self.pos;
        let ch = self.source[self.pos];
        self.pos += 1;

        let kind = match ch {
            '(' => { self.paren_depth += 1; TokenKind::LParen }
            ')' => { self.paren_depth = self.paren_depth.saturating_sub(1); TokenKind::RParen }
            '{' => { self.brace_depth += 1; TokenKind::LBrace }
            '}' => { self.brace_depth = self.brace_depth.saturating_sub(1); TokenKind::RBrace }
            '[' => { self.bracket_depth += 1; TokenKind::LBracket }
            ']' => { self.bracket_depth = self.bracket_depth.saturating_sub(1); TokenKind::RBracket }
            ',' => TokenKind::Comma,
            ':' => TokenKind::Colon,
            '.' => {
                if self.pos + 1 < self.source.len() && self.source[self.pos] == '.' && self.source[self.pos + 1] == '.' {
                    self.pos += 2;
                    TokenKind::DotDotDot
                } else {
                    TokenKind::Dot
                }
            }
            '+' => TokenKind::Plus,
            '-' => TokenKind::Minus,
            '*' => TokenKind::Star,
            '/' => TokenKind::Slash,
            '%' => TokenKind::Percent,
            '=' => {
                if self.pos < self.source.len() && self.source[self.pos] == '>' {
                    self.pos += 1;
                    TokenKind::Arrow
                } else if self.pos < self.source.len() && self.source[self.pos] == '=' {
                    self.pos += 1;
                    TokenKind::EqEq
                } else {
                    TokenKind::Eq
                }
            }
            '!' => {
                if self.pos < self.source.len() && self.source[self.pos] == '=' {
                    self.pos += 1;
                    TokenKind::NotEq
                } else {
                    TokenKind::Bang
                }
            }
            '<' => {
                if self.pos < self.source.len() && self.source[self.pos] == '=' {
                    self.pos += 1;
                    TokenKind::LtEq
                } else {
                    TokenKind::Lt
                }
            }
            '>' => {
                if self.pos < self.source.len() && self.source[self.pos] == '=' {
                    self.pos += 1;
                    TokenKind::GtEq
                } else {
                    TokenKind::Gt
                }
            }
            '&' => {
                if self.pos < self.source.len() && self.source[self.pos] == '&' {
                    self.pos += 1;
                    TokenKind::And
                } else {
                    TokenKind::Amp
                }
            }
            '|' => {
                if self.pos < self.source.len() && self.source[self.pos] == '|' {
                    self.pos += 1;
                    TokenKind::Or
                } else {
                    TokenKind::Pipe
                }
            }
            '^' => TokenKind::Caret,
            '~' => TokenKind::Tilde,
            _ => TokenKind::Ident(ch.to_string()),
        };
        Token::new(kind, self.span(start, self.pos))
    }
}
