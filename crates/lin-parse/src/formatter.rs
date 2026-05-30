/// AST pretty-printer for Lin source files.
///
/// Produces canonical, idempotent output from a parsed `Module`.
/// NOTE: Comments are not preserved because they are not represented in the
/// AST (they are stripped by the lexer). See docs/DECISIONS.md ADR-040.

use crate::ast::*;

pub struct Formatter;

impl Formatter {
    pub fn new() -> Self {
        Formatter
    }

    pub fn format_module(&self, module: &Module) -> String {
        let mut out = String::new();
        let mut first = true;
        for stmt in &module.statements {
            // Skip bare NullLit statements — they are either no-ops or artifacts
            // of DEDENT-token parsing from indented continuation lines.
            if matches!(stmt, Stmt::Expr(Expr::NullLit(_))) {
                continue;
            }
            if !first {
                out.push('\n');
            }
            first = false;
            out.push_str(&fmt_stmt(stmt, ""));
            out.push('\n');
        }
        out
    }
}

impl Default for Formatter {
    fn default() -> Self {
        Self::new()
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn binop_symbol(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Eq => "==",
        BinOp::NotEq => "!=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::BAnd => "&",
        BinOp::BOr => "|",
        BinOp::BXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
    }
}

fn unaryop_symbol(op: &UnaryOp) -> &'static str {
    match op {
        UnaryOp::BNot => "~",
    }
}

fn format_float(f: f64) -> String {
    let s = format!("{}", f);
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{}.0", s)
    }
}

fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '$' if i + 1 < chars.len() && chars[i + 1] == '{' => {
                // Escape ${ to prevent it being interpreted as string interpolation.
                out.push_str("\\$");
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

/// Given a string `s` that follows the "first line no indent" convention
/// (first line no indent, subsequent lines have absolute indentation),
/// prepend `ind` only to the first line.
fn indent_first(s: &str, ind: &str) -> String {
    let mut out = String::from(ind);
    out.push_str(s);
    out
}

// ── type expressions ──────────────────────────────────────────────────────────

fn fmt_type(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named(name, _) => name.clone(),
        TypeExpr::Generic(name, params, _) => {
            let ps: Vec<String> = params.iter().map(fmt_type).collect();
            format!("{}[{}]", name, ps.join(", "))
        }
        TypeExpr::Array(inner, _) => format!("{}[]", fmt_type(inner)),
        TypeExpr::FixedArray(types, _) => {
            let ts: Vec<String> = types.iter().map(fmt_type).collect();
            format!("[{}]", ts.join(", "))
        }
        TypeExpr::Union(types, _) | TypeExpr::TaggedUnion(types, _) => {
            let ts: Vec<String> = types.iter().map(fmt_type).collect();
            ts.join(" | ")
        }
        TypeExpr::Function(params, ret, _) => {
            let ps: Vec<String> = params.iter().map(fmt_type).collect();
            format!("({}) => {}", ps.join(", "), fmt_type(ret))
        }
        TypeExpr::Object(fields, _) => {
            let fs: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("\"{}\": {}", k, fmt_type(v)))
                .collect();
            format!("{{ {} }}", fs.join(", "))
        }
    }
}

// ── patterns ──────────────────────────────────────────────────────────────────

fn fmt_pattern(pat: &Pattern) -> String {
    match pat {
        Pattern::Ident(name, _) => name.clone(),
        Pattern::TypeName(name, _) => name.clone(),
        Pattern::Wildcard(_) => "_".to_string(),
        Pattern::Literal(e) => fmt_inline(e),
        Pattern::Object(fields, rest, _) => {
            let mut parts: Vec<String> = fields
                .iter()
                .map(|f| {
                    if let Some(key) = &f.key {
                        let pat_str = fmt_pattern(&f.pattern);
                        if let Some(vp) = &f.value_pattern {
                            // Literal value pattern: "key": "value"
                            format!("\"{}\": {}", key, fmt_inline(vp))
                        } else if key == &pat_str && is_valid_ident(key) {
                            // Shorthand: key name matches binding name AND is a bare ident
                            key.clone()
                        } else {
                            // Non-shorthand: always use quoted key to ensure valid syntax
                            format!("\"{}\": {}", key, pat_str)
                        }
                    } else {
                        fmt_pattern(&f.pattern)
                    }
                })
                .collect();
            if let Some(r) = rest {
                parts.push(format!("...{}", r));
            }
            format!("{{ {} }}", parts.join(", "))
        }
        Pattern::Array(pats, rest, _) => {
            let mut parts: Vec<String> = pats.iter().map(fmt_pattern).collect();
            if let Some(r) = rest {
                parts.push(format!("...{}", r));
            }
            format!("[{}]", parts.join(", "))
        }
    }
}

/// Returns true if `s` is a valid bare identifier (starts with letter/_ and contains only
/// alphanumeric/_ characters). Used to decide whether object pattern keys can be unquoted.
fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => chars.all(|c| c.is_alphanumeric() || c == '_'),
        _ => false,
    }
}

fn fmt_match_pattern(mp: &MatchPattern) -> (String, &'static str) {
    match mp {
        MatchPattern::Is(p) => (fmt_pattern(p), "is"),
        MatchPattern::Has(p) => (fmt_pattern(p), "has"),
        MatchPattern::Else => ("".to_string(), "else"),
    }
}

// ── atomicity check ───────────────────────────────────────────────────────────

fn is_atomic(expr: &Expr) -> bool {
    match expr {
        Expr::IntLit(..)
        | Expr::FloatLit(..)
        | Expr::StringLit(..)
        | Expr::BoolLit(..)
        | Expr::NullLit(..)
        | Expr::Ident(..)
        | Expr::StringInterp(..) => true,
        Expr::BinaryOp { left, right, .. } => is_atomic(left) && is_atomic(right),
        Expr::UnaryOp { operand, .. } => is_atomic(operand),
        Expr::Index { object, key, .. } => is_atomic(object) && is_atomic(key),
        Expr::Call { func, args, .. } => {
            is_atomic(func) && args.iter().all(is_atomic)
        }
        Expr::DotCall { receiver, args, .. } => {
            is_atomic(receiver) && args.as_ref().is_none_or(|a| a.iter().all(is_atomic))
        }
        Expr::Assign { value, .. } => is_atomic(value),
        Expr::IndexAssign { object, key, value, .. } => {
            is_atomic(object) && is_atomic(key) && is_atomic(value)
        }
        Expr::Is { expr, .. } | Expr::Has { expr, .. } => is_atomic(expr),
        Expr::TupleArgs(args, _) => args.iter().all(is_atomic),
        Expr::Array(items, _) => items.iter().all(is_atomic),
        Expr::Object(fields, _) => fields.iter().all(|f| match f {
            ObjectField::Pair(k, v) => is_atomic(k) && is_atomic(v),
            ObjectField::Spread(e) => is_atomic(e),
        }),
        Expr::Function { body, .. } => is_atomic(body),
        Expr::If { condition, then_branch, else_branch, .. } => {
            is_atomic(condition) && is_atomic(then_branch) && is_atomic(else_branch)
        }
        Expr::Block(..) | Expr::Match { .. } => false,
    }
}

// ── inline (single-line, no context) formatting ───────────────────────────────

/// Format an expression as a single line, regardless of complexity.
/// Used for string interpolation parts, patterns, and cases where we know
/// the expression fits on one line.
fn fmt_inline(expr: &Expr) -> String {
    match expr {
        Expr::IntLit(n, _) => n.to_string(),
        Expr::FloatLit(f, _) => format_float(*f),
        Expr::StringLit(s, _) => format!("\"{}\"", escape_string(s)),
        Expr::BoolLit(b, _) => b.to_string(),
        Expr::NullLit(_) => "null".to_string(),
        Expr::Ident(name, _) => name.clone(),
        Expr::StringInterp(parts, _) => fmt_interp(parts),
        Expr::BinaryOp { left, op, right, .. } => {
            format!("{} {} {}", fmt_inline(left), binop_symbol(op), fmt_inline(right))
        }
        Expr::UnaryOp { op, operand, .. } => {
            format!("{}{}", unaryop_symbol(op), fmt_inline(operand))
        }
        Expr::Call { func, args, .. } => {
            let fs = fmt_inline(func);
            let as_: Vec<String> = args.iter().map(fmt_inline).collect();
            format!("{}({})", fs, as_.join(", "))
        }
        Expr::DotCall { receiver, method, args, .. } => {
            let r = fmt_inline(receiver);
            match args {
                None => format!("{}.{}", r, method),
                Some(a) => {
                    let as_: Vec<String> = a.iter().map(fmt_inline).collect();
                    format!("{}.{}({})", r, method, as_.join(", "))
                }
            }
        }
        Expr::Index { object, key, .. } => {
            format!("{}[{}]", fmt_inline(object), fmt_inline(key))
        }
        Expr::Array(items, _) => {
            let ss: Vec<String> = items.iter().map(fmt_inline).collect();
            format!("[{}]", ss.join(", "))
        }
        Expr::Object(fields, _) => {
            let fs: Vec<String> = fields
                .iter()
                .map(|f| match f {
                    ObjectField::Pair(k, v) => format!("{}: {}", fmt_inline(k), fmt_inline(v)),
                    ObjectField::Spread(e) => format!("...{}", fmt_inline(e)),
                })
                .collect();
            format!("{{ {} }}", fs.join(", "))
        }
        Expr::Is { expr, pattern, .. } => {
            format!("{} is {}", fmt_inline(expr), fmt_pattern(pattern))
        }
        Expr::Has { expr, pattern, .. } => {
            format!("{} has {}", fmt_inline(expr), fmt_pattern(pattern))
        }
        Expr::Assign { target, value, .. } => format!("{} = {}", target, fmt_inline(value)),
        Expr::IndexAssign { object, key, value, .. } => {
            format!("{}[{}] = {}", fmt_inline(object), fmt_inline(key), fmt_inline(value))
        }
        Expr::TupleArgs(args, _) => {
            let ss: Vec<String> = args.iter().map(fmt_inline).collect();
            format!("({})", ss.join(", "))
        }
        Expr::If { condition, then_branch, else_branch, .. } => {
            format!(
                "if {} then {} else {}",
                fmt_inline(condition),
                fmt_inline(then_branch),
                fmt_inline(else_branch)
            )
        }
        Expr::Function { params, return_type, body, .. } => {
            let ps: Vec<String> = params
                .iter()
                .map(|p| {
                    let pat = fmt_pattern(&p.pattern);
                    if let Some(t) = &p.type_ann {
                        format!("{}: {}", pat, fmt_type(t))
                    } else {
                        pat
                    }
                })
                .collect();
            let ret = return_type
                .as_ref()
                .map(|t| format!(": {}", fmt_type(t)))
                .unwrap_or_default();
            let body = fmt_inline(body);
            if params.len() == 1 && params[0].type_ann.is_none() {
                if let Pattern::Ident(name, _) = &params[0].pattern {
                    return format!("{}{} => {}", name, ret, body);
                }
            }
            format!("({}){} => {}", ps.join(", "), ret, body)
        }
        Expr::Block(stmts, tail, _) => {
            // In Lin, there's no semicolon separator. An inline block with stmts
            // can't be represented on a single line; just show the tail.
            if stmts.is_empty() {
                fmt_inline(tail)
            } else {
                // Return a multi-line representation — this will cause the chain
                // to not use the inline path.
                let parts: Vec<String> = stmts
                    .iter()
                    .map(fmt_stmt_inline)
                    .chain(std::iter::once(fmt_inline(tail)))
                    .collect();
                // Join with \n — this is "long" so callers won't use inline path.
                parts.join("\n")
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            let arm_strs: Vec<String> = arms
                .iter()
                .map(|arm| {
                    let (pat, kw) = fmt_match_pattern(&arm.pattern);
                    let guard = arm
                        .guard
                        .as_ref()
                        .map(|g| format!(" when {}", fmt_inline(g)))
                        .unwrap_or_default();
                    if kw == "else" {
                        format!("else => {}", fmt_inline(&arm.body))
                    } else {
                        format!("{} {}{} => {}", kw, pat, guard, fmt_inline(&arm.body))
                    }
                })
                .collect();
            format!("match {} {}", fmt_inline(scrutinee), arm_strs.join("; "))
        }
    }
}

fn fmt_stmt_inline(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Val { pattern, type_ann, value, exported, .. } => {
            let pfx = if *exported { "export " } else { "" };
            let pat = fmt_pattern(pattern);
            let ty = type_ann
                .as_ref()
                .map(|t| format!(": {}", fmt_type(t)))
                .unwrap_or_default();
            format!("{}val {}{} = {}", pfx, pat, ty, fmt_inline(value))
        }
        Stmt::Var { name, type_ann, value, exported, .. } => {
            let pfx = if *exported { "export " } else { "" };
            let ty = type_ann
                .as_ref()
                .map(|t| format!(": {}", fmt_type(t)))
                .unwrap_or_default();
            format!("{}var {}{} = {}", pfx, name, ty, fmt_inline(value))
        }
        Stmt::Expr(e) => fmt_inline(e),
        _ => {
            // Use a long placeholder that won't fit inline.
            "____non_inline_stmt____".to_string()
        }
    }
}

fn fmt_interp(parts: &[StringPart]) -> String {
    let mut out = String::from('"');
    for p in parts {
        match p {
            StringPart::Literal(s) => {
                for ch in s.chars() {
                    match ch {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        _ => out.push(ch),
                    }
                }
            }
            StringPart::Expr(e) => {
                // Inner expression uses fmt_expr at no particular indent.
                out.push_str(&format!("${{{}}}", fmt_expr(e, false, "")));
            }
        }
    }
    out.push('"');
    out
}

// ── main expression formatter ─────────────────────────────────────────────────

/// Format an expression.
///
/// Contract: the returned string's first line does NOT include leading
/// indentation — the caller supplies `ind` separately. All subsequent lines
/// DO include their absolute indentation (built from `ind` + "  " per nesting
/// level).
///
/// `ind`    — the absolute indentation of the expression itself (used for
///            building child indents and line-length budget).
/// `is_stmt`— true when this expression is in statement position.
fn fmt_expr(expr: &Expr, is_stmt: bool, ind: &str) -> String {
    let child_ind = format!("{}  ", ind);

    match expr {
        // ── atomics ───────────────────────────────────────────────────────────
        Expr::IntLit(n, _) => n.to_string(),
        Expr::FloatLit(f, _) => format_float(*f),
        Expr::StringLit(s, _) => format!("\"{}\"", escape_string(s)),
        Expr::BoolLit(b, _) => b.to_string(),
        Expr::NullLit(_) => "null".to_string(),
        Expr::Ident(name, _) => name.clone(),
        Expr::StringInterp(parts, _) => fmt_interp(parts),

        Expr::BinaryOp { left, op, right, .. } => {
            format!(
                "{} {} {}",
                fmt_expr(left, false, ind),
                binop_symbol(op),
                fmt_expr(right, false, ind)
            )
        }
        Expr::UnaryOp { op, operand, .. } => {
            format!("{}{}", unaryop_symbol(op), fmt_expr(operand, false, ind))
        }
        Expr::Assign { target, value, .. } => {
            format!("{} = {}", target, fmt_expr(value, false, ind))
        }
        Expr::IndexAssign { object, key, value, .. } => {
            format!(
                "{}[{}] = {}",
                fmt_expr(object, false, ind),
                fmt_expr(key, false, ind),
                fmt_expr(value, false, ind)
            )
        }
        Expr::Index { object, key, .. } => {
            format!("{}[{}]", fmt_expr(object, false, ind), fmt_expr(key, false, ind))
        }
        Expr::Is { expr, pattern, .. } => {
            format!("{} is {}", fmt_expr(expr, false, ind), fmt_pattern(pattern))
        }
        Expr::Has { expr, pattern, .. } => {
            format!("{} has {}", fmt_expr(expr, false, ind), fmt_pattern(pattern))
        }
        Expr::TupleArgs(args, _) => {
            let ss: Vec<String> = args.iter().map(|a| fmt_expr(a, false, ind)).collect();
            format!("({})", ss.join(", "))
        }

        // ── Call ──────────────────────────────────────────────────────────────
        Expr::Call { func, args, .. } => {
            let fs = fmt_expr(func, false, ind);
            let as_: Vec<String> = args.iter().map(|a| fmt_expr(a, false, ind)).collect();
            format!("{}({})", fs, as_.join(", "))
        }

        // ── DotCall / method chain ────────────────────────────────────────────
        Expr::DotCall { .. } => fmt_chain(expr, ind),

        // ── Array ─────────────────────────────────────────────────────────────
        Expr::Array(items, _) => {
            if items.len() <= 4 && items.iter().all(is_atomic) {
                let inline = fmt_inline(expr);
                if inline.len() + ind.len() <= 80 {
                    return inline;
                }
            }
            // Multi-line. Each item is at child_ind.
            let item_strs: Vec<String> = items
                .iter()
                .map(|i| {
                    let s = fmt_expr(i, false, &child_ind);
                    format!("{}{},", child_ind, s)
                })
                .collect();
            format!("[\n{}\n{}]", item_strs.join("\n"), ind)
        }

        // ── Object ────────────────────────────────────────────────────────────
        Expr::Object(fields, _) => {
            if fields.is_empty() {
                return "{}".to_string();
            }
            let all_atomic = fields.iter().all(|f| match f {
                ObjectField::Pair(k, v) => is_atomic(k) && is_atomic(v),
                ObjectField::Spread(e) => is_atomic(e),
            });
            if all_atomic && fields.len() <= 2 {
                let inline = fmt_inline(expr);
                if inline.len() + ind.len() <= 80 {
                    return inline;
                }
            }
            let field_strs: Vec<String> = fields
                .iter()
                .map(|f| match f {
                    ObjectField::Pair(k, v) => {
                        let ks = fmt_expr(k, false, &child_ind);
                        let vs = fmt_expr(v, false, &child_ind);
                        format!("{}{}: {},", child_ind, ks, vs)
                    }
                    ObjectField::Spread(e) => {
                        format!("{}...{},", child_ind, fmt_expr(e, false, &child_ind))
                    }
                })
                .collect();
            format!("{{\n{}\n{}}}", field_strs.join("\n"), ind)
        }

        // ── Function ──────────────────────────────────────────────────────────
        Expr::Function { params, return_type, body, .. } => {
            fmt_function(params, return_type.as_ref(), body, ind)
        }

        // ── If ────────────────────────────────────────────────────────────────
        Expr::If { condition, then_branch, else_branch, .. } => {
            let cond = fmt_expr(condition, false, ind);
            let is_null_else = matches!(else_branch.as_ref(), Expr::NullLit(_));

            // Try inline.
            if is_atomic(then_branch) && is_atomic(else_branch) {
                let t = fmt_inline(then_branch);
                let e = fmt_inline(else_branch);
                let inline = format!("if {} then {} else {}", cond, t, e);
                if inline.len() + ind.len() <= 80 {
                    return inline;
                }
            }

            // Block form.
            // fmt_expr returns with "first line no indent"; we add child_ind to first line.
            let then_body = fmt_expr(then_branch, false, &child_ind);
            let then_block = indent_first(&then_body, &child_ind);

            if is_null_else && is_stmt {
                format!("if {} then\n{}", cond, then_block)
            } else {
                let else_body = fmt_expr(else_branch, false, &child_ind);
                let else_block = indent_first(&else_body, &child_ind);
                format!(
                    "if {} then\n{}\n{}else\n{}",
                    cond, then_block, ind, else_block
                )
            }
        }

        // ── Match ─────────────────────────────────────────────────────────────
        Expr::Match { scrutinee, arms, .. } => {
            let scr = fmt_expr(scrutinee, false, ind);
            // Arm lines are at child_ind.
            let arm_strs: Vec<String> = arms
                .iter()
                .map(|arm| {
                    let (pat, kw) = fmt_match_pattern(&arm.pattern);
                    let guard = arm
                        .guard
                        .as_ref()
                        .map(|g| format!(" when {}", fmt_expr(g, false, &child_ind)))
                        .unwrap_or_default();
                    let arm_body_ind = format!("{}  ", child_ind);
                    let body_s = fmt_expr(&arm.body, false, &arm_body_ind);
                    let header = if kw == "else" {
                        format!("{}else =>", child_ind)
                    } else {
                        format!("{}{} {}{} =>", child_ind, kw, pat, guard)
                    };
                    // If body is multi-line, put it on the next line (indented block form).
                    if body_s.contains('\n') {
                        let indented = indent_first(&body_s, &arm_body_ind);
                        format!("{}\n{}", header, indented)
                    } else {
                        format!("{} {}", header, body_s)
                    }
                })
                .collect();
            format!("match {}\n{}", scr, arm_strs.join("\n"))
        }

        // ── Block ─────────────────────────────────────────────────────────────
        Expr::Block(stmts, tail, _) => fmt_block(stmts, tail, ind),
    }
}

/// Format a function expression.
/// `ind` is the indentation of the function expression itself.
/// The body is indented at `ind + "  "`.
fn fmt_function(
    params: &[Param],
    return_type: Option<&TypeExpr>,
    body: &Expr,
    ind: &str,
) -> String {
    let child_ind = format!("{}  ", ind);

    let ps: Vec<String> = params
        .iter()
        .map(|p| {
            let pat = fmt_pattern(&p.pattern);
            if let Some(t) = &p.type_ann {
                format!("{}: {}", pat, fmt_type(t))
            } else {
                pat
            }
        })
        .collect();
    let ret = return_type
        .as_ref()
        .map(|t| format!(": {}", fmt_type(t)))
        .unwrap_or_default();

    let bare = if params.len() == 1 && params[0].type_ann.is_none() {
        if let Pattern::Ident(name, _) = &params[0].pattern {
            Some(name.clone())
        } else {
            None
        }
    } else {
        None
    };
    let param_part = match bare {
        Some(name) => format!("{}{}", name, ret),
        None => format!("({}){}", ps.join(", "), ret),
    };

    // Block / match / complex if → multi-line.
    let needs_multiline = matches!(body, Expr::Block(..) | Expr::Match { .. })
        || (matches!(body, Expr::If { .. }) && !is_atomic(body));

    let body_str = fmt_expr(body, false, &child_ind);
    // Use multi-line form if the body is inherently multi-line or if the
    // body_str spans multiple lines (e.g. a nested if/else that didn't
    // fit inline).
    if needs_multiline || body_str.contains('\n') {
        let indented = indent_first(&body_str, &child_ind);
        format!("{} =>\n{}", param_part, indented)
    } else {
        format!("{} => {}", param_part, body_str)
    }
}

/// Format a block expression (stmts + tail).
///
/// `ind` is the absolute indentation for all lines of the block.
///
/// Contract: follows the "first line no indent" rule of `fmt_expr`.
/// - The FIRST line of the result has NO leading indentation.
/// - All subsequent lines have `ind` as their leading indentation.
fn fmt_block(stmts: &[Stmt], tail: &Expr, ind: &str) -> String {
    let mut lines: Vec<String> = Vec::new();

    // Each stmt is rendered as a fully-indented multi-line string at `ind`.
    // Skip bare NullLit statements (DEDENT artifacts).
    for stmt in stmts {
        if matches!(stmt, Stmt::Expr(Expr::NullLit(_))) {
            continue;
        }
        let s = fmt_stmt_in_block(stmt, ind);
        lines.push(s);
    }

    // Tail: fmt_expr with `ind` → first line NO indent, rest have `ind`.
    let tail_s = fmt_expr(tail, false, ind);
    // Prefix the first line of tail_s with `ind` so all lines have uniform indent.
    lines.push(format!("{}{}", ind, tail_s));

    // Now lines[0] has `ind` on first line. Strip it to satisfy the "first line no indent" rule.
    let joined = lines.join("\n");
    if joined.starts_with(ind) && !ind.is_empty() {
        joined[ind.len()..].to_string()
    } else {
        joined
    }
}

/// Format a statement appearing inside a block, at indentation level `ind`.
/// Returns fully-indented multi-line text (WITH `ind` on all lines, including first).
fn fmt_stmt_in_block(stmt: &Stmt, ind: &str) -> String {
    match stmt {
        Stmt::Val { pattern, type_ann, value, exported, .. } => {
            let pfx = if *exported { "export " } else { "" };
            let pat = fmt_pattern(pattern);
            let ty = type_ann
                .as_ref()
                .map(|t| format!(": {}", fmt_type(t)))
                .unwrap_or_default();
            // Pass `ind` so function bodies are at ind + "  ".
            let rhs = fmt_expr(value, false, ind);
            let header = format!("{}{}{}{}{} = ", ind, pfx, "val ", pat, ty);
            multiline_concat(&header, &rhs)
        }
        Stmt::Var { name, type_ann, value, exported, .. } => {
            let pfx = if *exported { "export " } else { "" };
            let ty = type_ann
                .as_ref()
                .map(|t| format!(": {}", fmt_type(t)))
                .unwrap_or_default();
            let rhs = fmt_expr(value, false, ind);
            let header = format!("{}{}var {}{} = ", ind, pfx, name, ty);
            multiline_concat(&header, &rhs)
        }
        Stmt::Expr(e) => {
            let s = fmt_expr(e, true, ind);
            format!("{}{}", ind, s)
        }
        _ => {
            fmt_stmt(stmt, ind)
        }
    }
}

/// Concatenate a single-line header with a possibly-multi-line body.
/// `header` is the prefix (e.g., "  val x = ").
/// `body` is the body expression string (first line: no leading indent;
/// subsequent lines: have their absolute indentation already).
fn multiline_concat(header: &str, body: &str) -> String {
    let mut lines = body.lines();
    let mut out = format!("{}{}", header, lines.next().unwrap_or(""));
    for line in lines {
        out.push('\n');
        out.push_str(line);
    }
    out
}

// ── top-level statement formatting ───────────────────────────────────────────

/// Format a top-level (or nested) statement at indentation level `ind`.
/// Returns a multi-line string with `ind` as the leading indent on each line.
fn fmt_stmt(stmt: &Stmt, ind: &str) -> String {
    match stmt {
        Stmt::Import { bindings, path, .. } => {
            let parts: Vec<String> = bindings
                .iter()
                .map(|b| match &b.alias {
                    Some(a) => format!("{} as {}", b.name, a),
                    None => b.name.clone(),
                })
                .collect();
            format!("{}import {{ {} }} from \"{}\"", ind, parts.join(", "), path)
        }

        Stmt::ForeignImport { path, bindings, .. } => {
            let mut out = format!("{}import foreign \"{}\"", ind, path);
            for b in bindings {
                out.push_str(&format!("\n{}  val {}: {}", ind, b.name, fmt_type(&b.type_ann)));
            }
            out
        }

        Stmt::Val { pattern, type_ann, value, exported, .. } => {
            let pfx = if *exported { "export " } else { "" };
            let pat = fmt_pattern(pattern);
            let ty = type_ann
                .as_ref()
                .map(|t| format!(": {}", fmt_type(t)))
                .unwrap_or_default();
            // Pass `ind` (not child_ind) so function bodies are at ind + "  ".
            let rhs = fmt_expr(value, false, ind);
            let header = format!("{}{}{}{}{} = ", ind, pfx, "val ", pat, ty);
            multiline_concat(&header, &rhs)
        }

        Stmt::Var { name, type_ann, value, exported, .. } => {
            let pfx = if *exported { "export " } else { "" };
            let ty = type_ann
                .as_ref()
                .map(|t| format!(": {}", fmt_type(t)))
                .unwrap_or_default();
            let rhs = fmt_expr(value, false, ind);
            let header = format!("{}{}var {}{} = ", ind, pfx, name, ty);
            multiline_concat(&header, &rhs)
        }

        Stmt::TypeDecl { name, params, body, exported, .. } => {
            let pfx = if *exported { "export " } else { "" };
            let ty = fmt_type(body);
            if params.is_empty() {
                format!("{}{}type {} = {}", ind, pfx, name, ty)
            } else {
                format!("{}{}type {}<{}> = {}", ind, pfx, name, params.join(", "), ty)
            }
        }

        Stmt::Expr(e) => {
            let s = fmt_expr(e, true, ind);
            format!("{}{}", ind, s)
        }
    }
}

// ── dot chain ─────────────────────────────────────────────────────────────────

fn collect_chain(expr: &Expr) -> (&Expr, Vec<(&str, &Option<Vec<Expr>>)>) {
    let mut chain = Vec::new();
    let mut cur = expr;
    loop {
        if let Expr::DotCall { receiver, method, args, .. } = cur {
            chain.push((method.as_str(), args));
            cur = receiver;
        } else {
            break;
        }
    }
    chain.reverse();
    (cur, chain)
}

fn fmt_chain(expr: &Expr, ind: &str) -> String {
    let (root, chain) = collect_chain(expr);
    let child_ind = format!("{}  ", ind);

    // Try inline for short chains.
    if chain.len() < 4 {
        let inline = fmt_inline(expr);
        // Only use inline if it truly fits on one line (no newlines and fits in budget).
        if !inline.contains('\n') && inline.len() + ind.len() <= 120 {
            return inline;
        }
    }

    // Multi-line.
    let root_str = fmt_expr(root, false, ind);
    let call_strs: Vec<String> = chain
        .iter()
        .map(|(method, args)| match args {
            None => format!("{}.{}", child_ind, method),
            Some(a) => {
                let as_: Vec<String> = a.iter().map(|x| fmt_expr(x, false, &child_ind)).collect();
                format!("{}.{}({})", child_ind, method, as_.join(", "))
            }
        })
        .collect();
    format!("{}\n{}", root_str, call_strs.join("\n"))
}
