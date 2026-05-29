pub mod ir;
pub mod lower;
pub mod liveness;
pub mod rc_elide;

pub use ir::*;
pub use lower::{lower_import_module, lower_module, mangle_module_key};
