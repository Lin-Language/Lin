use lin_check::Checker;
use lin_lex::Lexer;
use lin_parse::Parser;

fn check_errors(source: &str) -> Vec<String> {
    let mut lexer = Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens);
    let module = parser.parse_module();
    let mut checker = Checker::new();
    match checker.check_module(&module) {
        Ok(_) => vec![],
        Err(diags) => diags.iter().map(|d| d.message.clone()).collect(),
    }
}

#[test]
fn snap_type_mismatch() {
    let errors = check_errors("val x: Int32 = \"hello\"");
    insta::assert_debug_snapshot!(errors);
}

#[test]
fn snap_undefined_variable() {
    let errors = check_errors("val x = undefined_var");
    insta::assert_debug_snapshot!(errors);
}

#[test]
fn snap_function_return_type_mismatch() {
    let errors = check_errors("val f = (n: Int32): String => n");
    insta::assert_debug_snapshot!(errors);
}

#[test]
fn snap_missing_object_field() {
    let errors = check_errors(r#"
type Point = { "x": Int32, "y": Int32 }
val p: Point = { "x": 1 }
"#);
    insta::assert_debug_snapshot!(errors);
}

#[test]
fn snap_narrowing_disallowed() {
    let errors = check_errors("val x: Int32 = 3.14");
    insta::assert_debug_snapshot!(errors);
}

#[test]
fn snap_unknown_type() {
    let errors = check_errors("val x: NonExistentType = 1");
    insta::assert_debug_snapshot!(errors);
}

/// Like `check_errors` but captures the help text too, so the snapshot records the
/// scalar-annotation-on-array-literal hint as well as the primary message.
fn check_errors_with_help(source: &str) -> Vec<String> {
    let mut lexer = Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens);
    let module = parser.parse_module();
    let mut checker = Checker::new();
    match checker.check_module(&module) {
        Ok(_) => vec![],
        Err(diags) => diags
            .iter()
            .map(|d| match &d.help {
                Some(h) => format!("{}\n  help: {}", d.message, h),
                None => d.message.clone(),
            })
            .collect(),
    }
}

#[test]
fn snap_scalar_annotation_on_array_literal() {
    let errors = check_errors_with_help("val x: UInt8 = [1, 2, 3]");
    insta::assert_debug_snapshot!(errors);
}
