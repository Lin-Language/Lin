pub mod ast;
pub mod formatter;
pub mod parser;

pub use ast::*;
pub use formatter::Formatter;
pub use parser::*;

use lin_common::Diagnostic;

/// Format Lin source into canonical form, preserving comments.
///
/// This is the single canonical lex → parse → error-check → format-with-comments
/// sequence used by every consumer (CLI `lin fmt`, the LSP, etc.). Returns the parse
/// diagnostics if the source doesn't parse (callers decide how to surface them).
pub fn format_source(source: &str) -> Result<String, Vec<Diagnostic>> {
    let mut lexer = lin_lex::Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    let comments = lexer.comments().to_vec();
    let mut parser = Parser::new(tokens);
    let module = parser.parse_module();
    if !parser.diagnostics.is_empty() {
        return Err(parser.diagnostics.clone());
    }
    Ok(Formatter::with_comments(source, comments).format_module(&module))
}

#[cfg(test)]
mod format_source_tests {
    use super::*;

    #[test]
    fn format_source_preserves_trailing_comment() {
        // Regression guard for the LSP comment-stripping bug: the shared
        // `format_source` path must keep `//` comments in its output.
        let src = "val x = 1 // keep me\n";
        let out = format_source(src).expect("should parse");
        assert!(
            out.contains("// keep me"),
            "comment was stripped by format_source; got:\n{out}"
        );
    }

    /// Re-parse `source` and find the first top-level `Call`/`DotCall`, returning its
    /// `partial` flag. Used to assert the formatter round-trips the partial-application
    /// trailing comma (`f(x,)`) — dropping it would silently change call semantics.
    fn first_call_partial(source: &str) -> bool {
        let mut lexer = lin_lex::Lexer::new(source, 0);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let module = parser.parse_module();
        assert!(parser.diagnostics.is_empty(), "round-trip output did not parse: {source:?}");
        fn find(e: &Expr) -> Option<bool> {
            match e {
                Expr::Call { partial, .. } => Some(*partial),
                Expr::DotCall { partial, .. } => Some(*partial),
                _ => None,
            }
        }
        for stmt in &module.statements {
            if let Stmt::Val { value, .. } = stmt {
                if let Some(p) = find(value) {
                    return p;
                }
            }
        }
        panic!("no Call/DotCall found in {source:?}");
    }

    #[test]
    fn format_source_preserves_partial_application_comma() {
        // BUG: the formatter never read `partial` and dropped the trailing comma, turning
        // a partial application `add(1,)` into a different-typed full call `add(1)`.
        let src = "val f = add(1,)\n";
        let out = format_source(src).expect("should parse");
        assert!(
            out.contains("add(1,)"),
            "partial-application trailing comma was dropped; got:\n{out}"
        );
        assert!(first_call_partial(&out), "re-parsed call lost partial == true");
    }

    #[test]
    fn format_source_preserves_partial_application_dotcall() {
        let src = "val f = x.add(1,)\n";
        let out = format_source(src).expect("should parse");
        assert!(
            out.contains("add(1,)"),
            "partial-application trailing comma was dropped on DotCall; got:\n{out}"
        );
        assert!(first_call_partial(&out), "re-parsed DotCall lost partial == true");
    }

    #[test]
    fn format_source_non_partial_call_has_no_trailing_comma() {
        // The inverse guard: a normal call must NOT gain a spurious trailing comma.
        let src = "val f = add(1)\n";
        let out = format_source(src).expect("should parse");
        assert!(!out.contains(",)"), "spurious trailing comma added; got:\n{out}");
        assert!(!first_call_partial(&out), "non-partial call gained partial == true");
    }

    #[test]
    fn semicolon_separator_gives_actionable_diagnostic() {
        // Lin has no semicolons (spec §1.2). A C-style `;` statement separator inside an
        // inline closure body must produce a clear, well-spanned diagnostic — NOT the old
        // misleading "Undefined variable ';'" that the lexer's Ident catch-all caused.
        let src = "fields.for(c => idx[c] = i; i = i + 1)\n";
        let mut lexer = lin_lex::Lexer::new(src, 0);
        let tokens = lexer.tokenize();
        let mut parser = Parser::new(tokens);
        let _ = parser.parse_module();
        let msgs: Vec<&str> = parser.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert!(
            msgs.iter().any(|m| m.contains("no semicolons")),
            "expected a 'no semicolons' diagnostic, got: {msgs:?}"
        );
        // The semicolon must be a distinct token, never an identifier.
        let help = parser
            .diagnostics
            .iter()
            .find(|d| d.message.contains("no semicolons"))
            .and_then(|d| d.help.clone());
        assert_eq!(
            help.as_deref(),
            Some("separate statements with newlines, not ';'"),
            "expected actionable newline help text"
        );
    }

    #[test]
    fn format_source_preserves_partial_multiline_call() {
        // A long argument list that the formatter splits across lines must still re-emit
        // the trailing comma after the final argument.
        let long = "a".repeat(90);
        let src = format!("val f = add({long}, 2,)\n");
        let out = format_source(&src).expect("should parse");
        assert!(out.contains('\n'), "expected a multi-line arglist; got:\n{out}");
        assert!(first_call_partial(&out), "multi-line partial call lost partial == true");
    }
}
