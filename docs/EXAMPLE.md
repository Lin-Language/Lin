# Minimal JSON Expression Language — Syntax Examples

```txt
// ------------------------------------------------------------
// Imports
// ------------------------------------------------------------

import { substring, indexOf, trim, toUpper } from "std/string"
import { parseInt32, parseFloat64 } from "std/number"
import { map, filter, reduce } from "std/iter"
import { print } from "std/io"

import { substring as substr } from "std/string"


// ------------------------------------------------------------
// Exports
// ------------------------------------------------------------

// Any val, var, or type can be exported
// Functions are ordinary values, so exported functions are exported vals or vars

export val version: String = "0.1.0"

export var debugEnabled: Boolean = false

export val add = (a: Int32, b: Int32): Int32 =>
  a + b


// ------------------------------------------------------------
// Primitive values
// ------------------------------------------------------------

val name: String = "Bob"
val active: Boolean = true
val missing: Null = null

val smallSigned: Int8 = 12
val normalSigned: Int32 = 42
val largeSigned: Int64 = 9000000000
val negative: Int32 = -5

val smallUnsigned: UInt8 = 255
val normalUnsigned: UInt32 = 123_456
val largeUnsigned: UInt64 = 9_000_000_000

val smallFloat: Float16 = 1.5
val normalFloat: Float32 = 3.14
val largeFloat: Float64 = 3.141592653589793
val avogadro: Float64 = 6.022e23

// Literals can use hex, binary, octal, and underscore separators

val hex: Int32 = 0xFF
val binary: Int32 = 0b1010
val octal: Int32 = 0o755
val million: Int32 = 1_000_000

// Literal suffixes override default inference

val explicitByte: Int8 = 42i8
val explicitFloat: Float32 = 3.14f32

// Without context, integer literals default to Int32, floats to Float64

val inferredInteger = 42
val inferredFloat = 3.14


// ------------------------------------------------------------
// Strings
// ------------------------------------------------------------

val greeting: String = "Hello"

// Escape sequences

val tabbed: String = "col1\tcol2"
val quoted: String = "she said \"hi\""
val unicode: String = "snowman: \u{2603}"

// Multi-line strings preserve newlines verbatim

val poem: String = "Roses are red,
Violets are blue."

// Interpolation with ${ expression }

val who: String = "Bob"
val age: Int32 = 42
val message: String = "Hello ${who}, you are ${age + 1} next year"


// ------------------------------------------------------------
// Strict JSON values
// ------------------------------------------------------------

val person = {
  "name": "Bob",
  "age": 42,
  "active": true,
  "address": {
    "line1": "1 Fish Street",
    "city": "Norwich"
  },
  "tags": ["admin", "customer", "beta"],
  "spouse": null
}

val numbers = [1, 2, 3, 4]
val strings = ["one", "two", "three"]
val mixedJson: Json = [1, "two", true, null, { "x": 10 }]


// ------------------------------------------------------------
// JSON access
// ------------------------------------------------------------

val personName = person["name"]
val personAge = person["age"]
val city = person["address"]["city"]

val firstNumber = numbers[0]
val firstTag = person["tags"][0]

// Bracket access is safe by default
// Missing keys return Null, and Null propagates through chains

val missing = person["doesnt_exist"]                                  // null
val deeplyMissing = person["a"]["b"]["c"]["d"]                        // null
val nullChained: Null = null["anything"]["here"]                      // null

// Arrays: OOB is a runtime error (not safe)
// Use length() to guard, or stay within known bounds


// ------------------------------------------------------------
// Types
// ------------------------------------------------------------

export type Address = {
  "line1": String,
  "city": String
}

export type Person = {
  "name": String,
  "age": Int32,
  "active": Boolean,
  "address": Address,
  "tags": String[],
  "spouse": Person | Null
}

type Named = {
  "name": String
}

type Id = String | Int64

type Result<T, E> =
  | { "type": "success", "value": T }
  | { "type": "failure", "error": E }

type ParseInt32Result = Result<Int32, String>

type Predicate<T> = (T) => Boolean
type Mapper<T, U> = (T) => U
type Reducer<T, U> = (U, T) => U


// ------------------------------------------------------------
// Array types
// ------------------------------------------------------------

// Unbounded array: T[]

val ages: Int32[] = [10, 20, 30]
val labels: String[] = ["a", "b", "c"]

// Fixed-length array: [T1, T2, T3]
// The runtime value is still an ordinary JSON array

val labelledPoint: [String, Int32, Int32] = ["origin", 0, 0]
val pair: [String, Int32] = ["age", 42]


// ------------------------------------------------------------
// Structural typing
// ------------------------------------------------------------

val bob: Person = {
  "name": "Bob",
  "age": 42,
  "active": true,
  "address": {
    "line1": "1 Fish Street",
    "city": "Norwich"
  },
  "tags": ["admin", "customer"],
  "spouse": null
}

// A value can have extra fields and still match a smaller shape

val greet = (item: Named): String =>
  "Hello ${item["name"]}"

val greeting2 = greet({
  "name": "Alice",
  "age": 99
})


// ------------------------------------------------------------
// Functions
// ------------------------------------------------------------

val multiply = (a: Int32, b: Int32): Int32 =>
  a * b

val fullName = (first: String, last: String): String =>
  "${first} ${last}"

val answer = add(40, 2)

// Blocks evaluate to their final expression

val calculateTotal = (price: Float64, quantity: Int32): Float64 =>
  val subtotal = price * quantity.toFloat64()
  val tax = subtotal * 0.2
  subtotal + tax

val total = calculateTotal(10.0, 3)


// ------------------------------------------------------------
// Null and union types
// ------------------------------------------------------------

val maybeName: String | Null = null
val maybeOtherName: String | Null = "Jane"

val displayName = (input: String | Null): String =>
  if input is Null then "Anonymous"
  else input

val displayed = displayName(maybeName)


// ------------------------------------------------------------
// If expressions (flexible layout)
// ------------------------------------------------------------

// Single-line form

val l1 = if person["age"] >= 18 then "adult" else "child"

// Multi-line form

val l2 = if person["age"] >= 18 then
  "adult"
else
  "child"

// Block branches

val l3 = if person["age"] >= 18 then
  val prefix = "ad"
  "${prefix}ult"
else
  val prefix = "ch"
  "${prefix}ild"

// Continuation with && or || on the next line

val l4 = if person["age"] >= 18
  && person["name"] != "something" then "adult"
else "child"


// ------------------------------------------------------------
// is and has in if expressions
// ------------------------------------------------------------

// is checks exact type, exact literal, or exact shape

val describePrimitive = (input: String | Int32 | Null): String =>
  if input is Null then "Null"
  else if input is Int32 then "Int32"
  else "String"

val isBigDave = (input: String): Boolean =>
  if input is "Dave" then true
  else false

// has checks whether a value structurally contains a shape

val hasName = (input: Json): Boolean =>
  if input has { name } then true
  else false

val describeNamedThing = (input: Json): String =>
  if input has { name } then "Named thing: ${input["name"]}"
  else "Unnamed thing"

val describeOlderNamedThing = (input: Json): String =>
  if input has { name, age }
  && input["age"] > 30 then "Older named thing: ${input["name"]}"
  else "Something else"


// ------------------------------------------------------------
// Equality (objects are unordered, arrays are ordered)
// ------------------------------------------------------------

val eq1 = 1 == 1                                          // true
val eq2 = "1" == 1                                        // false
val eq3 = null == null                                    // true
val eq4 = "str" == "str"                                  // true
val eq5 = { "a": 1 } == { "a": 1 }                        // true
val eq6 = { "a": 1, "b": 2 } == { "b": 2, "a": 1 }        // true
val eq7 = [1, 2] == [1, 2]                                // true
val eq8 = [1, 2] == [2, 1]                                // false
val eq9 = 1 == 1.0                                        // true (numeric)

val neq1 = 1 != 2                                         // true
val neq2 = "fish" != "fish"                               // false


// ------------------------------------------------------------
// Dot application
// ------------------------------------------------------------

// x.f(y, z) is equivalent to f(x, y, z)

val directSubstring = substring("myString", 1, 5)
val dottedSubstring = "myString".substring(1, 5)
val aliasedSubstring = "myString".substr(1, 5)


// ------------------------------------------------------------
// Partial application
// ------------------------------------------------------------

// Function arguments are applied from left to right
// Too few arguments => the result is another function
// Too many arguments => compile error

val addTen = add(10)
val fifteen = addTen(5)

// Dot partial application: x.f is f partially applied with x as first arg

val takeFromMyString = "myString".substring
val firstFive = takeFromMyString(0, 5)

// Multi-argument partial via (x, y).f

val takeNext = ("myString", 1).substring
val partialResult = takeNext(5)


// ------------------------------------------------------------
// Method calls require ()
// ------------------------------------------------------------

val ns = [1, 2, 3]
val n = ns.length()       // calls length(ns)
val getLen = ns.length    // partially applied, type () => Int32


// ------------------------------------------------------------
// Chaining
// ------------------------------------------------------------

val cleaned = "  hello  "
  .trim()
  .toUpper()


// ------------------------------------------------------------
// Destructuring val bindings
// ------------------------------------------------------------

val { "name": destructuredName, "age": destructuredAge } = person

val { name } = person

val { "name": displayPersonName } = person

val {
  "address": {
    "city": homeCity
  }
} = person

val [first, second] = ["a", "b"]

val [head, ...tail] = ["a", "b", "c"]
val { name: justName, ...remainingPersonFields } = person


// ------------------------------------------------------------
// Destructuring function parameters
// ------------------------------------------------------------

val describePerson = ({ name, age }: Person): String =>
  "${name} is ${age.toString()}"

val describeFirstTwo = ([first, second]: String[]): String =>
  "${first}, ${second}"


// ------------------------------------------------------------
// Arrays, map, filter, reduce
// ------------------------------------------------------------

val people: Person[] = [
  {
    "name": "Bob",
    "age": 42,
    "active": true,
    "address": { "line1": "1 Fish Street", "city": "Norwich" },
    "tags": ["admin"],
    "spouse": null
  },
  {
    "name": "Alice",
    "age": 37,
    "active": true,
    "address": { "line1": "2 Bird Road", "city": "Norwich" },
    "tags": ["customer"],
    "spouse": null
  }
]

val names = people
  .map(person => person["name"])

val adultPeople = people
  .filter(person => person["age"] >= 18)

val adultNames = people
  .filter(person => person["age"] >= 18)
  .map(person => person["name"])

val totalAge = people
  .map(person => person["age"])
  .reduce(0, (total, age) => total + age)


// ------------------------------------------------------------
// Iteration
// ------------------------------------------------------------

// range is a stdlib function returning an Iterator<Int32>

range(0, 10).for(i =>
  print(i)
)

// iter builds an opaque iterator from state-transition functions
// The first argument is a thunk so the iterator can be restarted

val list: String[] = ["a", "b", "c"]

val listIter: Iterator<String> = iter(
  () => 0,
  i => i < list.length(),
  i => i + 1,
  i => list[i]
)

listIter.for(item => print(item))

val ints: Int32[] = [1, 3, 5]
ints.for(num => print(num * 2)) // 2, 6, 10


// ------------------------------------------------------------
// Mutation and stateful closures
// ------------------------------------------------------------

val immutableCount = 0

var mutableCount = 0
mutableCount = mutableCount + 1

// Assignment expressions evaluate to the assigned value

val assignmentResult = mutableCount = mutableCount + 1

val makeCounter = (start: Int32) =>
  var count = start

  () =>
    count = count + 1
    count

val counter = makeCounter(0)

val one = counter()
val two = counter()
val three = counter()


// ------------------------------------------------------------
// Recursive functions
// ------------------------------------------------------------

val factorial = (n: Int32): Int32 =>
  if n == 0 then 1
  else n * factorial(n - 1)

val factorialFive = factorial(5)


// ------------------------------------------------------------
// Pattern matching with is
// ------------------------------------------------------------

val describeId = (id: String | Int64 | Null): String =>
  match id
    is Null =>
      "No id"

    is Int64 =>
      "Numeric id: ${id.toString()}"

    is String =>
      "String id: ${id}"

val describeSpecialName = (input: String | Null): String =>
  match input
    is Null =>
      "No name"

    is "Dave" =>
      "Big Dave!"

    is String =>
      "Name: ${input}"


// ------------------------------------------------------------
// Pattern matching with has
// ------------------------------------------------------------

val describeName = (input: String | Person | Null): String =>
  match input
    is Null =>
      "No name"

    is "Dave" =>
      "Big Dave!"

    has { name, age } when age > 30 =>
      "Old person: ${name}"

    has { name } =>
      "Young person: ${name}"

    is String =>
      "Name: ${input}"

// Catch-all with else

val describeLooseName = (input: Json): String =>
  match input
    has { name } =>
      "Name: ${name}"

    is Null =>
      "No value"

    is String =>
      "String: ${input}"

    else =>
      "Some other JSON value"


// ------------------------------------------------------------
// is vs has for object shape
// ------------------------------------------------------------

val exactNameOnly = (input: Json): String =>
  match input
    is { name } =>
      "Exactly one field called name: ${name}"

    has { name } =>
      "Has at least a name field: ${name}"

    is Null =>
      "Nothing"

    else =>
      "Other"


// ------------------------------------------------------------
// is vs has for arrays
// ------------------------------------------------------------

val describeArray = (items: String[]): String =>
  match items
    is [] =>
      "empty"

    is [one] =>
      "exactly one: ${one}"

    is [first, second] =>
      "exactly two: ${first}, ${second}"

    has [first, second, ...rest] =>
      "at least two"

    has [first] =>
      "at least one: ${first}"


// ------------------------------------------------------------
// Pattern guards with when
// ------------------------------------------------------------

val classifyPerson = (input: Json): String =>
  match input
    has { name, age } when age >= 100 =>
      "${name} is ancient"

    has { name, age } when age >= 18 =>
      "${name} is an adult"

    has { name, age } =>
      "${name} is a child"

    has { name } =>
      "${name} has no age"

    is Null =>
      "No person"

    else =>
      "Unrecognised"


// ------------------------------------------------------------
// Tagged unions and error handling as values
// ------------------------------------------------------------

val divide = (a: Float64, b: Float64): Result<Float64, String> =>
  if b == 0.0 then {
    "type": "failure",
    "error": "Cannot divide by zero"
  }
  else {
    "type": "success",
    "value": a / b
  }

val division = divide(10.0, 2.0)

val divisionMessage = match division
  has { "type": "success", value } =>
    "Result: ${value.toString()}"

  has { "type": "failure", error } =>
    "Failure: ${error}"

val parseAge = (input: String): Result<Int32, String> =>
  if input.isInt32() then {
    "type": "success",
    "value": input.toInt32()
  }
  else {
    "type": "failure",
    "error": "Invalid age"
  }

val parsedAge = parseAge("42")

val parsedAgeLabel = match parsedAge
  has { "type": "success", value } =>
    "Parsed age: ${value.toString()}"

  has { "type": "failure", error } =>
    "Failed: ${error}"


// ------------------------------------------------------------
// Recursive types
// ------------------------------------------------------------

type Tree = {
  "name": String,
  "children": Tree[]
}

val tree: Tree = {
  "name": "root",
  "children": [
    {
      "name": "child",
      "children": []
    }
  ]
}


// ------------------------------------------------------------
// Json interop
// ------------------------------------------------------------

val arbitraryJson: Json = {
  "type": "event",
  "payload": {
    "id": 123,
    "name": "created",
    "metadata": null
  }
}

val describeJson = (input: Json): String =>
  match input
    is Null =>
      "null"

    is String =>
      "string"

    is Boolean =>
      "boolean"

    is Int32 =>
      "integer"

    is Float64 =>
      "float"

    has [] =>
      "array"

    has {} =>
      "object"

    else =>
      "unknown"


// ------------------------------------------------------------
// Complete flow example
// ------------------------------------------------------------

export val processPerson = (input: Json): Result<String, String> =>
  match input
    has { name, age } when age >= 18 =>
      {
        "type": "success",
        "value": name.trim().toUpper()
      }

    has { name } =>
      {
        "type": "failure",
        "error": "${name} is not an adult"
      }

    is Null =>
      {
        "type": "failure",
        "error": "Missing person"
      }

    else =>
      {
        "type": "failure",
        "error": "Expected a person-like object"
      }

val processed = processPerson({
  "name": "  Bob  ",
  "age": 42
})

val processedMessage = match processed
  has { "type": "success", value } =>
    "Success: ${value}"

  has { "type": "failure", error } =>
    "Failure: ${error}"
```
