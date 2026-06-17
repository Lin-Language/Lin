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
    ToJson,
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
    /// HTTP server (`serve`, spec §25.5). `serve(handler, port)` → `lin_serve(h_fn, h_env,
    /// h_has, port)`. Blocks forever; the handler is invoked once per request.
    Serve,
    // Shared<T> — opt-in shared mutable state (ADR-028 §2.3.1). shared(v) boxes a private copy;
    // get/set/withLock are the only accessors (copy out / copy in / locked in-place mutate).
    SharedNew,
    SharedGet,
    SharedSet,
    SharedWithLock,
    // Frozen<T> — opt-in shared read-only state (ADR-028 §2.3.2): deep immortal seal of a graph.
    Freeze,
    Request,
    Message,
    Close,
    // Stream<T> — opaque, effectful, fallible pull-source owning an OS resource (streams brief,
    // ADR-047). `StreamOpen` opens a file source → `Stream<UInt8[]> | Error`; `StreamRead` pulls
    // the next chunk → `UInt8[] | Null | Error` (Null = EOF); `StreamClose` closes the resource
    // (idempotent). Dispatch is modelled on the `Shared*` family.
    StreamOpen,
    StreamRead,
    StreamClose,
    // Lazy adapters (Stage 4): each builds a new Stream node over an upstream Stream. map/filter
    // carry a transform closure (called boxed-in/boxed-out); take/chunks carry an Int count.
    StreamMap,
    StreamFilter,
    StreamTake,
    StreamLines,
    StreamChunks,
    // Net-new lazy adapters (std/iter unification Stage 3): drop/takeWhile/dropWhile/flatMap/
    // flatten/concat. drop carries an Int count; takeWhile/dropWhile/flatMap carry a closure;
    // flatten takes only the stream; concat takes TWO streams (both retained, both closed).
    StreamDrop,
    StreamTakeWhile,
    StreamDropWhile,
    StreamFlatMap,
    StreamFlatten,
    StreamConcat,
    // Enrichment lazy adapters + infinite sources (iter-combinators proposal). sliding carries an
    // Int width; pairwise/dedup take only the stream; intersperse carries a boxed separator value;
    // zipWith carries a boxed `b` array + a closure. count/repeat/cycle are SOURCES (no upstream):
    // count takes two i64s, repeat a boxed value + i64 count, cycle a boxed array.
    StreamSliding,
    StreamPairwise,
    StreamIntersperse,
    StreamDedup,
    StreamZipWith,
    StreamCount,
    StreamRepeat,
    StreamCycle,
    // Streaming compression byte-adapters (std/compress): each takes ONE Stream<UInt8[]> and
    // returns a new Stream<UInt8[]> that (de)compresses bytes incrementally. gunzip/gzip use the
    // gzip container; inflate/deflate use raw DEFLATE.
    StreamGunzip,
    StreamGzip,
    StreamInflate,
    StreamDeflate,
    // tar splitting (std/archive). `untar(s, body)` is a TERMINAL driver (stream + 2-arg closure →
    // Null | Error), modelled on StreamFor's dispatch. `manifest(s)`/`files(s)` are single-stream-arg
    // ADAPTERS returning a Stream<Object>, modelled on StreamFlatten. All three CONSUME the parent.
    StreamUntar,
    StreamManifest,
    StreamFiles,
    // Composable tar entries adapter (std/archive): `entries(s)` splits a byte stream into a
    // Stream<TarEntry>. `header(e)` and `body(e)` extract metadata/body from a TarEntry handle.
    StreamTarEntries,
    TarHeader,
    TarBody,
    // Sink + terminal drivers (Stage 4). writeStream builds a RAW sink (item bytes verbatim, no
    // separator); writeLines builds a line-oriented sink (each item + '\n'); drain drives on the
    // calling thread; collect/readText pull-all-into-one-value. All terminals close the stream.
    StreamWrite,
    StreamWriteLines,
    StreamDrain,
    StreamCollect,
    StreamReadText,
    // Unified OS sources (Stage 5): TCP socket / process stdout / stdin → Stream<UInt8[]>.
    StreamTcp,
    StreamStdout,
    StreamStdin,
    // `.for(fn)` over a Stream (Stage 5): drive each item through `fn` on the calling thread →
    // Null | Error (EOF → Null; a read error → the Error value). Closes the stream at the end.
    StreamFor,
    // Net-new stream terminals (std/iter unification Stage 4). Each drives the stream on the
    // calling thread, returns a boxed `X | Error`, and closes the stream:
    //   StreamReduce  → U | Error        (fold with init + (acc,item)=>acc)
    //   StreamFind    → T | Null | Error (first truthy predicate match; Null if none)
    //   StreamSome    → Boolean | Error  (true on first truthy, short-circuit)
    //   StreamEvery   → Boolean | Error  (false on first falsy, short-circuit)
    //   StreamWhile   → Null | Error     (drive until predicate false or EOF)
    StreamReduce,
    StreamFind,
    StreamSome,
    StreamEvery,
    StreamWhile,
    // `.promise()` (Stage 8): MOVE the pipeline onto a worker thread that drives it to EOF →
    // Promise<Null | Error>. The stream arg is moved (caller release suppressed).
    StreamPromise,
    /// `fromJson` type-directed decode (ADR-031). Carries the resolved target `Type` T and the
    /// resolved bodies of every reachable `Named` type (so codegen can build a recursive schema
    /// descriptor with no type environment). Runtime: `lin_from_json(value, descriptor) -> ptr`
    /// returns the input value retained (+1) on success, or a fresh `Error` object on the first
    /// structural mismatch.
    FromJson {
        target: Box<Type>,
        named_defs: Vec<(String, Type)>,
    },
}

/// How a closure env releases one captured slot when the closure is freed (ADR-041: owning
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
    /// Captured SEALED scalar record (sealed-records Stage 1): a packed `[u32 rc | u32 size |
    /// scalars]` struct, NOT a `LinObject`. Retained on capture via `lin_rc_retain` (offset-0 rc),
    /// released by `lin_closure_release` via `lin_sealed_release_self` (which reads the byte size
    /// from the struct's offset-4 header) — NOT `lin_object_release`, which would mis-walk the
    /// struct as object entries (a heap-buffer-overflow). Deep-copied across threads by a flat
    /// byte copy (`transfer::CAP_SEALED`).
    Sealed,
    /// MOVED resource capture (streams brief §9, ADR-047): a `Stream` (or `Stream | Error`) crosses
    /// the thread boundary by MOVE, not copy. The pointer is handed off verbatim — NO clone on
    /// capture, NO retain — and the SOURCE must not release it (the affine check guarantees it is
    /// never touched again). The WORKER owns it and releases it (`lin_tagged_release`, whose
    /// TAG_STREAM arm runs the auto-close finalizer) when the closure env is torn down. This yields
    /// a disjoint object graph on the worker, so the non-atomic RC of the rest of the graph stays
    /// sound. The release action is the SAME as `Tagged` (`lin_tagged_release`); `Move` differs
    /// only in the CAPTURE side (no clone/retain) and in suppressing the source's scope release.
    Move,
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
            CaptureRelease::Sealed => 7,
            // CAP_MOVE: the worker releases a moved resource the same way it releases a Tagged
            // capture (`lin_tagged_release` → TAG_STREAM finalizer). The distinction is on the
            // capture/source side, not the release side. Mirrors `transfer::CAP_MOVE`.
            CaptureRelease::Move => 6,
        }
    }
}

/// Ownership convention of a function parameter or return value (Path-10/11 Leg 1).
///
/// This makes ownership a *verified IR fact* rather than an emergent property of scattered
/// retain/release emission. It is declared on every `LinFunction` signature and on every
/// runtime intrinsic (the hand-audited table in `RuntimeFns`). In SHADOW MODE (this round) the
/// convention is *inferred and verified* but NEVER consumed by codegen — every signature defaults
/// to `Own`, which is exactly today's behaviour, so emitting it changes no output. A later wave
/// can consume the conventions to delete the per-site RC heuristics.
///
/// Semantics (Swift SIL / Lean "Counting Immutable Beans" model):
/// - `Own`    — the value is TRANSFERRED to the callee/return. The caller hands over one owned
///   reference (+1) and must not release it afterward; the callee (or the returned-to context)
///   is responsible for releasing it. This is the conservative default — today's behaviour, where
///   every boundary materializes/clones defensively.
/// - `Borrow` — the callee only READS the value and does not extend its lifetime: it does not
///   store it in a container, capture it in a closure, return it, or otherwise let it (or an alias)
///   outlive the call. Ownership stays with the caller, who must keep the value alive across the
///   call and release it afterward. No +1 is transferred.
/// - `Inout`  — the value is passed by mutable reference (a `var`-slot/cell or an in-place
///   container mutation): the callee may mutate it in place, ownership stays with the caller.
///   Used conservatively — only when clearly an in-place mutation, else `Own`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Convention {
    Borrow,
    Own,
    Inout,
}

impl Convention {
    /// The single-letter mnemonic used in IR dumps / shadow reports.
    pub fn mnemonic(self) -> &'static str {
        match self {
            Convention::Borrow => "borrow",
            Convention::Own => "own",
            Convention::Inout => "inout",
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
    /// result = closure(func_id, env_temps[...]) — allocates the closure struct + env.
    /// `capture_kinds[i]` is the release kind of `captures[i]` (see `CaptureRelease`): the env
    /// OWNS one reference per owning capture, so `lin_closure_release` drops it on free. The
    /// lowerer fills these (it knows `is_mutable` + the capture type); cell-pointer captures are
    /// `CaptureRelease::None` (borrow-only — the cell has its own lifecycle).
    MakeClosure { dst: Temp, func: FuncId, captures: Vec<Temp>, capture_kinds: Vec<CaptureRelease>, ret_ty: Type },
    /// result = closure value wrapping an EXTERNAL named function symbol (an imported
    /// top-level function or FFI symbol) referenced as a value rather than called. `sym`
    /// is the mangled/foreign symbol; `ty` is the function's full Lin type (params + ret)
    /// so codegen can build the capture-less boxed-ABI wrapper. Mirrors `MakeClosure` for a
    /// local function, but the callee is resolved by symbol name, not FuncId.
    MakeNamedClosure { dst: Temp, sym: String, ty: Type },
    /// result = { fields... }  — allocates object on heap.
    ///
    /// `stack` (sealed-records Stage 4): when `true`, this constructs an all-scalar SEALED record
    /// that the escape analysis (`escape.rs`) PROVED non-escaping, so codegen allocates it in a
    /// REUSED function-entry-block `alloca` (no `lin_sealed_alloc`, no heap, no per-iteration stack
    /// growth). With RC-emission suppression (this milestone) the lowerer ALSO omits the
    /// Retain/Release instructions on the value entirely (so the alloca SROA-promotes to registers);
    /// the immortal-sentinel header refcount remains as defense-in-depth for any RC that slips
    /// through. ALWAYS `false` for any heap-field record, any non-sealed/anonymous object, or any
    /// construction whose value can reach a Return / container store / closure capture / async
    /// boundary / unknown-retaining call — those stay heap. A wrong `true` is a use-after-return;
    /// the analysis fails safe to `false` (heap) on any doubt.
    MakeObject { dst: Temp, fields: Vec<(String, Temp)>, spreads: Vec<Temp>, ty: Type, stack: bool },
    /// result = [ elements... ]  — allocates array on heap.
    ///
    /// `inline` (0xFE Phase 1): when `true`, the escape analysis (`escape.rs`) proved that NONE of
    /// the element temps escape (no aliasing after the copy), so codegen uses `lin_sealed_array_alloc`
    /// + `lin_sealed_array_push_struct_retaining` (contiguous inline 0xFE buffer) instead of the
    /// pointer-backed 0xFD path. Only set on sealed-record-array literals whose elements are all
    /// proven non-escaping. `false` → 0xFD pointer-backed (current default; safe, supports aliasing).
    ///
    /// `columnar` (Phase 1 columnar POC): when `true` AND `inline == true`, the escape analysis
    /// additionally proved ALL element fields are flat scalars (no String/Array/nested-record fields),
    /// so codegen uses `lin_columnar_array_alloc` (tag `0xFC`) — one contiguous column buffer per
    /// field. Field reads (`SealedArrayFieldGet`) are two pointer loads + GEP + load (same depth as
    /// 0xFE, but cache-line-efficient for single-field sequential scans). Always `false` unless
    /// `inline == true` AND `all_scalar_fields`. `false` → fall back to 0xFE/0xFD (safe default).
    MakeArray { dst: Temp, elements: Vec<Temp>, elem_ty: Type, inline: bool, columnar: bool },
    /// result = object[key]  — safe field access (missing key → null temp)
    Index { dst: Temp, object: Temp, key: Temp, obj_ty: Type, key_ty: Type, result_ty: Type },
    /// object[key] = value  — in-place array/object element assignment (no result).
    IndexSet { object: Temp, key: Temp, value: Temp, obj_ty: Type, key_ty: Type, val_ty: Type },
    /// result = object.field  — known-shape field access
    FieldGet { dst: Temp, object: Temp, field: String, obj_ty: Type, result_ty: Type },
    /// object.field = value  — known-shape (literal-key) field WRITE into a PACKED SEALED RECORD.
    /// The write counterpart of `FieldGet`: codegen stores `value` at the field's constant struct
    /// offset (a scalar field is a direct store; a heap field releases the old pointer and retains
    /// the new). Only emitted when `object` is a sealed-scalar-record and `field` is a statically
    /// present field; a runtime key, an absent field, or a boxed object stay as `IndexSet`.
    FieldSet { object: Temp, field: String, value: Temp, obj_ty: Type, val_ty: Type },
    /// result = array[index].field — FUSED constant-offset read of a SCALAR field of element
    /// `index` of a sealed-record array (sealed-records Stage 3). Avoids materializing a standalone
    /// sealed struct for the element: codegen GEPs `data + index*stride + (field_off - HEADER)` and
    /// loads the scalar directly. `arr_ty` is the `Array(elem)` type (so codegen recovers the
    /// element fields/stride); `result_ty` is the field's type. Sound only for SCALAR fields (no RC).
    SealedArrayFieldGet { dst: Temp, array: Temp, index: Temp, field: String, arr_ty: Type, result_ty: Type },
    /// result = (BOXED `Object[]` array)[index][field] — a single field read of one element of a
    /// BOXED array whose element is a sealed/typed record stored as a heap `LinObject` (the boxed
    /// `Token[]` representation: a record with heap fields, NOT a packed sealed-scalar array).
    /// Codegen reads the BORROWED element box via `lin_array_get` (no fresh box, no per-element
    /// sealed materialization), unboxes to the raw `LinObject`, does the single `lin_object_get` for
    /// `field`, then unboxes/coerces to `result_ty`. The lowerer registers `dst` owned (a `Retain`
    /// follows for an RC `result_ty`), matching the materialize-then-read path it replaces. Avoids
    /// the alloc + 2-field read + 2 retains + reload + release the generic `arr[i]` sealed projection
    /// pays per access in a hot parser loop. `arr_ty` is the `Array(elem)` type; `result_ty` is the
    /// field's type.
    BoxedArrayFieldGet { dst: Temp, array: Temp, index: Temp, field: String, arr_ty: Type, result_ty: Type },
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
    ///
    /// `immutable` is true for a top-level `val` (single static store) and false for a
    /// top-level `var` (mutable, multiple stores). Codegen uses it to give an immutable
    /// global's backing `_ir_gv_{slot}` LLVM `Internal` linkage, which lets LLVM GlobalOpt
    /// prove a single-store-of-a-constant global is itself constant and fold reads of it
    /// (e.g. a literal divisor `val MOD = …` becomes a magic multiply-shift instead of a
    /// per-iteration `idiv`). See codegen `GlobalValSet`/`GlobalValGet`.
    GlobalValSet { slot: usize, value: Temp, ty: Type, immutable: bool },
    /// dst = the module-global val for `slot` (load from its LLVM global). Used when a
    /// closure references a top-level val that is neither a parameter nor a capture.
    /// `immutable`: see `GlobalValSet`.
    GlobalValGet { dst: Temp, slot: usize, ty: Type, immutable: bool },
    /// dst = heap cell holding `init` (a `var` mutably captured by a closure). The cell
    /// pointer is shared by reference: closures capture it and read/write the live value
    /// through CellGet/CellSet (ADR-012). `ty` is the stored value's type.
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
    /// FULLY release a `TaggedVal*` element box (inner heap payload + shell), but ONLY when `val` is
    /// a distinct pointer from `other`. The full-release counterpart of `FreeBoxShellIfDistinct`,
    /// used by `for`/`while` to reclaim the per-iteration element box that `lin_array_get_tagged`
    /// returned as a fresh +1 WITH the inner heap payload RETAINED. A side-effecting `for`/`while`
    /// body never MOVES that inner anywhere, so it must be fully reclaimed — freeing only the shell
    /// (the old `FreeBoxShellIfDistinct`) leaked the retained inner of every heap-bearing element.
    /// The `if distinct` guard avoids a double-free when the callback returned (an alias of) the box,
    /// whose separate full release already reclaimed it. Maps to `lin_tagged_release_if_distinct`.
    /// Null/cached-box safe. A flat-scalar element box has no inner, so this degrades to a shell free.
    ReleaseIfDistinct { val: Temp, other: Temp },
    /// result = val is type_tag? (returns bool)
    IsType { dst: Temp, val: Temp, ty: Type },
    /// UNBOXED SUM TYPE (unboxed-sumtype Stage 1): `result = (val's inline tag == the tag of the
    /// variant whose discriminant value is `disc_value`)` — the O(1) match/`is` dispatch over a
    /// packed `SumNode` scrutinee. Emitted by `emit_discriminator` when the scrutinee's repr is
    /// `Packed(SumNode)`, REPLACING the boxed `Index(scrut, disc) == StrLit` (materialize + object_get
    /// + string-eq) with a single tag load + integer compare. `val` MUST be a SumNode (the lowerer
    /// only emits this when the discriminator's scrutinee is sum-eligible; codegen reads `func.repr`
    /// to confirm and falls back to the boxed compare otherwise — soundness).
    SumTagEq { dst: Temp, val: Temp, sum_ty: Type, disc_value: String },
    /// result = val has pattern? (returns bool)
    HasPattern { dst: Temp, val: Temp, pattern: HasDesc },
    /// result = `val` deeply conforms to `target`? (returns bool) — `is <ObjectType>` deep
    /// type validation (ADR-036). Reuses the `fromJson` structural walker via
    /// `lin_matches_schema(value, descriptor)`: codegen emits the same schema descriptor it
    /// builds for `Intrinsic::FromJson` (from `target` + the resolved `named_defs` bodies of
    /// reachable Named types) and calls the runtime to recursively validate field TYPES, not
    /// just presence. `val` is a boxed `TaggedVal*` (borrowed, no ownership change).
    MatchesSchema {
        dst: Temp,
        val: Temp,
        target: Type,
        named_defs: Vec<(String, Type)>,
    },
    /// result = box(val, ty) — wrap a scalar as a tagged union value
    Box { dst: Temp, val: Temp, ty: Type },
    /// result = unbox(val, ty) — extract scalar from tagged union
    Unbox { dst: Temp, val: Temp, result_ty: Type },
    /// Bind a pattern variable: dst = source val.
    Bind { dst: Temp, src: Temp, ty: Type },
    /// Panic with a message string.
    Panic { msg: Temp },
    /// DEBUG-ONLY metadata (Phase 3 of the Lin debugger): associate the SSA temp holding a Lin
    /// `val`/`var`/parameter binding with its SOURCE name and type, so the codegen DWARF pass can
    /// emit a `DILocalVariable` + `DIType` for it (and `llvm.dbg.declare` over a stack home) under
    /// `--debug`. `param_no` is `Some(n)` (1-based) for a function parameter — emitted as a
    /// `DW_TAG_formal_parameter` with that argument ordinal (each parameter MUST have a distinct
    /// index or LLVM rejects the debug info) — and `None` for a `val`/`var` local (a
    /// `DW_TAG_variable`). `span` is the binding-site source span (for the declared line/col).
    /// Purely additive: it produces NO machine instructions and is IGNORED by non-debug codegen (the
    /// debug_info state is `None`), so it never changes program semantics or non-debug output. The
    /// lowerer emits it at binding sites; the liveness/RC passes treat it as a pure metadata marker
    /// (it neither defines nor uses `temp` for ownership purposes — `temp` is already defined by the
    /// preceding binding instruction). See `crates/lin-codegen/src/codegen/debug_info.rs`.
    DebugDeclare { temp: Temp, name: String, ty: Type, param_no: Option<u32>, span: lin_common::Span },
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
    /// Per-instruction source spans, PARALLEL to `instructions` (`instr_spans[i]` is the source
    /// span of `instructions[i]`, or `None` for synthetic instructions with no source location).
    /// Populated by the lowerer (`emit`) so the codegen DWARF pass can attach statement-granularity
    /// `DILocation`s in `--debug` builds. Purely additive debug metadata: it MUST be kept in lockstep
    /// with `instructions` whenever an instruction is inserted/removed (the lowerer's `emit` plus the
    /// RC-elision `remove` and escape `retain` sites do this). It never affects IR semantics or
    /// non-debug codegen; if it is out of sync or empty the DWARF pass simply emits fewer locations.
    pub instr_spans: Vec<Option<lin_common::Span>>,
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
    /// Ownership convention of each parameter, PARALLEL to `params` (Path-10/11 Leg 1).
    /// Inferred at lowering (`infer_conventions`): `Borrow` for a read-only-never-escaping
    /// param, `Own` (today's behaviour) when it is stored/captured/returned or on any doubt,
    /// `Inout` for an in-place-mutated cell. SHADOW MODE: populated + verified, never consumed by
    /// codegen. Empty until the inference pass runs (a function with no params has an empty vec
    /// either way); `param_convention(i)` fails safe to `Own` if absent.
    pub param_conventions: Vec<Convention>,
    /// Ownership convention of the return value (Path-10/11 Leg 1): `Own` (the caller receives a
    /// +1 it must release — today's behaviour) or `Borrow` (the function returns a value it does
    /// not own, e.g. a bare-param pass-through; the caller must not release it). `Inout` is never
    /// a return convention. SHADOW MODE: inferred + verified, never consumed.
    pub ret_convention: Convention,
    pub blocks: Vec<BasicBlock>,
    /// Type of every temp in this function.
    pub temp_types: HashMap<Temp, Type>,
    /// Total number of temps allocated (0..temp_count-1 are valid).
    pub temp_count: u32,
    /// Intrinsic slot index → intrinsic name (inherited from TypedModule).
    pub intrinsic_slots: HashMap<usize, String>,
    /// Per-temp physical representation table, indexed by `Temp.0` (`repr[t.0]` is temp `t`'s repr).
    /// Empty until the representation-inference pass (`repr::run`) populates it; codegen reads it at
    /// every packed-vs-boxed DECIDE / ASSUME site instead of re-deriving from the static `Type`.
    /// See ADR-069 (`docs/DECISIONS.md`, which supersedes ADR-062).
    pub repr: Vec<crate::repr::Repr>,
    /// Coverage attribution origin. `Some(path)` for a CROSS-MODULE monomorphized specialization
    /// (`name$Int32`) whose body was cloned from another module's generic definition: its block
    /// spans index into THAT module's source, not the importer being compiled here. Codegen uses
    /// this to attribute the specialization's coverage regions to the generic definition's file
    /// (so an imported generic exercised by tests reports real coverage instead of 0%). `None` for
    /// ordinary functions, whose spans belong to the module currently being compiled.
    pub coverage_origin: Option<String>,
}

impl LinFunction {
    pub fn entry_block(&self) -> &BasicBlock {
        &self.blocks[0]
    }

    pub fn block(&self, id: BlockId) -> Option<&BasicBlock> {
        self.blocks.iter().find(|b| b.id == id)
    }

    /// The ownership convention of parameter `i` (Path-10/11 Leg 1). Fails safe to `Own` (today's
    /// behaviour) when the inference pass has not populated the table or `i` is out of range —
    /// exactly mirroring the conservative default so an un-inferred param is never treated as a
    /// borrow (which would be unsound for a consumer).
    pub fn param_convention(&self, i: usize) -> Convention {
        self.param_conventions.get(i).copied().unwrap_or(Convention::Own)
    }

    /// The physical representation of temp `t` (Stage 3: codegen's single source of truth at every
    /// packed-vs-boxed DECIDE / ASSUME site). Fails safe to `Boxed(Opaque)` if the table is empty
    /// (pass not run / synthetic function) or `t` is out of range, exactly mirroring the analysis's
    /// own fail-safe so an un-analyzed temp is never mistakenly treated as packed.
    pub fn repr_of(&self, t: Temp) -> crate::repr::Repr {
        self.repr
            .get(t.0 as usize)
            .cloned()
            .unwrap_or_else(crate::repr::Repr::boxed_opaque)
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

/// Concrete refcounted heap value types: the set for which `lin_rc_retain`/typed-release
/// manages ownership. Includes Stage-3 NullableRecord (`T|Null` where T is a sealed record) —
/// a raw nullable `*sealed_T` pointer whose RC lives at offset 0, managed via
/// `lin_rc_retain`/`lin_sealed_release` exactly like a sealed struct.
///
/// This is the single authority shared by `lower`, `rc_elide`, and `ownership_verify`.
/// Sum types (`sum_type_eligible`) are intentionally excluded: a SumNode's ownership is
/// tracked via construction-site `register_owned` + runtime KIND_SUMNODE drop walk, not
/// via this predicate (adding them would double-retain match scrutinees and leak the tree).
pub fn is_concrete_rc_ty(ty: &Type) -> bool {
    if crate::repr::nullable_sealed_record(ty).is_some() {
        return true;
    }
    matches!(
        ty,
        Type::Str
            | Type::StrLit(_)
            | Type::Array(_)
            | Type::FixedArray(_)
            | Type::Object { .. }
            | Type::Map { .. }
            | Type::Iterator(_)
            | Type::Function { .. }
    )
}
