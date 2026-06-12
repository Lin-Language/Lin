# Changelog

All notable changes to Lin are documented here.
## [1.0.0](https://github.com/Lin-Language/Lin/releases/tag/v1.0.0) - 2026-06-12

### Bug Fixes

- **codegen**: Share runtime tags via lin-common; fix Float32/64 tag + UInt64 signedness
- **check**: Honour numeric literal suffixes; large bare literals widen, not truncate

### Documentation

- Rationalise and renumber ADRs contiguously (ADR-001..060)
- Holistic spec rewrite (restructure + correct to match impl), remove stdlib index

### Features

- **archive**: Add composable TarEntry handles — entries/header/body API
- **codegen,runtime**: Keep-packed sum value through record fields (TAG_SUMNODE)
- **parse**: Add additive full_span to compound Expr nodes
- **version**: Adopt single workspace version 1.0.0
- **types**: Typed index-signature object type `{ String: T }` (ADR-082)

### Other

- Wire+merge compiler-wiring, relocate url/os/result tests, add bignum/decimal tests (bignum/decimal blocked on foreign scalar-union compiler bug)
- Compiler
- Expand string stdlib with contains, startsWith, endsWith, split, join, replace
- Implement lin-lang interpreter with stdlib and test suite
