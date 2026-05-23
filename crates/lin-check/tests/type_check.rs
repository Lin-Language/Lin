use lin_check::Checker;
use lin_lex::Lexer;
use lin_parse::Parser;

fn parse_and_check(source: &str) -> Result<lin_check::TypedModule, Vec<lin_common::Diagnostic>> {
    let mut lexer = Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens);
    let module = parser.parse_module();
    let mut checker = Checker::new();
    checker.check_module(&module)
}

#[test]
fn test_integer_literal() {
    let result = parse_and_check("val x: Int32 = 42");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

#[test]
fn test_float_literal() {
    let result = parse_and_check("val x: Float64 = 3.14");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

#[test]
fn test_string_literal() {
    let result = parse_and_check("val x: String = \"hello\"");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

#[test]
fn test_bool_literal() {
    let result = parse_and_check("val x: Boolean = true");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

#[test]
fn test_type_mismatch() {
    let result = parse_and_check("val x: Int32 = \"hello\"");
    assert!(result.is_err());
}

#[test]
fn test_arithmetic_widening() {
    let result = parse_and_check("val x: Int32 = 1 + 2");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

#[test]
fn test_function_type() {
    let result = parse_and_check("val add = (a: Int32, b: Int32): Int32 => a + b");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}

#[test]
fn test_undefined_variable() {
    let result = parse_and_check("val x = y");
    assert!(result.is_err());
}

#[test]
fn test_immutable_assignment() {
    let result = parse_and_check("val x = 1\nx = 2");
    assert!(result.is_err());
}

#[test]
fn test_mutable_assignment() {
    let result = parse_and_check("var x = 1\nx = 2");
    assert!(result.is_ok(), "Expected Ok, got: {:?}", result.err());
}
