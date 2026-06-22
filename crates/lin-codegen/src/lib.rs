//! LLVM code generation for Lin.
//! Compiles TypedIR from lin-check into LLVM IR using inkwell.

pub mod codegen;
pub mod coverage;

pub use codegen::Codegen;

/// PGO mode for the optimization pipeline.
#[derive(Clone, Debug, Default)]
pub enum PgoMode {
    /// No PGO: standard `default<O2>` pipeline (default).
    #[default]
    None,
    /// Instrument for profile generation. The compiled binary writes `.profraw` on run.
    /// Set `LIN_PGO_GEN=1` to activate.
    Generate,
    /// Use a merged profile to guide optimization. `path` points to a `.profdata` file
    /// produced by `llvm-profdata merge`. Set `LIN_PGO_USE=<path>` to activate.
    Use { path: String },
}
