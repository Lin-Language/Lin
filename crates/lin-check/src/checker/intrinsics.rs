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

        // map: (Iterable<T>, (T) => U) => Iterator<U>
        self.define_intrinsic(
            "lin_map",
            Type::func(vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9030))),
                        Type::Iterator(Box::new(Type::TypeVar(9030))),
                    ]),
                    Type::func(vec![Type::TypeVar(9030)], Type::TypeVar(9031)),
                ], Type::Iterator(Box::new(Type::TypeVar(9031)))),
        );

        // filter: (Iterable<T>, (T) => Boolean) => Iterator<T>
        self.define_intrinsic(
            "lin_filter",
            Type::func(vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9040))),
                        Type::Iterator(Box::new(Type::TypeVar(9040))),
                    ]),
                    Type::func(vec![Type::TypeVar(9040)], Type::Bool),
                ], Type::Iterator(Box::new(Type::TypeVar(9040)))),
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

        // Concurrency intrinsics (spec §32)
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
        // TypeVar here (no dedicated Type variant yet); the runtime enforces the box semantics.
        //   shared:   <T>(T) => Shared<T>
        //   get:      <T>(Shared<T>) => T          (snapshot copy-out)
        //   set:      <T>(Shared<T>, T) => Null    (copy-in)
        //   withLock: <T,R>(Shared<T>, (T) => R) => R
        self.define_intrinsic("lin_shared", Type::func(vec![Type::TypeVar(9130)], Type::TypeVar(9131)));
        self.define_intrinsic("lin_shared_get", Type::func(vec![Type::TypeVar(9132)], Type::TypeVar(9133)));
        self.define_intrinsic("lin_shared_set", Type::func(vec![Type::TypeVar(9134), Type::TypeVar(9135)], Type::Null));
        self.define_intrinsic("lin_shared_with_lock", Type::func(vec![
                Type::TypeVar(9136),
                Type::func(vec![Type::TypeVar(9137)], Type::TypeVar(9138)),
            ], Type::TypeVar(9138)));
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
