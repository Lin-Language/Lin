//! LLVM code generation for Lin.
//! Compiles TypedIR from lin-check into LLVM IR using inkwell.

pub mod codegen;

pub use codegen::Codegen;
