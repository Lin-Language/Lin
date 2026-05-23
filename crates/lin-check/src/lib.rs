pub mod checker;
pub mod compat;
pub mod env;
pub mod resolve;
pub mod typed_ir;
pub mod types;
pub mod widen;

pub use checker::Checker;
pub use typed_ir::TypedModule;
pub use types::Type;
