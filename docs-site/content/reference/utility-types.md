# Utility Types

Lin provides a set of built-in **type operators** that derive new types from existing ones. They are the same family of tools you may know from TypeScript — `Partial`, `Pick`, `Omit`, and friends — plus the `keyof` operator and indexed-access types `T["field"]`.

Every one of these is a pure **compile-time** transform. It runs during type checking and *erases* to an ordinary record, union, map, or array type — there is no runtime representation, no boxing, and no cost. `Pick<User, "id">` is exactly the record `{ "id": Int32 }` as far as the compiled program is concerned.

For the underlying type system, see [Types](types.html); for generic declarations and inference, see [Generics](generics.html).

All the examples below assume this record:

```lin
type User = {
  "id": Int32,
  "name": String,
  "email": String | Null
}
```

## `keyof T` — the field names of a record

`keyof T` is the union of `T`'s field-name string-literal types. It is most useful for constraining a value to "a key of this record".

```lin
import { print } from "std/io"

type User = { "id": Int32, "name": String, "email": String | Null }

type Keys = keyof User      // "id" | "name" | "email"

val k: Keys = "name"        // ok — "name" is a field of User
// val bad: Keys = "phone"  // compile error — not a field of User

print(k)                    // name
```

`keyof` also applies to an index-signature map, where it yields the key type — `keyof { String: Int32 }` is `String`.

## `T["field"]` — indexed access

An indexed-access type reads out the type of one or more fields by name. The key may be a single string literal or a union of them (such as the output of `keyof`):

```lin
type User = { "id": Int32, "name": String, "email": String | Null }

type NameType  = User["name"]            // String
type IdOrEmail = User["id" | "email"]    // Int32 | (String | Null)
```

An unknown key is a compile-time error with a "did you mean" suggestion.

## `Partial<T>` — make every field optional

`Partial<T>` takes a record and makes every field nullable (`V` becomes `V | Null`). It is the type of a "patch" — an update that may carry any subset of the fields.

```lin
import { print } from "std/io"

type User  = { "id": Int32, "name": String, "email": String | Null }
type Patch = Partial<User>
// { "id": Int32 | Null, "name": String | Null, "email": String | Null }

val rename: Patch = { "id": 1, "name": "ann", "email": null }
print(rename["name"])       // ann
```

## `Required<T>` — make every field non-nullable

`Required<T>` is the inverse of `Partial`: it strips `Null` from every field type.

```lin
type User     = { "id": Int32, "name": String, "email": String | Null }
type FullUser = Required<User>
// { "id": Int32, "name": String, "email": String }

val u: FullUser = { "id": 2, "name": "bob", "email": "bob@example.com" }
// "email" can no longer be null here
```

## `Pick<T, K>` — keep only some fields

`Pick<T, K>` builds a record with only the fields named by `K`, in `T`'s original field order. `K` is a string literal or a union of them (or `keyof` something).

```lin
import { print } from "std/io"

type User  = { "id": Int32, "name": String, "email": String | Null }
type Names = Pick<User, "id" | "name">
// { "id": Int32, "name": String }

val n: Names = { "id": 7, "name": "cay" }
print(n["name"])            // cay
```

Requesting a key that is not a field of `T` is a compile-time error naming the bad key.

## `Omit<T, K>` — drop some fields

`Omit<T, K>` is the complement of `Pick`: it keeps every field *except* those named by `K`.

```lin
type User    = { "id": Int32, "name": String, "email": String | Null }
type NoEmail = Omit<User, "email">
// { "id": Int32, "name": String }
```

`Omit` is lenient: naming a key that `T` does not have is simply ignored, not an error.

## `NonNullable<T>` — remove `Null`

`NonNullable<T>` removes `Null` from a type. On a union it drops the `Null` member; a `Null`-only type becomes `Never`.

```lin
type MaybeName = String | Null
type DefName   = NonNullable<MaybeName>   // String
```

## `Exclude<U, M>` and `Extract<U, M>` — filter a union

`Exclude<U, M>` removes from union `U` every member that also appears in `M`. `Extract<U, M>` does the opposite — it keeps only the members that appear in `M`. Members are compared structurally.

```lin
import { print } from "std/io"

type Status     = "active" | "inactive" | "pending"
type NotPending = Exclude<Status, "pending">    // "active" | "inactive"
type OnlyActive = Extract<Status, "active">     // "active"

val s: NotPending = "active"
print(s)                    // active
```

## `ReturnType<F>` and `Parameters<F>` — read a function type

`ReturnType<F>` is the return type of a function type `F`. `Parameters<F>` is its parameter list as a fixed-length array (a tuple).

```lin
type Handler = (Int32, String) => Boolean

type Ret    = ReturnType<Handler>     // Boolean
type Params = Parameters<Handler>     // [Int32, String]

val args: Params = [3, "x"]
val ok: Ret = true
```

## `Record<K, V>` — a uniform map

`Record<K, V>` builds a map type `{ K: V }`. When `K` is `String` (or an integer family) the result is a dynamic [map](maps.html); when `K` is a closed union of string literals it is sugar for a fixed record with one field per literal.

```lin
import { print } from "std/io"

type Flags = Record<"a" | "b", Boolean>   // { "a": Boolean, "b": Boolean }
type Counts = Record<String, Int32>        // a string-keyed map of Int32

val flags: Flags = { "a": true, "b": false }
print(if flags["a"] then "on" else "off")  // on
```

## Composing operators

Because every operator erases to an ordinary structural type, the results compose freely — with intersection (`&`), unions, generics, and each other:

```lin
type User = { "id": Int32, "name": String, "email": String | Null }

// A patch that can only touch the public fields:
type PublicPatch = Partial<Omit<User, "id">>
// { "name": String | Null, "email": String | Null }

// The value type behind a chosen key:
type EmailField = Pick<User, "email">["email"]   // String | Null
```

## There is no `Readonly`

TypeScript's `Readonly<T>` has no equivalent in Lin, by design. Lin records are observably-mutable [reference types](records.html), so a "read-only record" type would carry no enforceable meaning. Use immutability at the **binding** level (`val`) where you need it.
