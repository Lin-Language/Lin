use std::alloc::{alloc, realloc, Layout};
use crate::string::LinString;
use crate::tagged::TaggedVal;

/// Dynamic object (Json-typed) represented as an array of key-value pairs.
/// Layout: refcount (u32) | len (u32) | cap (u32) | flags (u32) | entries (*mut LinObjectEntry)
///
/// SINGLE-ALLOCATION optimization: a freshly-`alloc`'d object places its entries buffer
/// *immediately after the header in the same allocation* (`entries` points just past the
/// header) and sets `FLAG_INLINE`. This halves the per-object allocator traffic (1 malloc/free
/// instead of header + separate entries) and co-locates header + entries on the same cache line
/// — a big win for record-heavy code that builds millions of small objects. The `entries`
/// pointer field is RETAINED (not removed), so every reader/writer is unchanged: they keep
/// loading `(*obj).entries` and indexing it. On the rare GROW, an inline object MIGRATES its
/// entries to a separate heap buffer (clearing `FLAG_INLINE`); the *header* never moves, so
/// shared `LinObject*` holders stay valid (they already re-read `(*obj).entries` each access).
#[repr(C)]
pub struct LinObject {
    pub refcount: u32,        // @0   unchanged (codegen never touches)
    pub len: u32,             // @4   unchanged (codegen writes this)
    pub cap: u32,             // @8   unchanged
    flags: u32,               // @12  unchanged
    pub entries: *mut LinObjectEntry, // @16 unchanged (codegen reads this)
    // ── O(1)-lookup hash side-index (RAPTOR #4b) ──────────────────────────────────────────
    // ALL new fields live at offset >= 24, which the codegen inline `MakeObject` path never
    // reads or writes (audited: it touches only len@4, entries@16, and the 24-byte entries).
    // The index is OPTIONAL and LAZY: small objects (len < HASH_INDEX_THRESHOLD) never build it
    // and keep the byte-for-byte linear-scan code path. The codegen inline-literal path builds
    // large literals with `index == null`, so the lazy build MUST key off `index.is_null() ||
    // index_dirty != 0` — never assume any constructor maintained the index.
    index: *mut u32,          // @24  open-addressing table of (entry_slot+1); 0 = empty; null = none
    index_cap: u32,           // @32  power-of-two table size, or 0 when `index` is null
    index_dirty: u32,         // @36  set when entries changed without index maintenance
}

/// `flags` bit: the entries buffer is inline (part of the header allocation), so it must NOT be
/// freed separately and a grow must migrate it to a heap buffer first.
const FLAG_INLINE: u32 = 1;

/// Objects with at least this many entries get a lazily-built open-addressing hash index for
/// O(1)-average key lookup. Below this, the linear scan is faster (no hashing, no allocation)
/// and the layout/code paths are byte-for-byte unchanged. RAPTOR #4b.
const HASH_INDEX_THRESHOLD: u32 = 16;

#[repr(C)]
pub struct LinObjectEntry {
    pub key: *mut LinString,
    pub value: TaggedVal,
}

/// Layout for a one-block object: the `LinObject` header followed by `cap` inline entries.
unsafe fn inline_object_layout(cap: u32) -> Layout {
    let size = std::mem::size_of::<LinObject>()
        + std::mem::size_of::<LinObjectEntry>() * cap as usize;
    // Both types are 8-aligned; the header size is a multiple of the entry alignment so the
    // inline entries start correctly aligned right after it.
    let align = std::mem::align_of::<LinObject>().max(std::mem::align_of::<LinObjectEntry>());
    Layout::from_size_align_unchecked(size, align)
}

unsafe fn object_layout() -> Layout {
    Layout::from_size_align_unchecked(
        std::mem::size_of::<LinObject>(),
        std::mem::align_of::<LinObject>(),
    )
}

unsafe fn entries_layout(cap: u32) -> Layout {
    Layout::from_size_align_unchecked(
        std::mem::size_of::<LinObjectEntry>() * cap as usize,
        std::mem::align_of::<LinObjectEntry>(),
    )
}

/// Pointer to the inline entries region (immediately past the header).
#[inline]
unsafe fn inline_entries_ptr(obj: *mut LinObject) -> *mut LinObjectEntry {
    (obj as *mut u8).add(std::mem::size_of::<LinObject>()) as *mut LinObjectEntry
}

/// Migrate an inline-entries object to a separately-heap-allocated entries buffer of `new_cap`
/// (copying the existing `len` entries by raw bytes — ownership of keys/values moves with them).
/// Clears `FLAG_INLINE`. Used on the first grow of an inline object. The header does not move.
#[inline]
unsafe fn migrate_inline_to_heap(obj: *mut LinObject, new_cap: u32) {
    let len = (*obj).len as usize;
    let buf = alloc(entries_layout(new_cap)) as *mut LinObjectEntry;
    if len > 0 {
        std::ptr::copy_nonoverlapping(inline_entries_ptr(obj), buf, len);
    }
    (*obj).entries = buf;
    (*obj).cap = new_cap;
    (*obj).flags &= !FLAG_INLINE;
}

// ── Hash side-index (RAPTOR #4b) ──────────────────────────────────────────────────────────
//
// An open-addressing table mapping `hash(key bytes) -> entry_slot + 1` (0 means empty). It is
// a pure ACCELERATOR over the existing `entries` association list: `entries` remains the source
// of truth (insertion order, ownership, equality, keys/values all read it directly). The index
// stores only u32 slot indices — no refcounted pointers — so it can never cause a UAF / double
// free; the worst a bug here can do is point at the wrong slot, which is why every probe HIT is
// reconfirmed with `lin_string_key_eq` before it is trusted, and the fuzz/oracle test asserts
// agreement with a linear scan on every key including absent ones.

/// FNV-1a over the key bytes. Cheap, good enough distribution for short string keys.
#[inline]
unsafe fn hash_key(key: *const LinString) -> u64 {
    if key.is_null() {
        return 0;
    }
    let len = (*key).len as usize;
    let data = (*key).data.as_ptr();
    let bytes = std::slice::from_raw_parts(data, len);
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

unsafe fn index_table_layout(cap: u32) -> Layout {
    Layout::from_size_align_unchecked(
        std::mem::size_of::<u32>() * cap as usize,
        std::mem::align_of::<u32>(),
    )
}

/// Free the index table (if any) and reset the index fields to "none". Called on release and
/// whenever the table must be discarded (it is cheap to rebuild lazily from `entries`).
#[inline]
unsafe fn free_index(obj: *mut LinObject) {
    if !(*obj).index.is_null() {
        std::alloc::dealloc((*obj).index as *mut u8, index_table_layout((*obj).index_cap));
        (*obj).index = std::ptr::null_mut();
        (*obj).index_cap = 0;
    }
    (*obj).index_dirty = 0;
}

/// Choose a power-of-two table capacity giving a load factor <= ~0.7 for `len` live entries
/// (with headroom so a few post-build appends don't immediately force a rebuild).
#[inline]
fn index_cap_for(len: u32) -> u32 {
    // Target capacity ~= 2 * len, rounded up to a power of two, min 16.
    let want = (len as u64).saturating_mul(2).max(16);
    let mut cap: u32 = 16;
    while (cap as u64) < want {
        cap <<= 1;
    }
    cap
}

/// Insert `slot` (an entry index) into the open-addressing table for its key. Assumes the table
/// has room (load factor kept < 1 by `index_cap_for`). Linear probing on a power-of-two table.
#[inline]
unsafe fn index_insert(table: *mut u32, cap: u32, key: *const LinString, slot: u32) {
    let mask = (cap - 1) as u64;
    let mut i = hash_key(key) & mask;
    loop {
        let cell = table.add(i as usize);
        if *cell == 0 {
            *cell = slot + 1;
            return;
        }
        i = (i + 1) & mask;
    }
}

/// (Re)build the hash index from the current `entries`. One O(n) pass. Frees any prior table.
unsafe fn rebuild_index(obj: *mut LinObject) {
    let len = (*obj).len;
    free_index(obj);
    let cap = index_cap_for(len);
    let table = alloc(index_table_layout(cap)) as *mut u32;
    // alloc does not zero — explicitly clear (0 = empty).
    std::ptr::write_bytes(table, 0, cap as usize);
    for slot in 0..len {
        let entry = (*obj).entries.add(slot as usize);
        index_insert(table, cap, (*entry).key, slot);
    }
    (*obj).index = table;
    (*obj).index_cap = cap;
    (*obj).index_dirty = 0;
}

/// Probe the index for `key`. Returns the matching entry slot, or `u32::MAX` if absent.
/// Caller must have ensured the index is built and clean. Reconfirms each probe hit with
/// `lin_string_key_eq` (the index stores hash-derived positions; collisions must be verified).
#[inline]
unsafe fn index_probe(obj: *const LinObject, key: *const LinString) -> u32 {
    let table = (*obj).index;
    let cap = (*obj).index_cap;
    let mask = (cap - 1) as u64;
    let mut i = hash_key(key) & mask;
    loop {
        let cell = *table.add(i as usize);
        if cell == 0 {
            return u32::MAX; // empty slot ⇒ key not present
        }
        let slot = cell - 1;
        let entry = (*obj).entries.add(slot as usize);
        if lin_string_key_eq((*entry).key, key) {
            return slot;
        }
        i = (i + 1) & mask;
    }
}

/// Ensure a usable index exists for a lookup: build (or rebuild if dirty) when the object is at
/// or above the threshold. `obj` is logically const for a lookup, but the lazy build mutates the
/// index fields (which are not observable through the assoc-list API), so we take `*mut`.
/// Returns true if the index is now usable (so the caller should probe), false to fall back to
/// the linear scan (small object, or a zero-length table edge case).
///
/// THREAD-SAFETY (frozen objects): a `frozen(v)` graph has `refcount >= IMMORTAL_RC` and is
/// designed for lock-free concurrent reads across threads (ADR-043). The lazy build MUTATES the
/// object's index fields (`rebuild_index` allocs + writes `index`/`index_cap`/`index_dirty`), so
/// two threads calling a lookup (`get`/`has`/`eq`) on the SAME frozen object would race — torn
/// index pointer, double-alloc leak, probing a half-built table. We therefore NEVER build (mutate)
/// a frozen object here: a frozen object is only probed if its index was already built CLEANLY at
/// freeze time (`freeze_object` calls `build_index_for_freeze` single-threaded; the immortal guard
/// then ensures it is never rebuilt or freed). A frozen object with no/dirty index falls back to
/// the (read-only, race-free) linear scan. Non-frozen objects keep the lazy build as before.
#[inline]
unsafe fn ensure_index(obj: *mut LinObject) -> bool {
    if (*obj).len < HASH_INDEX_THRESHOLD {
        return false;
    }
    let clean = !(*obj).index.is_null() && (*obj).index_dirty == 0;
    if clean {
        return true;
    }
    // Index is null or dirty: a build would MUTATE the object. Refuse on a frozen (immortal)
    // object — concurrent readers must never trigger a write. Caller falls back to linear scan.
    if (*obj).refcount >= crate::string::IMMORTAL_RC {
        return false;
    }
    rebuild_index(obj);
    !(*obj).index.is_null()
}

/// Build the hash index for a LARGE object AT FREEZE TIME (single-threaded). After this, all
/// threads can probe the immutable, clean index lock-free; the immortal-RC guard in `ensure_index`
/// guarantees it is never rebuilt or freed. Below the threshold, do nothing (the linear scan is
/// used and is itself read-only/race-free). Called from `frozen::freeze_object`. The caller has
/// already (or is about to) set `refcount = IMMORTAL_RC`; this only writes the index cache fields.
pub(crate) unsafe fn build_index_for_freeze(obj: *mut LinObject) {
    if obj.is_null() || (*obj).len < HASH_INDEX_THRESHOLD {
        return;
    }
    rebuild_index(obj);
}

#[no_mangle]
pub unsafe extern "C" fn lin_object_alloc(initial_cap: u32) -> *mut LinObject {
    // Honor the caller's exact capacity hint (codegen passes the literal's field count for the
    // no-spread case, so a 3-field literal allocates 3 entries). Keep a minimum of 1 so the
    // entries region is never zero-size and the grow path's `cap * 2` always makes progress.
    //
    // SINGLE ALLOCATION: header + `cap` entries in one block; `entries` points just past the
    // header; FLAG_INLINE set. One malloc instead of two, header+entries on one cache line.
    let cap = initial_cap.max(1);
    let ptr = alloc(inline_object_layout(cap)) as *mut LinObject;
    (*ptr).refcount = 1;
    (*ptr).len = 0;
    (*ptr).cap = cap;
    (*ptr).flags = FLAG_INLINE;
    (*ptr).entries = inline_entries_ptr(ptr);
    // No hash index yet — built lazily on the first lookup past the threshold.
    (*ptr).index = std::ptr::null_mut();
    (*ptr).index_cap = 0;
    (*ptr).index_dirty = 0;
    ptr
}

/// Release the heap-allocated payload of a TaggedVal (decrement refcount / free).
/// Does NOT free the TaggedVal box itself (used for inline-stored entries).
unsafe fn release_tagged_payload(tv: &TaggedVal) {
    use crate::tagged::*;
    let payload = tv.payload;
    match tv.tag {
        TAG_STR => crate::string::lin_string_release(payload as *mut crate::string::LinString),
        TAG_ARRAY => crate::array::lin_array_release(payload as *mut crate::array::LinArray),
        TAG_OBJECT => lin_object_release(payload as *mut LinObject),
        TAG_MAP => crate::map::lin_map_release(payload as *mut crate::map::LinMap),
        // KEEP-PACKED sum node in a record/object FIELD slot (keep-packed-through-record-fields):
        // a `*SumNode` wrapped by-pointer. Release it via the SumNode self-release, NOT
        // lin_object_release. This is hit when the OWNING record drops and walks its field payloads.
        TAG_SUMNODE => crate::sumnode::lin_sumnode_release_self(payload as *mut u8),
        TAG_FUNCTION => crate::memory::lin_closure_release(payload as *mut u8),
        TAG_SHARED => crate::shared::lin_shared_release_box(payload as *const u8),
        TAG_STREAM => crate::stream::lin_stream_release_box(payload as *const u8),
        TAG_BIGNUM => crate::bignum::lin_bignum_release_box(payload as *const u8),
        TAG_DECIMAL => crate::decimal::lin_decimal_release_box(payload as *const u8),
        _ => {}
    }
}

/// Retain the heap-allocated payload of a TaggedVal (increment refcount).
/// Used when copying a TaggedVal into an object/array slot so the new owner has a reference.
unsafe fn retain_tagged_payload(tv: &TaggedVal) {
    use crate::tagged::*;
    let payload = tv.payload;
    match tv.tag {
        TAG_STR => {
            // inc_ref leaves interned literals (saturated refcount) untouched; ordinary strings
            // are bumped as before.
            crate::string::lin_string_inc_ref(payload as *mut crate::string::LinString);
        }
        TAG_ARRAY => {
            let a = payload as *mut crate::array::LinArray;
            // Skip frozen (immortal) arrays — their refcount must never be written (read-only,
            // shared across threads). Mirror of the string immortal guard.
            if !a.is_null() && (*a).refcount < crate::string::IMMORTAL_RC { (*a).refcount += 1; }
        }
        TAG_OBJECT => {
            let o = payload as *mut LinObject;
            if !o.is_null() && (*o).refcount < crate::string::IMMORTAL_RC { (*o).refcount += 1; }
        }
        TAG_MAP => {
            let m = payload as *mut crate::map::LinMap;
            if !m.is_null() && (*m).refcount < crate::string::IMMORTAL_RC { (*m).refcount += 1; }
        }
        TAG_SUMNODE => {
            // KEEP-PACKED sum node: offset-0 u32 refcount, immortal-guarded — same shape as a
            // sealed record header, so the generic rc bump applies. The matching release is the
            // TAG_SUMNODE arm of release_tagged_payload.
            let s = payload as *mut u32;
            if !s.is_null() && *s < crate::string::IMMORTAL_RC { *s += 1; }
        }
        TAG_FUNCTION => {
            // Closure refcount lives at offset 0 (u32). Mirror of the TAG_FUNCTION arm in
            // release_tagged_payload (which calls lin_closure_release). Without this retain, a
            // closure stored into an object/array field via the tagged-payload path was NOT
            // refcounted by its new owner, so when the constructing frame released its own ref the
            // closure (and its captured-var cell) was freed while the escaping object still held it
            // — a use-after-free (segfault / garbage read on a captured var). See the object-literal
            // -field closure-capture and worker-captured-var bugs.
            let c = payload as *mut u32;
            if !c.is_null() {
                crate::memory::lin_rc_retain(c);
            }
        }
        TAG_SHARED => {
            // Atomic refcount on the Shared box (cross-thread-shared).
            crate::shared::lin_shared_retain_box(payload as *const u8);
        }
        TAG_STREAM => {
            // Refcount on the Stream box; the matching release runs the auto-close finalizer.
            crate::stream::lin_stream_retain_box(payload as *const u8);
        }
        // Opaque bignum/decimal handles: bump the box's atomic refcount (mirror of the
        // TAG_BIGNUM/TAG_DECIMAL release arms). Without this a handle stored into an object/array
        // slot would be freed while the container still held it (UAF).
        TAG_BIGNUM => {
            crate::bignum::lin_bignum_retain_box(payload as *const u8);
        }
        TAG_DECIMAL => {
            crate::decimal::lin_decimal_retain_box(payload as *const u8);
        }
        _ => {} // scalars: no heap payload
    }
}

/// Public wrapper for retain_tagged_payload, used by array.rs when pushing tagged values.
pub unsafe fn retain_tagged_payload_pub(tv: &TaggedVal) {
    retain_tagged_payload(tv);
}

/// Public wrapper for release_tagged_payload, used by map.rs (the typed-map container reuses the
/// exact object value RC discipline; see ADR-055).
pub unsafe fn release_tagged_payload_pub(tv: &TaggedVal) {
    release_tagged_payload(tv);
}

/// Retain the heap payload of a boxed TaggedVal* (tag-aware). The codegen `Retain`
/// instruction can't use `lin_rc_retain` on a TaggedVal* — offset 0 is the tag byte, not a
/// refcount, so that would corrupt the tag. This reads the tag and bumps the inner value's
/// refcount, the mirror of the tag-aware `lin_tagged_release`. Null-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_retain(p: *const u8) {
    if p.is_null() {
        return;
    }
    retain_tagged_payload(&*(p as *const TaggedVal));
}

/// Clone a boxed TaggedVal*: allocate a FRESH TaggedVal box copying the tag+payload and
/// retain the inner heap payload (if any). Returns an independently-owned box that can be
/// released with `lin_tagged_release` without affecting the source box.
///
/// This is the union analogue of `lin_rc_retain` for the OWNING var-cell/global model: a
/// cell/global holding a Json/union value owns its OWN box (not an alias of a borrowed
/// caller box). Storing clones the incoming box; reading clones the cell's box; the cell's
/// release-old and the read's scope-exit release each free a box they exclusively own. This
/// keeps store/read/release-old/teardown perfectly symmetric (mirroring the concrete-rc
/// retain/release pairs) and never frees a box owned by someone else.
///
/// Null-safe (null Json → null box). Cached scalar boxes (small ints, bools) are returned
/// as-is: they are immutable statics, carry no heap payload, and `lin_tagged_release`
/// no-ops on them, so an alias is safe and avoids a needless allocation.
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_clone(p: *const u8) -> *mut u8 {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    if crate::tagged::is_cached_box_pub(p) {
        return p as *mut u8;
    }
    let src = &*(p as *const TaggedVal);
    retain_tagged_payload(src);
    crate::tagged::alloc_tagged(src.tag, src.payload)
}

/// Set a field. Key must be a LinString*. Value is a TaggedVal* (pointer to tagged payload).
/// Copies the 16-byte TaggedVal struct and retains the inner value (the object owns a reference).
/// Takes ownership of the key reference (caller must not release it — use lin_object_keys'
/// retained references or freshly-allocated strings).
#[no_mangle]
pub unsafe extern "C" fn lin_object_set(obj: *mut LinObject, key: *mut LinString, val: *const TaggedVal) {
    use crate::tagged::TAG_NULL;
    let null_tv = TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 };
    // Null pointer represents the null Json value — use a local null TaggedVal instead.
    let val_ref: &TaggedVal = if val.is_null() { &null_tv } else { &*val };
    let len = (*obj).len;
    // Check if key already exists — via the O(1) index when available, else a linear scan.
    // (Using the index here is what turns a build-by-repeated-`set` from O(n²) into O(n):
    // the dup-check is otherwise the second linear scan per insert.)
    let existing_slot = if ensure_index(obj) {
        index_probe(obj, key)
    } else {
        let mut found = u32::MAX;
        for i in 0..len {
            let entry = (*obj).entries.add(i as usize);
            if lin_string_key_eq((*entry).key, key) {
                found = i;
                break;
            }
        }
        found
    };
    if existing_slot != u32::MAX {
        // Update existing entry: release old value, copy new, retain new.
        // The caller is responsible for releasing the new key they passed in. The index
        // is unaffected (same key, same slot).
        let entry = (*obj).entries.add(existing_slot as usize);
        release_tagged_payload(&(*entry).value);
        std::ptr::copy_nonoverlapping(val_ref, &mut (*entry).value, 1);
        retain_tagged_payload(val_ref);
        return;
    }
    // New key — grow if needed.
    let cap = (*obj).cap;
    if len == cap {
        let new_cap = cap * 2;
        if (*obj).flags & FLAG_INLINE != 0 {
            // Inline entries can't be realloc'd in place (they're part of the header block, and
            // the header must not move). Migrate to a separate heap buffer of the new capacity.
            migrate_inline_to_heap(obj, new_cap);
        } else {
            let old_layout = entries_layout(cap);
            let new_layout = entries_layout(new_cap);
            (*obj).entries = realloc((*obj).entries as *mut u8, old_layout, new_layout.size()) as *mut LinObjectEntry;
            (*obj).cap = new_cap;
        }
    }
    let slot = (*obj).entries.add(len as usize);
    // Retain the key: the object owns one reference.
    // Caller retains their own reference and must release it separately.
    // inc_ref is a no-op for interned literal keys (saturated refcount).
    crate::string::lin_string_inc_ref(key);
    (*slot).key = key;
    std::ptr::copy_nonoverlapping(val_ref, &mut (*slot).value, 1);
    // Retain the value's inner payload — the object now owns a reference.
    retain_tagged_payload(val_ref);
    (*obj).len = len + 1;
    // Maintain the hash index for the appended slot (no-op if no index is built yet — the
    // first lookup past the threshold will build it from scratch). Slot indices are stable
    // across an entries realloc (only the buffer base moves), so the table survives a grow.
    index_after_append(obj, key, len);
}

/// Maintain the hash index after appending an entry at `slot` with key `key`. If no index is
/// built, do nothing (it will be built lazily). If the appended slot would push the load factor
/// too high, mark the index dirty so the next lookup rebuilds a larger table; otherwise insert
/// the single new slot in O(1).
#[inline]
unsafe fn index_after_append(obj: *mut LinObject, key: *const LinString, slot: u32) {
    if (*obj).index.is_null() {
        return;
    }
    if (*obj).index_dirty != 0 {
        return; // already scheduled for rebuild
    }
    // Keep load factor < ~0.7: rebuild (lazily) once live count exceeds 0.7 * cap.
    let live = slot + 1; // new len
    if (live as u64) * 10 >= (*obj).index_cap as u64 * 7 {
        (*obj).index_dirty = 1;
        return;
    }
    index_insert((*obj).index, (*obj).index_cap, key, slot);
}

/// Append a field for a statically-known-distinct key, with the SAME ownership semantics as
/// `lin_object_set`'s append branch but WITHOUT the linear dup-check scan.
///
/// Used only for object-literal construction (codegen `MakeObject`), where the keys appended
/// through this path are guaranteed distinct: the codegen de-duplicates literal keys (last
/// wins) before emitting, and this path is only used when the literal has no spreads (so a
/// field cannot collide with a spread-provided key). For genuine dynamic `obj[k]=v`, spreads,
/// and merges, `lin_object_set` (which dup-checks/overwrites) is still used.
///
/// Ownership (must stay identical to `lin_object_set`'s append branch so RC stays balanced —
/// the IR lowering accounts for object_set RETAINING the value's inner payload):
///   - the key is `inc_ref`'d (no-op for interned literal keys with a saturated refcount); the
///     object owns one reference, the caller keeps and releases their own;
///   - the 16-byte TaggedVal is copied into the slot and its inner heap payload retained, so the
///     object owns its own reference to the value (the source box keeps its own).
/// A null `val` pointer is treated as the null Json value (matching `lin_object_set`).
#[no_mangle]
pub unsafe extern "C" fn lin_object_set_fresh(obj: *mut LinObject, key: *mut LinString, val: *const TaggedVal) {
    use crate::tagged::TAG_NULL;
    let null_tv = TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 };
    let val_ref: &TaggedVal = if val.is_null() { &null_tv } else { &*val };
    let len = (*obj).len;
    let cap = (*obj).cap;
    if len == cap {
        let new_cap = if cap == 0 { 1 } else { cap * 2 };
        if (*obj).flags & FLAG_INLINE != 0 {
            migrate_inline_to_heap(obj, new_cap);
        } else {
            let old_layout = entries_layout(cap);
            let new_layout = entries_layout(new_cap);
            (*obj).entries = realloc((*obj).entries as *mut u8, old_layout, new_layout.size()) as *mut LinObjectEntry;
            (*obj).cap = new_cap;
        }
    }
    let slot = (*obj).entries.add(len as usize);
    // inc_ref is a no-op for interned literal keys (saturated refcount).
    crate::string::lin_string_inc_ref(key);
    (*slot).key = key;
    std::ptr::copy_nonoverlapping(val_ref, &mut (*slot).value, 1);
    // Retain the value's inner payload — the object now owns a reference.
    retain_tagged_payload(val_ref);
    (*obj).len = len + 1;
    // Maintain the index if one was lazily built mid-construction (rare for this path, which
    // is mostly used by the literal-construction fast path before any lookup happens).
    index_after_append(obj, key, len);
}

/// Append an entry taking OWNERSHIP of an already-owned `key` and `value` (no retain). The
/// caller must not release either afterwards. Assumes `key` does not already exist (used by
/// the thread-transfer deep-copy path, which builds a fresh object from distinct keys). Grows
/// the entry buffer as needed.
pub unsafe fn object_push_owned(obj: *mut LinObject, key: *mut LinString, value: TaggedVal) {
    let len = (*obj).len;
    let cap = (*obj).cap;
    if len == cap {
        let new_cap = cap * 2;
        if (*obj).flags & FLAG_INLINE != 0 {
            migrate_inline_to_heap(obj, new_cap);
        } else {
            let old_layout = entries_layout(cap);
            let new_layout = entries_layout(new_cap);
            (*obj).entries = realloc((*obj).entries as *mut u8, old_layout, new_layout.size()) as *mut LinObjectEntry;
            (*obj).cap = new_cap;
        }
    }
    let slot = (*obj).entries.add(len as usize);
    (*slot).key = key;
    (*slot).value = value;
    (*obj).len = len + 1;
    // Deep-copy path: keep any built index consistent (cheap O(1) insert / lazy rebuild).
    index_after_append(obj, key, len);
}

/// Get a field value as a pointer to TaggedVal. Returns null if key not found.
#[no_mangle]
pub unsafe extern "C" fn lin_object_get(obj: *const LinObject, key: *const LinString) -> *const TaggedVal {
    if obj.is_null() {
        return std::ptr::null();
    }
    // Large objects: lazily build/probe the O(1) hash index. The build mutates only the
    // (non-observable) index cache fields, so the const→mut cast is sound. Below the threshold
    // (or a degenerate empty table) we keep the linear scan — faster for tiny N, no allocation.
    if ensure_index(obj as *mut LinObject) {
        let slot = index_probe(obj, key);
        if slot == u32::MAX {
            return std::ptr::null();
        }
        return &(*(*obj).entries.add(slot as usize)).value;
    }
    let len = (*obj).len;
    for i in 0..len {
        let entry = (*obj).entries.add(i as usize);
        if lin_string_key_eq((*entry).key, key) {
            return &(*entry).value;
        }
    }
    std::ptr::null()
}

/// Look up `key` in `obj` ONCE. If it maps to an array, return that array (the live interior
/// one, so a subsequent `push` mutates it in place); otherwise insert a fresh empty array under
/// `key` and return that. The result is a `Json` value (`TaggedVal*(Array)`) so the caller can
/// pass it straight to `push`. Backs `std/array.groupBy`, replacing the get-then-set double
/// lookup with one lookup + push.
///
/// `obj` crosses the FFI boundary as a `Json` (`TaggedVal*(Object)`) or raw `LinObject*`; `key`
/// is a `String` (`LinString*`). Both are BORROWED.
///
/// RC: the array always lives INSIDE the object (object owns it). The returned `Json` box is an
/// owned +1 like every other foreign `Json` result, so we RETAIN the array before boxing it —
/// the box's eventual scope-exit release brings the count back down, leaving the object's own
/// reference intact. `push` into the returned box mutates the interior array IN PLACE (push
/// borrows its array arg; it neither retains nor replaces it), so the mutation is visible
/// through the object. On the insert path the object takes its own +1 via `object_push_owned`
/// over a freshly-allocated (rc=1) array, and we bump once more for the returned box.
#[no_mangle]
pub unsafe extern "C" fn lin_object_get_or_insert_array(obj: *const u8, key: *const u8) -> *mut u8 {
    use crate::tagged::*;
    if obj.is_null() {
        // No object to mutate; hand back a fresh empty array so `push` is still well-defined.
        return alloc_tagged(TAG_ARRAY, crate::array::lin_array_alloc(4) as u64);
    }
    // A typed index-signature map `{ String: T[] }` is backed by `LinMap` (TAG_MAP), not
    // `LinObject`. Dispatch on the tag and route map values through the map intrinsics — without
    // this branch, treating a `LinMap*` as a `LinObject*` corrupts memory (ADR-055). `groupBy`'s
    // result is map-typed, so this is the path it actually takes.
    if *obj == TAG_MAP {
        let lin_map = (*(obj as *const TaggedVal)).payload as *mut crate::map::LinMap;
        let key_str = if !key.is_null() && *key == TAG_STR {
            (*(key as *const TaggedVal)).payload as *const LinString
        } else {
            key as *const LinString
        };
        let existing = crate::map::lin_map_get(lin_map, key_str);
        if !existing.is_null() && (*existing).tag == TAG_ARRAY {
            let arr = (*existing).payload as *mut crate::array::LinArray;
            if !arr.is_null() && (*arr).refcount < crate::string::IMMORTAL_RC {
                (*arr).refcount += 1;
            }
            return alloc_tagged(TAG_ARRAY, arr as u64);
        }
        // Absent (or present-but-not-an-array): create a fresh array and insert it. `lin_map_set`
        // retains the value's inner payload (arr -> rc 2) and keeps its own key ref.
        let arr = crate::array::lin_array_alloc(4); // rc = 1
        let val = TaggedVal { tag: TAG_ARRAY, _pad: [0; 7], payload: arr as u64 };
        crate::map::lin_map_set(lin_map, key_str as *mut LinString, &val);
        // Drop our build +1 (map owns its retained ref), then bump once for the returned box.
        crate::array::lin_array_release(arr);
        if (*arr).refcount < crate::string::IMMORTAL_RC {
            (*arr).refcount += 1;
        }
        return alloc_tagged(TAG_ARRAY, arr as u64);
    }
    // Unwrap a boxed Json object to the raw LinObject*; a raw LinObject* is used as-is.
    let lin_obj = if *obj == TAG_OBJECT {
        (*(obj as *const TaggedVal)).payload as *mut LinObject
    } else {
        obj as *mut LinObject
    };
    // `key` arrives as a LinString* (possibly boxed as Json(Str) — unwrap if so).
    let key_str = if !key.is_null() && *key == TAG_STR {
        (*(key as *const TaggedVal)).payload as *const LinString
    } else {
        key as *const LinString
    };

    // Single lookup.
    let existing = lin_object_get(lin_obj, key_str);
    if !existing.is_null() && (*existing).tag == TAG_ARRAY {
        let arr = (*existing).payload as *mut crate::array::LinArray;
        // Retain: the returned Json box owns a +1; the object keeps its own reference.
        if !arr.is_null() && (*arr).refcount < crate::string::IMMORTAL_RC {
            (*arr).refcount += 1;
        }
        return alloc_tagged(TAG_ARRAY, arr as u64);
    }

    // Absent (or present-but-not-an-array): create a fresh empty array and insert it.
    let arr = crate::array::lin_array_alloc(4); // rc = 1
    // Build a TaggedVal(Array) and store it via the dup-checking setter so a present-but-
    // non-array value is overwritten (release its old payload) rather than duplicated.
    let val = TaggedVal { tag: TAG_ARRAY, _pad: [0; 7], payload: arr as u64 };
    // lin_object_set RETAINS the value's inner payload (arr -> rc 2) and keeps its own key ref.
    lin_object_set(lin_obj, key_str as *mut LinString, &val);
    // Drop our construction +1: the object now owns its retained reference (rc back to 1 in the
    // object). Then bump once for the returned box's owned +1 (rc 2: object + returned box).
    // Net since alloc (rc1): set retained (+1 = 2), we release our build ref (-1 = 1), box (+1 = 2).
    crate::array::lin_array_release(arr);
    if (*arr).refcount < crate::string::IMMORTAL_RC {
        (*arr).refcount += 1;
    }
    alloc_tagged(TAG_ARRAY, arr as u64)
}

/// Copy all fields from `src` into `dst`, overwriting existing keys.
/// Used to implement object spread: `{ ...src, ... }`.
#[no_mangle]
pub unsafe extern "C-unwind" fn lin_object_merge(dst: *mut LinObject, src: *const LinObject) {
    if src.is_null() {
        return; // spreading null contributes nothing
    }
    let src_len = (*src).len;
    for i in 0..src_len {
        let entry = (*src).entries.add(i as usize);
        lin_object_set(dst, (*entry).key, &(*entry).value);
    }
}

/// Return a LinArray* containing all keys as LinString* (tagged TAG_STR).
/// Each key string's refcount is incremented so the array owns a reference.
#[no_mangle]
pub unsafe extern "C" fn lin_object_keys(obj: *const LinObject) -> *mut crate::array::LinArray {
    let len = if obj.is_null() { 0 } else { (*obj).len };
    let arr = crate::array::lin_array_alloc(len as u64);
    for i in 0..len {
        let entry = (*obj).entries.add(i as usize);
        let key = (*entry).key;
        // Retain so the array owns a reference to each key string (no-op for interned literals).
        crate::string::lin_string_inc_ref(key);
        let slot = (*arr).data.add(i as usize);
        (*slot).tag = crate::tagged::TAG_STR;
        (*slot).payload = key as u64;
    }
    (*arr).len = len as u64;
    arr
}

/// Return a LinArray* containing all values as TaggedVal (each stored inline).
#[no_mangle]
pub unsafe extern "C" fn lin_object_values(obj: *const LinObject) -> *mut crate::array::LinArray {
    let len = if obj.is_null() { 0 } else { (*obj).len };
    let arr = crate::array::lin_array_alloc(len as u64);
    for i in 0..len {
        let entry = (*obj).entries.add(i as usize);
        let src = &(*entry).value;
        let slot = (*arr).data.add(i as usize) as *mut TaggedVal;
        std::ptr::copy_nonoverlapping(src as *const TaggedVal, slot, 1);
        retain_tagged_payload(src);
    }
    (*arr).len = len as u64;
    arr
}

/// Return a LinArray* of pairs (each pair is a LinArray* with [key, value]).
#[no_mangle]
pub unsafe extern "C" fn lin_object_entries(obj: *const LinObject) -> *mut crate::array::LinArray {
    let len = if obj.is_null() { 0 } else { (*obj).len };
    let out = crate::array::lin_array_alloc(len as u64);
    for i in 0..len {
        let entry = (*obj).entries.add(i as usize);
        // Build pair array [key, value]
        let pair = crate::array::lin_array_alloc(2);
        (*(*pair).data.add(0)).tag = crate::tagged::TAG_STR;
        (*(*pair).data.add(0)).payload = (*entry).key as u64;
        crate::string::lin_string_inc_ref((*entry).key); // array slot owns a ref to the key string (no-op for interned literals)
        let val_src = &(*entry).value;
        std::ptr::copy_nonoverlapping(val_src as *const TaggedVal, (*pair).data.add(1) as *mut TaggedVal, 1);
        retain_tagged_payload(val_src);
        (*pair).len = 2;
        // Store pair pointer in output array as TAG_ARRAY
        let slot = (*out).data.add(i as usize);
        (*slot).tag = crate::tagged::TAG_ARRAY;
        (*slot).payload = pair as u64;
    }
    (*out).len = len as u64;
    out
}

/// Check if two LinString keys are equal.
unsafe fn lin_string_key_eq(a: *const LinString, b: *const LinString) -> bool {
    if a == b { return true; }
    if a.is_null() || b.is_null() { return false; }
    let a_len = (*a).len;
    let b_len = (*b).len;
    if a_len != b_len { return false; }
    let a_data = (*a).data.as_ptr();
    let b_data = (*b).data.as_ptr();
    let a_slice = std::slice::from_raw_parts(a_data, a_len as usize);
    let b_slice = std::slice::from_raw_parts(b_data, b_len as usize);
    a_slice == b_slice
}

/// Check if an object has a given key. Returns 1 if present, 0 if not.
#[no_mangle]
pub unsafe extern "C" fn lin_object_has(obj: *const LinObject, key: *const LinString) -> u8 {
    if obj.is_null() { return 0; }
    if ensure_index(obj as *mut LinObject) {
        return (index_probe(obj, key) != u32::MAX) as u8;
    }
    let len = (*obj).len;
    for i in 0..len {
        let entry = (*obj).entries.add(i as usize);
        if lin_string_key_eq((*entry).key, key) {
            return 1;
        }
    }
    0
}

/// Check if a boxed value (TaggedVal*) is an object that has `key`. Returns 0 for null
/// or non-object values. Does the tag check + unbox internally so callers need no
/// branching (used by the IR `has`-pattern lowering).
#[no_mangle]
pub unsafe extern "C" fn lin_value_has_field(tagged: *const u8, key: *const LinString) -> u8 {
    use crate::tagged::{TaggedVal, TAG_OBJECT};
    if tagged.is_null() { return 0; }
    let tv = &*(tagged as *const TaggedVal);
    if tv.tag != TAG_OBJECT { return 0; }
    let obj = tv.payload as *const LinObject;
    lin_object_has(obj, key)
}

/// Check if a boxed value (TaggedVal*) is an array of length `n` (exact) or `>= n` when
/// `at_least != 0`. Returns 0 for null/non-array values. Branchless helper for the IR
/// array-pattern lowering.
#[no_mangle]
pub unsafe extern "C" fn lin_value_array_len_check(tagged: *const u8, n: u64, at_least: u8) -> u8 {
    use crate::tagged::{TaggedVal, TAG_ARRAY};
    if tagged.is_null() { return 0; }
    let tv = &*(tagged as *const TaggedVal);
    if tv.tag != TAG_ARRAY { return 0; }
    let arr = tv.payload as *const crate::array::LinArray;
    if arr.is_null() { return 0; }
    let len = (*arr).len as u64;
    let ok = if at_least != 0 { len >= n } else { len == n };
    ok as u8
}

/// Deep structural equality for two objects: same keys and values, order-independent.
/// Returns 1 if equal, 0 if not.
#[no_mangle]
pub unsafe extern "C" fn lin_object_eq(a: *const LinObject, b: *const LinObject) -> u8 {

    if a == b { return 1; }
    if a.is_null() || b.is_null() { return 0; }
    let a_len = (*a).len;
    let b_len = (*b).len;
    if a_len != b_len { return 0; }
    // Positional fast path: when both objects list their keys in the SAME ORDER (the common case —
    // same record type, or same-shape JSON), keys line up slot-for-slot and are usually pointer-
    // identical (interned literals). Walk in parallel comparing values; bail to the order-independent
    // path the moment a key position diverges. Zero allocation, no hashing.
    let mut positional_ok = true;
    for i in 0..a_len {
        let ae = (*a).entries.add(i as usize);
        let be = (*b).entries.add(i as usize);
        if !lin_string_key_eq((*ae).key, (*be).key) {
            positional_ok = false;
            break;
        }
        let av = &(*ae).value as *const TaggedVal;
        let bv = &(*be).value as *const TaggedVal;
        if !tagged_val_eq(av, bv) { return 0; }   // same key, different value → definitively unequal
    }
    if positional_ok { return 1; }
    // else fall through to the existing ensure_index / linear order-independent path.
    // Large objects (>= HASH_INDEX_THRESHOLD): use B's O(1) hash side-index for the inner lookup
    // instead of a per-key linear scan, turning O(n*m) into O(n) average. Equality stays
    // order-independent — the index finds the key regardless of its slot. `ensure_index` builds
    // the index lazily for non-frozen B; for a frozen B it only USES an index that was built at
    // freeze time (it never mutates a frozen object — see its thread-safety note), otherwise it
    // returns false here and we fall through to the read-only linear scan. Small objects fall
    // through too (faster for tiny N, no hashing/alloc). This branch reads B's index but never
    // writes A, so an A == B compare where A is frozen is unaffected.
    if ensure_index(b as *mut LinObject) {
        for i in 0..a_len {
            let ae = (*a).entries.add(i as usize);
            let a_key = (*ae).key;
            let slot = index_probe(b, a_key);
            if slot == u32::MAX { return 0; }
            let be = (*b).entries.add(slot as usize);
            let av = &(*ae).value as *const TaggedVal;
            let bv = &(*be).value as *const TaggedVal;
            if !tagged_val_eq(av, bv) { return 0; }
        }
        return 1;
    }
    // For each entry in a, find matching entry in b with equal value.
    for i in 0..a_len {
        let ae = (*a).entries.add(i as usize);
        let a_key = (*ae).key;
        // Find this key in b.
        let mut found = false;
        for j in 0..b_len {
            let be = (*b).entries.add(j as usize);
            let b_key = (*be).key;
            if lin_string_key_eq(a_key, b_key) {
                // Compare values.
                let av = &(*ae).value as *const TaggedVal;
                let bv = &(*be).value as *const TaggedVal;
                if !tagged_val_eq(av, bv) { return 0; }
                found = true;
                break;
            }
        }
        if !found { return 0; }
    }
    1
}

// ── SPIKE-ONLY benchmark variants (object-eq positional fast path) ──────────────────────────────
// These replicate the BASELINE-LINEAR (pre-index) and SHIPPED-INDEX (current master, no positional
// walk) code paths so a single benchmark binary can A/B all three eq strategies apples-to-apples
// without three separate runtime rebuilds. They have access to the private helpers (ensure_index,
// index_probe, lin_string_key_eq, tagged_val_eq). NOT for production use — remove before shipping.

/// BASELINE-LINEAR: pure O(n*m) order-independent scan (the pre-index implementation, parent of
/// 6af9284c). No hashing, no positional walk.
#[doc(hidden)]
#[no_mangle]
pub unsafe extern "C" fn lin_object_eq_spike_linear(a: *const LinObject, b: *const LinObject) -> u8 {
    if a == b { return 1; }
    if a.is_null() || b.is_null() { return 0; }
    let a_len = (*a).len;
    let b_len = (*b).len;
    if a_len != b_len { return 0; }
    for i in 0..a_len {
        let ae = (*a).entries.add(i as usize);
        let a_key = (*ae).key;
        let mut found = false;
        for j in 0..b_len {
            let be = (*b).entries.add(j as usize);
            let b_key = (*be).key;
            if lin_string_key_eq(a_key, b_key) {
                let av = &(*ae).value as *const TaggedVal;
                let bv = &(*be).value as *const TaggedVal;
                if !tagged_val_eq(av, bv) { return 0; }
                found = true;
                break;
            }
        }
        if !found { return 0; }
    }
    1
}

/// SHIPPED-INDEX: the current-master implementation WITHOUT the positional fast path — hash-index
/// probe for large B, linear scan otherwise.
#[doc(hidden)]
#[no_mangle]
pub unsafe extern "C" fn lin_object_eq_spike_index(a: *const LinObject, b: *const LinObject) -> u8 {
    if a == b { return 1; }
    if a.is_null() || b.is_null() { return 0; }
    let a_len = (*a).len;
    let b_len = (*b).len;
    if a_len != b_len { return 0; }
    if ensure_index(b as *mut LinObject) {
        for i in 0..a_len {
            let ae = (*a).entries.add(i as usize);
            let a_key = (*ae).key;
            let slot = index_probe(b, a_key);
            if slot == u32::MAX { return 0; }
            let be = (*b).entries.add(slot as usize);
            let av = &(*ae).value as *const TaggedVal;
            let bv = &(*be).value as *const TaggedVal;
            if !tagged_val_eq(av, bv) { return 0; }
        }
        return 1;
    }
    for i in 0..a_len {
        let ae = (*a).entries.add(i as usize);
        let a_key = (*ae).key;
        let mut found = false;
        for j in 0..b_len {
            let be = (*b).entries.add(j as usize);
            let b_key = (*be).key;
            if lin_string_key_eq(a_key, b_key) {
                let av = &(*ae).value as *const TaggedVal;
                let bv = &(*be).value as *const TaggedVal;
                if !tagged_val_eq(av, bv) { return 0; }
                found = true;
                break;
            }
        }
        if !found { return 0; }
    }
    1
}

unsafe fn tagged_val_eq(a: *const crate::tagged::TaggedVal, b: *const crate::tagged::TaggedVal) -> bool {
    use crate::tagged::*;
    if a.is_null() && b.is_null() { return true; }
    if a.is_null() || b.is_null() { return false; }
    let at = (*a).tag;
    let bt = (*b).tag;
    let ap = (*a).payload;
    let bp = (*b).payload;
    if at == TAG_NULL && bt == TAG_NULL { return true; }
    if at == TAG_NULL || bt == TAG_NULL { return false; }
    // Cross-numeric: widen to f64 so Int32(1) == Int64(1).
    let at_is_num = (at >= TAG_INT32 && at <= TAG_FLOAT64) || at == crate::tagged::TAG_UINT64;
    let bt_is_num = (bt >= TAG_INT32 && bt <= TAG_FLOAT64) || bt == crate::tagged::TAG_UINT64;
    if at_is_num && bt_is_num && at != bt {
        return tagged_as_f64(at, ap) == tagged_as_f64(bt, bp);
    }
    if at != bt { return false; }
    if at == TAG_BOOL { return ap == bp; }
    if at == TAG_INT32 { return (ap as i32) == (bp as i32); }
    if at == TAG_INT64 { return (ap as i64) == (bp as i64); }
    if at == crate::tagged::TAG_UINT64 { return ap == bp; }
    if at == TAG_FLOAT32 {
        let af = f32::from_bits(ap as u32);
        let bf = f32::from_bits(bp as u32);
        return af == bf;
    }
    if at == TAG_FLOAT64 {
        let af = f64::from_bits(ap);
        let bf = f64::from_bits(bp);
        return af == bf;
    }
    if at == TAG_STR {
        let as_ptr = ap as *const crate::string::LinString;
        let bs_ptr = bp as *const crate::string::LinString;
        return crate::string::lin_string_eq(as_ptr, bs_ptr);
    }
    if at == TAG_OBJECT {
        let ao = ap as *const LinObject;
        let bo = bp as *const LinObject;
        return lin_object_eq(ao, bo) != 0;
    }
    if at == TAG_ARRAY {
        let aa = ap as *const crate::array::LinArray;
        let ba = bp as *const crate::array::LinArray;
        return lin_array_eq_deep(aa, ba);
    }
    if at == crate::tagged::TAG_SUMNODE {
        // KEEP-PACKED-THROUGH-RECORD-FIELDS boundary: both sides are kept-packed `*SumNode`s (a
        // record field comparison). Materialize each to a real LinObject and compare structurally
        // (order-independent), releasing the transient materializations. (`at == bt` holds here.)
        let ao = crate::sumnode::lin_sumnode_materialize(ap as *mut u8);
        let bo = crate::sumnode::lin_sumnode_materialize(bp as *mut u8);
        let eq = !ao.is_null() && !bo.is_null()
            && lin_object_eq(ao as *const LinObject, bo as *const LinObject) != 0;
        if !ao.is_null() { lin_object_release(ao as *mut LinObject); }
        if !bo.is_null() { lin_object_release(bo as *mut LinObject); }
        return eq;
    }
    // For other types (closures, iterators): pointer equality.
    ap == bp
}

/// Deep equality for arrays: dispatches on elem_tag to handle flat vs tagged layouts.
unsafe fn lin_array_eq_deep(a: *const crate::array::LinArray, b: *const crate::array::LinArray) -> bool {
    use crate::tagged::*;
    if a == b { return true; }
    if a.is_null() || b.is_null() { return false; }
    let len = (*a).len;
    if len != (*b).len { return false; }
    let tag_a = (*a).elem_tag;
    let tag_b = (*b).elem_tag;
    if tag_a != tag_b { return false; }
    match tag_a {
        TAG_INT32 => {
            let da = (*a).data as *const i32;
            let db = (*b).data as *const i32;
            for i in 0..len as usize {
                if *da.add(i) != *db.add(i) { return false; }
            }
        }
        TAG_INT64 => {
            let da = (*a).data as *const i64;
            let db = (*b).data as *const i64;
            for i in 0..len as usize {
                if *da.add(i) != *db.add(i) { return false; }
            }
        }
        TAG_FLOAT32 => {
            let da = (*a).data as *const f32;
            let db = (*b).data as *const f32;
            for i in 0..len as usize {
                if *da.add(i) != *db.add(i) { return false; }
            }
        }
        TAG_FLOAT64 => {
            let da = (*a).data as *const f64;
            let db = (*b).data as *const f64;
            for i in 0..len as usize {
                if *da.add(i) != *db.add(i) { return false; }
            }
        }
        _ => {
            // Tagged array (elem_tag == 0xFF or any other): elements are LinArrayElem.
            for i in 0..len as usize {
                let ae = (*a).data.add(i);
                let be = (*b).data.add(i);
                let av = ae as *const crate::tagged::TaggedVal;
                let bv = be as *const crate::tagged::TaggedVal;
                if !tagged_val_eq(av, bv) { return false; }
            }
        }
    }
    true
}

/// Decrement refcount; when it reaches zero, release all keys and heap-typed values then free.
#[no_mangle]
pub unsafe extern "C" fn lin_object_release(obj: *mut LinObject) {
    if obj.is_null() {
        return;
    }
    // Frozen (immortal) objects: saturated refcount, never freed/decremented (Frozen<T>,
    // ADR-030). Guard makes retain/release no-ops so concurrent reads are race-free.
    if (*obj).refcount >= crate::string::IMMORTAL_RC {
        return;
    }
    // Zero refcount ⇒ double-release (ownership bug); the decrement below would wrap u32.
    // Debug/ASan-only guard, no release-build cost.
    debug_assert!((*obj).refcount > 0, "lin_object_release: refcount underflow (double free)");
    (*obj).refcount -= 1;
    if (*obj).refcount == 0 {
        let len = (*obj).len as usize;
        for i in 0..len {
            let entry = (*obj).entries.add(i);
            // Keys are always owned LinString*.
            crate::string::lin_string_release((*entry).key);
            // Values: release heap-typed payloads. Route through the canonical
            // `release_tagged_payload` so EVERY tag is handled — this loop was a hand-rolled
            // copy that omitted TAG_MAP (and TAG_SHARED/TAG_STREAM), so a `{ String: T }` map
            // stored as an OBJECT/record FIELD (e.g. `ScanResults.bestArrivals`) was never
            // released when the record dropped, leaking the whole map + its nested contents
            // every time the record was discarded (the dominant RAPTOR per-scan leak). Using the
            // shared helper keeps this in lockstep with the map/array value-walks permanently.
            release_tagged_payload(&(*entry).value);
        }
        // Free the hash side-index (if built) BEFORE freeing the object header. The table
        // holds only u32 slot indices — no refcounted pointers — so there is nothing to
        // release inside it; just its backing allocation.
        free_index(obj);
        let cap = (*obj).cap;
        if (*obj).flags & FLAG_INLINE != 0 {
            // Entries live inside the header allocation — one dealloc frees both.
            std::alloc::dealloc(obj as *mut u8, inline_object_layout(cap));
        } else {
            // Entries were migrated to a separate heap buffer (object grew).
            std::alloc::dealloc((*obj).entries as *mut u8, entries_layout(cap));
            std::alloc::dealloc(obj as *mut u8, object_layout());
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_object_length(obj: *const LinObject) -> i64 {
    if obj.is_null() { return 0; }
    (*obj).len as i64
}

/// Copy all fields from `src` into `dst` except those whose keys are in `excluded`.
/// `excluded` is a pointer to `n_excluded` LinString* values.
/// Used to implement object rest destructuring: `val { a, b, ...rest } = obj`.
#[no_mangle]
pub unsafe extern "C" fn lin_object_copy_except(
    dst: *mut LinObject,
    src: *const LinObject,
    excluded: *const *const LinString,
    n_excluded: u32,
) {
    if src.is_null() { return; }
    let len = (*src).len;
    'outer: for i in 0..len {
        let entry = (*src).entries.add(i as usize);
        let key = (*entry).key;
        for j in 0..n_excluded {
            if lin_string_key_eq(key, *excluded.add(j as usize)) {
                continue 'outer;
            }
        }
        lin_object_set(dst, key, &(*entry).value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tagged::{alloc_tagged, lin_tagged_release, TAG_STR, TAG_ARRAY, TAG_OBJECT, TAG_INT32, TAG_MAP};

    unsafe fn mk_string(s: &str) -> *mut LinString {
        crate::string::lin_string_from_bytes(s.as_ptr(), s.len() as u32)
    }

    unsafe fn get_tag(obj: *const LinObject, key: *const LinString) -> u8 {
        let tv = lin_object_get(obj, key);
        if tv.is_null() { crate::tagged::TAG_NULL } else { (*tv).tag }
    }

    // Build an object literal-style (fresh append) whose VALUES are heap types, then drop
    // it. Under ASan this proves the new no-dup-check append retains the inner payloads
    // exactly like lin_object_set (one balanced reference per slot) — no leak, no UAF.
    #[test]
    fn fresh_append_heap_values_balanced_rc() {
        unsafe {
            // Distinct literal keys (caller owns one ref each, as codegen does).
            let kx = mk_string("x");
            let ky = mk_string("y");
            let kz = mk_string("z");

            // Heap-typed values: a string, an array, a nested object. The caller box owns
            // the +1; lin_object_set_fresh retains the inner so the object owns its own ref.
            let sval = mk_string("hello");
            let s_box = alloc_tagged(TAG_STR, sval as u64) as *const TaggedVal;

            let arr = crate::array::lin_array_alloc(2);
            (*arr).len = 0;
            let a_box = alloc_tagged(TAG_ARRAY, arr as u64) as *const TaggedVal;

            let inner = lin_object_alloc(0); // empty {} -> min cap honored
            let o_box = alloc_tagged(TAG_OBJECT, inner as u64) as *const TaggedVal;

            // cap exactly 3 (right-sized, no over-alloc / no min-4).
            let obj = lin_object_alloc(3);
            assert_eq!((*obj).cap, 3, "right-sized cap honored");

            lin_object_set_fresh(obj, kx, s_box);
            lin_object_set_fresh(obj, ky, a_box);
            lin_object_set_fresh(obj, kz, o_box);

            assert_eq!((*obj).len, 3);
            assert_eq!((*obj).cap, 3, "no growth for exactly-sized literal");
            assert_eq!(get_tag(obj, kx), TAG_STR);
            assert_eq!(get_tag(obj, ky), TAG_ARRAY);
            assert_eq!(get_tag(obj, kz), TAG_OBJECT);

            // Drop the object: releases each key (no-op for any saturated) and each inner
            // payload's owned reference.
            lin_object_release(obj);

            // The caller's own boxes still hold a live reference; release them now. If the
            // object's release had over-released, this would be a UAF/double-free under ASan.
            crate::tagged::lin_tagged_release(s_box as *mut u8);
            crate::tagged::lin_tagged_release(a_box as *mut u8);
            crate::tagged::lin_tagged_release(o_box as *mut u8);

            // Free the caller's key references (object held its own inc_ref'd copies).
            crate::string::lin_string_release(kx);
            crate::string::lin_string_release(ky);
            crate::string::lin_string_release(kz);
        }
    }

    // Growth path of fresh append: starting from a 0-hint (min cap 1) object, append past
    // capacity and confirm the realloc keeps every entry intact and RC stays balanced.
    #[test]
    fn fresh_append_growth() {
        unsafe {
            let obj = lin_object_alloc(0);
            assert_eq!((*obj).cap, 1, "min cap 1 for empty hint (no zero-size alloc)");
            let keys: Vec<*mut LinString> =
                (0..5).map(|i| mk_string(&format!("k{i}"))).collect();
            for (i, &k) in keys.iter().enumerate() {
                let v = alloc_tagged(TAG_INT32, i as u64) as *const TaggedVal;
                lin_object_set_fresh(obj, k, v);
                // scalar box: lin_tagged_release no-ops, but call it for symmetry safety.
                crate::tagged::lin_tagged_release(v as *mut u8);
            }
            assert_eq!((*obj).len, 5);
            assert!((*obj).cap >= 5);
            for (i, &k) in keys.iter().enumerate() {
                let tv = lin_object_get(obj, k);
                assert!(!tv.is_null());
                assert_eq!((*tv).payload as i32, i as i32);
            }
            lin_object_release(obj);
            for k in keys { crate::string::lin_string_release(k); }
        }
    }

    // Mirror std/array.groupBy: for each item, get-or-insert the group array under its key and
    // push the item into the returned (boxed) array IN PLACE. The returned Json box is an owned
    // +1; releasing it after each push must leave the object's own reference intact. Under ASan
    // an over-release (returning a +0 alias) frees the group while the object still points at it
    // (UAF on the next push/read); an under-release leaks every group box. Loop to amplify.
    #[test]
    fn get_or_insert_array_groupby_rc_balanced() {
        unsafe {
            use crate::array::{lin_array_length, lin_array_get_tagged};
            let obj = lin_object_alloc(2); // the `var result = {}`
            let obj_box = alloc_tagged(TAG_OBJECT, obj as u64); // object crosses as Json
            // Two interned-ish keys reused every iteration (caller owns one ref each).
            let even = crate::string::lin_string_from_bytes("even".as_ptr(), 4);
            let odd = crate::string::lin_string_from_bytes("odd".as_ptr(), 3);

            for i in 0..100i64 {
                let key = if i % 2 == 0 { even } else { odd };
                let key_box = alloc_tagged(TAG_STR, key as u64);
                // get-or-insert returns a boxed Json(Array), owned +1.
                let group_box = lin_object_get_or_insert_array(
                    obj_box as *const u8, key_box as *const u8,
                );
                let group = (*(group_box as *const TaggedVal)).payload as *mut crate::array::LinArray;
                // push the integer item into the group in place (mirrors `push(group, item)`).
                let item = alloc_tagged(TAG_INT32, i as u64);
                crate::array::lin_push_dyn(group, item as *const TaggedVal);
                // scalar box: release no-ops on cached small ints, frees otherwise.
                lin_tagged_release(item as *mut u8);
                // Release the returned group box (its +1 was for this binding only).
                lin_tagged_release(group_box);
                // key_box is a fresh box aliasing `even`/`odd`; free the shell, the key lives on.
                crate::tagged::lin_tagged_free_box(key_box);
            }

            // Two groups, 50 each, all items intact.
            let g_even = lin_object_get(obj, even);
            let g_odd = lin_object_get(obj, odd);
            assert!(!g_even.is_null() && (*g_even).tag == TAG_ARRAY);
            assert!(!g_odd.is_null() && (*g_odd).tag == TAG_ARRAY);
            let ea = (*g_even).payload as *const crate::array::LinArray;
            let oa = (*g_odd).payload as *const crate::array::LinArray;
            assert_eq!(lin_array_length(ea), 50);
            assert_eq!(lin_array_length(oa), 50);
            // Read back the first/last of each (UAF check).
            let first_even = lin_array_get_tagged(ea, 0);
            assert_eq!((*first_even).payload as i32, 0);
            std::alloc::dealloc(first_even as *mut u8, std::alloc::Layout::new::<TaggedVal>());

            // Teardown: releasing the object frees both group arrays exactly once.
            lin_object_release(obj);
            crate::tagged::lin_tagged_free_box(obj_box);
            crate::string::lin_string_release(even);
            crate::string::lin_string_release(odd);
        }
    }

    // Same get-or-insert-array contract, but over a TAG_MAP (`LinMap`) backing — the typed
    // `{ String: T[] }` result that std/array.groupBy now produces (ADR-055). Without the TAG_MAP
    // branch in `lin_object_get_or_insert_array`, the map would be (mis)read as a `LinObject` and
    // corrupt memory; this guards that the map path inserts/looks-up/RC-balances correctly.
    #[test]
    fn get_or_insert_array_groupby_over_map() {
        unsafe {
            use crate::array::{lin_array_length, lin_array_get_tagged};
            use crate::map::{lin_map_alloc, lin_map_get, lin_map_release};
            let map = lin_map_alloc(2); // the `var result: { String: T[] } = {}`
            let map_box = alloc_tagged(TAG_MAP, map as u64); // map crosses as a boxed value
            let even = crate::string::lin_string_from_bytes("even".as_ptr(), 4);
            let odd = crate::string::lin_string_from_bytes("odd".as_ptr(), 3);

            for i in 0..100i64 {
                let key = if i % 2 == 0 { even } else { odd };
                let key_box = alloc_tagged(TAG_STR, key as u64);
                let group_box = lin_object_get_or_insert_array(
                    map_box as *const u8, key_box as *const u8,
                );
                let group = (*(group_box as *const TaggedVal)).payload as *mut crate::array::LinArray;
                let item = alloc_tagged(TAG_INT32, i as u64);
                crate::array::lin_push_dyn(group, item as *const TaggedVal);
                lin_tagged_release(item as *mut u8);
                lin_tagged_release(group_box);
                crate::tagged::lin_tagged_free_box(key_box);
            }

            let g_even = lin_map_get(map, even);
            let g_odd = lin_map_get(map, odd);
            assert!(!g_even.is_null() && (*g_even).tag == TAG_ARRAY);
            assert!(!g_odd.is_null() && (*g_odd).tag == TAG_ARRAY);
            let ea = (*g_even).payload as *const crate::array::LinArray;
            let oa = (*g_odd).payload as *const crate::array::LinArray;
            assert_eq!(lin_array_length(ea), 50);
            assert_eq!(lin_array_length(oa), 50);
            let first_even = lin_array_get_tagged(ea, 0);
            assert_eq!((*first_even).payload as i32, 0);
            std::alloc::dealloc(first_even as *mut u8, std::alloc::Layout::new::<TaggedVal>());

            lin_map_release(map);
            crate::tagged::lin_tagged_free_box(map_box);
            crate::string::lin_string_release(even);
            crate::string::lin_string_release(odd);
        }
    }

    // Build an object with `n` integer-valued keys "k0".."k{n-1}" in the given slot order.
    // Caller owns the returned object (rc 1) and is responsible for releasing it.
    unsafe fn build_int_object(order: &[usize]) -> *mut LinObject {
        let obj = lin_object_alloc(order.len() as u32);
        for &i in order {
            let k = mk_string(&format!("k{i}"));
            let v = alloc_tagged(TAG_INT32, i as u64) as *const TaggedVal;
            // lin_object_set_fresh retains key + value; we drop our own refs after.
            lin_object_set_fresh(obj, k, v);
            crate::string::lin_string_release(k);
            crate::tagged::lin_tagged_release(v as *mut u8);
        }
        obj
    }

    // The indexed eq fast path (large objects) must agree with structural equality on every
    // dimension: order-independence, value-diff detection, key-rename detection, symmetry — and
    // it must also stay correct for SMALL objects (which take the linear-scan path). 24 keys >
    // HASH_INDEX_THRESHOLD (16) so `b` gets a hash index; the reversed-order `a` proves the
    // comparison is order-independent through the index probe.
    #[test]
    fn object_eq_indexed_is_order_independent_and_exact() {
        unsafe {
            let n = 24usize;
            let fwd: Vec<usize> = (0..n).collect();
            let rev: Vec<usize> = (0..n).rev().collect();

            // Equal, but built in opposite slot orders → must compare equal both ways.
            let a = build_int_object(&fwd);
            let b = build_int_object(&rev);
            assert_eq!(lin_object_eq(a, b), 1, "reversed-order large objects compare equal");
            assert_eq!(lin_object_eq(b, a), 1, "symmetric");

            // Value-diff: change one value in b → unequal both directions.
            let kd = mk_string("k7");
            let v999 = alloc_tagged(TAG_INT32, 999u64) as *const TaggedVal;
            lin_object_set(b, kd, v999); // overwrite existing key (dup-check path)
            crate::string::lin_string_release(kd);
            crate::tagged::lin_tagged_release(v999 as *mut u8);
            assert_eq!(lin_object_eq(a, b), 0, "value difference detected");
            assert_eq!(lin_object_eq(b, a), 0, "value difference symmetric");
            lin_object_release(b);

            // Key-rename: same count, same values, but one key renamed → unequal (the index probe
            // for the missing key must miss). Build c = fwd then rename "k0" to "kX" by rebuilding.
            let mut c_order = fwd.clone();
            c_order.remove(0); // drop k0
            let c = build_int_object(&c_order);
            // add a renamed key "kX" carrying value 0 (so counts/values match a but a key differs).
            let kx = mk_string("kX");
            let v0 = alloc_tagged(TAG_INT32, 0u64) as *const TaggedVal;
            lin_object_set(c, kx, v0);
            crate::string::lin_string_release(kx);
            crate::tagged::lin_tagged_release(v0 as *mut u8);
            assert_eq!((*c).len, n as u32, "same key count");
            assert_eq!(lin_object_eq(a, c), 0, "key rename detected");
            assert_eq!(lin_object_eq(c, a), 0, "key rename symmetric");
            lin_object_release(c);
            lin_object_release(a);

            // SMALL objects (< threshold) take the linear-scan path; verify it still works.
            let sfwd: Vec<usize> = (0..4).collect();
            let srev: Vec<usize> = (0..4).rev().collect();
            let sa = build_int_object(&sfwd);
            let sb = build_int_object(&srev);
            assert_eq!(lin_object_eq(sa, sb), 1, "small reversed-order equal");
            let ksd = mk_string("k1");
            let sv = alloc_tagged(TAG_INT32, 42u64) as *const TaggedVal;
            lin_object_set(sb, ksd, sv);
            crate::string::lin_string_release(ksd);
            crate::tagged::lin_tagged_release(sv as *mut u8);
            assert_eq!(lin_object_eq(sa, sb), 0, "small value difference detected");
            lin_object_release(sa);
            lin_object_release(sb);
        }
    }

    // POSITIONAL FAST PATH (spike): the early-return-0-on-value-mismatch is only sound while keys
    // are still aligned. Build objects with SHARED key pointers (models same-record-type / interned
    // literals so slot-for-slot keys are pointer-identical and the positional walk's `key==key`
    // shortcut fires). Covers: equal same-order (fast 1), equal reversed-order (fallback 1),
    // value-diff same-order (early 0), key-rename (0), small same-order (1).
    #[test]
    fn object_eq_positional_fast_path_is_exact() {
        unsafe {
            // Shared key set (pointer-identical between objects), sizes spanning the threshold.
            for &n in &[4usize, 16, 24, 32] {
                let keys: Vec<*mut LinString> = (0..n).map(|i| mk_string(&format!("k{i}"))).collect();

                let build = |reversed: bool, diff_at: Option<usize>| -> *mut LinObject {
                    let obj = lin_object_alloc(n as u32);
                    let order: Vec<usize> =
                        if reversed { (0..n).rev().collect() } else { (0..n).collect() };
                    for &i in &order {
                        let val = if Some(i) == diff_at { 999u64 } else { i as u64 };
                        let v = alloc_tagged(TAG_INT32, val) as *const TaggedVal;
                        lin_object_set(obj, keys[i], v); // shares key ptr
                        crate::tagged::lin_tagged_release(v as *mut u8);
                    }
                    obj
                };

                // 1) equal, SAME order → positional fast path returns 1.
                let a = build(false, None);
                let b = build(false, None);
                assert_eq!(lin_object_eq(a, b), 1, "n={n}: equal same-order");
                assert_eq!(lin_object_eq(b, a), 1, "n={n}: symmetric");

                // 2) equal, REVERSED order → positional walk fails (keys diverge), fallback returns 1.
                let c = build(true, None);
                assert_eq!(lin_object_eq(a, c), 1, "n={n}: equal reversed-order via fallback");
                assert_eq!(lin_object_eq(c, a), 1, "n={n}: reversed symmetric");

                // 3) value-diff, SAME order → positional early-return 0 (keys aligned at the diff slot).
                let d = build(false, Some(n / 2));
                assert_eq!(lin_object_eq(a, d), 0, "n={n}: value-diff same-order");
                assert_eq!(lin_object_eq(d, a), 0, "n={n}: value-diff symmetric");

                // 4) value-diff, REVERSED order → keys diverge before the diff slot, fallback returns 0.
                let e = build(true, Some(n / 2));
                assert_eq!(lin_object_eq(a, e), 0, "n={n}: value-diff reversed via fallback");

                lin_object_release(a);
                lin_object_release(b);
                lin_object_release(c);
                lin_object_release(d);
                lin_object_release(e);

                // 5) key-rename: same count, same values, one key replaced → unequal. Use a fresh,
                // non-shared key so slot keys diverge there (forces fallback miss).
                let f = build(false, None);
                let g = lin_object_alloc(n as u32);
                for i in 0..n {
                    let v = alloc_tagged(TAG_INT32, i as u64) as *const TaggedVal;
                    if i == n / 2 {
                        let kx = mk_string("RENAMED");
                        lin_object_set(g, kx, v);
                        crate::string::lin_string_release(kx);
                    } else {
                        lin_object_set(g, keys[i], v);
                    }
                    crate::tagged::lin_tagged_release(v as *mut u8);
                }
                assert_eq!((*g).len, n as u32, "n={n}: same key count after rename");
                assert_eq!(lin_object_eq(f, g), 0, "n={n}: key-rename detected");
                assert_eq!(lin_object_eq(g, f), 0, "n={n}: key-rename symmetric");
                lin_object_release(f);
                lin_object_release(g);

                for k in keys {
                    crate::string::lin_string_release(k);
                }
            }
        }
    }

    // THREAD-SAFETY (mandatory): N threads each compare against the SAME frozen >= 16-key object
    // via `lin_object_eq` in a loop. With the freeze-time index build + the immortal guard in
    // `ensure_index`, no thread ever mutates the frozen object's index fields, so there is no data
    // race (no torn pointer, no double-alloc leak, no half-built table probe). Without the fix the
    // first lookup on the frozen object would lazily `rebuild_index` it concurrently → UB. Modeled
    // on `frozen::frozen_array_read_concurrently_is_race_free`. Run under TSan to prove race-free.
    #[test]
    fn frozen_object_eq_concurrently_is_race_free() {
        unsafe {
            let n = 32usize;
            let order: Vec<usize> = (0..n).collect();
            // The shared frozen reference object `b`.
            let b = build_int_object(&order);
            let b_box = alloc_tagged(TAG_OBJECT, b as u64);
            crate::frozen::lin_freeze(b_box);
            assert!((*b).refcount >= crate::string::IMMORTAL_RC, "b is frozen");
            // Freeze must have built the index single-threaded so all threads probe it lock-free.
            assert!(!(*b).index.is_null(), "freeze built the hash index for the large object");

            let b_addr = b as usize;
            let mut handles = Vec::new();
            for t in 0..8 {
                handles.push(std::thread::spawn(move || {
                    let bp = b_addr as *const LinObject;
                    // Each thread builds its OWN equal probe object (reversed order to force the
                    // index path to do real work) and compares it against the shared frozen b.
                    let rev: Vec<usize> = (0..n).rev().collect();
                    for _ in 0..200 {
                        let a = build_int_object(&rev);
                        assert_eq!(lin_object_eq(a, bp), 1, "thread {t}: equal compare");
                        // A fast-reject case too (differing key count).
                        let short: Vec<usize> = (0..n - 1).collect();
                        let a2 = build_int_object(&short);
                        assert_eq!(lin_object_eq(a2, bp), 0, "thread {t}: reject compare");
                        lin_object_release(a);
                        lin_object_release(a2);
                    }
                }));
            }
            for h in handles {
                h.join().unwrap();
            }
            // The frozen object survives unchanged (never freed, index never torn).
            assert!((*b).refcount >= crate::string::IMMORTAL_RC);
            assert!(!(*b).index.is_null());
            crate::tagged::lin_tagged_free_box(b_box);
        }
    }
}
