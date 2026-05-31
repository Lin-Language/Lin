# matrix — 3×3 matrix & 2D vector math

A small linear-algebra demo: 3×3 matrix multiply, 2D rotation matrices, applying
a transform to a point, and 2D vector dot product / magnitude / angle. Matrices
and vectors are stored as **flat `Float64[]` arrays** (a 3×3 matrix is 9 elements,
row-major: `m[i*3 + j]`).

## What it demonstrates

- **Flat `Float64[]` storage** for matrices and vectors, indexed arithmetically.
- `Float64` arithmetic and `std/math` (`sin`, `cos`, `atan2`, `sqrt`, `PI`, `toFixed`).
- Typed array parameters/returns throughout (`mat3`, `matMul`, `rotation2D`).
- A **named record alias** `Point = { "x": Float64, "y": Float64 }` typing the
  result of `applyToPoint`.
- Iteration with `range`/`for` and interpolated, fixed-precision output.

## Structure

| File | What it is |
| --- | --- |
| `matrix.lin` | `mat3`, `mat3Get`, `matMul`, `rotation2D`, `applyToPoint(): Point`. |
| `vector.lin` | `dot2`, `magnitude2`, `angleBetween` over 2D `Float64[]` vectors. |
| `main.lin` | Multiply, rotate, dot/magnitude, angle, and trig spot-checks. |
| `matrix.test.lin` | Matrix multiply and rotation assertions. |
| `vector.test.lin` | Dot product, magnitude, and angle assertions. |

## Run / Test

```sh
lin run examples/matrix/main.lin
lin test examples/matrix/
```
