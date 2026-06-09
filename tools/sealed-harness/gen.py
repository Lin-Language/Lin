#!/usr/bin/env python3
"""Generate the sealed-record representation matrix (ADR-063 Phase 1).

Emits one .lin program per (operation x position x field-shape) cell into the output dir.
Each program runs the operation in a build/drop loop N times and prints a single integer
(the harness checks it against the cell's `expect`). N is templated as @N@ so run.sh can
instantiate two loop counts for the leak-scaling test.
"""
import sys, os

# ---- field shapes: (type decls, literal for an element with seed i, a scalar-field read expr) ----
# Each shape defines a record type `T` (and any helper types). `lit(i)` builds an instance whose
# scalar field `k` == i (so reads are checkable). `read(e)` extracts that Int32 from element expr e.
SHAPES = {
    "scalar":        (['type T = { "a": Int32, "b": Int32 }'],
                      lambda i: '{ "a": %s, "b": 0 }' % i, lambda e: '%s["a"]' % e),
    "string":        (['type T = { "id": String, "a": Int32 }'],
                      lambda i: '{ "id": "x", "a": %s }' % i, lambda e: '%s["a"]' % e),
    "scalar_array":  (['type T = { "xs": Int32[], "a": Int32 }'],
                      lambda i: '{ "xs": [%s], "a": %s }' % (i, i), lambda e: '%s["a"]' % e),
    "record_array":  (['type Leg = { "d": Int32 }', 'type T = { "legs": Leg[], "a": Int32 }'],
                      lambda i: '{ "legs": [{ "d": %s }], "a": %s }' % (i, i), lambda e: '%s["a"]' % e),
    "nested_record": (['type Inner = { "v": Int32 }', 'type T = { "inner": Inner, "a": Int32 }'],
                      lambda i: '{ "inner": { "v": %s }, "a": %s }' % (i, i), lambda e: '%s["a"]' % e),
}

PRELUDE = '''import { print } from "std/io"
import { toString } from "std/string"
import { push, length, sort } from "std/array"
import { for, range, map, filter } from "std/iter"
'''

# ---- operations: each returns (body-of-`once(i)`, expect-per-call). `once` returns an Int32. ----
# `once(i)` must build the value(s) using lit(i)/decls, exercise the operation, and return a small
# Int32 deterministic in i; the loop sums once(i) over i in 0..N (mod nothing — kept small).
# `expect` is a python fn of N giving the loop's printed total.

def op_build_read(s):
    lit, read = s[1], s[2]
    body = (f'  val t: T = {lit("i")}\n'
            f'  {read("t")}\n')
    return body, lambda N: sum(range(N))  # reads back `a`==i

def op_factory_return(s):
    lit, read = s[1], s[2]
    # a factory fn returns the record literal directly (the return-position path)
    extra = f'val mk = (i: Int32): T => {lit("i")}\n'
    body = (f'  val t: T = mk(i)\n'
            f'  {read("t")}\n')
    return body, (lambda N: sum(range(N))), extra

def op_push_read(s):
    lit, read = s[1], s[2]
    body = (f'  var ts: T[] = []\n'
            f'  push(ts, {lit("i")})\n'
            f'  {read("ts[0]")}\n')
    return body, lambda N: sum(range(N))

def op_index_set(s):
    lit, read = s[1], s[2]
    body = (f'  var ts: T[] = []\n'
            f'  push(ts, {lit("0")})\n'
            f'  ts[0] = {lit("i")}\n'
            f'  {read("ts[0]")}\n')
    return body, lambda N: sum(range(N))

def op_sort(s):
    lit, read = s[1], s[2]
    body = (f'  var ts: T[] = []\n'
            f'  push(ts, {lit("i")})\n'
            f'  push(ts, {lit("0")})\n'
            f'  val sorted: T[] = sort(ts, (x, y) => x["a"] - y["a"])\n'
            f'  sorted[length(sorted) - 1]["a"]\n')  # max == i
    return body, lambda N: sum(range(N))

def op_array_drop(s):
    lit, read = s[1], s[2]
    body = (f'  var ts: T[] = []\n'
            f'  push(ts, {lit("i")})\n'
            f'  push(ts, {lit("i")})\n'
            f'  length(ts)\n')   # 2 each; drop happens at scope exit
    return body, lambda N: 2 * N

def op_tail_thread(s):
    # the scanRouteAt shape: a T|Null threaded through a tail-recursive param, fed by arr[i]
    lit, read = s[1], s[2]
    extra = ('val scan = (arr: Json, j: Int32, n: Int32, cur: T | Null): Int32 =>\n'
             '  if j >= n then\n'
             '    match cur\n'
             '      is T => cur["a"]\n'
             '      else => -1\n'
             '  else\n'
             '    val nx: T = arr[j]\n'
             '    scan(arr, j + 1, n, nx)\n')
    body = (f'  var arr: Json = []\n'
            f'  push(arr, {lit("i")})\n'
            f'  scan(arr, 0, 1, null)\n')
    return body, (lambda N: sum(range(N))), extra

def op_map_field(s):
    lit, read = s[1], s[2]
    body = (f'  var ts: T[] = []\n'
            f'  push(ts, {lit("i")})\n'
            f'  val ds: Int32[] = map(ts, (x) => x["a"])\n'
            f'  ds[0]\n')
    return body, lambda N: sum(range(N))

def op_for_field(s):
    # PATH-1 in-place packed iteration: `for` over a packed T[] reading a scalar field each
    # iteration into a captured `var`. Exercises the const-offset packed-element read path
    # (`try_lower_packed_elem_field`) AND its per-iteration RC (the element is NOT materialized, so
    # there must be NO per-iteration leak — a sound in-place read allocates nothing per element).
    lit, read = s[1], s[2]
    body = (f'  var ts: T[] = []\n'
            f'  push(ts, {lit("i")})\n'
            f'  push(ts, {lit("0")})\n'
            f'  var acc: Int32 = 0\n'
            f'  ts.for(p => acc = acc + {read("p")})\n'
            f'  acc\n')   # i + 0 == i
    return body, lambda N: sum(range(N))

OPS = {
    "build_read": op_build_read,
    "factory_return": op_factory_return,
    "push_read": op_push_read,
    "index_set": op_index_set,
    "sort": op_sort,
    "array_drop": op_array_drop,
    "tail_thread": op_tail_thread,
    "map_field": op_map_field,
    "for_field": op_for_field,
}

def main():
    outdir = sys.argv[1]
    os.makedirs(outdir, exist_ok=True)
    manifest = []
    for sname, shape in SHAPES.items():
        decls = shape[0]
        for oname, opfn in OPS.items():
            res = opfn(shape)
            body, expect = res[0], res[1]
            extra = res[2] if len(res) > 2 else ""
            N_total = expect(0)  # placeholder; real expected computed at two Ns in run.sh via formula tag
            prog = PRELUDE + "\n".join(decls) + "\n" + extra
            prog += (
                "val once = (i: Int32): Int32 =>\n" + body +
                "val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>\n"
                "  if i >= n then acc\n"
                "  else loop(i + 1, n, acc + once(i))\n"
                'print(toString(loop(0, @N@, 0)))\n'
            )
            # expected formula: encode which closed form (sum 0..N-1, or 2*N)
            if oname == "array_drop":
                formula = "2*N"
            else:
                formula = "sum"  # sum_{i<N} i = N*(N-1)/2
            name = f"{sname}__{oname}"
            with open(os.path.join(outdir, name + ".lin.tmpl"), "w") as f:
                f.write(prog)
            manifest.append(f"{name}\t{formula}")
    with open(os.path.join(outdir, "MANIFEST.tsv"), "w") as f:
        f.write("\n".join(manifest) + "\n")
    print(f"generated {len(manifest)} cells into {outdir}")

if __name__ == "__main__":
    main()
