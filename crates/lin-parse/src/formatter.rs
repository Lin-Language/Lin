/// AST pretty-printer for Lin source files.
///
/// Produces canonical, idempotent output from a parsed `Module`.
///
/// Comments are preserved (reversing the original ADR-025 decision). Lin has only `//`
/// line comments; the lexer no longer discards them but records each on a side channel
/// (`Lexer::comments()`). `Formatter::with_comments` consumes that side channel, attaches
/// each comment to an AST anchor (a statement, a block tail, or a single-expression
/// function body), and re-emits it during formatting: own-line comments as leading lines
/// at the anchor's indentation; trailing comments one space after the anchor's code.
/// `Formatter::new()` keeps the old comment-free behaviour for callers that don't have a
/// comment side channel handy.

use crate::ast::*;
use lin_common::{NumSuffix, Span};
use lin_lex::Comment;
use std::collections::HashMap;
use std::cell::RefCell;

/// Method-chain inlining threshold. A dot-call chain with MORE than this many calls is
/// ALWAYS rendered multiline (one `.method(...)` per line), regardless of length. Chains
/// with this many or fewer calls keep the inline-if-fits behaviour.
const CHAIN_INLINE_MAX: usize = 2;

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
    /// When true, an array/object literal must render multiline even if it would fit
    /// inline — set while rendering the contents of a multiline literal so that every
    /// nested literal is also multiline (Rule 4, fully-recursive JSON). A top-level
    /// literal decides inline-vs-multiline by its own fit; once it goes multiline its
    /// descendants inherit the force flag.
    static FORCE_ML: RefCell<bool> = const { RefCell::new(false) };
    /// The current module's source line starts (char offsets), installed for the duration
    /// of a `format_module` call so the free-function formatters can compute the source
    /// line gap between consecutive statements (Rule 2, blank-line preservation).
    static LINE_STARTS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    /// The current module's source text (as chars), installed alongside LINE_STARTS, used
    /// to detect whether the line preceding a statement is blank (Rule 2). Statement span
    /// ends are unreliable (e.g. an `import` span covers only the keyword), so blank-line
    /// detection works from the FOLLOWING statement's position, scanning the source.
    static SOURCE_CHARS: RefCell<Vec<char>> = const { RefCell::new(Vec::new()) };
    /// Body-span starts of last-arg lambda array/object literals that are hoist targets
    /// (Rule 6). For these, the trailing-lambda `=> [`/`=> {` collapse (Rule 5a) must win
    /// over the author-newline rule (Rule B): the call-form `f(..., () => [` layout is
    /// canonical and a between-`=>`-and-body comment is hoisted to the statement.
    static HOIST_BODIES: RefCell<std::collections::HashSet<u32>> =
        RefCell::new(std::collections::HashSet::new());
    /// True while rendering a call/method ARGUMENT. A single-identifier, type-less lambda in
    /// argument position renders bare (`i => …`, ADR-006); elsewhere (a `val` RHS etc.) it must
    /// be parenthesised (`(i) => …`) to re-parse. `fmt_function` reads this for its OWN params,
    /// then clears it while rendering the body so a nested non-argument lambda isn't affected.
    static IN_ARG_POSITION: RefCell<bool> = const { RefCell::new(false) };
    /// One-shot HARD break: the NEXT array/object literal renders multi-line even if it fits and
    /// the author wrote it inline (the author-inline-wins rule yields to this). Set by the
    /// over-budget trailing-lambda path so `test("long name", () => [ shortbody ])` breaks the
    /// body rather than splitting the arg list. Consumed by the first literal that reads it.
    static HARD_BREAK_LITERAL: RefCell<bool> = const { RefCell::new(false) };
    /// One-shot: when set, the NEXT `fmt_function` whose body is a collection literal renders
    /// that body multi-line and clears the flag. Used by `fmt_call_arglist` for an over-budget
    /// `test("long name", () => [ … ])` so the array breaks (keeping `=> [` on the call line)
    /// rather than the arg list fully splitting. Distinct from FORCE_ML, which `fmt_function`
    /// deliberately clears at a lambda boundary (Rule 4 is about nested JSON, not lambda code).
    static FORCE_NEXT_LAMBDA_BODY_ML: RefCell<bool> = const { RefCell::new(false) };
}

/// True if `body_start` is a Rule 6 hoist target (a last-arg lambda array/object body of a
/// statement-level call), so Rule 5a `=> [` collapse should win over Rule B.
fn is_hoist_body(body_start: u32) -> bool {
    HOIST_BODIES.with(|s| s.borrow().contains(&body_start))
}

/// True if, in the source, the line immediately preceding the line containing char offset
/// `pos` is empty (whitespace only) — i.e. there's a blank line just before the statement
/// (or its leading comment) at `pos`. This is the Rule 2 signal: a single source blank is
/// preserved as exactly one blank line; runs collapse because only one blank is emitted.
/// Returns false when no source info is installed (comment-free formatter).
fn source_blank_before(pos: u32) -> bool {
    LINE_STARTS.with(|ls_c| {
        SOURCE_CHARS.with(|src_c| {
            let ls = ls_c.borrow();
            let src = src_c.borrow();
            if ls.is_empty() {
                return false;
            }
            let line = line_of(&ls, pos as usize);
            if line == 0 {
                return false;
            }
            // The preceding line spans [ls[line-1], ls[line]).
            let start = ls[line - 1];
            let end = ls[line].saturating_sub(1); // exclude the '\n'
            let slice = &src[start..end.min(src.len())];
            slice.iter().all(|c| c.is_whitespace())
        })
    })
}

/// True if the two char offsets are on different source lines (per the installed
/// LINE_STARTS). False if no source info installed (comment-free formatter) — callers
/// then fall back to fit-based layout. This is how the formatter respects the AUTHOR'S
/// newline choice: an `if`/function-body/2-chain that the author wrote multiline stays
/// multiline, and one written inline stays inline.
fn spans_on_different_source_lines(a: u32, b: u32) -> bool {
    LINE_STARTS.with(|ls_c| {
        let ls = ls_c.borrow();
        if ls.is_empty() {
            return false;
        }
        line_of(&ls, a as usize) != line_of(&ls, b as usize)
    })
}

/// Run `f` with the FORCE_ML flag set to `v`, restoring the previous value afterwards.
fn with_force_ml<R>(v: bool, f: impl FnOnce() -> R) -> R {
    let prev = FORCE_ML.with(|c| c.replace(v));
    let r = f();
    FORCE_ML.with(|c| *c.borrow_mut() = prev);
    r
}

fn force_ml() -> bool {
    FORCE_ML.with(|c| *c.borrow())
}

/// Run `f` with IN_ARG_POSITION set to `v`, restoring the previous value afterwards. Used to
/// mark call/method argument rendering so a single-ident lambda there renders bare.
fn with_arg_position<R>(v: bool, f: impl FnOnce() -> R) -> R {
    let prev = IN_ARG_POSITION.with(|c| c.replace(v));
    let r = f();
    IN_ARG_POSITION.with(|c| *c.borrow_mut() = prev);
    r
}

fn in_arg_position() -> bool {
    IN_ARG_POSITION.with(|c| *c.borrow())
}

/// Leading comments for `anchor_start`, rendered as `"{ind}{text}\n"` lines (joined), or empty.
/// Blank lines the author left within the leading block are preserved as exactly one blank each
/// (runs collapse to one) — the same single-blank policy Rule 2 applies between statements:
///   * a blank BETWEEN two consecutive leading comments, and
///   * a blank between the LAST leading comment and the declaration it precedes
/// both survive. This lets a module-header comment block stay visually separated from the doc
/// comment of the first declaration, and a doc comment stay separated from its `val`/`type` when
/// the author wrote it that way (it still hugs the declaration when there's no blank).
fn take_leading(anchor_start: u32, ind: &str) -> String {
    CTX.with(|c| {
        let c = c.borrow();
        match c.leading.get(&anchor_start) {
            Some(cs) if !cs.is_empty() => {
                let line_of_pos = |pos: u32| -> usize {
                    LINE_STARTS.with(|ls_c| {
                        let ls = ls_c.borrow();
                        if ls.is_empty() { 0 } else { line_of(&ls, pos as usize) }
                    })
                };
                let mut out = String::new();
                let mut prev_line: Option<usize> = None;
                for cm in cs {
                    let line = line_of_pos(cm.span.start);
                    // A source gap of >1 line between this comment and the previous one means the
                    // author left a blank line there; reproduce a single blank line.
                    if let Some(p) = prev_line {
                        if line > p + 1 {
                            out.push('\n');
                        }
                    }
                    out.push_str(ind);
                    out.push_str(&cm.text);
                    out.push('\n');
                    prev_line = Some(line);
                }
                out
            }
            _ => String::new(),
        }
    })
}

/// True when the author left a blank line between the LAST leading comment of `anchor_start` and
/// the declaration's code (the anchor sits >1 source line below that comment). Used by the
/// statement emitter to keep a module-header block visually separated from the first declaration.
/// False when no source info is installed (comment-free formatter) or the anchor has no leading.
fn blank_between_last_leading_and(anchor_start: u32) -> bool {
    LINE_STARTS.with(|ls_c| {
        let ls = ls_c.borrow();
        if ls.is_empty() {
            return false;
        }
        CTX.with(|c| {
            let c = c.borrow();
            match c.leading.get(&anchor_start).and_then(|cs| cs.last()) {
                Some(last) => {
                    let last_line = line_of(&ls, last.span.start as usize);
                    let anchor_line = line_of(&ls, anchor_start as usize);
                    anchor_line > last_line + 1
                }
                None => false,
            }
        })
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

/// True if the author column-aligned a match's `=>` — any arm has MORE than one space
/// immediately before its `=>` in source. Opt-in signal; false with no source installed.
fn match_arms_aligned_in_source(arms: &[MatchArm]) -> bool {
    SOURCE_CHARS.with(|src_c| {
        let src = src_c.borrow();
        if src.is_empty() { return false; }
        arms.iter().any(|arm| {
            let lo = arm.span.start as usize;
            let hi = (arm.body.span().start as usize).min(src.len());
            if lo >= hi { return false; }
            let slice = &src[lo..hi];
            let mut arrow = None;
            let mut i = 0;
            while i + 1 < slice.len() {
                if slice[i] == '=' && slice[i + 1] == '>' { arrow = Some(i); }
                i += 1;
            }
            match arrow {
                Some(a) => {
                    let mut spaces = 0; let mut j = a;
                    while j > 0 && slice[j - 1] == ' ' { spaces += 1; j -= 1; }
                    spaces > 1
                }
                None => false,
            }
        })
    })
}

/// True if the trailing comment on `anchor_start` was column-aligned by the author — >1 space
/// before its `//` in source. Opt-in signal; false with no source. Requires a non-space,
/// non-newline char before the space run (so an own-line comment doesn't count).
fn trailing_aligned_in_source(anchor_start: u32) -> bool {
    let cm_start = CTX.with(|c| c.borrow().trailing.get(&anchor_start).map(|cm| cm.span.start as usize));
    let Some(start) = cm_start else { return false };
    SOURCE_CHARS.with(|src_c| {
        let src = src_c.borrow();
        if src.is_empty() || start == 0 || start > src.len() { return false; }
        let mut spaces = 0; let mut j = start;
        while j > 0 && src[j - 1] == ' ' { spaces += 1; j -= 1; }
        spaces > 1 && j > 0 && src[j - 1] != '\n'
    })
}

/// Emit a maximal run of consecutive statements with run-based trailing-comment alignment.
/// Each entry is `(code, trailing, aligned)` where `code` is the rendered statement (no
/// trailing comment), `trailing` is the comment text ("" = none), and `aligned` is the
/// author's opt-in signal. If ANY member opted in, align all trailing `//` to the widest
/// code member (the widest keeps a single space); otherwise single space. Clears `run`.
fn flush_aligned_run(run: &mut Vec<(String, String, bool)>, lines: &mut Vec<String>) {
    if run.is_empty() { return; }
    let any_aligned = run.iter().any(|(_, t, a)| *a && !t.is_empty());
    let width = if any_aligned {
        run.iter()
            .filter(|(_, t, _)| !t.is_empty())
            .map(|(code, _, _)| code.chars().count())
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    for (code, trailing, _) in run.drain(..) {
        if trailing.is_empty() {
            lines.push(code);
        } else if any_aligned {
            let pad = width.saturating_sub(code.chars().count());
            lines.push(format!("{}{} {}", code, " ".repeat(pad), trailing));
        } else {
            lines.push(format!("{} {}", code, trailing));
        }
    }
}

/// The source char offset where the statement at `anchor_start` effectively begins for
/// blank-line purposes: the start of its first leading comment if it has any, else
/// `anchor_start` itself. This keeps a blank line that precedes a leading comment.
fn leading_start(anchor_start: u32) -> u32 {
    CTX.with(|c| {
        let c = c.borrow();
        c.leading
            .get(&anchor_start)
            .and_then(|cs| cs.first())
            .map(|cm| cm.span.start)
            .unwrap_or(anchor_start)
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
    /// The source text as a char vector (for blank-line detection, Rule 2). Empty for the
    /// comment-free `Formatter::new()`.
    source_chars: Vec<char>,
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
        Formatter { comments: Vec::new(), line_starts: Vec::new(), source_chars: Vec::new() }
    }

    /// Comment-preserving formatter. `comments` is the lexer's side channel for the SAME
    /// `source` that produced the `Module` passed to `format_module`.
    pub fn with_comments(source: &str, comments: Vec<Comment>) -> Self {
        Formatter {
            comments,
            line_starts: compute_line_starts(source),
            source_chars: source.chars().collect(),
        }
    }

    pub fn format_module(&self, module: &Module) -> String {
        // Build the comment attachment for this module and install it on the thread-local
        // for the duration of the format, then clear it (so a comment-free Formatter::new()
        // call doesn't see stale state from a previous run).
        let ctx = build_comment_ctx(&self.comments, &self.line_starts, module);
        CTX.with(|c| *c.borrow_mut() = ctx);
        LINE_STARTS.with(|c| *c.borrow_mut() = self.line_starts.clone());
        SOURCE_CHARS.with(|c| *c.borrow_mut() = self.source_chars.clone());
        // Hoist targets: last-arg lambda array/object bodies of statement-level calls. For
        // these the Rule 5a `=> [` collapse wins over the author-newline rule (Rule B).
        let hoist_bodies = build_hoist_redirects(module).collapse_bodies;
        HOIST_BODIES.with(|c| *c.borrow_mut() = hoist_bodies);

        // Top-level statements, with run-based trailing-comment alignment. A run is a
        // maximal sequence of consecutive statements; a blank line or a leading comment
        // breaks the run. We accumulate `lines` (each entry = one emitted output line,
        // joined by '\n' at the end). A blank line is an empty entry.
        let mut lines: Vec<String> = Vec::new();
        let mut run: Vec<(String, String, bool)> = Vec::new();
        let mut first = true;
        for stmt in &module.statements {
            // Skip bare NullLit statements — they are either no-ops or artifacts
            // of DEDENT-token parsing from indented continuation lines.
            if matches!(stmt, Stmt::Expr(Expr::NullLit(_))) {
                continue;
            }
            let anchor = stmt.span().start;
            if !first {
                // Rule 2: emit a blank line before this statement only if the source had a
                // blank line just before it (or its leading comment). Runs collapse to one.
                if source_blank_before(leading_start(anchor)) {
                    flush_aligned_run(&mut run, &mut lines);
                    lines.push(String::new());
                }
            }
            first = false;
            // A leading comment breaks the alignment run.
            let leading = take_leading(anchor, "");
            if !leading.is_empty() {
                flush_aligned_run(&mut run, &mut lines);
                lines.push(leading.trim_end_matches('\n').to_string());
                // Preserve a blank the author left between the last leading comment and the
                // statement itself (e.g. a module-header block above the first declaration).
                if blank_between_last_leading_and(anchor) {
                    lines.push(String::new());
                }
            }
            let s = fmt_stmt(stmt, "");
            let trailing = trailing_text(anchor);
            // A multi-line statement (or a comment-less one) flushes the run, then emits
            // standalone — only single-line statements participate in alignment.
            if s.contains('\n') {
                flush_aligned_run(&mut run, &mut lines);
                if trailing.is_empty() {
                    lines.push(s);
                } else {
                    lines.push(format!("{} {}", s, trailing));
                }
            } else {
                run.push((s, trailing, trailing_aligned_in_source(anchor)));
            }
        }
        flush_aligned_run(&mut run, &mut lines);

        let mut out = String::new();
        for line in &lines {
            out.push_str(line);
            out.push('\n');
        }

        // EOF-dangling own-line comments (after the last anchor).
        let dangling = CTX.with(|c| std::mem::take(&mut c.borrow_mut().dangling));
        for cm in &dangling {
            out.push_str(&cm.text);
            out.push('\n');
        }

        // Clear the thread-locals so they don't leak into a later comment-free format.
        CTX.with(|c| *c.borrow_mut() = CommentCtx::default());
        LINE_STARTS.with(|c| c.borrow_mut().clear());
        SOURCE_CHARS.with(|c| c.borrow_mut().clear());
        HOIST_BODIES.with(|c| c.borrow_mut().clear());
        FORCE_NEXT_LAMBDA_BODY_ML.with(|c| *c.borrow_mut() = false);
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
        Expr::Block(stmts, tail, _, _) => {
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
            if renders_single_line(then_branch) {
                let tb = then_branch.span();
                out.push(Anchor { start: tb.start, end: tb.end, trailing_ok: true });
            }
            collect_anchors_expr(then_branch, out);
            // A chained `else if` recurses into the nested `If` (adding its own branch
            // anchors). A terminal `else` with a single-line body gets its own anchor.
            if !matches!(**else_branch, Expr::If { .. }) && renders_single_line(else_branch) {
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
        Expr::Array(items, _, _) => {
            // Rule ii: each array element is a comment anchor, so an own-line comment before
            // an element renders above it at the element's indent. A trailing comment on a
            // SINGLE-LINE element is stable (kept trailing); a multi-line element's last line is
            // an unreliable anchor, so there it stays leading-only (demotes to the next anchor).
            for it in items {
                let sp = it.span();
                out.push(Anchor { start: sp.start, end: sp.end, trailing_ok: renders_single_line(it) });
                collect_anchors_expr(it, out);
            }
        }
        Expr::TupleArgs(items, _) => {
            for it in items {
                collect_anchors_expr(it, out);
            }
        }
        Expr::Object(fields, _, _) => {
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

/// The result of the hoist analysis. `redirects` re-anchors a comment that would attach to a
/// lambda array/object body to an enclosing anchor (so `() =>` `// comment` `[ … ]` becomes a
/// leading comment of that anchor). `collapse_bodies` is the subset of those lambda bodies whose
/// `=> [`/`=> {` Rule 5a collapse must win over Rule B (statement-level last-arg lambdas only).
struct HoistResult {
    redirects: HashMap<u32, u32>,
    collapse_bodies: std::collections::HashSet<u32>,
}

/// Build the Rule 6 / Rule ii hoist analysis.
///
/// Rule 6 (statement-level): a leading comment that would attach to a lambda array/object body
/// which is the last argument of a call that is itself a statement's whole expression/value is
/// re-anchored to the ENCLOSING STATEMENT, and `() => [` collapses onto one line (collapse body).
///
/// Rule ii (array element): a leading comment between a lambda's `=>` and its array/object body,
/// where the lambda is the last arg of a call that is an ARRAY ELEMENT, is re-anchored to that
/// array element. Here Rule B is preserved (the body keeps the author's own-line layout), so the
/// body is NOT a collapse body. Keys = lambda body span start; values = enclosing anchor start.
fn build_hoist_redirects(module: &Module) -> HoistResult {
    let mut res = HoistResult { redirects: HashMap::new(), collapse_bodies: Default::default() };
    for stmt in &module.statements {
        let (stmt_start, value) = match stmt {
            Stmt::Val { value, span, .. }
            | Stmt::Var { value, span, .. }
            | Stmt::Replace { value, span, .. } => (span.start, Some(value)),
            Stmt::Expr(e) => (e.span().start, Some(e)),
            _ => (0, None),
        };
        if let Some(value) = value {
            collect_hoist_redirects(value, stmt_start, true, &mut res);
        }
    }
    res
}

/// Walk `expr`, recording lambda-body → enclosing-anchor hoist redirects.
///
/// `anchor_start` is the start of the nearest enclosing comment anchor (statement, or array
/// element). `collapse` is true when that anchor is the statement-level outermost call (so the
/// `=> [` collapse should win — Rule 6) and false for an array-element call (Rule ii, Rule B kept).
fn collect_hoist_redirects(expr: &Expr, anchor_start: u32, collapse: bool, res: &mut HoistResult) {
    match expr {
        Expr::Call { args, .. } | Expr::DotCall { args: Some(args), .. } => {
            if let Some(Expr::Function { body, .. }) = args.last() {
                if matches!(**body, Expr::Array(..) | Expr::Object(..)) {
                    res.redirects.insert(body.span().start, anchor_start);
                    if collapse {
                        res.collapse_bodies.insert(body.span().start);
                    }
                }
            }
            // Recurse into every argument so a nested array argument (e.g. `suite("…", [ … ])`)
            // and the lambda bodies inside its elements are processed with their own anchors. A
            // lambda arg's body is recursed through explicitly (the arg itself is a Function,
            // which `collect_hoist_redirects` does not descend into directly).
            for a in args {
                if let Expr::Function { body, .. } = a {
                    collect_hoist_redirects(body, anchor_start, collapse, res);
                } else {
                    collect_hoist_redirects(a, anchor_start, collapse, res);
                }
            }
        }
        // An array literal: each element is its own comment anchor (Rule ii). A call element
        // whose last-arg lambda body carries a between-`=>`-and-body comment hoists that comment
        // to the element (no collapse — Rule B keeps the author's own-line body layout).
        Expr::Array(items, _, _) => {
            for it in items {
                collect_hoist_redirects(it, it.span().start, false, res);
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
    let redirects = build_hoist_redirects(module).redirects;

    // Apply the Rule 6 / Rule ii hoist: if a leading comment's anchor is a lambda array/object
    // body that is the last arg of a statement-level or array-element call, re-anchor it to the
    // enclosing statement / array element.
    let redirect = |start: u32| -> u32 { redirects.get(&start).copied().unwrap_or(start) };

    for cm in comments {
        if cm.own_line {
            // Leading: attach to the first anchor that starts strictly after the comment.
            match anchors.iter().find(|a| a.start > cm.span.start) {
                Some(a) => {
                    ctx.leading.entry(redirect(a.start)).or_default().push(cm.clone())
                }
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
            // Among same-line trailing-ok anchors ending before the comment, prefer the
            // OUTERMOST (smallest start) — a trailing comment belongs to the whole statement/
            // line, not a nested sub-expression. E.g. `val a = [96]   // c` must attach to the
            // val statement, not the inner array element `96` (which, in an inline array, would
            // never be emitted, dropping the comment). A genuinely element-level comment in a
            // MULTI-LINE array sits on the element's OWN line, so the statement isn't a same-
            // line competitor there. Tiebreak on larger end for stability.
            let anchor = anchors
                .iter()
                .filter(|a| {
                    a.trailing_ok
                        && a.end <= cm.span.start
                        && line_of(line_starts, a.start as usize) == cm_line
                        && line_of(line_starts, a.end.saturating_sub(1) as usize) == cm_line
                })
                .min_by_key(|a| (a.start, std::cmp::Reverse(a.end)));
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

/// Binding precedence of a binary operator, matching the parser's ladder
/// (`parser/expr.rs`): larger = binds tighter. Used to decide when a `BinaryOp`
/// operand must be parenthesised to preserve the parsed grouping. ALL binary
/// operators are left-associative.
fn binop_prec(op: &BinOp) -> u8 {
    match op {
        BinOp::Or => 1,
        BinOp::And => 2,
        BinOp::BOr => 3,
        BinOp::BXor => 4,
        BinOp::BAnd => 5,
        BinOp::Eq | BinOp::NotEq => 6,
        BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => 7,
        BinOp::Shl | BinOp::Shr => 8,
        BinOp::Add | BinOp::Sub => 9,
        BinOp::Mul | BinOp::Div | BinOp::Mod => 10,
    }
}

/// True if `base`, when used as the receiver/object/function of a postfix operator
/// (`.method`, `[index]`, `(args)`), must be parenthesised to preserve the parse.
/// Postfix binds tighter than every binary/unary operator and than `is`/`has`, and a
/// bare function literal / if / match / block as a postfix base would also misparse,
/// so all of these need wrapping. Atomic primaries (literals, idents, an already
/// parenthesised-by-construction Array/Object, and nested postfix chains) do not.
fn needs_postfix_parens(base: &Expr) -> bool {
    matches!(
        base,
        Expr::BinaryOp { .. }
            | Expr::UnaryOp { .. }
            | Expr::If { .. }
            | Expr::Match { .. }
            | Expr::Function { .. }
            | Expr::Is { .. }
            | Expr::Has { .. }
            | Expr::Assign { .. }
            | Expr::IndexAssign { .. }
            | Expr::Block(..)
    )
}

/// Render a postfix operator's base (`render` produces its own text), parenthesising
/// when `needs_postfix_parens` says the parse would otherwise change.
fn fmt_postfix_base(base: &Expr, render: impl Fn(&Expr) -> String) -> String {
    let s = render(base);
    if needs_postfix_parens(base) {
        format!("({})", s)
    } else {
        s
    }
}

/// Format a `BinaryOp` operand, wrapping it in parentheses when the parsed
/// grouping would otherwise be lost. `parent` is the enclosing operator; `is_right`
/// marks the right operand (which needs parens at EQUAL precedence too, because the
/// operators are left-associative — `a - (b - c)` must keep its parens, `(a - b) - c`
/// must not). A child binding strictly looser than the parent always needs parens.
/// The rendering closure `render` produces the operand's own (paren-free) text.
fn fmt_binop_operand(operand: &Expr, parent: &BinOp, is_right: bool, render: impl Fn(&Expr) -> String) -> String {
    let s = render(operand);
    if let Expr::BinaryOp { op: child_op, .. } = operand {
        let (cp, pp) = (binop_prec(child_op), binop_prec(parent));
        // Parenthesise when precedence REQUIRES it (a looser child, or equal precedence on the
        // right under left-associativity), OR when the AUTHOR wrote parens around this
        // sub-expression in the source. The parser discards parens, so we recover the author's
        // grouping from the source extent and preserve it rather than stripping "redundant"
        // parens (which, though value-equal, read worse — e.g. `(a / b) * c`).
        let needs = cp < pp || (cp == pp && is_right) || source_parenthesized(operand);
        if needs {
            return format!("({})", s);
        }
    }
    s
}

/// The source extent [start, end) of an expression — start of its leftmost leaf to end of its
/// rightmost leaf. The `BinaryOp` span only covers the operator token, so paren detection needs
/// the operand's true bounds, computed by descending the leftmost/rightmost children.
fn expr_extent(expr: &Expr) -> (u32, u32) {
    fn leftmost(e: &Expr) -> u32 {
        match e {
            Expr::BinaryOp { left, .. } => leftmost(left),
            Expr::DotCall { receiver, .. } => leftmost(receiver),
            Expr::Call { func, .. } => leftmost(func),
            Expr::Index { object, .. } => leftmost(object),
            Expr::Is { expr, .. } | Expr::Has { expr, .. } => leftmost(expr),
            other => other.span().start,
        }
    }
    fn rightmost(e: &Expr) -> u32 {
        match e {
            Expr::BinaryOp { right, .. } => rightmost(right),
            other => other.span().end,
        }
    }
    (leftmost(expr), rightmost(expr))
}

/// True if, in the source, the expression is immediately wrapped in parentheses — the next
/// non-whitespace char before its extent is `(` and the next non-whitespace char after is `)`.
/// Used to preserve author-written grouping parens that the parser discarded. False with no
/// source installed.

/// True if the author wrote an EXPLICIT `else null` (vs the implicit one the parser synthesises
/// for a missing else). The parser gives an implicit null else the `if` KEYWORD's span — which
/// sits before the condition — while an explicit `else null` gets the real `null` token span
/// after the then-branch. So an else whose span starts at/after the condition is explicit. Used
/// to omit only the implicit `else null` (in statement position) and keep an author-written one.
fn else_is_explicit(else_branch: &Expr, condition: &Expr) -> bool {
    else_branch.span().start >= condition.span().start
}

/// True if the source between char offsets `start` and `end` contains a newline — i.e. the
/// author wrote this span across multiple lines. Used to RESPECT an author-multilined
/// array/object literal (never roll a multi-line literal onto one line; we only force one-line
/// literals to break). False with no source installed (fit-based fallback).
/// True if `expr` is (or contains, in argument position) a lambda whose body the AUTHOR wrote
/// on a different source line than the `=>` — Rule B. The inline fast-paths (`fmt_inline`) don't
/// consult Rule B, so they would collapse such a body; callers use this to suppress the inline
/// path and route through `fmt_function`, which honours the author's newline. Only descends the
/// argument-bearing forms a lambda can hide in (call/method args, the chain receiver).
fn has_author_newline_lambda(expr: &Expr) -> bool {
    fn lambda_body_on_new_line(f: &Expr) -> bool {
        if let Expr::Function { body, span, .. } = f {
            // The function span starts at the params/`(`; compare to the body's start line.
            return spans_on_different_source_lines(span.start, body.span().start)
                && !is_hoist_body(body.span().start);
        }
        false
    }
    fn scan_args(args: &[Expr]) -> bool {
        args.iter().any(|a| lambda_body_on_new_line(a) || has_author_newline_lambda(a))
    }
    match expr {
        Expr::Function { .. } => lambda_body_on_new_line(expr),
        Expr::Call { func, args, .. } => {
            scan_args(args) || has_author_newline_lambda(func)
        }
        Expr::DotCall { receiver, args, .. } => {
            args.as_deref().map(scan_args).unwrap_or(false) || has_author_newline_lambda(receiver)
        }
        Expr::Index { object, .. } => has_author_newline_lambda(object),
        _ => false,
    }
}

/// True if the module source is installed (the comment-preserving formatter). When available,
/// author-intent checks (`source_span_multiline`) drive literal layout; when not (a bare
/// `Formatter::new()`), the fit-based / FORCE_ML fallback is used.
fn source_available() -> bool {
    SOURCE_CHARS.with(|c| !c.borrow().is_empty())
}

/// Run `f` with HARD_BREAK_LITERAL set to `v`, restoring afterwards.
fn with_hard_break_literal<R>(v: bool, f: impl FnOnce() -> R) -> R {
    let prev = HARD_BREAK_LITERAL.with(|c| c.replace(v));
    let r = f();
    HARD_BREAK_LITERAL.with(|c| *c.borrow_mut() = prev);
    r
}

/// Consume the one-shot HARD_BREAK_LITERAL flag (returns its value and clears it).
fn take_hard_break_literal() -> bool {
    HARD_BREAK_LITERAL.with(|c| c.replace(false))
}

fn source_span_multiline(start: u32, end: u32) -> bool {
    SOURCE_CHARS.with(|src_c| {
        let src = src_c.borrow();
        if src.is_empty() {
            return false;
        }
        let (s, e) = (start as usize, (end as usize).min(src.len()));
        s < e && src[s..e].iter().any(|&c| c == '\n')
    })
}

fn source_parenthesized(expr: &Expr) -> bool {
    SOURCE_CHARS.with(|src_c| {
        let src = src_c.borrow();
        if src.is_empty() {
            return false;
        }
        let (start, end) = expr_extent(expr);
        let (start, end) = (start as usize, end as usize);
        if start == 0 || end > src.len() {
            return false;
        }
        let mut i = start;
        while i > 0 && src[i - 1].is_whitespace() {
            i -= 1;
        }
        if i == 0 || src[i - 1] != '(' {
            return false;
        }
        let open = i - 1;
        let mut j = end;
        while j < src.len() && src[j].is_whitespace() {
            j += 1;
        }
        if j >= src.len() || src[j] != ')' {
            return false;
        }
        // The `(` at `open` and `)` at `j` must be a MATCHING pair (paren depth returns to 0
        // exactly at `j`). Otherwise the leading `(` belongs to a child sub-expression — e.g.
        // in `(total / X) * X`, the product's leftmost char is preceded by the INNER `(`, which
        // does not wrap the product. Without this, a spurious outer pair would be added.
        let mut depth = 0i32;
        for k in open..=j {
            match src[k] {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return k == j;
                    }
                }
                _ => {}
            }
        }
        false
    })
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
/// Render an integer literal, PRESERVING a radix-prefixed source spelling (`0xFF`, `0b1010`,
/// `0o17`) so the author's intent isn't flattened to decimal (e.g. `0x0F` must not become `15`).
/// The lexer discards the radix (it stores only the value), so we recover the spelling from the
/// original source at the literal's span. We use the source spelling only when it is genuinely a
/// radix-prefixed literal whose value matches `n` — this guards against synthetic IntLit nodes
/// (e.g. the `0` in unary-minus desugaring, whose span points at `-`) and any value mismatch,
/// falling back to plain decimal. Decimal literals are always rendered as decimal.
fn fmt_int_lit(n: i64, suffix: &Option<NumSuffix>, span: &Span) -> String {
    if let Some(spelled) = radix_spelling_at(span, n) {
        format!("{}{}", spelled, suffix_str(suffix))
    } else {
        format!("{}{}", n, suffix_str(suffix))
    }
}

/// If the source text at `span` is a radix-prefixed integer literal (`0x`/`0b`/`0o`, optional
/// leading `-`, optional `_` digit separators, optional type suffix) whose value equals `n`,
/// return its digit spelling INCLUDING the prefix (and sign), without the type suffix. Else None.
fn radix_spelling_at(span: &Span, n: i64) -> Option<String> {
    SOURCE_CHARS.with(|src_c| {
        let src = src_c.borrow();
        if src.is_empty() {
            return None;
        }
        let (s, e) = (span.start as usize, span.end as usize);
        if e > src.len() || s >= e {
            return None;
        }
        let text: String = src[s..e].iter().collect();
        let body = text.trim();
        let (neg, digits) = match body.strip_prefix('-') {
            Some(rest) => (true, rest.trim_start()),
            None => (false, body),
        };
        // Strip an optional type suffix (i8/u32/…) so just the numeric part remains.
        let core = digits.split(|c: char| c == 'i' || c == 'u').next().unwrap_or(digits);
        let lower = core.to_ascii_lowercase();
        let (radix, rest) = if let Some(r) = lower.strip_prefix("0x") {
            (16u32, r)
        } else if let Some(r) = lower.strip_prefix("0b") {
            (2, r)
        } else if let Some(r) = lower.strip_prefix("0o") {
            (8, r)
        } else {
            return None; // decimal — render as decimal
        };
        let cleaned = rest.replace('_', "");
        if cleaned.is_empty() {
            return None;
        }
        // Verify the spelling actually denotes `n` (using the original-case core digits).
        let parsed = i64::from_str_radix(&cleaned, radix).ok()?;
        let value = if neg { parsed.checked_neg()? } else { parsed };
        if value != n {
            return None;
        }
        // Re-emit the prefix + original digits (preserve author case, drop `_`? keep them).
        let core_digits = core.trim();
        Some(if neg { format!("-{}", core_digits) } else { core_digits.to_string() })
    })
}

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
            // A generic type APPLICATION is spelled `Name<Args>` (the parser reads it with
            // `<`…`>` — types.rs), NOT `Name[Args]`. Emitting `[ ]` here produced code that no
            // longer parsed (`val b: Bus<Int32>` round-tripped to `Bus[Int32]`), changing meaning —
            // a formatter invariant violation. Use angle brackets to round-trip.
            let ps: Vec<String> = params.iter().map(fmt_type).collect();
            format!("{}<{}>", name, ps.join(", "))
        }
        TypeExpr::Array(inner, _) => {
            // The postfix `[]` binds tighter than `|`/`&`/`=>`, so an inner union/intersection/
            // function type MUST be parenthesized to round-trip: `Array(Union(String, Null))` is
            // `(String | Null)[]`, not `String | Null[]` (which would parse as `String | (Null[])`).
            let inner_str = fmt_type(inner);
            match inner.as_ref() {
                TypeExpr::Union(..)
                | TypeExpr::TaggedUnion(..)
                | TypeExpr::Intersection(..)
                | TypeExpr::Function(..) => format!("({})[]", inner_str),
                _ => format!("{}[]", inner_str),
            }
        }
        TypeExpr::FixedArray(types, _) => {
            let ts: Vec<String> = types.iter().map(fmt_type).collect();
            format!("[{}]", ts.join(", "))
        }
        TypeExpr::Union(types, _) | TypeExpr::TaggedUnion(types, _) => {
            let ts: Vec<String> = types.iter().map(fmt_type).collect();
            ts.join(" | ")
        }
        TypeExpr::Intersection(types, _) => {
            // `&` binds tighter than `|` (ADR-061); operands are primaries, so no parens needed.
            let ts: Vec<String> = types.iter().map(fmt_type).collect();
            ts.join(" & ")
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
        TypeExpr::IndexSig(value, _) => format!("{{ String: {} }}", fmt_type(value)),
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

/// True if `expr` is a call whose callee is the bare identifier `test` (e.g. `test("…", () => …)`).
/// Used for Rule iii: a blank line is auto-injected between two consecutive `test(...)` elements
/// of an array literal (a test suite's case list). Only the literal callee name `test` qualifies.
fn is_test_call(expr: &Expr) -> bool {
    matches!(expr, Expr::Call { func, .. } if matches!(func.as_ref(), Expr::Ident(name, _) if name == "test"))
}

// ── atomicity check ───────────────────────────────────────────────────────────

/// True if `expr` contains a function or method call anywhere (directly or nested). Used to
/// force a multi-element array literal multi-line when its elements are calls (e.g. a list of
/// `expect(...).toBe(...)` assertions): several calls packed on one line read poorly.
fn contains_call(expr: &Expr) -> bool {
    match expr {
        Expr::Call { .. } | Expr::DotCall { .. } => true,
        Expr::BinaryOp { left, right, .. } => contains_call(left) || contains_call(right),
        Expr::UnaryOp { operand, .. } => contains_call(operand),
        Expr::Index { object, key, .. } => contains_call(object) || contains_call(key),
        Expr::Is { expr, .. } | Expr::Has { expr, .. } => contains_call(expr),
        _ => false,
    }
}

/// True if `expr` will render on a single line — `is_atomic` AND not a multi-element array
/// whose elements contain calls (which Fix 1 forces multi-line). Used to decide whether a
/// trailing comment can stably anchor to it; a multi-line body must not (it would oscillate
/// between trailing on the last line and leading on the next pass).
fn renders_single_line(expr: &Expr) -> bool {
    if !is_atomic(expr) {
        return false;
    }
    if let Expr::Array(items, _, _) = expr {
        // Matches the array inline rule: forced multi-line only when MORE THAN ONE element
        // contains a call. A single call among simple elements still renders single-line.
        if items.len() > 1 && items.iter().filter(|it| contains_call(it)).count() > 1 {
            return false;
        }
    }
    true
}

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
        Expr::Array(items, _, _) => items.iter().all(is_atomic),
        Expr::Object(fields, _, _) => fields.iter().all(|f| match f {
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
        Expr::IntLit(n, suffix, span) => fmt_int_lit(*n, suffix, span),
        Expr::FloatLit(f, suffix, _) => format!("{}{}", format_float(*f), suffix_str(suffix)),
        Expr::StringLit(s, _) => format!("\"{}\"", escape_string(s)),
        Expr::BoolLit(b, _) => b.to_string(),
        Expr::NullLit(_) => "null".to_string(),
        Expr::Ident(name, _) => name.clone(),
        Expr::StringInterp(parts, _) => fmt_interp(parts),
        Expr::BinaryOp { left, op, right, .. } => {
            format!(
                "{} {} {}",
                fmt_binop_operand(left, op, false, fmt_inline),
                binop_symbol(op),
                fmt_binop_operand(right, op, true, fmt_inline)
            )
        }
        Expr::UnaryOp { op, operand, .. } => {
            format!("{}{}", unaryop_symbol(op), fmt_inline(operand))
        }
        Expr::Call { func, args, partial, .. } => {
            let fs = fmt_postfix_base(func, fmt_inline);
            let as_: Vec<String> = with_arg_position(true, || args.iter().map(fmt_inline).collect());
            let trailing = if *partial && !args.is_empty() { "," } else { "" };
            format!("{}({}{})", fs, as_.join(", "), trailing)
        }
        Expr::DotCall { receiver, method, args, partial, .. } => {
            let r = fmt_postfix_base(receiver, fmt_inline);
            match args {
                None => format!("{}.{}", r, method),
                Some(a) => {
                    let as_: Vec<String> = with_arg_position(true, || a.iter().map(fmt_inline).collect());
                    let trailing = if *partial && !a.is_empty() { "," } else { "" };
                    format!("{}.{}({}{})", r, method, as_.join(", "), trailing)
                }
            }
        }
        Expr::Index { object, key, .. } => {
            format!("{}[{}]", fmt_postfix_base(object, fmt_inline), fmt_inline(key))
        }
        Expr::Array(items, _, _) => {
            let ss: Vec<String> = items.iter().map(fmt_inline).collect();
            format!("[{}]", ss.join(", "))
        }
        Expr::Object(fields, _, _) => {
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
        Expr::Function { type_params, params, return_type, body, .. } => {
            let ps: Vec<String> = params.iter().map(fmt_param).collect();
            let ret = return_type
                .as_ref()
                .map(|t| format!(": {}", fmt_type(t)))
                .unwrap_or_default();
            let generics = fmt_type_params(type_params);
            // A single-ident, type-less, non-generic lambda renders BARE only in argument
            // position (ADR-006); elsewhere it is parenthesised so the output re-parses. The
            // body is the lambda's own scope, not argument position — render it with the flag
            // cleared so a nested non-argument lambda stays parenthesised.
            let bare_ok = in_arg_position()
                && generics.is_empty()
                && params.len() == 1
                && params[0].type_ann.is_none()
                && params[0].default.is_none()
                && matches!(&params[0].pattern, Pattern::Ident(..) | Pattern::Wildcard(..));
            let body = with_arg_position(false, || fmt_inline(body));
            if bare_ok {
                format!("{}{} => {}", ps[0], ret, body)
            } else {
                format!("{}({}){} => {}", generics, ps.join(", "), ret, body)
            }
        }
        Expr::Block(stmts, tail, _, _) => {
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
        Expr::IntLit(n, suffix, span) => fmt_int_lit(*n, suffix, span),
        Expr::FloatLit(f, suffix, _) => format!("{}{}", format_float(*f), suffix_str(suffix)),
        Expr::StringLit(s, _) => format!("\"{}\"", escape_string(s)),
        Expr::BoolLit(b, _) => b.to_string(),
        Expr::NullLit(_) => "null".to_string(),
        Expr::Ident(name, _) => name.clone(),
        Expr::StringInterp(parts, _) => fmt_interp(parts),

        Expr::BinaryOp { left, op, right, .. } => {
            let render = |e: &Expr| fmt_expr(e, false, ind);
            format!(
                "{} {} {}",
                fmt_binop_operand(left, op, false, render),
                binop_symbol(op),
                fmt_binop_operand(right, op, true, render)
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
            let base = fmt_postfix_base(object, |e| fmt_expr(e, false, ind));
            format!("{}[{}]", base, fmt_expr(key, false, ind))
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
        Expr::Call { func, args, partial, .. } => {
            let fs = fmt_postfix_base(func, |e| fmt_expr(e, false, ind));
            format!("{}{}", fs, fmt_call_arglist(args, *partial, ind))
        }

        // ── DotCall / method chain ────────────────────────────────────────────
        Expr::DotCall { .. } => fmt_chain(expr, ind),

        // ── Array ─────────────────────────────────────────────────────────────
        Expr::Array(items, span, _) => {
            // Inline fast-path — suppressed once an ancestor literal went multiline
            // (Rule 4): a nested literal must then also render multiline. A MULTI-element
            // array whose elements contain function calls (e.g. a list of `expect(...)`
            // assertions) always renders multiline — packing several calls on one line reads
            // poorly. A single-element array may stay inline regardless. Finally, respect the
            // author's choice: an array the author broke across source lines stays multiline
            // (we never roll a multi-line literal up — only force one-line literals to break).
            // Force multi-line only when MORE THAN ONE element contains a function call (a list
            // of `expect(...)` assertions reads poorly packed on one line). A single call among
            // simple elements — e.g. `[clampSpeed(left + STEP), right]` — stays inline.
            let call_elems = items.iter().filter(|it| contains_call(it)).count();
            let inline_ok = items.len() <= 1
                || (items.len() <= 4 && items.iter().all(is_atomic) && call_elems <= 1);
            // Author-multiline check: the array's `[` span is only the bracket, so scan from
            // the `[` to the last element's end for a newline (the author broke it across lines).
            let author_ml = items
                .last()
                .map(|last| source_span_multiline(span.start, expr_extent(last).1))
                .unwrap_or(false);
            // A HARD break (over-budget trailing lambda) forces multi-line even if it fits and
            // the author wrote it inline; consume the one-shot flag.
            let hard_break = take_hard_break_literal();
            // Inline when it fits AND the author didn't break it. FORCE_ML (Rule 4, recursive
            // JSON) suppresses inline for a nested literal — UNLESS we have the source and can
            // see the author deliberately wrote that nested literal inline, in which case their
            // choice wins (so `[ {…inline…}, {…inline…} ]` keeps the inner objects on one line).
            // An element with an attached comment (leading or trailing) forces multi-line — the
            // inline path can't render element comments, so inlining would DROP them.
            let any_element_comment = items.iter().any(|it| anchor_has_comment(it.span().start));
            if inline_ok && !author_ml && !hard_break && !any_element_comment
                && (!force_ml() || source_available())
            {
                let inline = fmt_inline(expr);
                if inline.len() + ind.len() <= 80 {
                    return inline;
                }
            }
            // Multi-line. Each item is at child_ind. No trailing comma (Rule 3); the
            // separator sits between items only. Contents render with FORCE_ML set so
            // every nested literal is also multiline (Rule 4).
            //
            // Rule ii: each element is a comment anchor — emit its own-line leading comments
            // above it at child_ind.
            // Rule iii: emit exactly one blank line between two consecutive `test(...)` call
            // elements (after the first's comma, before the second's leading comment).
            with_force_ml(true, || {
                let mut out = String::from("[\n");
                let last = items.len() - 1;
                for (idx, i) in items.iter().enumerate() {
                    out.push_str(&take_leading(i.span().start, &child_ind));
                    let s = fmt_expr(i, false, &child_ind);
                    out.push_str(&child_ind);
                    out.push_str(&s);
                    if idx != last {
                        out.push(',');
                    }
                    // A trailing comment on a single-line element renders after its comma,
                    // on the same line (kept stable by the `renders_single_line` anchor above).
                    let trailing = trailing_text(i.span().start);
                    if !trailing.is_empty() {
                        out.push(' ');
                        out.push_str(&trailing);
                    }
                    if idx != last {
                        out.push('\n');
                        // Rule iii: blank line between consecutive `test(...)` elements.
                        if is_test_call(i) && is_test_call(&items[idx + 1]) {
                            out.push('\n');
                        }
                    }
                }
                out.push('\n');
                out.push_str(ind);
                out.push(']');
                out
            })
        }

        // ── Object ────────────────────────────────────────────────────────────
        Expr::Object(fields, span, _) => {
            if fields.is_empty() {
                return "{}".to_string();
            }
            let all_atomic = fields.iter().all(|f| match f {
                ObjectField::Pair(k, v) => is_atomic(k) && is_atomic(v),
                ObjectField::Spread(e) => is_atomic(e),
            });
            // Respect the author's choice: an object broken across source lines stays multiline.
            let last_field_end = fields.last().map(|f| match f {
                ObjectField::Pair(_, v) => expr_extent(v).1,
                ObjectField::Spread(e) => expr_extent(e).1,
            });
            let author_ml = last_field_end
                .map(|end| source_span_multiline(span.start, end))
                .unwrap_or(false);
            let hard_break = take_hard_break_literal();
            if all_atomic && fields.len() <= 2 && !author_ml && !hard_break && (!force_ml() || source_available()) {
                let inline = fmt_inline(expr);
                if inline.len() + ind.len() <= 80 {
                    return inline;
                }
            }
            // No trailing comma (Rule 3); contents forced multiline (Rule 4).
            let field_strs: Vec<String> = with_force_ml(true, || {
                fields
                    .iter()
                    .map(|f| match f {
                        ObjectField::Pair(k, v) => {
                            let ks = fmt_expr(k, false, &child_ind);
                            let vs = fmt_expr(v, false, &child_ind);
                            format!("{}{}: {}", child_ind, ks, vs)
                        }
                        ObjectField::Spread(e) => {
                            format!("{}...{}", child_ind, fmt_expr(e, false, &child_ind))
                        }
                    })
                    .collect()
            });
            format!("{{\n{}\n{}}}", field_strs.join(",\n"), ind)
        }

        // ── Function ──────────────────────────────────────────────────────────
        Expr::Function { type_params, params, return_type, body, span, full_span: _ } => {
            // A lambda body is a fresh layout scope: a multiline ancestor literal must NOT
            // force literals inside the lambda's body to be multiline (Rule 4 is about JSON
            // data nesting, not code buried in a lambda). Clearing FORCE_ML here also keeps
            // formatting idempotent — otherwise a `val x = [1, 2, 3]` inside a lambda body
            // that is an argument to a call inside a literal would flip layout between passes.
            with_force_ml(false, || {
                fmt_function(type_params, params, return_type.as_ref(), body, span.start, ind)
            })
        }

        // ── If ────────────────────────────────────────────────────────────────
        Expr::If { condition, then_branch, else_branch, span, full_span: _ } => {
            let cond = fmt_expr(condition, false, ind);
            let is_null_else = matches!(else_branch.as_ref(), Expr::NullLit(_));
            // A statement-position `if` with a NULL else: the `else null` is implicit and may
            // be omitted. Omit it ONLY when the author did NOT write an explicit `else` in the
            // source (an `else null` they wrote is kept — it may signal intent). `omit_else`
            // gates both the inline and block forms below.
            let _ = span;
            let omit_else = is_stmt && is_null_else && !else_is_explicit(else_branch, condition);

            // Try inline. Skipped when any branch body carries an attached comment —
            // the inline form (`if c then a else b`) has nowhere to place it, so we fall
            // through to block form where each branch renders on its own commented line.
            // Rule A: only take the inline path if the author ALSO wrote the `if` inline,
            // i.e. condition, then_branch and else_branch all START on the same source
            // line. An author-multiline `if` stays multiline even though it would fit.
            let author_inline = !spans_on_different_source_lines(condition.span().start, then_branch.span().start)
                && !spans_on_different_source_lines(condition.span().start, else_branch.span().start);
            if author_inline
                && is_atomic(then_branch)
                && is_atomic(else_branch)
                && !if_has_branch_comment(expr)
            {
                let t = fmt_inline(then_branch);
                let inline = if omit_else {
                    format!("if {} then {}", cond, t)
                } else {
                    format!("if {} then {} else {}", cond, t, fmt_inline(else_branch))
                };
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
                let anchor = body.span().start;
                let lead = take_leading(anchor, &child_ind);
                // A Block/if/match body owns its comments via its OWN inner anchors (fmt_block
                // etc. emit them) — and its span start collides with its first statement's, so
                // reading `trailing_text` here would DOUBLE-emit that statement's comment. Only
                // a single-expression body (e.g. a multi-element array) is handled here.
                let body_owns_comments = matches!(body, Expr::Block(..) | Expr::If { .. } | Expr::Match { .. });
                if body_owns_comments {
                    // A Block body's span.start IS its first statement's start, and `fmt_block`
                    // already emits that statement's leading comment. Prepending `lead` here
                    // would emit it a SECOND time, compounding on every fmt pass. So for a
                    // Block, drop `lead`. (If/Match bodies don't register an anchor at their
                    // span start, so `lead` is empty for them — emit it harmlessly.)
                    if matches!(body, Expr::Block(..)) {
                        return block;
                    }
                    return format!("{}{}", lead, block);
                }
                let trailing = trailing_text(anchor);
                if !is_atomic(body) {
                    // A multi-line single-expression body renders across several lines, so a
                    // trailing comment on its LAST line (e.g. on a `]`) is not a stable anchor —
                    // re-formatting would re-attach it as a leading comment, breaking
                    // idempotency. Hoist it ABOVE the body (own line at the body indent).
                    if trailing.is_empty() {
                        return format!("{}{}", lead, block);
                    }
                    return format!("{}{}{}\n{}", lead, child_ind, trailing, block);
                }
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
                    // Terminal else. Omit an implicit `else null` (stmt position, author didn't
                    // write it); keep an author-written one. Only the OUTERMOST `if`'s else is
                    // gated by `omit_else` (it was computed for `else_branch`); a chained
                    // `else if`'s own null-else is implicit by construction, so `cur_null_else`
                    // && is_stmt still omits it.
                    _ => {
                        let omit = if cur_null_else && std::ptr::eq(cur_else, else_branch.as_ref()) {
                            omit_else
                        } else {
                            cur_null_else && is_stmt
                        };
                        if !omit {
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
            let arm_body_ind = format!("{}  ", child_ind);
            // Opt-in: align `=>` only if the author already column-aligned some arm.
            let align = match_arms_aligned_in_source(arms);
            // Build (head, body, multiline) for each arm. `head` is the `{kw} {pat}{guard}`
            // (or "else") WITHOUT leading indent and WITHOUT ` =>`.
            struct ArmR {
                head: String,
                body: String,
                multiline: bool,
            }
            let rendered: Vec<ArmR> = arms
                .iter()
                .map(|arm| {
                    let (pat, kw) = fmt_match_pattern(&arm.pattern);
                    let guard = arm
                        .guard
                        .as_ref()
                        .map(|g| format!(" when {}", fmt_expr(g, false, &child_ind)))
                        .unwrap_or_default();
                    let body = fmt_expr(&arm.body, false, &arm_body_ind);
                    // Multi-line if it doesn't fit on one line, OR the author put the body on a
                    // different source line than the arm (`has … =>\n  body`) — respect that
                    // choice (Rule B for match arms), don't collapse it onto the `=>` line.
                    let multiline = body.contains('\n')
                        || spans_on_different_source_lines(arm.span.start, arm.body.span().start);
                    let head = if kw == "else" {
                        "else".to_string()
                    } else {
                        format!("{} {}{}", kw, pat, guard)
                    };
                    ArmR { head, body, multiline }
                })
                .collect();
            // Align target: widest head among NON-multiline arms (recomputed from FORMATTED
            // widths, so it stays idempotent even when content reflows).
            let pad_to = if align {
                rendered
                    .iter()
                    .filter(|a| !a.multiline)
                    .map(|a| a.head.chars().count())
                    .max()
                    .unwrap_or(0)
            } else {
                0
            };
            let arm_strs: Vec<String> = rendered
                .iter()
                .map(|a| {
                    if a.multiline {
                        let indented = indent_first(&a.body, &arm_body_ind);
                        format!("{}{} =>\n{}", child_ind, a.head, indented)
                    } else if align {
                        let pad = pad_to.saturating_sub(a.head.chars().count());
                        format!("{}{}{} => {}", child_ind, a.head, " ".repeat(pad), a.body)
                    } else {
                        format!("{}{} => {}", child_ind, a.head, a.body)
                    }
                })
                .collect();
            format!("match {}\n{}", scr, arm_strs.join("\n"))
        }

        // ── Block ─────────────────────────────────────────────────────────────
        Expr::Block(stmts, tail, _, _) => fmt_block(stmts, tail, ind),
    }
}

/// Format a function expression.
/// `ind` is the indentation of the function expression itself.
/// The body is indented at `ind + "  "`.
/// Render a function's generic type-parameter list, e.g. `<T, U>`. Empty for a
/// non-generic function (no angle brackets emitted).
fn fmt_type_params(type_params: &[String]) -> String {
    if type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", type_params.join(", "))
    }
}

/// Render a single function parameter, including its type annotation and any
/// default value (`x: T = expr`). Dropping the default would silently change the
/// program's meaning, which the formatter must never do.
fn fmt_param(p: &Param) -> String {
    let pat = fmt_pattern(&p.pattern);
    let mut s = if let Some(t) = &p.type_ann {
        format!("{}: {}", pat, fmt_type(t))
    } else {
        pat
    };
    if let Some(d) = &p.default {
        s = format!("{} = {}", s, fmt_inline(d));
    }
    s
}

fn fmt_function(
    type_params: &[String],
    params: &[Param],
    return_type: Option<&TypeExpr>,
    body: &Expr,
    fn_start: u32,
    ind: &str,
) -> String {
    let child_ind = format!("{}  ", ind);

    let ps: Vec<String> = params.iter().map(fmt_param).collect();
    let ret = return_type
        .as_ref()
        .map(|t| format!(": {}", fmt_type(t)))
        .unwrap_or_default();
    let generics = fmt_type_params(type_params);

    // A single-identifier, type-less, non-generic lambda renders BARE (`x => …`) ONLY in
    // argument position, where ADR-006 allows it (and idiomatic Lin prefers it: `.for(i => …)`).
    // Anywhere else (a `val` RHS, a block tail, …) the bare form does not re-parse, so the
    // parameter list is parenthesised (`(x) => …`) to keep the formatter's output round-trip
    // safe. A generic function always uses the parenthesised form.
    let bare_ok = in_arg_position()
        && generics.is_empty()
        && params.len() == 1
        && params[0].type_ann.is_none()
        && params[0].default.is_none()
        && matches!(&params[0].pattern, Pattern::Ident(..) | Pattern::Wildcard(..));
    let param_part = if bare_ok {
        format!("{}{}", ps[0], ret)
    } else {
        format!("{}({}){}", generics, ps.join(", "), ret)
    };
    // The body is NOT in argument position (it's the lambda's own scope). Bodies below are
    // rendered inside `with_arg_position(false, …)` so a nested lambda on a `val` RHS inside the
    // body is parenthesised — WITHOUT corrupting the flag for sibling renders in the caller
    // (a bare global set here would leak `false` into the call-arglist's measurement renders).

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

    // Rule B: respect the author's newline choice for the body. If the author wrote the
    // body on a DIFFERENT source line than the function's `=>` (detected via the function
    // span start's line vs the body span start's line), keep the body on its own indented
    // line; if they wrote it inline after `=>`, keep it inline (subject to fit). False when
    // no source info is installed (comment-free formatter falls back to fit-based layout).
    // A Rule 6 hoist body is exempt: its canonical layout is the Rule 5a `=> [` collapse.
    let author_body_on_new_line = spans_on_different_source_lines(fn_start, body.span().start)
        && !is_hoist_body(body.span().start);

    // Rule 5a: a lambda whose body is an array/object literal keeps `=> [` / `=> {`
    // attached on the param line, with the literal's contents flowing beneath. The literal
    // renders at the FUNCTION's own indent (not child_ind), so its closing `]`/`}` lines up
    // under the `=>` line and a leading comment between `=>` and the body has been hoisted
    // away (handled by the caller / comment anchoring). Only when there's no leading comment
    // forcing the split, and only when the author did NOT put the literal on its own line
    // (Rule B): an author who wrote the `[`/`{` on the next line keeps it on its own line.
    if body_leading.is_empty()
        && !author_body_on_new_line
        && matches!(body, Expr::Array(..) | Expr::Object(..))
    {
        // An over-budget enclosing call (e.g. `test("long name", () => [ … ])`) sets
        // FORCE_NEXT_LAMBDA_BODY_ML so this collection body breaks — keeping `=> [` on the call
        // line rather than fully splitting the arg list. The flag is one-shot (consumed here,
        // not propagated into nested lambdas). FORCE_ML can't be reused: `fmt_expr`'s Function
        // arm clears it at the lambda boundary (Rule 4). When set, render the body with FORCE_ML
        // so its (and only its top-level) literal breaks.
        let forced = FORCE_NEXT_LAMBDA_BODY_ML.with(|c| c.replace(false));
        // Also hard-break when an element of the array/object body carries a comment: an inline
        // body can't render element comments, so a single-element `() => [ //c elem ]` would
        // otherwise DROP the comment. Forcing multi-line lets the element's comment emit.
        let body_has_element_comment = match body {
            Expr::Array(items, _, _) => items.iter().any(|it| anchor_has_comment(it.span().start)),
            _ => false,
        };
        let do_force = forced || body_has_element_comment;
        let body_str = with_arg_position(false, || {
            if do_force {
                // HARD break: break the body even though it fits / the author wrote it inline.
                with_hard_break_literal(true, || with_force_ml(true, || fmt_expr(body, false, ind)))
            } else {
                fmt_expr(body, false, ind)
            }
        });
        if body_str.contains('\n') {
            return format!("{} => {}", param_part, body_str);
        }
    }

    // Block / match / complex if → multi-line. Rule B: an author-newline body is also
    // multi-line (rendered on its own indented line under `=>`).
    let needs_multiline = matches!(body, Expr::Block(..) | Expr::Match { .. })
        || (matches!(body, Expr::If { .. }) && !is_atomic(body))
        || author_body_on_new_line
        || !body_leading.is_empty();

    // The body is rendered in STATEMENT position: a lambda's body is its own statement context,
    // so an `if … then …` body with an implicit (author-omitted) null else drops the `else null`.
    let mut body_str = with_arg_position(false, || fmt_expr(body, true, &child_ind));
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
    // Run-based trailing-comment alignment: a maximal run of consecutive single-line
    // statements; a blank line, a leading comment, or a multi-line statement breaks it.
    let mut run: Vec<(String, String, bool)> = Vec::new();

    // Each stmt is rendered as a fully-indented multi-line string at `ind`.
    // Skip bare NullLit statements (DEDENT artifacts).
    let mut seen_stmt = false;
    for stmt in stmts {
        if matches!(stmt, Stmt::Expr(Expr::NullLit(_))) {
            continue;
        }
        let anchor = stmt.span().start;
        // Rule 2: a blank source line before this statement (or its leading comment) is
        // preserved as exactly one blank entry; runs collapse to one. An empty `lines`
        // entry becomes a blank line via the final `join("\n")`. Not applied before the
        // first statement of the block (no leading blank inside a block body).
        if seen_stmt && source_blank_before(leading_start(anchor)) {
            flush_aligned_run(&mut run, &mut lines);
            lines.push(String::new());
        }
        seen_stmt = true;
        // Leading comments at `ind` (each its own line) — already include trailing newlines,
        // so trim the final one and push as a separate joined-by-\n line entry. A leading
        // comment breaks the alignment run.
        let leading = take_leading(anchor, ind);
        if !leading.is_empty() {
            flush_aligned_run(&mut run, &mut lines);
            lines.push(leading.trim_end_matches('\n').to_string());
        }
        let s = fmt_stmt_in_block(stmt, ind);
        let trailing = trailing_text(anchor);
        // Only single-line statements participate in alignment; a multi-line one flushes
        // the run, then emits standalone.
        if s.contains('\n') {
            flush_aligned_run(&mut run, &mut lines);
            if trailing.is_empty() {
                lines.push(s);
            } else {
                lines.push(format!("{} {}", s, trailing));
            }
        } else {
            run.push((s, trailing, trailing_aligned_in_source(anchor)));
        }
    }
    flush_aligned_run(&mut run, &mut lines);

    // Tail: leading comments, then the tail expr.
    let tail_anchor = tail.span().start;
    // A blank source line between the last statement and the tail is preserved.
    if seen_stmt && source_blank_before(leading_start(tail_anchor)) {
        lines.push(String::new());
    }
    let tail_leading = take_leading(tail_anchor, ind);
    if !tail_leading.is_empty() {
        lines.push(tail_leading.trim_end_matches('\n').to_string());
    }
    // fmt_expr with `ind` → first line NO indent, rest have `ind`. The tail is rendered in
    // STATEMENT position: as the block's final expression it's effectively a statement, so an
    // `if … then …` tail with an implicit (author-omitted) null else drops the `else null`.
    let tail_s = fmt_expr(tail, true, ind);
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
            let header = format!("{}{}{}{}{} = ", ind, pfx, "val ", pat, ty);
            fmt_binding_rhs(&header, value, ind)
        }
        Stmt::Var { name, type_ann, value, exported, .. } => {
            let pfx = if *exported { "export " } else { "" };
            let ty = type_ann
                .as_ref()
                .map(|t| format!(": {}", fmt_type(t)))
                .unwrap_or_default();
            let header = format!("{}{}var {}{} = ", ind, pfx, name, ty);
            fmt_binding_rhs(&header, value, ind)
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

/// Render the RHS of a binding (`val`/`var`/`replace`) given an already-built
/// `header` ending in `"= "` (with trailing space) at indentation `ind`.
/// A block-bodied RHS cannot be inlined after `= `: `fmt_block` puts its first
/// statement on the first line (no indent), so `multiline_concat` would collapse
/// the block onto the header line, producing unparseable source. Instead emit
/// `=` (no trailing space), a newline, then the block body at `ind + "  "`.
fn fmt_binding_rhs(header: &str, value: &Expr, ind: &str) -> String {
    if matches!(value, Expr::Block(..)) {
        let child_ind = format!("{}  ", ind);
        let body = fmt_expr(value, false, &child_ind);
        let header = header.strip_suffix(' ').unwrap_or(header);
        format!("{}\n{}{}", header, child_ind, body)
    } else {
        let rhs = fmt_expr(value, false, ind);
        multiline_concat(header, &rhs)
    }
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
            let header = format!("{}{}{}{}{} = ", ind, pfx, "val ", pat, ty);
            fmt_binding_rhs(&header, value, ind)
        }

        Stmt::Var { name, type_ann, value, exported, .. } => {
            let pfx = if *exported { "export " } else { "" };
            let ty = type_ann
                .as_ref()
                .map(|t| format!(": {}", fmt_type(t)))
                .unwrap_or_default();
            let header = format!("{}{}var {}{} = ", ind, pfx, name, ty);
            fmt_binding_rhs(&header, value, ind)
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
            let header = format!("{}replace {} = ", ind, name);
            fmt_binding_rhs(&header, value, ind)
        }
    }
}

// ── call argument lists ─────────────────────────────────────────────────────────

/// Render the parenthesised argument list of a call (`(a, b, c)`), at the call's own
/// indentation `ind`. Implements Rule 5:
///
/// - INLINE when every argument renders single-line and the joined list fits the budget.
/// - TRAILING style (5a) when ALL-but-the-last argument render single-line and only the
///   LAST argument is multi-line: `(a, b, <lastArgFirstLine>` stays on the call's line and
///   the last argument's body flows beneath at `ind`. A trailing lambda `() => [ … ]` keeps
///   its `() => [` on the call line.
/// - FULLY-SPLIT style (5b) otherwise (multiple multi-line args, or a non-last multi-line
///   arg): open paren on the call line, each argument on its own line at child indent, a
///   multi-line lambda arg renders `param =>` then its body indented beneath, comma between
///   args, closing paren on its own line at `ind`.
fn fmt_call_arglist(args: &[Expr], partial: bool, ind: &str) -> String {
    with_arg_position(true, || fmt_call_arglist_inner(args, partial, ind))
}

fn fmt_call_arglist_inner(args: &[Expr], partial: bool, ind: &str) -> String {
    // A trailing comma after the LAST argument requests partial application (`f(x,)`,
    // AST `partial == true`). The checker dispatches partial vs full call on this flag
    // (lin-check call.rs), so the formatter MUST re-emit the comma — dropping it would
    // silently change `add(1,)` (partial) into `add(1)` (full call), changing meaning.
    // An empty arg list cannot be partial (there is no preceding arg for the comma).
    let trailing = if partial && !args.is_empty() { "," } else { "" };
    if args.is_empty() {
        return "()".to_string();
    }
    let child_ind = format!("{}  ", ind);

    // Inline attempt: render each arg at the call's own indent. Suppressed when an arg is a
    // lambda whose body the author put on its own line (Rule B) — inlining would collapse it.
    let inline_args: Vec<String> = args.iter().map(|a| fmt_expr(a, false, ind)).collect();
    let any_inline_multiline = inline_args.iter().any(|s| s.contains('\n'));
    let any_author_newline_lambda = args.iter().any(has_author_newline_lambda);
    if !any_inline_multiline && !any_author_newline_lambda {
        let joined = inline_args.join(", ");
        if joined.len() + ind.len() <= 80 {
            return format!("({}{})", joined, trailing);
        }
    }

    // Decide between trailing (5a) and fully-split (5b). Render every arg at child_ind to
    // see which are multi-line in that position.
    let rendered: Vec<String> = args
        .iter()
        .map(|a| fmt_expr(a, false, &child_ind))
        .collect();
    let multiline_flags: Vec<bool> = rendered.iter().map(|s| s.contains('\n')).collect();
    let multiline_count = multiline_flags.iter().filter(|b| **b).count();
    let only_last_multiline = multiline_count >= 1
        && multiline_flags[..multiline_flags.len() - 1].iter().all(|b| !b)
        && multiline_flags[multiline_flags.len() - 1];

    if only_last_multiline {
        // TRAILING style (5a): leading args inline on the call line, last arg flows. The
        // last arg is rendered at `ind` so its body (and closing `]`/`}`) line up under the
        // call, and a trailing lambda keeps `=> [`/`=> {` on the same line.
        let n = args.len();
        let last = fmt_expr(&args[n - 1], false, ind);
        // Rule i: when the last arg is a lambda whose BODY renders multi-line, put the
        // call's closing `)` on its OWN line, dedented to the call's indentation. A
        // single-line lambda arg keeps `)` glued (handled by the inline fast-path above,
        // which never reaches here). This applies only to lambda last args — a multi-line
        // object/array last arg keeps the `)` glued to its `]`/`}` (Rule 5a).
        let last_is_multiline_lambda =
            matches!(&args[n - 1], Expr::Function { .. }) && last.contains('\n');
        // A multi-line lambda last arg puts the call's `)` on its own line at `ind` — UNLESS the
        // lambda body is a collection literal whose `]`/`}` already sits at `ind` (the `=> [`/
        // `=> {` collapse), in which case glue `)` directly so the close reads `])` / `})`.
        let last_line = last.rsplit('\n').next().unwrap_or("");
        let collapsed_close = last_line == format!("{}]", ind) || last_line == format!("{}}}", ind);
        let lead = if n == 1 { String::new() } else { format!("{}, ", inline_args[..n - 1].join(", ")) };
        if last_is_multiline_lambda && !collapsed_close {
            return format!("({}{}{}\n{})", lead, last, trailing, ind);
        }
        return format!("({}{}{})", lead, last, trailing);
    }

    // OVER-BUDGET TRAILING LAMBDA-WITH-COLLECTION-BODY: the call exceeds 80 cols but no arg is
    // multi-line on its own — `test("long name", () => [ … ])`. Fully splitting the arg list
    // would strand the leading string on its own line and lose the idiomatic `=> [`. Instead,
    // FORCE the trailing lambda's collection body multi-line (a lambda with a *block* body is
    // already multi-line and took the 5a path above), keeping the leading args + `=> [` on the
    // call line and breaking the array beneath, `)` dedented on its own line.
    let n = args.len();
    let last_is_lambda_collection = matches!(
        &args[n - 1],
        Expr::Function { body, .. } if matches!(**body, Expr::Array(..) | Expr::Object(..))
    );
    let leading_single = multiline_flags[..n - 1].iter().all(|b| !b);
    if last_is_lambda_collection && leading_single {
        FORCE_NEXT_LAMBDA_BODY_ML.with(|c| *c.borrow_mut() = true);
        let last = fmt_expr(&args[n - 1], false, ind);
        FORCE_NEXT_LAMBDA_BODY_ML.with(|c| *c.borrow_mut() = false); // clear if unused
        if last.contains('\n') {
            // Two shapes for the forced body, both acceptable:
            //  - `=> [` collapsed (Rule 5a): `last` ends with the literal's `]`/`}` at the call
            //    indent — glue `)` directly so the close reads `])` / `})` (no stray line).
            //  - `=>` then body on its own line: `last`'s `]`/`}` is at child indent — put `)`
            //    on its own line at the call indent.
            let last_line = last.rsplit('\n').next().unwrap_or("");
            let collapsed = last_line == format!("{}]", ind) || last_line == format!("{}}}", ind);
            let lead = if n == 1 { String::new() } else { format!("{}, ", inline_args[..n - 1].join(", ")) };
            if collapsed {
                return format!("({}{}{})", lead, last, trailing);
            }
            return format!("({}{}{}\n{})", lead, last, trailing, ind);
        }
    }

    // FULLY-SPLIT style (5b): one arg per line at child_ind, close paren at `ind`.
    let arg_lines: Vec<String> = rendered
        .iter()
        .map(|s| format!("{}{}", child_ind, s))
        .collect();
    format!("(\n{}{}\n{})", arg_lines.join(",\n"), trailing, ind)
}

// ── dot chain ─────────────────────────────────────────────────────────────────

type ChainLink<'a> = (&'a str, &'a Option<Vec<Expr>>, bool, u32);

fn collect_chain(expr: &Expr) -> (&Expr, Vec<ChainLink<'_>>) {
    let mut chain = Vec::new();
    let mut cur = expr;
    loop {
        if let Expr::DotCall { receiver, method, args, partial, .. } = cur {
            // The link's source position: the receiver's span end is where `.method`
            // begins, which is the offset that tells us whether the author broke the
            // chain across lines (the `.` sits just after the receiver). `partial` is the
            // trailing-comma flag (`x.f(a,)`) — must be re-emitted (see fmt_call_arglist).
            chain.push((method.as_str(), args, *partial, receiver.span().end));
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

    // Rule C: a chain with MORE than CHAIN_INLINE_MAX (2) calls is ALWAYS multiline. For a
    // chain with ≤2 calls, preserve the author's newline choice: if the author broke any
    // `.method` onto a different source line than the previous link (or the root), keep it
    // multiline; if the author wrote it inline, keep it inline (subject to fit). A single
    // `.method` (1-call chain) stays inline if it fits.
    let author_multiline = {
        let mut prev = root.span().end;
        let mut broke = false;
        for (_, _, _, link_pos) in &chain {
            if spans_on_different_source_lines(prev, *link_pos) {
                broke = true;
            }
            prev = *link_pos;
        }
        broke
    };

    // Don't take the inline path if a lambda arg has an author-newline body (Rule B): the
    // inline form would collapse it. Route through the per-link rendering instead.
    if chain.len() <= CHAIN_INLINE_MAX && !author_multiline && !has_author_newline_lambda(expr) {
        let inline = fmt_inline(expr);
        // Only use inline if it truly fits on one line (no newlines and fits in budget).
        if !inline.contains('\n') && inline.len() + ind.len() <= 120 {
            return inline;
        }
    }

    // Single-call chain that the author did NOT break across lines (e.g.
    // `nodes.for(_ => …multiline lambda…)`): the chain itself stays on one line —
    // `receiver.method(<arglist>)` — and the multi-line argument flows via `fmt_call_arglist`,
    // rather than splitting the receiver onto its own `.method` line (which would be wrong for
    // a 1-call chain whose only multi-line-ness comes from its argument).
    if chain.len() == 1 && !author_multiline {
        let (method, args, partial, _) = &chain[0];
        let r = fmt_postfix_base(root, |e| fmt_expr(e, false, ind));
        if let Some(a) = args {
            return format!("{}.{}{}", r, method, fmt_call_arglist(a, *partial, ind));
        }
        return format!("{}.{}", r, method);
    }

    // Multi-line.
    let root_str = fmt_postfix_base(root, |e| fmt_expr(e, false, ind));
    let call_strs: Vec<String> = chain
        .iter()
        .map(|(method, args, partial, _)| match args {
            None => format!("{}.{}", child_ind, method),
            Some(a) => {
                format!("{}.{}{}", child_ind, method, fmt_call_arglist(a, *partial, &child_ind))
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
    /// (`x => x`) is only legal in argument position (ADR-006), so the formatter must keep the
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

    /// A union/intersection/function inside an array type MUST keep its parentheses, or the
    /// postfix `[]` (which binds tighter) would re-parse to a different type. `(String | Null)[]`
    /// must NOT be rewritten to the meaning-changing `String | Null[]`.
    #[test]
    fn grouped_type_array_keeps_parens() {
        let cases = [
            "type G = (String | Null)[]\n",
            "type R = { \"groups\": (String | Null)[] }\n",
            "val f = (xs: (Int32 | String)[]): Int32 => 0\n",
        ];
        for src in cases {
            let out = format(src);
            assert!(out.contains(")[]"), "expected parenthesised array element, got {out:?}");
            assert!(parses_clean(&out), "formatter output did not re-parse: {out:?}");
            let out2 = format(&out);
            assert_eq!(out, out2, "formatter not idempotent for {src:?}");
        }
    }
}
