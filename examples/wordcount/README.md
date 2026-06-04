# wordcount

A tiny example of the typed **index-signature map** `{ String: T }` (the dictionary type — see
Specification §5.1.1 and ADR-082).

`count(words)` builds a `{ String: Int32 }` mapping each distinct word to its number of
occurrences. The map is a String-keyed dictionary with **O(1) average** lookup/insert (backed by the
hashed `LinMap` runtime container), distinct from a fixed-field record. Reading a key yields
`Int32 | Null`, so a first-seen word (`Null`) starts its count at 0.

```
lin run  examples/wordcount/main.lin
lin test examples/wordcount/
```

Expected output:

```
cat: 2
mat: 1
on: 1
ran: 1
sat: 1
the: 3
distinct words: 6
```
