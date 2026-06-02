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
}
