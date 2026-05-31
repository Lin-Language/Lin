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
        // Uses TypeVar(u32::MAX) as the "any" Json type for the object case.
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

        // keys: (Object) => String[]
        self.define_intrinsic(
            "lin_keys",
            Type::func(vec![Type::Object(IndexMap::new())], Type::Array(Box::new(Type::Str))),
        );

        // lin_object_set: (Object, String, Json) => Null — in-place object key mutation
        self.define_intrinsic(
            "lin_object_set",
            Type::func(vec![Type::Object(IndexMap::new()), Type::Str, Type::TypeVar(u32::MAX)], Type::Null),
        );

        // for: (Iterable<T>, (T) => Json) => Null  — callback return type is ignored
        self.define_intrinsic(
            "lin_for",
            Type::func(vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9010))),
                        Type::Iterator(Box::new(Type::TypeVar(9010))),
                    ]),
                    Type::func(vec![Type::TypeVar(9010)], Type::TypeVar(u32::MAX)),
                ], Type::Null),
        );

        // while: (Array<T> | Iterator<T>, (T) => Boolean) => Null
        self.define_intrinsic(
            "lin_while",
            Type::func(vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9011))),
                        Type::Iterator(Box::new(Type::TypeVar(9011))),
                    ]),
                    Type::func(vec![Type::TypeVar(9011)], Type::Bool),
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

        // map: (Iterable<T>, (T) => U) => U[]
        // The lowering MATERIALIZES a concrete array (flat for scalar U, tagged otherwise), so the
        // return type is `U[]` — the stdlib `map` thin wrapper declares the same, and callers chain
        // `.filter`/`.reduce`/index on it as an array.
        self.define_intrinsic(
            "lin_map",
            Type::func(vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9030))),
                        Type::Iterator(Box::new(Type::TypeVar(9030))),
                    ]),
                    Type::func(vec![Type::TypeVar(9030)], Type::TypeVar(9031)),
                ], Type::Array(Box::new(Type::TypeVar(9031)))),
        );

        // filter: (Iterable<T>, (T) => Boolean) => T[]
        self.define_intrinsic(
            "lin_filter",
            Type::func(vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9040))),
                        Type::Iterator(Box::new(Type::TypeVar(9040))),
                    ]),
                    Type::func(vec![Type::TypeVar(9040)], Type::Bool),
                ], Type::Array(Box::new(Type::TypeVar(9040)))),
        );

        // reduce: (Iterable<T>, U, (U, T) => U) => U
        self.define_intrinsic(
            "lin_reduce",
            Type::func(vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9050))),
                        Type::Iterator(Box::new(Type::TypeVar(9050))),
                    ]),
                    Type::TypeVar(9051),
                    Type::func(vec![Type::TypeVar(9051), Type::TypeVar(9050)], Type::TypeVar(9051)),
                ], Type::TypeVar(9051)),
        );

        // Concurrency intrinsics (spec §24)
        // async: (() => T) => Promise<T>  (TypeVar-based, overloaded: also accepts T[])
        let promise_t = Type::TypeVar(9100);
        self.define_intrinsic("lin_async", Type::func(vec![Type::Union(vec![
                Type::func(vec![], promise_t.clone()),
                Type::Array(Box::new(Type::func(vec![], promise_t.clone()))),
            ])], Type::TypeVar(9100)));
        // await: accepts a promise or array of promises
        self.define_intrinsic("lin_await", Type::func(vec![Type::TypeVar(9101)], Type::TypeVar(9101)));
        // parallel: variadic — always returns a tagged array (TypeVar(u32::MAX) = Json/any).
        // Using u32::MAX prevents zonking from resolving the element type to a flat scalar,
        // which would cause codegen to use a flat array representation for a tagged array.
        self.define_intrinsic("lin_parallel", Type::func(vec![Type::Array(Box::new(Type::func(vec![], Type::TypeVar(9102))))], Type::Array(Box::new(Type::TypeVar(u32::MAX)))));
        // race: Promise[] => Promise
        self.define_intrinsic("lin_race", Type::func(vec![Type::Array(Box::new(Type::TypeVar(9103)))], Type::TypeVar(9103)));
        // timeout: (Promise, Int32) => Promise
        self.define_intrinsic("lin_timeout", Type::func(vec![Type::TypeVar(9104), Type::Int32], Type::TypeVar(9104)));
        // retry: (() => T, Int32) => Promise<T>
        self.define_intrinsic("lin_retry", Type::func(vec![
                Type::func(vec![], Type::TypeVar(9105)),
                Type::Int32,
            ], Type::TypeVar(9105)));
        // threadPool: (Int32) => ThreadPool
        self.define_intrinsic("lin_thread_pool", Type::func(vec![Type::Int32], Type::TypeVar(9106)));
        // poolAsync: (ThreadPool, () => T) => Promise<T>  — enqueue a thunk on a bounded pool.
        self.define_intrinsic("lin_pool_async", Type::func(vec![
                Type::TypeVar(9120),
                Type::func(vec![], Type::TypeVar(9121)),
            ], Type::TypeVar(9121)));
        // Shared<T> accessors (ADR-043 §2.3.1). The opaque Shared<T> type is modelled with a
        // The opaque `Shared<T>` type (ADR-044): the four accessors below are the ONLY operations.
        // `Shared<T>` is invariant and never auto-unwraps to `T`/`Json`, so any other op on it
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
        // it transparently). Frozen<T> read-only coercion / mutation-inference is deferred (ADR-045).
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

        // Stream<T> — opaque, effectful, fallible pull-source (streams brief, ADR-072). These
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

        // Lazy adapters (Stage 4). Transform closures operate on BOXED items (Json-in/Json-out)
        // so the runtime can call them uniformly regardless of the concrete item type — stream
        // items (UInt8[] chunks, String lines, …) are all JSON-compatible. Each adapter returns a
        // fresh `Stream` (= Stream<Json>); the opaque Stream type confines ops to this API.
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
        self.define_intrinsic("lin_stream_lines",
            Type::func(vec![any_stream()], any_stream()));
        self.define_intrinsic("lin_stream_chunks",
            Type::func(vec![any_stream(), Type::Int32], any_stream()));
        // Sink + terminals. writeStream → a sink Stream; drain → Null|Error; collect → UInt8[]|
        // Error; readText → String|Error. All terminals consume + close the stream.
        self.define_intrinsic("lin_stream_write",
            Type::func(vec![any_stream(), Type::Str], any_stream()));
        self.define_intrinsic("lin_stream_drain",
            Type::func(vec![any_stream()], Type::Union(vec![Type::Null, crate::resolve::error_type()])));
        self.define_intrinsic("lin_stream_collect",
            Type::func(vec![any_stream()], Type::Union(vec![Type::Array(Box::new(Type::UInt8)), crate::resolve::error_type()])));
        self.define_intrinsic("lin_stream_read_text",
            Type::func(vec![any_stream()], Type::Union(vec![Type::Str, crate::resolve::error_type()])));

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

        // arrayAllocate(n) => Json[] — null-filled tagged array of length n
        self.define_intrinsic("lin_array_allocate", Type::func(vec![Type::Int32], Type::Array(Box::new(Type::TypeVar(u32::MAX)))));

        // arrayAllocateFilled(n, val) => T[] — flat scalar array of length n filled with val
        // Uses TypeVar(u32::MAX) for val so any scalar can be passed; returns Json[] (TypeVar).
        self.define_intrinsic("lin_array_allocate_filled", Type::func(vec![Type::Int32, Type::TypeVar(u32::MAX)], Type::Array(Box::new(Type::TypeVar(u32::MAX)))));
    }
}
