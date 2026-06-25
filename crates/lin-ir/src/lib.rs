pub mod bounds_elide;
pub mod box_unbox_elide;
pub mod carry;
pub mod escape;
pub mod ir;
pub mod lower;
pub mod liveness;
pub mod monomorphize;
pub mod ownership_verify;
pub mod rc_elide;
pub mod rc_verify;
pub mod redundant_read;
pub mod repr;
pub mod sink_pure_val;
pub mod substr_map_fuse;
pub mod getset_map_fuse;

pub use ir::*;
pub use lower::{
    lower_import_module, lower_import_module_with_imports, lower_module, lower_module_with_imports,
    mangle_module_key,
};
pub use monomorphize::{
    monomorphize, monomorphize_import_with_imports, monomorphize_with_imports,
};
