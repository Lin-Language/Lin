//! Runtime library for compiled Lin programs.
//! Provides memory management, string operations, array operations, and I/O
//! that are linked into every compiled binary.

pub mod array;
pub mod io;
pub mod memory;
pub mod number;
pub mod object;
pub mod string;
pub mod tagged;
