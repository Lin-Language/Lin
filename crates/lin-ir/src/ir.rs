//! Flat 3-address IR for Lin, between TypedExpr and LLVM codegen.
//!
//! Design principles:
//! - No nested expressions: every sub-expression result is named as a Temp.
//! - No phi nodes: merge-points use explicit Copy instructions to pre-allocated temps.
//! - RC operations are explicit: Retain/Release instructions for strings, arrays, objects.
//! - Liveness analysis and RC elision operate on this representation before LLVM codegen.

use std::collections::HashMap;
use lin_check::types::Type;
use lin_parse::ast::BinOp;

/// Identity for temporaries (SSA values within a function).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Temp(pub u32);

/// Identity for basic blocks within a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

/// Identity for functions within a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);

/// Compile-time constant values.
#[derive(Debug, Clone)]
pub enum Const {
    Int(i64, Type),
    Float(f64, Type),
    Bool(bool),
    Null,
    /// String literal: pointer to a heap-allocated LinString.
    Str(String),
}

/// Known runtime operations that map 1:1 to lin-runtime functions.
// Note: not `Eq` — `FromJson(Box<Type>)` carries a `Type`, which is only `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub enum Intrinsic {
    Print,
    ToString,
    Length,
    Push,
    Concat,
    StringConcat,
    StringLength,
    StringEq,
    StringRelease,
    ArrayAlloc,
    ArrayPush,
    ArrayGet,
    ArrayLength,
    ArrayRelease,
    FlatArrayAlloc(FlatElemKind),
    FlatArrayPush(FlatElemKind),
    FlatArrayGet(FlatElemKind),
    ObjectAlloc,
    ObjectSet,
    ObjectGet,
    ObjectHas,
    ObjectEq,
    BoxNull,
    BoxBool,
    BoxInt32,
    BoxInt64,
    BoxFloat64,
    BoxStr,
    BoxObject,
    BoxArray,
    BoxFunction,
    GetTag,
    UnboxInt32,
    UnboxInt64,
    UnboxFloat64,
    UnboxBool,
    UnboxPtr,
    TaggedToString,
    IntToString,
    FloatToString,
    BoolToString,
    NullToString,
    Alloc,
    Panic,
    // Object/array mutation + dynamic helpers exposed to stdlib as `lin_*` builtins.
    // These dispatch on argument runtime types (flat/tagged, boxed/concrete) and box
    // value arguments to TaggedVal* where the runtime expects Json, mirroring the AST
    // path's special-case handlers. Used by std/array, std/object, std/hash.
    ObjectSetDyn,
    ArraySetDyn,
    Keys,
    ValueKey,
    ArrayAllocate,
    ArrayAllocateFilled,
    // Concurrency / process intrinsics (see std/async). In this runtime async is
    // effectively synchronous: a thunk runs immediately and its result is wrapped in a
    // promise; await unwraps it.
    Async,
    Await,
    Exit,
    // Remaining async/worker family (value-input ports of compile_async_intrinsic). Used by
    // std/async. In this synchronous runtime: parallel runs each thunk and collects results;
    // race/timeout/retry are simplified (return/await the given promise); the worker family
    // maps to lin_worker_* runtime calls.
    Parallel,
    Race,
    Timeout,
    Retry,
    ThreadPool,
    Worker,
    // Shared<T> — opt-in shared mutable state (ADR-043 §2.3.1). shared(v) boxes a private copy;
    // get/set/withLock are the only accessors (copy out / copy in / locked in-place mutate).
    SharedNew,
    SharedGet,
    SharedSet,
    SharedWithLock,
    // Frozen<T> — opt-in shared read-only state (ADR-043 §2.3.2): deep immortal seal of a graph.
    Freeze,
    Request,
    Message,
    Close,
    /// `fromJson` type-directed decode (ADR-047). Carries the resolved target `Type` T and the
    /// resolved bodies of every reachable `Named` type (so codegen can build a recursive schema
    /// descriptor with no type environment). Runtime: `lin_from_json(value, descriptor) -> ptr`
    /// returns the input value retained (+1) on success, or a fresh `Error` object on the first
    /// structural mismatch.
    FromJson {
        target: Box<Type>,
        named_defs: Vec<(String, Type)>,
    },
}

/// How a closure env releases one captured slot when the closure is freed (ADR-051: owning
/// captures). The env owns one reference per owning capture; `lin_closure_release` walks the
/// emitted capture descriptor and applies the matching release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureRelease {
    /// Borrow-only: scalar, or a mutably-captured `var` cell pointer (the cell has its own
    /// MakeCell/FreeCell lifecycle). Nothing to release.
    None,
    /// Concrete refcounted heap pointer: String/Array/Object → `lin_*_release`.
    Str,
    Array,
    Object,
    /// Captured closure value (Function) → `lin_closure_release`.
    Closure,
    /// Boxed `TaggedVal*` (union/Json) → `lin_tagged_release` (drops inner payload + frees box).
    Tagged,
}

impl CaptureRelease {
    /// The on-disk byte code stored in the capture descriptor for `lin_closure_release`.
    pub fn code(self) -> u8 {
        match self {
            CaptureRelease::None => 0,
            CaptureRelease::Str => 1,
            CaptureRelease::Array => 2,
            CaptureRelease::Object => 3,
            CaptureRelease::Closure => 4,
            CaptureRelease::Tagged => 5,
        }
    }
}

/// Element kinds for unboxed (flat) scalar arrays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlatElemKind {
    U8,
    I8,
    U16,
    I16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
}

impl FlatElemKind {
    pub fn suffix(self) -> &'static str {
        match self {
            FlatElemKind::U8 => "u8",
            FlatElemKind::I8 => "i8",
            FlatElemKind::U16 => "u16",
            FlatElemKind::I16 => "i16",
            FlatElemKind::I32 => "i32",
            FlatElemKind::U32 => "u32",
            FlatElemKind::I64 => "i64",
            FlatElemKind::U64 => "u64",
            FlatElemKind::F32 => "f32",
            FlatElemKind::F64 => "f64",
        }
    }

    pub fn from_type(ty: &Type) -> Option<Self> {
        match ty {
            Type::UInt8 => Some(FlatElemKind::U8),
            Type::Int8 => Some(FlatElemKind::I8),
            Type::UInt16 => Some(FlatElemKind::U16),
            Type::Int16 => Some(FlatElemKind::I16),
            Type::Int32 => Some(FlatElemKind::I32),
            Type::UInt32 => Some(FlatElemKind::U32),
            Type::Int64 => Some(FlatElemKind::I64),
            Type::UInt64 => Some(FlatElemKind::U64),
            Type::Float32 => Some(FlatElemKind::F32),
            Type::Float64 => Some(FlatElemKind::F64),
            _ => None,
        }
    }

    /// The Lin element type this flat kind corresponds to (for unboxing pushed values).
    pub fn elem_type(self) -> Type {
        match self {
            FlatElemKind::U8 => Type::UInt8,
            FlatElemKind::I8 => Type::Int8,
            FlatElemKind::U16 => Type::UInt16,
            FlatElemKind::I16 => Type::Int16,
            FlatElemKind::I32 => Type::Int32,
            FlatElemKind::U32 => Type::UInt32,
            FlatElemKind::I64 => Type::Int64,
            FlatElemKind::U64 => Type::UInt64,
            FlatElemKind::F32 => Type::Float32,
            FlatElemKind::F64 => Type::Float64,
        }
    }
}

/// A single 3-address instruction. Each instruction produces at most one result.
#[derive(Debug, Clone)]
pub enum Instruction {
    /// result = constant
    Const { dst: Temp, val: Const },
    /// result = src (copy / rename)
    Copy { dst: Temp, src: Temp },
    /// SSA merge: result takes the value of `incomings[i].0` when control arrives from
    /// predecessor block `incomings[i].1`. Must appear at the start of a merge block.
    /// This is the only correct way to merge a value computed differently per branch in
    /// the single-pass codegen (a plain Copy into a shared temp is overwritten per block).
    Phi { dst: Temp, ty: Type, incomings: Vec<(Temp, BlockId)> },
    /// result = unary op applied to operand
    Unary { dst: Temp, op: UnaryOp, operand: Temp, ty: Type },
    /// result = lhs op rhs. `operand_ty` is the type of the operands (needed for
    /// equality/comparison dispatch, e.g. object/array deep equality); `ty` is the
    /// result type.
    Binary { dst: Temp, op: BinOp, lhs: Temp, rhs: Temp, operand_ty: Type, ty: Type },
    /// result = coerce(src, from_ty, to_ty)
    Coerce { dst: Temp, src: Temp, from_ty: Type, to_ty: Type },
    /// result = callee(args...)
    Call { dst: Temp, callee: CallTarget, args: Vec<Temp>, ret_ty: Type },
    /// result = intrinsic(args...)
    CallIntrinsic { dst: Temp, intrinsic: Intrinsic, args: Vec<Temp>, ret_ty: Type },
    /// result = closure(func_id, env_temps[...])  — allocates closure struct
    /// result = closure(func_id, env_temps[...]) — allocates the closure struct + env.
    /// `capture_kinds[i]` is the release kind of `captures[i]` (see `CaptureRelease`): the env
    /// OWNS one reference per owning capture, so `lin_closure_release` drops it on free. The
    /// lowerer fills these (it knows `is_mutable` + the capture type); cell-pointer captures are
    /// `CaptureRelease::None` (borrow-only — the cell has its own lifecycle).
    MakeClosure { dst: Temp, func: FuncId, captures: Vec<Temp>, capture_kinds: Vec<CaptureRelease>, ret_ty: Type },
    /// result = { fields... }  — allocates object on heap
    MakeObject { dst: Temp, fields: Vec<(String, Temp)>, spreads: Vec<Temp>, ty: Type },
    /// result = [ elements... ]  — allocates array on heap
    MakeArray { dst: Temp, elements: Vec<Temp>, elem_ty: Type },
    /// result = object[key]  — safe field access (missing key → null temp)
    Index { dst: Temp, object: Temp, key: Temp, obj_ty: Type, key_ty: Type, result_ty: Type },
    /// object[key] = value  — in-place array/object element assignment (no result).
    IndexSet { object: Temp, key: Temp, value: Temp, obj_ty: Type, key_ty: Type, val_ty: Type },
    /// result = object.field  — known-shape field access
    FieldGet { dst: Temp, object: Temp, field: String, obj_ty: Type, result_ty: Type },
    /// result = env[index]  — load a captured value from a closure's environment struct
    /// (raw pointer load at byte offset 8 + index*8), NOT a Lin object field access.
    EnvCapture { dst: Temp, env: Temp, index: u32, ty: Type },
    /// result = (val is an array) && (len(val) == n)  [exact], or `>= n` when `at_least`.
    /// Used to test array patterns in match (`is [a, b]`). `val` is a boxed TaggedVal*.
    ArrayLenCheck { dst: Temp, val: Temp, n: u64, at_least: bool },
    /// result = a new (boxed) object containing all of `src`'s fields except `exclude`.
    /// Used by object rest destructuring (`val { a, ...rest } = obj`).
    ObjectRest { dst: Temp, src: Temp, src_ty: Type, exclude: Vec<String> },
    /// Store a top-level (module-level) non-function `val` into a per-slot LLVM global so
    /// closures can read it (they can't see `main`'s SSA temps). Emitted in `main`.
    GlobalValSet { slot: usize, value: Temp, ty: Type },
    /// dst = the module-global val for `slot` (load from its LLVM global). Used when a
    /// closure references a top-level val that is neither a parameter nor a capture.
    GlobalValGet { dst: Temp, slot: usize, ty: Type },
    /// dst = heap cell holding `init` (a `var` mutably captured by a closure). The cell
    /// pointer is shared by reference: closures capture it and read/write the live value
    /// through CellGet/CellSet (ADR-015). `ty` is the stored value's type.
    MakeCell { dst: Temp, init: Temp, ty: Type },
    /// result = *cell  (load the current value of a captured `var` cell).
    CellGet { dst: Temp, cell: Temp, ty: Type },
    /// *cell = value  (update a captured `var` cell in place).
    CellSet { cell: Temp, value: Temp, ty: Type },
    /// Release the cell's owned VALUE (`*cell`, tag-aware/concrete per `ty`), then free the
    /// cell allocation itself (`lin_cell_free`). Emitted ONCE at the creating function's scope
    /// exit, ONLY for cells the lowerer has PROVEN non-escaping: every closure that captured
    /// the cell was lowered as a synchronous, non-retained argument to a known consuming
    /// combinator (for/while/map/filter/reduce). Reclaims the per-call cell + its current value
    /// (fixing the captured-cell leak). Never emitted for an escaping cell (would be a
    /// use-after-free when a surviving closure later reads it).
    FreeCell { cell: Temp, ty: Type },
    /// Increment refcount of a heap value (string, array, object, closure env).
    Retain { val: Temp, ty: Type },
    /// Decrement refcount; free if zero. Only emitted for owned values.
    Release { val: Temp, ty: Type },
    /// Clone a boxed Json/union value (`TaggedVal*`): allocate a fresh, independently-owned
    /// box copying the tag+payload and retaining the inner heap payload. Used by the owning
    /// model for union var-cells/globals so the cell and each reader hold their OWN box rather
    /// than an alias of a borrowed box (whose free would be a double-free). Maps to
    /// `lin_tagged_clone`. For non-union `ty` this degrades to a plain Retain of `src` into
    /// `dst` (dst == src), so the lowerer can use it uniformly.
    CloneBox { dst: Temp, src: Temp, ty: Type },
    /// Free ONLY the `TaggedVal*` box shell of `val` (not its inner heap payload). Emitted for
    /// a transient box (e.g. a freshly-boxed concrete value coerced into a union cell/global)
    /// whose inner payload's ownership is held elsewhere — typically the raw value's own
    /// scope-exit release. A full `Release` would double-free the inner; this reclaims only the
    /// 16-byte box. Maps to `lin_tagged_free_box`. Null/cached-box safe.
    FreeBoxShell { val: Temp },
    /// Free ONLY the `TaggedVal*` box shell of `val`, but ONLY when `val` is a distinct pointer
    /// from `other`. Used by `for`/`while` to reclaim the per-iteration element box shell without
    /// double-freeing when the callback returned (an alias of) that box — whose separate full
    /// release already reclaimed it. Maps to `lin_tagged_free_box_if_distinct`. Null/cached-safe.
    FreeBoxShellIfDistinct { val: Temp, other: Temp },
    /// result = val is type_tag? (returns bool)
    IsType { dst: Temp, val: Temp, ty: Type },
    /// result = val has pattern? (returns bool)
    HasPattern { dst: Temp, val: Temp, pattern: HasDesc },
    /// result = box(val, ty) — wrap a scalar as a tagged union value
    Box { dst: Temp, val: Temp, ty: Type },
    /// result = unbox(val, ty) — extract scalar from tagged union
    Unbox { dst: Temp, val: Temp, result_ty: Type },
    /// Bind a pattern variable: dst = source val.
    Bind { dst: Temp, src: Temp, ty: Type },
    /// Panic with a message string.
    Panic { msg: Temp },
}

/// Description of what a `has` pattern checks (for pattern-match compilation).
#[derive(Debug, Clone)]
pub struct HasDesc {
    pub required_fields: Vec<String>,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

/// Where to call for a `Call` instruction.
#[derive(Debug, Clone)]
pub enum CallTarget {
    /// Direct call to a known function.
    Direct(FuncId),
    /// Indirect call via a closure value in a temp.
    Indirect(Temp),
    /// Call to a globally-named (imported) function.
    Named(String),
}

/// Terminator for a basic block. Exactly one per block.
#[derive(Debug, Clone)]
pub enum Terminator {
    /// Function return: return value temp, or None for void (Null).
    Return(Option<Temp>),
    /// Unconditional branch.
    Jump(BlockId),
    /// Conditional branch: if cond is truthy, jump to then_block, else else_block.
    CondJump { cond: Temp, then_block: BlockId, else_block: BlockId },
    /// Switch on integer tag — for match on tagged unions.
    Switch { val: Temp, cases: Vec<(u8, BlockId)>, default: BlockId },
    /// Tail-call optimization: re-enter function with new args.
    TailCall { args: Vec<Temp> },
    /// Control flow never reaches here (after a Panic, etc.)
    Unreachable,
}

/// A single basic block: a list of instructions ending with a terminator.
#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: BlockId,
    /// Optional human-readable label (for debugging / IR dumps).
    pub label: Option<String>,
    pub instructions: Vec<Instruction>,
    pub terminator: Terminator,
    /// Source span this block corresponds to, used for coverage region emission.
    /// Only populated for blocks that map to a user-meaningful source region
    /// (function bodies, if/match arms, loop bodies); `None` for synthetic blocks.
    pub span: Option<lin_common::Span>,
}

/// A compiled Lin function in flat IR form.
#[derive(Debug, Clone)]
pub struct LinFunction {
    pub id: FuncId,
    pub name: Option<String>,
    /// Parameter temps (index matches Lin parameter slots).
    pub params: Vec<(Temp, Type)>,
    /// Whether this is a closure (first param is an implicit env pointer).
    pub is_closure: bool,
    pub ret_ty: Type,
    pub blocks: Vec<BasicBlock>,
    /// Type of every temp in this function.
    pub temp_types: HashMap<Temp, Type>,
    /// Total number of temps allocated (0..temp_count-1 are valid).
    pub temp_count: u32,
    /// Intrinsic slot index → intrinsic name (inherited from TypedModule).
    pub intrinsic_slots: HashMap<usize, String>,
}

impl LinFunction {
    pub fn entry_block(&self) -> &BasicBlock {
        &self.blocks[0]
    }

    pub fn block(&self, id: BlockId) -> Option<&BasicBlock> {
        self.blocks.iter().find(|b| b.id == id)
    }
}

/// Default-argument dispatch info for one function with optional parameters.
/// Used to build the runtime closure descriptor so an INDIRECT call through a
/// function value (`val g = f; g(x)`) can fill omitted trailing defaults.
#[derive(Debug, Clone)]
pub struct DefaultDescriptor {
    /// Minimum (non-partial) call arity.
    pub required: usize,
    /// Total declared parameter count.
    pub total: usize,
    /// Entry function per arity: `entries[k - required]` is the FuncId to call when
    /// `k` arguments are supplied (`required <= k <= total`). The last entry
    /// (`k == total`) is the real function; the rest are default-fill adapters.
    pub entries: Vec<FuncId>,
}

/// A full Lin module in flat IR form.
#[derive(Debug, Clone)]
pub struct LinModule {
    pub functions: Vec<LinFunction>,
    /// Maps Lin slot index → FuncId for top-level named functions.
    pub global_fn_slots: HashMap<usize, FuncId>,
    /// Maps slot index → intrinsic name for intrinsic slots.
    pub intrinsics: HashMap<usize, String>,
    /// Real FuncId → default-argument descriptor, for functions with optional params.
    /// Codegen builds a static descriptor global per entry and attaches it to closure
    /// values so indirect under-arity calls dispatch to the right default-fill adapter.
    pub default_descriptors: HashMap<FuncId, DefaultDescriptor>,
}

impl LinModule {
    pub fn function(&self, id: FuncId) -> Option<&LinFunction> {
        self.functions.iter().find(|f| f.id == id)
    }

    pub fn function_mut(&mut self, id: FuncId) -> Option<&mut LinFunction> {
        self.functions.iter_mut().find(|f| f.id == id)
    }
}
