use indexmap::IndexMap;

use super::Checker;
use crate::types::Type;

impl Checker {
    pub(crate) fn register_intrinsics(&mut self) {
        // print: (T) => Null — accepts any value, converts to string at runtime
        let print_param = self.env.fresh_type_var();
        self.define_intrinsic(
            "lin_print",
            Type::func(vec![print_param], Type::Null),
        );

        // toString: (T) => String — accepts any value
        let to_string_param = self.env.fresh_type_var();
        self.define_intrinsic(
            "lin_to_string",
            Type::func(vec![to_string_param], Type::Str),
        );

        // length: (String | Array<T> | Iterator<T> | Object) => Int32
        // Uses TypeVar(u32::MAX) as the AnyVal (dynamic top) type for the object case.
        self.define_intrinsic(
            "lin_length",
            Type::func(vec![Type::Union(vec![
                    Type::Str,
                    Type::Array(Box::new(Type::TypeVar(9000))),
                    Type::Iterator(Box::new(Type::TypeVar(9000))),
                    Type::TypeVar(u32::MAX),
                ])], Type::Int32),
        );

        // push: (T[], T) => Null
        self.define_intrinsic(
            "lin_push",
            Type::func(vec![
                    Type::Array(Box::new(Type::TypeVar(9001))),
                    Type::TypeVar(9001),
                ], Type::Null),
        );

        // set: (T[], Int32, T) => Null — in-place array element mutation
        self.define_intrinsic(
            "lin_array_set",
            Type::func(vec![
                    Type::Array(Box::new(Type::TypeVar(9200))),
                    Type::Int32,
                    Type::TypeVar(9200),
                ], Type::Null),
        );

        // keys: <K>({ K: V } | {} | AnyVal) => K[]  (ADR-086, revised).
        // The result element type is the receiver map's KEY type `K`: a `{ UInt8: V }` map yields a
        // `UInt8[]` of native integer keys (usable to re-index the map and in arithmetic), a
        // `{ String: V }` map yields `String[]` (unchanged). The param is a UNION so a record literal
        // `{}` / `AnyVal` is also accepted (their keys are strings, so `K` binds to `String` for
        // those members) while a non-object argument (`keys(5)`, `keys("s")`, `keys([…])`) still
        // rejects. The KEY TypeVar (9170) is shared with the return element so a concrete-keyed map
        // arg binds it via `collect_type_subs`'s Map arm (which recurses into the key). When the arg
        // is a record / AnyVal the key var stays unbound and `infer_call` defaults it to `String`.
        self.define_intrinsic(
            "lin_keys",
            Type::func(
                vec![Type::Union(vec![
                    Type::Map {
                        key: Box::new(Type::TypeVar(9170)),
                        value: Box::new(Type::TypeVar(9171)),
                        name: None,
                    },
                    Type::object(IndexMap::new()),
                    Type::TypeVar(u32::MAX),
                ])],
                Type::Array(Box::new(Type::TypeVar(9170))),
            ),
        );

        // lin_object_set: (Object, String, AnyVal) => Null — in-place object key mutation
        self.define_intrinsic(
            "lin_object_set",
            Type::func(vec![Type::object(IndexMap::new()), Type::Str, Type::TypeVar(u32::MAX)], Type::Null),
        );

        // for: (Iterable<T>, (T) => AnyVal) => Null  — callback return type is ignored. A `Stream<T>`
        // is ALSO accepted as the iterable (streams brief §3): a stream `for` is driven by the
        // runtime (the IR lowerer branches on the Stream type → `lin_stream_for`), ends normally
        // at EOF, and a read Error becomes the for-expr's value. The declared result stays `Null`
        // so the array/iterator `for` wrappers (`: Null`) are unchanged; std/stream's `for` wrapper
        // widens its OWN declared return to `Null | Error` to surface the stream error arm.
        self.define_intrinsic(
            "lin_for",
            Type::func(vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9010))),
                        Type::Iterator(Box::new(Type::TypeVar(9010))),
                        Type::Stream(Box::new(Type::TypeVar(9010))),
                        // A `Null` receiver is a no-op (the loop bound is tag-checked to 0), so the
                        // `for` wrapper accepts `T[] | … | Null` (a nullable map lookup `adj[k].for`).
                        Type::Null,
                    ]),
                    // The callback OPTIONALLY receives a 0-based `Int32` SOURCE index as a trailing
                    // parameter (`(item, i) => …`). A 1-arg callback stays valid via arity-width
                    // subtyping (compat.rs). The index is threaded by the IR loop (Tier A inline).
                    Type::func(vec![Type::TypeVar(9010), Type::Int32], Type::TypeVar(u32::MAX)),
                ], Type::Null),
        );

        // while: (Array<T> | Iterator<T> | Stream<T>, (T) => Boolean) => Null
        // Streamish receiver accepted (std/iter unification Stage 2): the array/iterator return
        // stays `Null`; the std/iter `while` wrapper's call-site result is widened to `Null | Error`
        // by `streamish_combinator_ret` when arg0 is a stream (the read Error becomes the result).
        self.define_intrinsic(
            "lin_while",
            Type::func(vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9011))),
                        Type::Iterator(Box::new(Type::TypeVar(9011))),
                        Type::Stream(Box::new(Type::TypeVar(9011))),
                    ]),
                    // Optional 0-based `Int32` source index trailing param (see `lin_for`).
                    Type::func(vec![Type::TypeVar(9011), Type::Int32], Type::Bool),
                ], Type::Null),
        );

        // iter: (() => State, (State) => Boolean, (State) => State, (State) => T) => Iterator<T>
        self.define_intrinsic(
            "lin_iter",
            Type::func(vec![
                    Type::func(vec![], Type::TypeVar(9020)),
                    Type::func(vec![Type::TypeVar(9020)], Type::Bool),
                    Type::func(vec![Type::TypeVar(9020)], Type::TypeVar(9020)),
                    Type::func(vec![Type::TypeVar(9020)], Type::TypeVar(9021)),
                ], Type::Iterator(Box::new(Type::TypeVar(9021)))),
        );

        // range: (Int32, Int32) => Iterator<Int32>
        self.define_intrinsic(
            "lin_range",
            Type::func(vec![Type::Int32, Type::Int32], Type::Iterator(Box::new(Type::Int32))),
        );

        // map: (Iterable<T>, (T) => U) => U[]   (Iterable = Array | Iterator | Stream)
        // For Array/Iterator the lowering MATERIALIZES a concrete array (flat for scalar U, tagged
        // otherwise), so the declared return is `U[]`. A `Stream<T>` receiver is also accepted (std/
        // iter unification Stage 2): the call-site result is then re-typed to `Stream<U>` by
        // `streamish_combinator_ret` (lazy adapter — codegen lands in Stage 3).
        self.define_intrinsic(
            "lin_map",
            Type::func(vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9030))),
                        Type::Iterator(Box::new(Type::TypeVar(9030))),
                        Type::Stream(Box::new(Type::TypeVar(9030))),
                    ]),
                    // Optional 0-based `Int32` source index trailing param (see `lin_for`).
                    Type::func(vec![Type::TypeVar(9030), Type::Int32], Type::TypeVar(9031)),
                ], Type::Array(Box::new(Type::TypeVar(9031)))),
        );

        // filter: (Iterable<T>, (T) => Boolean) => T[]   (Stream receiver → Stream<T> at call site)
        self.define_intrinsic(
            "lin_filter",
            Type::func(vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9040))),
                        Type::Iterator(Box::new(Type::TypeVar(9040))),
                        Type::Stream(Box::new(Type::TypeVar(9040))),
                    ]),
                    // Optional 0-based `Int32` SOURCE index trailing param (see `lin_for`). The
                    // index is the source position even though filter's output position differs.
                    Type::func(vec![Type::TypeVar(9040), Type::Int32], Type::Bool),
                ], Type::Array(Box::new(Type::TypeVar(9040)))),
        );

        // reduce: (Iterable<T>, U, (U, T) => U) => U   (Stream receiver → U | Error at call site)
        self.define_intrinsic(
            "lin_reduce",
            Type::func(vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9050))),
                        Type::Iterator(Box::new(Type::TypeVar(9050))),
                        Type::Stream(Box::new(Type::TypeVar(9050))),
                    ]),
                    Type::TypeVar(9051),
                    // Optional 0-based `Int32` source index as the THIRD param (`(acc, item, i)`).
                    Type::func(vec![Type::TypeVar(9051), Type::TypeVar(9050), Type::Int32], Type::TypeVar(9051)),
                ], Type::TypeVar(9051)),
        );

        // Concurrency intrinsics (spec §24)
        // async: (() => T) => Promise<T>. The thunk's return type `T` (9100) is wrapped in the
        // opaque `Promise<T>` handle that `await` later resolves. (Also accepts an array of thunks
        // — the legacy overload — returning a Promise over that representation.)
        let async_t = Type::TypeVar(9100);
        self.define_intrinsic("lin_async", Type::func(vec![Type::Union(vec![
                Type::func(vec![], async_t.clone()),
                Type::Array(Box::new(Type::func(vec![], async_t.clone()))),
            ])], Type::Promise(Box::new(async_t.clone()))));
        // await: <T>(Promise<T>) => T | Error. Resolves the handle to its payload, or the fault
        // arm (ADR-045). The result type is the `T | Error` UNION — not bare `T` — so the value
        // stays tagged at runtime: a faulted await returns an Error object, and unboxing the result
        // as a raw `T` (e.g. i32) would corrupt it. 9101 links the Promise<T> payload to the result.
        self.define_intrinsic("lin_await", Type::func(
            vec![Type::Promise(Box::new(Type::TypeVar(9101)))],
            Type::Union(vec![Type::TypeVar(9101), crate::resolve::error_type()])));
        // parallel: variadic — always returns a tagged array (TypeVar(u32::MAX) = AnyVal).
        // Using u32::MAX prevents zonking from resolving the element type to a flat scalar,
        // which would cause codegen to use a flat array representation for a tagged array.
        self.define_intrinsic("lin_parallel", Type::func(vec![Type::Array(Box::new(Type::func(vec![], Type::TypeVar(9102))))], Type::Array(Box::new(Type::TypeVar(u32::MAX)))));
        // race: (Promise<T>[]) => Promise<T> — resolves with the first promise to settle.
        self.define_intrinsic("lin_race", Type::func(
            vec![Type::Array(Box::new(Type::Promise(Box::new(Type::TypeVar(9103)))))],
            Type::Promise(Box::new(Type::TypeVar(9103)))));
        // timeout: (Promise<T>, Int32) => Promise<T> — same handle, fails if the deadline passes.
        self.define_intrinsic("lin_timeout", Type::func(
            vec![Type::Promise(Box::new(Type::TypeVar(9104))), Type::Int32],
            Type::Promise(Box::new(Type::TypeVar(9104)))));
        // retry: (() => T, Int32) => Promise<T> — re-runs the thunk up to N times.
        self.define_intrinsic("lin_retry", Type::func(vec![
                Type::func(vec![], Type::TypeVar(9105)),
                Type::Int32,
            ], Type::Promise(Box::new(Type::TypeVar(9105)))));
        // threadPool: (Int32) => ThreadPool
        self.define_intrinsic("lin_thread_pool", Type::func(vec![Type::Int32], Type::TypeVar(9106)));
        // poolAsync: (ThreadPool, () => T) => Promise<T>  — enqueue a thunk on a bounded pool.
        self.define_intrinsic("lin_pool_async", Type::func(vec![
                Type::TypeVar(9120),
                Type::func(vec![], Type::TypeVar(9121)),
            ], Type::Promise(Box::new(Type::TypeVar(9121)))));
        // Shared<T> accessors (ADR-028 §2.3.1). The opaque Shared<T> type is modelled with a
        // The opaque `Shared<T>` type (ADR-029): the four accessors below are the ONLY operations.
        // `Shared<T>` is invariant and never auto-unwraps to `T`/`AnyVal`, so any other op on it
        // (push, indexing, …) is a compile-time type error. Each accessor shares a single TypeVar
        // `T` between its `Shared<T>` and the bare `T`, so inference links the two.
        //   shared:   <T>(T) => Shared<T>
        //   get:      <T>(Shared<T>) => T          (snapshot copy-out)
        //   set:      <T>(Shared<T>, T) => Null    (copy-in)
        //   withLock: <T, R>(Shared<T>, (T) => R) => R
        let shared_t = || Type::TypeVar(9130);
        self.define_intrinsic("lin_shared",
            Type::func(vec![shared_t()], Type::Shared(Box::new(shared_t()))));
        self.define_intrinsic("lin_shared_get",
            Type::func(vec![Type::Shared(Box::new(shared_t()))], shared_t()));
        self.define_intrinsic("lin_shared_set",
            Type::func(vec![Type::Shared(Box::new(shared_t())), shared_t()], Type::Null));
        self.define_intrinsic("lin_shared_with_lock", Type::func(vec![
                Type::Shared(Box::new(shared_t())),
                Type::func(vec![shared_t()], Type::TypeVar(9138)),
            ], Type::TypeVar(9138)));
        // frozen: <T>(T) => T  (deep immortal seal; the value keeps its plain type so readers use
        // it transparently). Frozen<T> read-only coercion / mutation-inference is deferred (ADR-030).
        self.define_intrinsic("lin_freeze", Type::func(vec![Type::TypeVar(9140)], Type::TypeVar(9140)));
        // worker: ((Msg) => Reply, () => Null) => Worker
        self.define_intrinsic("lin_worker", Type::func(vec![
                Type::func(vec![Type::TypeVar(9107)], Type::TypeVar(9108)),
                Type::func(vec![], Type::Null),
            ], Type::TypeVar(9109)));
        // worker.request(msg): (Worker, Msg) => Reply
        self.define_intrinsic("lin_request", Type::func(vec![Type::TypeVar(9109), Type::TypeVar(9107)], Type::TypeVar(9108)));
        // worker.message(msg): (Worker, Msg) => Null
        self.define_intrinsic("lin_message", Type::func(vec![Type::TypeVar(9109), Type::TypeVar(9107)], Type::Null));
        // worker.close(): (Worker) => Null
        self.define_intrinsic("lin_close", Type::func(vec![Type::TypeVar(9109)], Type::Null));

        // Stream<T> — opaque, effectful, fallible pull-source (streams brief, ADR-047). These
        // intrinsic signatures are the SOLE source of a `Stream<T>` type: `Stream` is not
        // spellable in source annotations (no `resolve.rs` case), so stdlib wrappers obtain it by
        // inference from these returns. Reading yields `T | Null | Error` (Null = EOF, the
        // fallible-stdlib error shape on I/O failure); opening yields `Stream<UInt8[]> | Error`.
        //   openRead: (String) => Stream<UInt8[]> | Error
        //   read:     <T>(Stream<T>) => T | Null | Error
        //   close:    <T>(Stream<T>) => Null
        let byte_chunk = || Type::Array(Box::new(Type::UInt8));
        self.define_intrinsic("lin_fs_open", Type::func(vec![Type::Str],
            Type::Union(vec![Type::Stream(Box::new(byte_chunk())), crate::resolve::error_type()])));
        let stream_t = || Type::TypeVar(9160);
        self.define_intrinsic("lin_stream_read", Type::func(vec![Type::Stream(Box::new(stream_t()))],
            Type::Union(vec![stream_t(), Type::Null, crate::resolve::error_type()])));
        self.define_intrinsic("lin_stream_close",
            Type::func(vec![Type::Stream(Box::new(Type::TypeVar(9161)))], Type::Null));

        // Lazy adapters (Stage 4). Transform closures operate on BOXED items (AnyVal-in/AnyVal-out)
        // so the runtime can call them uniformly regardless of the concrete item type — stream
        // items (UInt8[] chunks, String lines, …) are all AnyVal-compatible. Each adapter returns a
        // fresh `Stream` (= Stream<AnyVal>); the opaque Stream type confines ops to this API.
        let any_stream = || Type::Stream(Box::new(Type::TypeVar(u32::MAX)));
        self.define_intrinsic("lin_stream_map", Type::func(vec![
                any_stream(),
                Type::func(vec![Type::TypeVar(u32::MAX)], Type::TypeVar(u32::MAX)),
            ], any_stream()));
        self.define_intrinsic("lin_stream_filter", Type::func(vec![
                any_stream(),
                Type::func(vec![Type::TypeVar(u32::MAX)], Type::Bool),
            ], any_stream()));
        self.define_intrinsic("lin_stream_take",
            Type::func(vec![any_stream(), Type::Int32], any_stream()));
        // Net-new lazy adapters (Stage 3): drop(s, n); takeWhile/dropWhile(s, p); flatMap(s, f);
        // flatten(s); concat(a, b) — all return a fresh Stream. Closures operate on boxed AnyVal.
        self.define_intrinsic("lin_stream_drop",
            Type::func(vec![any_stream(), Type::Int32], any_stream()));
        self.define_intrinsic("lin_stream_take_while", Type::func(vec![
                any_stream(),
                Type::func(vec![Type::TypeVar(u32::MAX)], Type::Bool),
            ], any_stream()));
        self.define_intrinsic("lin_stream_drop_while", Type::func(vec![
                any_stream(),
                Type::func(vec![Type::TypeVar(u32::MAX)], Type::Bool),
            ], any_stream()));
        self.define_intrinsic("lin_stream_flat_map", Type::func(vec![
                any_stream(),
                Type::func(vec![Type::TypeVar(u32::MAX)], Type::TypeVar(u32::MAX)),
            ], any_stream()));
        self.define_intrinsic("lin_stream_flatten",
            Type::func(vec![any_stream()], any_stream()));
        self.define_intrinsic("lin_stream_concat",
            Type::func(vec![any_stream(), any_stream()], any_stream()));
        // Streaming compression byte-adapters (std/compress): each Stream<UInt8[]> → Stream<UInt8[]>.
        // gunzip/gzip use the gzip container; inflate/deflate use raw DEFLATE.
        self.define_intrinsic("lin_stream_gunzip",
            Type::func(vec![any_stream()], any_stream()));
        self.define_intrinsic("lin_stream_gzip",
            Type::func(vec![any_stream()], any_stream()));
        self.define_intrinsic("lin_stream_inflate",
            Type::func(vec![any_stream()], any_stream()));
        self.define_intrinsic("lin_stream_deflate",
            Type::func(vec![any_stream()], any_stream()));
        // tar splitting (std/archive). `untar(s, body)` is a TERMINAL: it drives the whole archive on
        // the calling thread, calling `body(meta, data)` per entry where `meta` is an Object and
        // `data` is a `Stream<UInt8[]>` SUB-STREAM (a legal stream PARAMETER position per ADR-049).
        // Returns `Null | Error`. The body's return is ignored, so it is typed `AnyVal` (TypeVar(MAX)).
        // `manifest(s)`/`files(s)` are ADAPTERS returning a fresh `Stream` (of Objects). All three
        // CONSUME the parent stream (the affine move is type-based in the IR; the checker mirrors it
        // via `callee_routes_to_stream_op`'s std/archive arm).
        self.define_intrinsic("lin_stream_untar", Type::func(vec![
                any_stream(),
                Type::func(vec![
                        Type::TypeVar(u32::MAX),
                        Type::Stream(Box::new(Type::Array(Box::new(Type::UInt8)))),
                    ], Type::TypeVar(u32::MAX)),
            ], Type::Union(vec![Type::Null, crate::resolve::error_type()])));
        self.define_intrinsic("lin_stream_manifest",
            Type::func(vec![any_stream()], any_stream()));
        self.define_intrinsic("lin_stream_files",
            Type::func(vec![any_stream()], any_stream()));
        // entries/header/body — composable streaming tar adapter (std/archive).
        //   lin_stream_tar_entries: (Stream<UInt8[]>) -> Stream<TarEntry>
        //   lin_tar_header: (TarEntry) -> TarHeader
        //   lin_tar_body: (TarEntry) -> Stream<UInt8[]>
        let byte_chunk_stream = || Type::Stream(Box::new(byte_chunk()));
        self.define_intrinsic("lin_stream_tar_entries",
            Type::func(vec![byte_chunk_stream()], Type::Stream(Box::new(Type::TarEntry))));
        let tar_header_type = Type::object({
            let mut fields = indexmap::IndexMap::new();
            fields.insert("name".to_string(), Type::Str);
            fields.insert("size".to_string(), Type::Int64);
            fields.insert("typeflag".to_string(), Type::Str);
            fields.insert("isDir".to_string(), Type::Bool);
            fields
        });
        self.define_intrinsic("lin_tar_header",
            Type::func(vec![Type::TarEntry], tar_header_type));
        self.define_intrinsic("lin_tar_body",
            Type::func(vec![Type::TarEntry], byte_chunk_stream()));
        // Net-new terminals (Stage 4): reduce → U | Error; find → T | Null | Error; some/every →
        // Boolean | Error; while → Null | Error. All consume + close the stream.
        self.define_intrinsic("lin_stream_reduce", Type::func(vec![
                any_stream(),
                Type::TypeVar(u32::MAX),
                Type::func(vec![Type::TypeVar(u32::MAX), Type::TypeVar(u32::MAX)], Type::TypeVar(u32::MAX)),
            ], Type::TypeVar(u32::MAX)));
        self.define_intrinsic("lin_stream_find", Type::func(vec![
                any_stream(),
                Type::func(vec![Type::TypeVar(u32::MAX)], Type::Bool),
            ], Type::Union(vec![Type::TypeVar(u32::MAX), Type::Null, crate::resolve::error_type()])));
        self.define_intrinsic("lin_stream_some", Type::func(vec![
                any_stream(),
                Type::func(vec![Type::TypeVar(u32::MAX)], Type::Bool),
            ], Type::Union(vec![Type::Bool, crate::resolve::error_type()])));
        self.define_intrinsic("lin_stream_every", Type::func(vec![
                any_stream(),
                Type::func(vec![Type::TypeVar(u32::MAX)], Type::Bool),
            ], Type::Union(vec![Type::Bool, crate::resolve::error_type()])));
        self.define_intrinsic("lin_stream_while", Type::func(vec![
                any_stream(),
                Type::func(vec![Type::TypeVar(u32::MAX)], Type::Bool),
            ], Type::Union(vec![Type::Null, crate::resolve::error_type()])));
        // lines(s, maxBytes): maxBytes ≤ 0 selects the default per-line cap; a positive value sets
        // an explicit bound. Stdlib exposes `lines(s)` (default) and `linesMax(s, n)` over this.
        self.define_intrinsic("lin_stream_lines",
            Type::func(vec![any_stream(), Type::Int32], any_stream()));
        self.define_intrinsic("lin_stream_chunks",
            Type::func(vec![any_stream(), Type::Int32], any_stream()));
        // Sink + terminals. writeStream → a sink Stream; drain → Null|Error; collect → UInt8[]|
        // Error; readText → String|Error. All terminals consume + close the stream.
        self.define_intrinsic("lin_stream_write",
            Type::func(vec![any_stream(), Type::Str], any_stream()));
        self.define_intrinsic("lin_stream_write_lines",
            Type::func(vec![any_stream(), Type::Str], any_stream()));
        self.define_intrinsic("lin_stream_drain",
            Type::func(vec![any_stream()], Type::Union(vec![Type::Null, crate::resolve::error_type()])));
        self.define_intrinsic("lin_stream_collect",
            Type::func(vec![any_stream()], Type::Union(vec![Type::Array(Box::new(Type::UInt8)), crate::resolve::error_type()])));
        self.define_intrinsic("lin_stream_read_text",
            Type::func(vec![any_stream()], Type::Union(vec![Type::Str, crate::resolve::error_type()])));

        // Unified OS sources (Stage 5): each yields a Stream<UInt8[]> over a different backend.
        // tcpStream(fd) / stdoutStream(handle) take an integer fd/handle; stdinStream() is nullary.
        let byte_stream = || Type::Stream(Box::new(Type::Array(Box::new(Type::UInt8))));
        self.define_intrinsic("lin_net_tcp_stream", Type::func(vec![Type::Int32], byte_stream()));
        self.define_intrinsic("lin_process_stdout_stream", Type::func(vec![Type::Int64], byte_stream()));
        self.define_intrinsic("lin_io_stdin_stream", Type::func(vec![], byte_stream()));

        // promise(s): (Stream) => Promise<Null | Error> (Stage 8). The promise handle round-trips
        // as a Promise<Null> handle (TAG_PROMISE), like every other promise; `await` then yields the
        // `Null | Error` result — Null on success, Error if a read/transform faulted upstream.
        self.define_intrinsic("lin_stream_promise",
            Type::func(vec![any_stream()], Type::Promise(Box::new(Type::Null))));

        // serve: ((Request) => Response, Int32) => Null  (spec §25.5). Handler-first so
        // `router.serve(port)` desugars to `serve(router, port)`. Blocks forever; typed Null.
        self.define_intrinsic("lin_serve", Type::func(vec![
                Type::func(vec![Type::TypeVar(9150)], Type::TypeVar(9151)),
                Type::Int32,
            ], Type::Null));

        // exit: (Int32) => Null — terminates the process with a status code
        self.define_intrinsic("lin_exit", Type::func(vec![Type::Int32], Type::Null));

        // value_key: (any) => String — canonical type-tagged key for any value
        self.define_intrinsic("lin_value_key", Type::func(vec![Type::TypeVar(u32::MAX)], Type::Str));

        // to_json: (AnyVal) => String — recursive strict-JSON serializer for any value.
        // The param is the AnyVal marker (TypeVar(u32::MAX)) so any value flows in.
        self.define_intrinsic("lin_to_json", Type::func(vec![Type::TypeVar(u32::MAX)], Type::Str));

        // arrayAllocate(n) => AnyVal[] — null-filled tagged array of length n
        self.define_intrinsic("lin_array_allocate", Type::func(vec![Type::Int32], Type::Array(Box::new(Type::TypeVar(u32::MAX)))));

        // arrayAllocateFilled(n, val) => T[] — flat scalar array of length n filled with val
        // Uses TypeVar(u32::MAX) for val so any scalar can be passed; returns AnyVal[] (TypeVar).
        self.define_intrinsic("lin_array_allocate_filled", Type::func(vec![Type::Int32, Type::TypeVar(u32::MAX)], Type::Array(Box::new(Type::TypeVar(u32::MAX)))));
    }
}
