use indexmap::IndexMap;

use super::Checker;
use crate::types::Type;

impl Checker {
    pub(crate) fn register_intrinsics(&mut self) {
        // print: (T) => Null — accepts any value, converts to string at runtime
        let print_param = self.env.fresh_type_var();
        self.define_intrinsic(
            "lin_print",
            Type::Function {
                params: vec![print_param],
                ret: Box::new(Type::Null),
            },
        );

        // toString: (T) => String — accepts any value
        let to_string_param = self.env.fresh_type_var();
        self.define_intrinsic(
            "lin_to_string",
            Type::Function {
                params: vec![to_string_param],
                ret: Box::new(Type::Str),
            },
        );

        // length: (String | Array<T> | Iterator<T> | Object) => Int32
        // Uses TypeVar(u32::MAX) as the "any" Json type for the object case.
        self.define_intrinsic(
            "lin_length",
            Type::Function {
                params: vec![Type::Union(vec![
                    Type::Str,
                    Type::Array(Box::new(Type::TypeVar(9000))),
                    Type::Iterator(Box::new(Type::TypeVar(9000))),
                    Type::TypeVar(u32::MAX),
                ])],
                ret: Box::new(Type::Int32),
            },
        );

        // push: (T[], T) => Null
        self.define_intrinsic(
            "lin_push",
            Type::Function {
                params: vec![
                    Type::Array(Box::new(Type::TypeVar(9001))),
                    Type::TypeVar(9001),
                ],
                ret: Box::new(Type::Null),
            },
        );

        // set: (T[], Int32, T) => Null — in-place array element mutation
        self.define_intrinsic(
            "lin_array_set",
            Type::Function {
                params: vec![
                    Type::Array(Box::new(Type::TypeVar(9200))),
                    Type::Int32,
                    Type::TypeVar(9200),
                ],
                ret: Box::new(Type::Null),
            },
        );

        // keys: (Object) => String[]
        self.define_intrinsic(
            "lin_keys",
            Type::Function {
                params: vec![Type::Object(IndexMap::new())],
                ret: Box::new(Type::Array(Box::new(Type::Str))),
            },
        );

        // lin_object_set: (Object, String, Json) => Null — in-place object key mutation
        self.define_intrinsic(
            "lin_object_set",
            Type::Function {
                params: vec![Type::Object(IndexMap::new()), Type::Str, Type::TypeVar(u32::MAX)],
                ret: Box::new(Type::Null),
            },
        );

        // for: (Iterable<T>, (T) => Json) => Null  — callback return type is ignored
        self.define_intrinsic(
            "lin_for",
            Type::Function {
                params: vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9010))),
                        Type::Iterator(Box::new(Type::TypeVar(9010))),
                    ]),
                    Type::Function {
                        params: vec![Type::TypeVar(9010)],
                        ret: Box::new(Type::TypeVar(u32::MAX)),
                    },
                ],
                ret: Box::new(Type::Null),
            },
        );

        // while: (Array<T> | Iterator<T>, (T) => Boolean) => Null
        self.define_intrinsic(
            "lin_while",
            Type::Function {
                params: vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9011))),
                        Type::Iterator(Box::new(Type::TypeVar(9011))),
                    ]),
                    Type::Function {
                        params: vec![Type::TypeVar(9011)],
                        ret: Box::new(Type::Bool),
                    },
                ],
                ret: Box::new(Type::Null),
            },
        );

        // iter: (() => State, (State) => Boolean, (State) => State, (State) => T) => Iterator<T>
        self.define_intrinsic(
            "lin_iter",
            Type::Function {
                params: vec![
                    Type::Function {
                        params: vec![],
                        ret: Box::new(Type::TypeVar(9020)),
                    },
                    Type::Function {
                        params: vec![Type::TypeVar(9020)],
                        ret: Box::new(Type::Bool),
                    },
                    Type::Function {
                        params: vec![Type::TypeVar(9020)],
                        ret: Box::new(Type::TypeVar(9020)),
                    },
                    Type::Function {
                        params: vec![Type::TypeVar(9020)],
                        ret: Box::new(Type::TypeVar(9021)),
                    },
                ],
                ret: Box::new(Type::Iterator(Box::new(Type::TypeVar(9021)))),
            },
        );

        // range: (Int32, Int32) => Iterator<Int32>
        self.define_intrinsic(
            "lin_range",
            Type::Function {
                params: vec![Type::Int32, Type::Int32],
                ret: Box::new(Type::Iterator(Box::new(Type::Int32))),
            },
        );

        // map: (Iterable<T>, (T) => U) => Iterator<U>
        self.define_intrinsic(
            "lin_map",
            Type::Function {
                params: vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9030))),
                        Type::Iterator(Box::new(Type::TypeVar(9030))),
                    ]),
                    Type::Function {
                        params: vec![Type::TypeVar(9030)],
                        ret: Box::new(Type::TypeVar(9031)),
                    },
                ],
                ret: Box::new(Type::Iterator(Box::new(Type::TypeVar(9031)))),
            },
        );

        // filter: (Iterable<T>, (T) => Boolean) => Iterator<T>
        self.define_intrinsic(
            "lin_filter",
            Type::Function {
                params: vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9040))),
                        Type::Iterator(Box::new(Type::TypeVar(9040))),
                    ]),
                    Type::Function {
                        params: vec![Type::TypeVar(9040)],
                        ret: Box::new(Type::Bool),
                    },
                ],
                ret: Box::new(Type::Iterator(Box::new(Type::TypeVar(9040)))),
            },
        );

        // reduce: (Iterable<T>, U, (U, T) => U) => U
        self.define_intrinsic(
            "lin_reduce",
            Type::Function {
                params: vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9050))),
                        Type::Iterator(Box::new(Type::TypeVar(9050))),
                    ]),
                    Type::TypeVar(9051),
                    Type::Function {
                        params: vec![Type::TypeVar(9051), Type::TypeVar(9050)],
                        ret: Box::new(Type::TypeVar(9051)),
                    },
                ],
                ret: Box::new(Type::TypeVar(9051)),
            },
        );

        // Concurrency intrinsics (spec §32)
        // async: (() => T) => Promise<T>  (TypeVar-based, overloaded: also accepts T[])
        let promise_t = Type::TypeVar(9100);
        self.define_intrinsic("lin_async", Type::Function {
            params: vec![Type::Union(vec![
                Type::Function { params: vec![], ret: Box::new(promise_t.clone()) },
                Type::Array(Box::new(Type::Function { params: vec![], ret: Box::new(promise_t.clone()) })),
            ])],
            ret: Box::new(Type::TypeVar(9100)),
        });
        // await: accepts a promise or array of promises
        self.define_intrinsic("lin_await", Type::Function {
            params: vec![Type::TypeVar(9101)],
            ret: Box::new(Type::TypeVar(9101)),
        });
        // parallel: variadic — always returns a tagged array (TypeVar(u32::MAX) = Json/any).
        // Using u32::MAX prevents zonking from resolving the element type to a flat scalar,
        // which would cause codegen to use a flat array representation for a tagged array.
        self.define_intrinsic("lin_parallel", Type::Function {
            params: vec![Type::Array(Box::new(Type::Function {
                params: vec![],
                ret: Box::new(Type::TypeVar(9102)),
            }))],
            ret: Box::new(Type::Array(Box::new(Type::TypeVar(u32::MAX)))),
        });
        // race: Promise[] => Promise
        self.define_intrinsic("lin_race", Type::Function {
            params: vec![Type::Array(Box::new(Type::TypeVar(9103)))],
            ret: Box::new(Type::TypeVar(9103)),
        });
        // timeout: (Promise, Int32) => Promise
        self.define_intrinsic("lin_timeout", Type::Function {
            params: vec![Type::TypeVar(9104), Type::Int32],
            ret: Box::new(Type::TypeVar(9104)),
        });
        // retry: (() => T, Int32) => Promise<T>
        self.define_intrinsic("lin_retry", Type::Function {
            params: vec![
                Type::Function { params: vec![], ret: Box::new(Type::TypeVar(9105)) },
                Type::Int32,
            ],
            ret: Box::new(Type::TypeVar(9105)),
        });
        // threadPool: (Int32) => ThreadPool
        self.define_intrinsic("lin_thread_pool", Type::Function {
            params: vec![Type::Int32],
            ret: Box::new(Type::TypeVar(9106)),
        });
        // worker: ((Msg) => Reply, () => Null) => Worker
        self.define_intrinsic("lin_worker", Type::Function {
            params: vec![
                Type::Function { params: vec![Type::TypeVar(9107)], ret: Box::new(Type::TypeVar(9108)) },
                Type::Function { params: vec![], ret: Box::new(Type::Null) },
            ],
            ret: Box::new(Type::TypeVar(9109)),
        });
        // worker.request(msg): (Worker, Msg) => Reply
        self.define_intrinsic("lin_request", Type::Function {
            params: vec![Type::TypeVar(9109), Type::TypeVar(9107)],
            ret: Box::new(Type::TypeVar(9108)),
        });
        // worker.message(msg): (Worker, Msg) => Null
        self.define_intrinsic("lin_message", Type::Function {
            params: vec![Type::TypeVar(9109), Type::TypeVar(9107)],
            ret: Box::new(Type::Null),
        });
        // worker.close(): (Worker) => Null
        self.define_intrinsic("lin_close", Type::Function {
            params: vec![Type::TypeVar(9109)],
            ret: Box::new(Type::Null),
        });

        // exit: (Int32) => Null — terminates the process with a status code
        self.define_intrinsic("lin_exit", Type::Function {
            params: vec![Type::Int32],
            ret: Box::new(Type::Null),
        });

        // value_key: (any) => String — canonical type-tagged key for any value
        self.define_intrinsic("lin_value_key", Type::Function {
            params: vec![Type::TypeVar(u32::MAX)],
            ret: Box::new(Type::Str),
        });

        // arrayAllocate(n) => Json[] — null-filled tagged array of length n
        self.define_intrinsic("lin_array_allocate", Type::Function {
            params: vec![Type::Int32],
            ret: Box::new(Type::Array(Box::new(Type::TypeVar(u32::MAX)))),
        });

        // arrayAllocateFilled(n, val) => T[] — flat scalar array of length n filled with val
        // Uses TypeVar(u32::MAX) for val so any scalar can be passed; returns Json[] (TypeVar).
        self.define_intrinsic("lin_array_allocate_filled", Type::Function {
            params: vec![Type::Int32, Type::TypeVar(u32::MAX)],
            ret: Box::new(Type::Array(Box::new(Type::TypeVar(u32::MAX)))),
        });
    }
}
