# Changelog

All notable changes to Lin are documented here.
## [1.0.0](https://github.com/Lin-Language/Lin/releases/tag/v1.0.0) - 2026-06-21

### Bug Fixes

- Correct sealed-record field offsets in materialize-to-map (toBe crash)
- **sealed**: NKIND_FLOAT32 separate from NKIND_FLOAT64 for correct 4-byte field layout
- **diagnostics**: Attribute import type errors to the correct source file
- **codegen**: Share runtime tags via lin-common; fix Float32/64 tag + UInt64 signedness
- **check**: Honour numeric literal suffixes; large bare literals widen, not truncate

### Documentation

- Update stale ADR-062 references to ADR-069 post-reset
- Rationalise and renumber ADRs contiguously (ADR-001..060)
- Holistic spec rewrite (restructure + correct to match impl), remove stdlib index

### Features

- **check**: Name-preserving display for map-keyed type aliases
- **runtime**: Introduce TAG_RECORD (Stage 6a leg 1) — sealed-struct-by-pointer in dynamic slots
- **archive**: Add composable TarEntry handles — entries/header/body API
- **codegen,runtime**: Keep-packed sum value through record fields (TAG_SUMNODE)
- **parse**: Add additive full_span to compound Expr nodes
- **version**: Adopt single workspace version 1.0.0
- **types**: Typed index-signature object type `{ String: T }` (ADR-082)

### Other

- Merge branch 'master' into feat/mismatch-drilldown
- Wire+merge compiler-wiring, relocate url/os/result tests, add bignum/decimal tests (bignum/decimal blocked on foreign scalar-union compiler bug)
- Compiler
- Expand string stdlib with contains, startsWith, endsWith, split, join, replace
- Implement lin-lang interpreter with stdlib and test suite

### Performance

- **check**: Seal single-pointer union fields in records (interp Cursor fix)

### Refactor

- **tags**: Single-source nkind→size table in lin-common, eliminate dual derivations
