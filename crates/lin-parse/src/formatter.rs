/// AST pretty-printer for Lin source files.
///
/// Produces canonical, idempotent output from a parsed `Module`.
///
/// Comments are preserved (reversing the original ADR-040 decision). Lin has only `//`
/// line comments; the lexer no longer discards them but records each on a side channel
/// (`Lexer::comments()`). `Formatter::with_comments` consumes that side channel, attaches
/// each comment to an AST anchor (a statement, a block tail, or a single-expression
/// function body), and re-emits it during formatting: own-line comments as leading lines
/// at the anchor's indentation; trailing comments one space after the anchor's code.
/// `Formatter::new()` keeps the old comment-free behaviour for callers that don't have a
/// comment side channel handy.

use crate::ast::*;
use lin_common::NumSuffix;
use lin_lex::Comment;
use std::collections::HashMap;
use std::cell::RefCell;

/// Comment attachment, computed once in `with_comments` and consulted by the free-function
/// formatters via a thread-local for the duration of a single `format_module` call. Keys are
/// anchor span starts (char offsets).
#[derive(Default)]
struct CommentCtx {
    /// Leading own-line comments, in source order, attached to the anchor that follows them.
    leading: HashMap<u32, Vec<Comment>>,
    /// One trailing comment (same source line as the anchor's code).
    trailing: HashMap<u32, Comment>,
    /// Own-line comments dangling after the last anchor (EOF), emitted at the end.
    dangling: Vec<Comment>,
}

thread_local! {
    static CTX: RefCell<CommentCtx> = RefCell::new(CommentCtx::default());
}

/// Leading comments for `anchor_start`, rendered as `"{ind}{text}\n"` lines (joined), or empty.
fn take_leading(anchor_start: u32, ind: &str) -> String {
    CTX.with(|c| {
        let c = c.borrow();
        match c.leading.get(&anchor_start) {
            Some(cs) if !cs.is_empty() => {
                let mut out = String::new();
                for cm in cs {
                    out.push_str(ind);
                    out.push_str(&cm.text);
                    out.push('\n');
                }
                out
            }
            _ => String::new(),
        }
    })
}

/// The trailing comment text for `anchor_start` (e.g. `"// note"`), or empty.
fn trailing_text(anchor_start: u32) -> String {
    CTX.with(|c| {
        c.borrow()
            .trailing
            .get(&anchor_start)
            .map(|cm| cm.text.clone())
            .unwrap_or_default()
    })
}

/// True if `anchor_start` has any attached comment (leading or trailing).
fn anchor_has_comment(anchor_start: u32) -> bool {
    CTX.with(|c| {
        let c = c.borrow();
        c.trailing.contains_key(&anchor_start)
            || c.leading.get(&anchor_start).is_some_and(|v| !v.is_empty())
    })
}

/// True if any branch body in the `if`/`else if` chain rooted at `expr` carries an
/// attached comment — in which case the inline single-line form must be skipped so the
/// comment has a line to live on. Walks the chain (each `else if` is a nested `Expr::If`).
fn if_has_branch_comment(expr: &Expr) -> bool {
    let mut cur = expr;
    loop {
        match cur {
            Expr::If { then_branch, else_branch, .. } => {
                if anchor_has_comment(then_branch.span().start) {
                    return true;
                }
                if matches!(**else_branch, Expr::If { .. }) {
                    cur = else_branch;
                } else {
                    return anchor_has_comment(else_branch.span().start);
                }
            }
            _ => return false,
        }
    }
}

pub struct Formatter {
    comments: Vec<Comment>,
    /// Char offset of each source line's start (`line_starts[i]` = start of line `i`). Used to
    /// decide whether a trailing comment is on the same source line as an anchor's code.
    line_starts: Vec<usize>,
}

/// Char offsets at which each source line begins. `line_starts[0] == 0`; each `\n` opens a new
/// line at the following char. Offsets are in char space (spans are char offsets — lexer.rs).
fn compute_line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, ch) in source.chars().enumerate() {
        if ch == '\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// 0-based source line containing char offset `pos`.
fn line_of(line_starts: &[usize], pos: usize) -> usize {
    match line_starts.binary_search(&pos) {
        Ok(i) => i,
        Err(i) => i - 1,
    }
}

impl Formatter {
    /// Comment-free formatter (legacy behaviour). Used by callers without a comment side
    /// channel; produces identical output to before comment preservation existed.
    pub fn new() -> Self {
        Formatter { comments: Vec::new(), line_starts: Vec::new() }
    }

    /// Comment-preserving formatter. `comments` is the lexer's side channel for the SAME
    /// `source` that produced the `Module` passed to `format_module`.
    pub fn with_comments(source: &str, comments: Vec<Comment>) -> Self {
        Formatter {
            comments,
            line_starts: compute_line_starts(source),
        }
    }

    pub fn format_module(&self, module: &Module) -> String {
        // Build the comment attachment for this module and install it on the thread-local
        // for the duration of the format, then clear it (so a comment-free Formatter::new()
        // call doesn't see stale state from a previous run).
        let ctx = build_comment_ctx(&self.comments, &self.line_starts, module);
        CTX.with(|c| *c.borrow_mut() = ctx);

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
            let anchor = stmt.span().start;
            out.push_str(&take_leading(anchor, ""));
            let mut s = fmt_stmt(stmt, "");
            let trailing = trailing_text(anchor);
            if !trailing.is_empty() {
                s.push(' ');
                s.push_str(&trailing);
            }
            out.push_str(&s);
            out.push('\n');
        }

        // EOF-dangling own-line comments (after the last anchor).
        let dangling = CTX.with(|c| std::mem::take(&mut c.borrow_mut().dangling));
        for cm in &dangling {
            out.push_str(&cm.text);
            out.push('\n');
        }

        // Clear the thread-local so it doesn't leak into a later comment-free format.
        CTX.with(|c| *c.borrow_mut() = CommentCtx::default());
        out
    }
}

impl Default for Formatter {
    fn default() -> Self {
        Self::new()
    }
}

// ── comment attachment ─────────────────────────────────────────────────────────

/// One comment anchor: an AST position a comment may attach to. `start` keys leading/trailing
/// maps; `end` is used for same-line trailing matching. `trailing_ok` is false for anchors
/// whose recorded span is unreliable for trailing placement (a single-expression function
/// body that is itself control flow — `if`/`match`/block — has a tiny, misleading span and
/// renders across multiple lines, so a trailing comment on it cannot be placed idempotently).
#[derive(Clone, Copy)]
struct Anchor {
    start: u32,
    end: u32,
    trailing_ok: bool,
}

/// Collect every comment anchor in the module, in source order: each statement in a
/// statement-list slot (module top level, every block's stmts, and each block tail), plus
/// each single-expression function body (so a comment between `=>` and the body is kept).
/// Returns anchors sorted by `start` ascending.
fn collect_anchors(module: &Module) -> Vec<Anchor> {
    let mut anchors = Vec::new();
    for stmt in &module.statements {
        collect_anchors_stmt(stmt, &mut anchors);
    }
    anchors.sort_unstable_by_key(|a| a.start);
    anchors
}

fn collect_anchors_stmt(stmt: &Stmt, out: &mut Vec<Anchor>) {
    let sp = stmt.span();
    out.push(Anchor { start: sp.start, end: sp.end, trailing_ok: true });
    match stmt {
        Stmt::Val { value, .. } | Stmt::Var { value, .. } | Stmt::Replace { value, .. } => {
            collect_anchors_expr(value, out)
        }
        Stmt::Expr(e) => collect_anchors_expr(e, out),
        _ => {}
    }
}

fn collect_anchors_expr(expr: &Expr, out: &mut Vec<Anchor>) {
    match expr {
        Expr::Block(stmts, tail, _) => {
            for s in stmts {
                collect_anchors_stmt(s, out);
            }
            let t = tail.span();
            out.push(Anchor { start: t.start, end: t.end, trailing_ok: true });
            collect_anchors_expr(tail, out);
        }
        Expr::Function { body, .. } => {
            // A single-expression body is itself an implicit anchor so a leading comment
            // between `=>` and the body survives (e.g. array.lin `range`). A Block body's
            // anchors are picked up by recursing. A control-flow body (`if`/`match`) has an
            // unreliable, often single-token span and renders multi-line, so it may only
            // carry LEADING comments, never trailing.
            if !matches!(**body, Expr::Block(..)) {
                let b = body.span();
                let trailing_ok = !matches!(**body, Expr::If { .. } | Expr::Match { .. });
                out.push(Anchor { start: b.start, end: b.end, trailing_ok });
            }
            collect_anchors_expr(body, out);
        }
        Expr::If { condition, then_branch, else_branch, .. } => {
            collect_anchors_expr(condition, out);
            // A SINGLE-LINE (atomic) branch body is a comment anchor so an inline branch
            // comment (`if c then BODY  // note`) stays attached to its branch when the
            // chain is re-rendered in block form. A multi-line body (block/if/match) is
            // NOT anchored here: its inner statements/tail are already anchors, and adding
            // a competing anchor at the body's span (which `render_body` won't emit for a
            // non-atomic body) would swallow a demoted comment and drop it.
            if is_atomic(then_branch) {
                let tb = then_branch.span();
                out.push(Anchor { start: tb.start, end: tb.end, trailing_ok: true });
            }
            collect_anchors_expr(then_branch, out);
            // A chained `else if` recurses into the nested `If` (adding its own branch
            // anchors). A terminal `else` with a single-line body gets its own anchor.
            if !matches!(**else_branch, Expr::If { .. }) && is_atomic(else_branch) {
                let eb = else_branch.span();
                out.push(Anchor { start: eb.start, end: eb.end, trailing_ok: true });
            }
            collect_anchors_expr(else_branch, out);
        }
        Expr::Match { scrutinee, arms, .. } => {
            collect_anchors_expr(scrutinee, out);
            for arm in arms {
                collect_anchors_expr(&arm.body, out);
            }
        }
        Expr::Call { func, args, .. } => {
            collect_anchors_expr(func, out);
            for a in args {
                collect_anchors_expr(a, out);
            }
        }
        Expr::DotCall { receiver, args, .. } => {
            collect_anchors_expr(receiver, out);
            if let Some(args) = args {
                for a in args {
                    collect_anchors_expr(a, out);
                }
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_anchors_expr(left, out);
            collect_anchors_expr(right, out);
        }
        Expr::UnaryOp { operand, .. } => collect_anchors_expr(operand, out),
        Expr::Index { object, key, .. } => {
            collect_anchors_expr(object, out);
            collect_anchors_expr(key, out);
        }
        Expr::IndexAssign { object, key, value, .. } => {
            collect_anchors_expr(object, out);
            collect_anchors_expr(key, out);
            collect_anchors_expr(value, out);
        }
        Expr::Assign { value, .. } => collect_anchors_expr(value, out),
        Expr::Array(items, _) | Expr::TupleArgs(items, _) => {
            for it in items {
                collect_anchors_expr(it, out);
            }
        }
        Expr::Object(fields, _) => {
            for f in fields {
                match f {
                    ObjectField::Pair(k, v) => {
                        collect_anchors_expr(k, out);
                        collect_anchors_expr(v, out);
                    }
                    ObjectField::Spread(e) => collect_anchors_expr(e, out),
                }
            }
        }
        Expr::Is { expr, .. } | Expr::Has { expr, .. } => collect_anchors_expr(expr, out),
        Expr::StringInterp(parts, _) => {
            for p in parts {
                if let StringPart::Expr(e) = p {
                    collect_anchors_expr(e, out);
                }
            }
        }
        _ => {}
    }
}

/// Compute leading/trailing/dangling attachment for `comments` against the module's anchors.
fn build_comment_ctx(comments: &[Comment], line_starts: &[usize], module: &Module) -> CommentCtx {
    let mut ctx = CommentCtx::default();
    if comments.is_empty() {
        return ctx;
    }
    let anchors = collect_anchors(module);

    for cm in comments {
        if cm.own_line {
            // Leading: attach to the first anchor that starts strictly after the comment.
            match anchors.iter().find(|a| a.start > cm.span.start) {
                Some(a) => ctx.leading.entry(a.start).or_default().push(cm.clone()),
                None => ctx.dangling.push(cm.clone()),
            }
        } else {
            // Trailing: attach to the closest anchor that both ENDS at or before the comment
            // and lies ENTIRELY on the comment's source line (`start` and `end` both on that
            // line). The whole-line requirement is what keeps formatting idempotent: an anchor
            // that merely *ends* on the comment's line but spans earlier lines (e.g. a
            // multi-line `if`/`match` whose recorded span end happens to fall on the first
            // line) would be rendered across several output lines, so appending the comment to
            // its last line moves the comment relative to the code — and the next format pass
            // would re-attach it differently. Comments with no single-line anchor are demoted
            // to a leading own-line comment of the next anchor (lossless, deterministic).
            let cm_line = line_of(line_starts, cm.span.start as usize);
            let anchor = anchors
                .iter()
                .filter(|a| {
                    a.trailing_ok
                        && a.end <= cm.span.start
                        && line_of(line_starts, a.start as usize) == cm_line
                        && line_of(line_starts, a.end.saturating_sub(1) as usize) == cm_line
                })
                .max_by_key(|a| a.end);
            match anchor {
                // Keep only the first trailing comment per anchor (canonical: one per line).
                Some(a) => {
                    ctx.trailing.entry(a.start).or_insert_with(|| cm.clone());
                }
                // No single-line anchor on this line: demote to a leading own-line comment of
                // the next anchor so it survives at a stable position.
                None => match anchors.iter().find(|a| a.start > cm.span.start) {
                    Some(a) => ctx.leading.entry(a.start).or_default().push(cm.clone()),
                    None => ctx.dangling.push(cm.clone()),
                },
            }
        }
    }
    ctx
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
        UnaryOp::Not => "!",
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

/// The source spelling of a numeric type suffix, for round-tripping in the formatter.
fn suffix_str(suffix: &Option<NumSuffix>) -> &'static str {
    match suffix {
        None => "",
        Some(NumSuffix::I8) => "i8",
        Some(NumSuffix::I16) => "i16",
        Some(NumSuffix::I32) => "i32",
        Some(NumSuffix::I64) => "i64",
        Some(NumSuffix::U8) => "u8",
        Some(NumSuffix::U16) => "u16",
        Some(NumSuffix::U32) => "u32",
        Some(NumSuffix::U64) => "u64",
        Some(NumSuffix::F32) => "f32",
        Some(NumSuffix::F64) => "f64",
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
        TypeExpr::StringLit(s, _) => format!("\"{}\"", s),
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
        Expr::IntLit(n, suffix, _) => format!("{}{}", n, suffix_str(suffix)),
        Expr::FloatLit(f, suffix, _) => format!("{}{}", format_float(*f), suffix_str(suffix)),
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
            // Always parenthesise the parameter list. A bare-identifier lambda (`x => x`) is
            // only legal in argument position (ADR-007), and `fmt_inline` has no notion of its
            // context — it is used for `val` RHS, block tails, etc. as well as arguments. The
            // parenthesised form `(x) => x` is valid in every position, so emitting it
            // unconditionally guarantees the formatter's output always re-parses.
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
        Expr::IntLit(n, suffix, _) => format!("{}{}", n, suffix_str(suffix)),
        Expr::FloatLit(f, suffix, _) => format!("{}{}", format_float(*f), suffix_str(suffix)),
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

            // Try inline. Skipped when any branch body carries an attached comment —
            // the inline form (`if c then a else b`) has nowhere to place it, so we fall
            // through to block form where each branch renders on its own commented line.
            if is_atomic(then_branch) && is_atomic(else_branch) && !if_has_branch_comment(expr) {
                let t = fmt_inline(then_branch);
                let e = fmt_inline(else_branch);
                let inline = format!("if {} then {} else {}", cond, t, e);
                if inline.len() + ind.len() <= 80 {
                    return inline;
                }
            }

            // Block form. Walk the `else if` chain iteratively so each `else if`
            // stays FLAT at `ind` rather than nesting one indent level deeper per
            // arm (which would produce an `else { if … else { if … } }` staircase).
            // Each arm renders as `[else ]if COND then\n  BODY`; the terminal arm
            // renders as `else\n  BODY` (or nothing for a null else in stmt position).
            // fmt_expr returns "first line no indent"; indent_first adds child_ind.
            // Render a branch body as an indented block, carrying any leading/trailing
            // comments anchored to that body (inline branch comments survive block form).
            // Only single-line (atomic) bodies pull comments by their own span key: a
            // multi-line body (Block/if/match) is handled by its inner anchors, and its
            // span.start collides with its first statement's start, so fetching by that key
            // would wrongly re-emit (and, each pass, duplicate) the inner statement's comment.
            let render_body = |body: &Expr| -> String {
                let block = indent_first(&fmt_expr(body, false, &child_ind), &child_ind);
                if !is_atomic(body) {
                    return block;
                }
                let anchor = body.span().start;
                let lead = take_leading(anchor, &child_ind);
                let trailing = trailing_text(anchor);
                if trailing.is_empty() {
                    format!("{}{}", lead, block)
                } else {
                    format!("{}{} {}", lead, block, trailing)
                }
            };

            let mut out = String::new();
            let mut cur_cond = cond;
            let mut cur_then: &Expr = then_branch;
            let mut cur_else: &Expr = else_branch;
            let mut cur_null_else = is_null_else;
            let mut first = true;
            loop {
                if first {
                    out.push_str(&format!("if {} then\n{}", cur_cond, render_body(cur_then)));
                    first = false;
                } else {
                    out.push_str(&format!("\n{}else if {} then\n{}", ind, cur_cond, render_body(cur_then)));
                }

                match cur_else {
                    // `else if` — continue the chain flat at the same `ind`.
                    Expr::If { condition, then_branch, else_branch, .. } => {
                        cur_cond = fmt_expr(condition, false, ind);
                        cur_then = then_branch;
                        cur_null_else = matches!(else_branch.as_ref(), Expr::NullLit(_));
                        cur_else = else_branch;
                    }
                    // Terminal else.
                    _ => {
                        if !(cur_null_else && is_stmt) {
                            out.push_str(&format!("\n{}else\n{}", ind, render_body(cur_else)));
                        }
                        break;
                    }
                }
            }
            out
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

    // Always parenthesise the parameter list. A bare-identifier lambda (`x => x`) is only legal
    // in argument position (ADR-007), and this formatter has no notion of its context, so the
    // paren-less form could land on a `val` RHS or other non-argument position where it does
    // not parse. The parenthesised form `(x) => x` is valid everywhere, keeping the formatter's
    // output round-trip safe.
    let param_part = format!("({}){}", ps.join(", "), ret);

    // Leading comments attached to a single-expression body (Block bodies carry their own
    // comments inside `fmt_block`). A leading comment forces the multi-line form so the
    // comment sits on its own line above the body, at the body's indentation.
    let body_anchor = body.span().start;
    let body_leading = if matches!(body, Expr::Block(..)) {
        String::new()
    } else {
        take_leading(body_anchor, &child_ind)
    };
    let body_trailing = if matches!(body, Expr::Block(..)) {
        String::new()
    } else {
        trailing_text(body_anchor)
    };

    // Block / match / complex if → multi-line.
    let needs_multiline = matches!(body, Expr::Block(..) | Expr::Match { .. })
        || (matches!(body, Expr::If { .. }) && !is_atomic(body))
        || !body_leading.is_empty();

    let mut body_str = fmt_expr(body, false, &child_ind);
    if !body_trailing.is_empty() {
        body_str.push(' ');
        body_str.push_str(&body_trailing);
    }
    // Use multi-line form if the body is inherently multi-line or if the
    // body_str spans multiple lines (e.g. a nested if/else that didn't
    // fit inline).
    if needs_multiline || body_str.contains('\n') {
        let indented = indent_first(&body_str, &child_ind);
        // `body_leading` lines already carry `child_ind` and trailing newlines.
        format!("{} =>\n{}{}", param_part, body_leading, indented)
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
        let anchor = stmt.span().start;
        // Leading comments at `ind` (each its own line) — already include trailing newlines,
        // so trim the final one and push as a separate joined-by-\n line entry.
        let leading = take_leading(anchor, ind);
        if !leading.is_empty() {
            lines.push(leading.trim_end_matches('\n').to_string());
        }
        let mut s = fmt_stmt_in_block(stmt, ind);
        let trailing = trailing_text(anchor);
        if !trailing.is_empty() {
            s.push(' ');
            s.push_str(&trailing);
        }
        lines.push(s);
    }

    // Tail: leading comments, then the tail expr.
    let tail_anchor = tail.span().start;
    let tail_leading = take_leading(tail_anchor, ind);
    if !tail_leading.is_empty() {
        lines.push(tail_leading.trim_end_matches('\n').to_string());
    }
    // fmt_expr with `ind` → first line NO indent, rest have `ind`.
    let tail_s = fmt_expr(tail, false, ind);
    // Prefix the first line of tail_s with `ind` so all lines have uniform indent.
    let mut tail_line = format!("{}{}", ind, tail_s);
    let tail_trailing = trailing_text(tail_anchor);
    if !tail_trailing.is_empty() {
        tail_line.push(' ');
        tail_line.push_str(&tail_trailing);
    }
    lines.push(tail_line);

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

        Stmt::Replace { name, value, .. } => {
            let rhs = fmt_expr(value, false, ind);
            let header = format!("{}replace {} = ", ind, name);
            multiline_concat(&header, &rhs)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;
    use lin_lex::Lexer;

    /// Parse `src`, returning the formatted output. Asserts the source itself parsed cleanly.
    fn format(src: &str) -> String {
        let tokens = Lexer::new(src, 0).tokenize();
        let mut parser = Parser::new(tokens);
        let module = parser.parse_module();
        assert!(
            parser.diagnostics.is_empty(),
            "source did not parse cleanly: {:?}",
            parser.diagnostics
        );
        Formatter::new().format_module(&module)
    }

    /// True if `src` parses with no diagnostics.
    fn parses_clean(src: &str) -> bool {
        let tokens = Lexer::new(src, 0).tokenize();
        let mut parser = Parser::new(tokens);
        let _ = parser.parse_module();
        parser.diagnostics.is_empty()
    }

    /// The core invariant: formatter output must always re-parse. A bare-identifier lambda
    /// (`x => x`) is only legal in argument position (ADR-007), so the formatter must keep the
    /// parens on a lambda bound to a `val` RHS (or any non-argument position). Before the fix,
    /// `val h = (x) => x` was rewritten to the unparseable `val h = x => x`.
    #[test]
    fn lambda_roundtrip_stays_parseable() {
        let cases = [
            "val h = (x) => x\n",
            "val f = (s: String): String => s\n",
            "val g = (x): Int => x\n",
            "val xs = ns.map((x) => x)\n",
        ];
        for src in cases {
            let out = format(src);
            assert!(
                parses_clean(&out),
                "formatter output did not re-parse.\ninput:  {src:?}\noutput: {out:?}"
            );
            // Idempotency: formatting the output again is a fixpoint.
            let out2 = format(&out);
            assert_eq!(out, out2, "formatter not idempotent for {src:?}");
        }
    }

    /// A bare param with a return-type annotation (`x: Ret => body`) is invalid everywhere,
    /// so the formatter must never emit it — it must parenthesise the param.
    #[test]
    fn lambda_with_return_type_is_parenthesised() {
        let out = format("val g = (x): Int => x\n");
        assert!(out.contains("(x): Int =>"), "expected parenthesised param, got {out:?}");
        assert!(parses_clean(&out), "output did not re-parse: {out:?}");
    }
}
