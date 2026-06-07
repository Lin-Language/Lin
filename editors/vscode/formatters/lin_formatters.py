# lin_formatters.py — lldb data formatters (pretty-printers) for the Lin runtime.
#
# Decodes Lin's boxed runtime value representation into logical Lin values for the
# CodeLLDB Variables / Watch panels: ints/floats/bools/null/strings inline, arrays as
# [e0, e1, ...] and objects as { "k": v, ... }, all expandable in the tree.
#
# This is Phase 2 of the Lin debugger. It is READ-ONLY: it never calls into the
# debuggee, never mutates memory, and never touches refcounts — it only reads bytes
# from the target process and decodes them. Every pointer read is guarded so the
# debugger can safely inspect partially-initialised / invalid memory without crashing.
#
# =============================================================================
# RUNTIME LAYOUT ENCODED HERE (must stay in lockstep with crates/lin-runtime)
# =============================================================================
#
# Tag values — crates/lin-common/src/tags.rs:16-37 (single source of truth, shared
# by the runtime and codegen). Mirrored in the TAG_* constants below.
#
# TaggedVal — crates/lin-runtime/src/tagged.rs:39-56 (#[repr(C)], 16 bytes):
#     offset 0 : u8  tag
#     offset 1 : [u8;7] _pad  (zeroed)
#     offset 8 : u64 payload
#   Layout pinned by a const_assert (tagged.rs:52-56): size==16, tag@0, payload@8.
#   Payload encoding by tag (tagged.rs:139-221, encode/decode helpers cited):
#     TAG_NULL    : the box is a NULL pointer; no heap alloc (lin_box_null @157).
#                   lin_get_tag(null) == TAG_NULL (@225).
#     TAG_BOOL    : payload low byte 0/1 (lin_box_bool @162 / lin_unbox_bool @259).
#     TAG_INT32   : payload = i32 value sign-extended into i64 bits (lin_box_int32 @168).
#     TAG_INT64   : payload = i64 value bits (lin_box_int64 @177 / lin_unbox_int64 @241).
#     TAG_UINT64  : payload = u64 value, read UNSIGNED (lin_box_uint64 @188).
#     TAG_FLOAT64 : payload = f64 to_bits() (lin_box_float64 @193 / lin_unbox_float64 @253).
#                   ALL boxed float scalars are FLOAT64 (tags.rs:9-11); FLOAT32 only
#                   appears as a flat-array elem_tag, never on a boxed scalar.
#     TAG_STR     : payload = *LinString          (lin_box_str @198).
#     TAG_OBJECT  : payload = *LinObject          (lin_box_object @203).
#     TAG_ARRAY   : payload = *LinArray           (lin_box_array @208).
#     TAG_FUNCTION: payload = closure pointer     (lin_box_function @213).
#     TAG_MAP     : payload = *LinMap             (lin_box_map @219).
#     TAG_PROMISE/HANDLE/SHARED/STREAM: opaque runtime handles (tagged.rs:30-32).
#
# LinString — crates/lin-runtime/src/string.rs:8-13 (#[repr(C)]):
#     offset 0 : u32 refcount
#     offset 4 : u32 len
#     offset 8 : [u8; len] data  (UTF-8 bytes, inline)
#
# LinArray — crates/lin-runtime/src/array.rs:16-29 (#[repr(C)]):
#     offset 0  : u32 refcount
#     offset 4  : u8  elem_tag   (0xFF tagged | 0xFE sealed | TAG_* flat scalar)
#     offset 5  : [u8;3] _pad3
#     offset 8  : u64 len
#     offset 16 : u64 cap
#     offset 24 : ptr data
#     offset 32 : u64 elem_stride (sealed only)
#     offset 40 : ptr elem_desc   (sealed only)
#   Element representation by elem_tag (array.rs:6-10, lin_array_get_tagged @671):
#     0xFF       : tagged — each element is a 16-byte LinArrayElem == TaggedVal
#                  (array.rs:36-52 pins LinArrayElem == TaggedVal byte-for-byte).
#     0xFE        : sealed-record (SEALED_ARRAY_TAG, array.rs:34) — inline header-less
#                  packed records of elem_stride bytes; not decoded (shown as <sealed>).
#     TAG_INT32   : raw i32 (4 bytes/elem)
#     TAG_INT64   : raw i64 (8 bytes/elem)
#     TAG_FLOAT32 : raw f32 (4 bytes/elem)
#     TAG_FLOAT64 : raw f64 (8 bytes/elem)
#     TAG_UINT8/INT8           : 1 byte/elem
#     TAG_UINT16/INT16         : 2 bytes/elem
#     TAG_UINT32               : raw u32 (4 bytes/elem, unsigned)
#     TAG_UINT64               : raw u64 (8 bytes/elem, unsigned)
#
# LinObject — crates/lin-runtime/src/object.rs:17-34 (#[repr(C)]):
#     offset 0  : u32 refcount
#     offset 4  : u32 len   (number of live entries)
#     offset 8  : u32 cap
#     offset 12 : u32 flags
#     offset 16 : ptr entries -> [LinObjectEntry]
#     (offsets >= 24 are the lazy hash side-index; never read here.)
#   LinObjectEntry — object.rs:45-49 (#[repr(C)], 24 bytes):
#     offset 0  : ptr key   (*LinString)
#     offset 8  : TaggedVal value  (16 bytes: tag@8, payload@16)
#   So entry i lives at entries + i*24; its value TaggedVal is at +8 within the entry.

import lldb

# --- Tag constants (mirror crates/lin-common/src/tags.rs) --------------------
TAG_NULL = 0
TAG_BOOL = 1
TAG_INT32 = 2
TAG_INT64 = 3
TAG_FLOAT32 = 4
TAG_FLOAT64 = 5
TAG_STR = 6
TAG_OBJECT = 7
TAG_ARRAY = 8
TAG_FUNCTION = 9
TAG_UINT8 = 10
TAG_INT8 = 11
TAG_UINT16 = 12
TAG_INT16 = 13
TAG_UINT64 = 14
TAG_UINT32 = 15
TAG_PROMISE = 16
TAG_HANDLE = 17
TAG_SHARED = 18
TAG_STREAM = 19
TAG_MAP = 20

TAG_NAMES = {
    TAG_NULL: "null", TAG_BOOL: "bool", TAG_INT32: "int32", TAG_INT64: "int64",
    TAG_FLOAT32: "float32", TAG_FLOAT64: "float64", TAG_STR: "string",
    TAG_OBJECT: "object", TAG_ARRAY: "array", TAG_FUNCTION: "function",
    TAG_UINT8: "uint8", TAG_INT8: "int8", TAG_UINT16: "uint16", TAG_INT16: "int16",
    TAG_UINT64: "uint64", TAG_UINT32: "uint32", TAG_PROMISE: "promise",
    TAG_HANDLE: "handle", TAG_SHARED: "shared", TAG_STREAM: "stream", TAG_MAP: "map",
}

SEALED_ARRAY_TAG = 0xFE  # array.rs:34
TAGGED_ARRAY_TAG = 0xFF  # array.rs:6

# Caps so the debugger never spends forever / OOMs on a giant or corrupt container.
MAX_ELEMS = 50          # render at most this many array elements / object keys inline
MAX_CHILDREN = 200      # synthetic children cap (Variables tree)
MAX_STR_BYTES = 4096    # cap a single string read
MAX_LEN_SANITY = 1 << 28  # reject obviously-bogus lengths from invalid memory


# =============================================================================
# Low-level guarded memory reads against the target process.
# =============================================================================
#
# We read by raw address + byte offset rather than through typed struct members so
# the formatter is robust no matter which of the (many) duplicate DWARF copies of a
# runtime type lldb resolved. Every read returns None on failure (bad pointer,
# read error) and callers degrade gracefully.

def _process(valobj):
    try:
        return valobj.GetProcess()
    except Exception:
        return None


def _read_mem(process, addr, size):
    if process is None or not addr or size <= 0:
        return None
    err = lldb.SBError()
    try:
        data = process.ReadMemory(int(addr), int(size), err)
    except Exception:
        return None
    if err.Fail() or data is None or len(data) < size:
        return None
    return data


def _u8(process, addr):
    b = _read_mem(process, addr, 1)
    return None if b is None else b[0]


def _u32(process, addr):
    b = _read_mem(process, addr, 4)
    return None if b is None else int.from_bytes(b, "little", signed=False)


def _u64(process, addr):
    b = _read_mem(process, addr, 8)
    return None if b is None else int.from_bytes(b, "little", signed=False)


def _ptr(process, addr):
    # Pointers are 64-bit on every platform we target.
    return _u64(process, addr)


def _i32_from_u64(p):
    p &= 0xFFFFFFFF
    return p - (1 << 32) if p >= (1 << 31) else p


def _i64_from_u64(p):
    return p - (1 << 64) if p >= (1 << 63) else p


def _f64_from_bits(p):
    import struct
    return struct.unpack("<d", (p & 0xFFFFFFFFFFFFFFFF).to_bytes(8, "little"))[0]


def _f32_from_bits(p):
    import struct
    return struct.unpack("<f", (p & 0xFFFFFFFF).to_bytes(4, "little"))[0]


def _fmt_float(f):
    # Match Lin's display: integral floats print with a trailing .0 (e.g. 3 -> "3.0").
    if f != f:
        return "NaN"
    if f in (float("inf"), float("-inf")):
        return "Infinity" if f > 0 else "-Infinity"
    if f == int(f):
        return "%d.0" % int(f)
    return repr(f)


# =============================================================================
# Decode a LinString at `addr` -> python str (quoted by callers as needed).
# =============================================================================

def read_lin_string(process, addr):
    if not addr:
        return None
    length = _u32(process, addr + 4)  # len@4
    if length is None:
        return None
    if length > MAX_LEN_SANITY:
        return None  # bogus pointer / uninitialised memory
    take = min(length, MAX_STR_BYTES)
    raw = _read_mem(process, addr + 8, take) if take > 0 else b""
    if raw is None:
        return None
    try:
        s = raw.decode("utf-8", errors="replace")
    except Exception:
        s = "<invalid utf-8>"
    if length > MAX_STR_BYTES:
        s += "…"
    return s


def _quote(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


# =============================================================================
# Flat-array element widths and decoders (array.rs flat_elem_size_align @66).
# =============================================================================

_FLAT_WIDTH = {
    TAG_INT32: 4, TAG_UINT32: 4, TAG_FLOAT32: 4,
    TAG_INT64: 8, TAG_UINT64: 8, TAG_FLOAT64: 8,
    TAG_UINT8: 1, TAG_INT8: 1, TAG_BOOL: 1,
    TAG_UINT16: 2, TAG_INT16: 2,
}


def _decode_flat_elem(process, data_ptr, elem_tag, i):
    w = _FLAT_WIDTH.get(elem_tag)
    if w is None:
        return None
    b = _read_mem(process, data_ptr + i * w, w)
    if b is None:
        return None
    u = int.from_bytes(b, "little", signed=False)
    if elem_tag == TAG_INT32:
        return str(_i32_from_u64(u))
    if elem_tag == TAG_INT64:
        return str(_i64_from_u64(u))
    if elem_tag == TAG_UINT32 or elem_tag == TAG_UINT64:
        return str(u)
    if elem_tag == TAG_FLOAT32:
        return _fmt_float(_f32_from_bits(u))
    if elem_tag == TAG_FLOAT64:
        return _fmt_float(_f64_from_bits(u))
    if elem_tag == TAG_UINT8 or elem_tag == TAG_UINT16:
        return str(u)
    if elem_tag == TAG_INT8:
        return str(u - 256 if u >= 128 else u)
    if elem_tag == TAG_INT16:
        return str(u - 65536 if u >= 32768 else u)
    if elem_tag == TAG_BOOL:
        return "true" if u else "false"
    return None


# =============================================================================
# Render a value given its (tag, payload) — the core decoder.
# `payload` is the raw 64-bit payload for scalars, or a pointer for heap types.
# Returns a short logical-Lin rendering string.
# =============================================================================

def render_value(process, tag, payload, depth=0):
    if tag == TAG_NULL:
        return "null"
    if tag == TAG_BOOL:
        return "true" if (payload & 0xFF) else "false"
    if tag == TAG_INT32:
        return str(_i32_from_u64(payload))
    if tag == TAG_INT64:
        return str(_i64_from_u64(payload))
    if tag == TAG_UINT64:
        return str(payload & 0xFFFFFFFFFFFFFFFF)
    if tag == TAG_FLOAT64:
        return _fmt_float(_f64_from_bits(payload))
    if tag == TAG_FLOAT32:
        return _fmt_float(_f32_from_bits(payload))
    if tag == TAG_STR:
        s = read_lin_string(process, payload)
        return _quote(s) if s is not None else "<str @0x%x>" % payload
    if tag == TAG_ARRAY:
        return render_array(process, payload, depth)
    if tag == TAG_OBJECT:
        return render_object(process, payload, depth)
    if tag == TAG_MAP:
        return "<map @0x%x>" % payload if payload else "{}"
    if tag == TAG_FUNCTION:
        return "<function>"
    name = TAG_NAMES.get(tag, "tag %d" % tag)
    return "<%s @0x%x>" % (name, payload)


def _array_header(process, arr_ptr):
    """Return (elem_tag, length, cap, data_ptr) for a LinArray, or None."""
    if not arr_ptr:
        return None
    elem_tag = _u8(process, arr_ptr + 4)   # elem_tag@4
    length = _u64(process, arr_ptr + 8)    # len@8
    cap = _u64(process, arr_ptr + 16)      # cap@16
    data_ptr = _ptr(process, arr_ptr + 24)  # data@24
    if elem_tag is None or length is None or data_ptr is None:
        return None
    if length > MAX_LEN_SANITY:
        return None
    return (elem_tag, length, cap, data_ptr)


def render_array(process, arr_ptr, depth=0):
    if not arr_ptr:
        return "null"
    hdr = _array_header(process, arr_ptr)
    if hdr is None:
        return "<array @0x%x>" % arr_ptr
    elem_tag, length, cap, data_ptr = hdr
    if length == 0:
        return "[]"
    if depth > 3:
        return "[ … %d items ]" % length  # avoid runaway nesting
    parts = []
    shown = min(length, MAX_ELEMS)
    for i in range(shown):
        s = _render_array_elem(process, elem_tag, data_ptr, i, depth)
        parts.append(s if s is not None else "?")
    out = "[" + ", ".join(parts)
    if length > shown:
        out += ", … +%d" % (length - shown)
    return out + "]"


def _render_array_elem(process, elem_tag, data_ptr, i, depth):
    if elem_tag == TAGGED_ARRAY_TAG:
        # 16-byte LinArrayElem == TaggedVal: tag@0, payload@8.
        elem = data_ptr + i * 16
        t = _u8(process, elem)
        p = _u64(process, elem + 8)
        if t is None or p is None:
            return None
        return render_value(process, t, p, depth + 1)
    if elem_tag == SEALED_ARRAY_TAG:
        return "<sealed record>"
    return _decode_flat_elem(process, data_ptr, elem_tag, i)


def _object_header(process, obj_ptr):
    """Return (length, entries_ptr) for a LinObject, or None."""
    if not obj_ptr:
        return None
    length = _u32(process, obj_ptr + 4)       # len@4
    entries_ptr = _ptr(process, obj_ptr + 16)  # entries@16
    if length is None or entries_ptr is None:
        return None
    if length > MAX_LEN_SANITY:
        return None
    return (length, entries_ptr)


def render_object(process, obj_ptr, depth=0):
    if not obj_ptr:
        return "null"
    hdr = _object_header(process, obj_ptr)
    if hdr is None:
        return "<object @0x%x>" % obj_ptr
    length, entries_ptr = hdr
    if length == 0:
        return "{}"
    if depth > 3:
        return "{ … %d keys }" % length
    parts = []
    shown = min(length, MAX_ELEMS)
    for i in range(shown):
        kv = _read_object_entry(process, entries_ptr, i, depth)
        if kv is None:
            parts.append("?")
        else:
            k, v = kv
            parts.append("%s: %s" % (k, v))
    out = "{ " + ", ".join(parts)
    if length > shown:
        out += ", … +%d" % (length - shown)
    return out + " }"


def _read_object_entry(process, entries_ptr, i, depth):
    """LinObjectEntry is 24 bytes: key ptr@0, value TaggedVal@8 (value.tag@8, value.payload@16)."""
    entry = entries_ptr + i * 24
    key_ptr = _ptr(process, entry)
    if key_ptr is None:
        return None
    key = read_lin_string(process, key_ptr)
    key_s = _quote(key) if key is not None else "<key @0x%x>" % key_ptr
    vtag = _u8(process, entry + 8)
    vpayload = _u64(process, entry + 16)
    if vtag is None or vpayload is None:
        return None
    return (key_s, render_value(process, vtag, vpayload, depth + 1))


# =============================================================================
# Resolving the address a TaggedVal SBValue points at.
# =============================================================================
#
# A formatter is attached to either a TaggedVal (by value) or a TaggedVal* (a Lin
# Json/union local). We normalize to "the address of the 16-byte TaggedVal box":
#   * TaggedVal*  -> the pointer value is the box address (may be NULL == Lin null).
#   * TaggedVal   -> the variable's own load address is the box.

def _taggedval_box_addr(valobj):
    t = valobj.GetType()
    try:
        is_ptr = t.IsPointerType()
    except Exception:
        is_ptr = False
    if is_ptr:
        return valobj.GetValueAsUnsigned(0)  # pointer value (0 => Lin null)
    addr = valobj.GetLoadAddress()
    if addr == lldb.LLDB_INVALID_ADDRESS:
        return 0
    return addr


def _read_taggedval(process, box_addr):
    """Return (tag, payload) for the TaggedVal at box_addr. A NULL box is Lin null."""
    if not box_addr:
        return (TAG_NULL, 0)
    tag = _u8(process, box_addr)        # tag@0
    payload = _u64(process, box_addr + 8)  # payload@8
    if tag is None:
        return None
    return (tag, payload if payload is not None else 0)


def _taggedval_inline_bytes(valobj):
    """If `valobj` is a by-value TaggedVal whose 16 bytes are carried in the SBValue's own
    SBData (e.g. a synthesized child created with CreateValueFromData, which has no live load
    address), return (tag, payload). Returns None when there are no inline bytes to read."""
    t = valobj.GetType()
    try:
        if t.IsValid() and t.IsPointerType():
            return None  # pointers are resolved via the address path
    except Exception:
        pass
    err = lldb.SBError()
    data = valobj.GetData()
    if not data or not data.IsValid() or data.GetByteSize() < 16:
        return None
    tag = data.GetUnsignedInt8(err, 0)
    if err.Fail():
        return None
    payload = data.GetUnsignedInt64(err, 8)
    if err.Fail():
        return None
    return (tag & 0xFF, payload & 0xFFFFFFFFFFFFFFFF)


# =============================================================================
# lldb summary providers (the inline rendering shown next to a variable).
# =============================================================================

def taggedval_summary(valobj, internal_dict):
    process = _process(valobj)
    # A by-value TaggedVal (incl. synthesized children) carries its 16 bytes inline; read those
    # directly so it renders even without a live backing address. Pointers fall through to the
    # address path below.
    inline = _taggedval_inline_bytes(valobj)
    if inline is not None:
        return render_value(process, inline[0], inline[1])
    box = _taggedval_box_addr(valobj)
    tp = _read_taggedval(process, box)
    if tp is None:
        return "<unreadable>"
    return render_value(process, tp[0], tp[1])


def linstring_summary(valobj, internal_dict):
    process = _process(valobj)
    addr = _value_target_addr(valobj)
    s = read_lin_string(process, addr)
    return _quote(s) if s is not None else "<unreadable string>"


def linarray_summary(valobj, internal_dict):
    process = _process(valobj)
    addr = _value_target_addr(valobj)
    return render_array(process, addr)


def linobject_summary(valobj, internal_dict):
    process = _process(valobj)
    addr = _value_target_addr(valobj)
    return render_object(process, addr)


def _value_target_addr(valobj):
    """For a LinArray/LinString/LinObject SBValue (struct or pointer): the struct address."""
    t = valobj.GetType()
    try:
        if t.IsPointerType():
            return valobj.GetValueAsUnsigned(0)
    except Exception:
        pass
    addr = valobj.GetLoadAddress()
    return 0 if addr == lldb.LLDB_INVALID_ADDRESS else addr


# =============================================================================
# Synthetic-children providers (expandable tree nodes in the Variables panel).
# =============================================================================
#
# We synthesize children as raw byte data (SBData) typed as TaggedVal so the SAME
# summary provider renders each child. Children are read-only views into target
# memory; nothing is retained or mutated.

def _make_taggedval_child(valobj, name, tag, payload):
    """Create a synthetic child SBValue holding a 16-byte TaggedVal {tag, pad, payload}."""
    target = valobj.GetTarget()
    tv_type = _find_taggedval_type(target)
    if tv_type is None:
        return None
    payload &= 0xFFFFFFFFFFFFFFFF
    blob = bytes([tag & 0xFF]) + b"\x00" * 7 + payload.to_bytes(8, "little")
    err = lldb.SBError()
    data = lldb.SBData()
    data.SetData(err, blob, target.GetByteOrder(), target.GetAddressByteSize())
    if err.Fail():
        return None
    return valobj.CreateValueFromData(name, data, tv_type)


def _find_taggedval_type(target):
    for nm in ("TaggedVal", "lin_runtime::tagged::TaggedVal"):
        tl = target.FindFirstType(nm)
        if tl and tl.IsValid():
            return tl
    return None


class _ArraySynthBase(object):
    """Synthetic children for an array at `arr_addr` (resolved by subclass)."""

    def __init__(self, valobj, internal_dict):
        self.valobj = valobj
        self.process = _process(valobj)
        self.arr_addr = 0
        self.elem_tag = TAGGED_ARRAY_TAG
        self.length = 0
        self.data_ptr = 0

    def _resolve_addr(self):
        return 0  # overridden

    def update(self):
        self.process = _process(self.valobj)
        self.arr_addr = self._resolve_addr()
        hdr = _array_header(self.process, self.arr_addr) if self.arr_addr else None
        if hdr is None:
            self.length = 0
            return False
        self.elem_tag, length, _cap, self.data_ptr = hdr
        self.length = min(length, MAX_CHILDREN)
        return False

    def num_children(self):
        return int(self.length)

    def has_children(self):
        return self.length > 0

    def get_child_index(self, name):
        try:
            return int(name.lstrip("[").rstrip("]"))
        except Exception:
            return -1

    def get_child_at_index(self, index):
        if index < 0 or index >= self.length:
            return None
        name = "[%d]" % index
        if self.elem_tag == TAGGED_ARRAY_TAG:
            elem = self.data_ptr + index * 16
            t = _u8(self.process, elem)
            p = _u64(self.process, elem + 8)
            if t is None or p is None:
                return None
            return _make_taggedval_child(self.valobj, name, t, p)
        if self.elem_tag == SEALED_ARRAY_TAG:
            return None  # packed records: not individually decoded
        # Flat scalar: box it into a TaggedVal so the summary renders it.
        return self._flat_child(name, index)

    def _flat_child(self, name, index):
        et = self.elem_tag
        w = _FLAT_WIDTH.get(et)
        if w is None:
            return None
        b = _read_mem(self.process, self.data_ptr + index * w, w)
        if b is None:
            return None
        u = int.from_bytes(b, "little", signed=False)
        # Normalize flat scalar widths into a boxed TaggedVal representation.
        if et in (TAG_INT32, TAG_UINT8, TAG_INT8, TAG_UINT16, TAG_INT16):
            val = u
            if et == TAG_INT32:
                val = _i32_from_u64(u) & 0xFFFFFFFFFFFFFFFF
            elif et == TAG_INT8 and u >= 128:
                val = (u - 256) & 0xFFFFFFFFFFFFFFFF
            elif et == TAG_INT16 and u >= 32768:
                val = (u - 65536) & 0xFFFFFFFFFFFFFFFF
            return _make_taggedval_child(self.valobj, name, TAG_INT32, val)
        if et == TAG_INT64:
            return _make_taggedval_child(self.valobj, name, TAG_INT64, u)
        if et == TAG_UINT32:
            return _make_taggedval_child(self.valobj, name, TAG_INT64, u)
        if et == TAG_UINT64:
            return _make_taggedval_child(self.valobj, name, TAG_UINT64, u)
        if et == TAG_FLOAT32:
            return _make_taggedval_child(self.valobj, name, TAG_FLOAT64,
                                         _f64_bits(_f32_from_bits(u)))
        if et == TAG_FLOAT64:
            return _make_taggedval_child(self.valobj, name, TAG_FLOAT64, u)
        if et == TAG_BOOL:
            return _make_taggedval_child(self.valobj, name, TAG_BOOL, u & 1)
        return None


def _f64_bits(f):
    import struct
    return int.from_bytes(struct.pack("<d", f), "little")


def _taggedval_tag_payload(valobj, process):
    """(tag, payload) for any TaggedVal SBValue: inline-byte path for by-value/synthesized
    values, else the box-address path for pointers / addressable structs."""
    inline = _taggedval_inline_bytes(valobj)
    if inline is not None:
        return inline
    box = _taggedval_box_addr(valobj)
    return _read_taggedval(process, box)


class TaggedValArraySynth(_ArraySynthBase):
    """Synthetic children for a TaggedVal that holds an array (or a TaggedVal*)."""

    def _resolve_addr(self):
        tp = _taggedval_tag_payload(self.valobj, self.process)
        if tp is None or tp[0] != TAG_ARRAY:
            return 0
        return tp[1]


class LinArraySynth(_ArraySynthBase):
    """Synthetic children for a raw LinArray / LinArray*."""

    def _resolve_addr(self):
        return _value_target_addr(self.valobj)


class _ObjectSynthBase(object):
    """Synthetic children for an object: one named child per key."""

    def __init__(self, valobj, internal_dict):
        self.valobj = valobj
        self.process = _process(valobj)
        self.obj_addr = 0
        self.length = 0
        self.entries_ptr = 0
        self._keys = []

    def _resolve_addr(self):
        return 0  # overridden

    def update(self):
        self.process = _process(self.valobj)
        self.obj_addr = self._resolve_addr()
        self._keys = []
        hdr = _object_header(self.process, self.obj_addr) if self.obj_addr else None
        if hdr is None:
            self.length = 0
            return False
        length, self.entries_ptr = hdr
        self.length = min(length, MAX_CHILDREN)
        # Pre-read keys so get_child_index works by name.
        for i in range(int(self.length)):
            kp = _ptr(self.process, self.entries_ptr + i * 24)
            k = read_lin_string(self.process, kp) if kp else None
            self._keys.append(k if k is not None else "<key %d>" % i)
        return False

    def num_children(self):
        return int(self.length)

    def has_children(self):
        return self.length > 0

    def get_child_index(self, name):
        try:
            return self._keys.index(name)
        except ValueError:
            return -1

    def get_child_at_index(self, index):
        if index < 0 or index >= self.length:
            return None
        entry = self.entries_ptr + index * 24
        vtag = _u8(self.process, entry + 8)
        vpayload = _u64(self.process, entry + 16)
        if vtag is None or vpayload is None:
            return None
        name = self._keys[index] if index < len(self._keys) else "[%d]" % index
        return _make_taggedval_child(self.valobj, name, vtag, vpayload)


class TaggedValObjectSynth(_ObjectSynthBase):
    def _resolve_addr(self):
        tp = _taggedval_tag_payload(self.valobj, self.process)
        if tp is None or tp[0] != TAG_OBJECT:
            return 0
        return tp[1]


class LinObjectSynth(_ObjectSynthBase):
    def _resolve_addr(self):
        return _value_target_addr(self.valobj)


# A TaggedVal can hold EITHER an array or an object (or a scalar). lldb attaches one
# synthetic provider per type, so we dispatch at runtime: this provider delegates to
# the array or object synth depending on the box's tag, and is a no-op for scalars.
class TaggedValSynth(object):
    def __init__(self, valobj, internal_dict):
        self.valobj = valobj
        self.internal_dict = internal_dict
        self.delegate = None

    def update(self):
        process = _process(self.valobj)
        tp = _taggedval_tag_payload(self.valobj, process)
        tag = tp[0] if tp else TAG_NULL
        if tag == TAG_ARRAY:
            self.delegate = TaggedValArraySynth(self.valobj, self.internal_dict)
        elif tag == TAG_OBJECT:
            self.delegate = TaggedValObjectSynth(self.valobj, self.internal_dict)
        else:
            self.delegate = None
        if self.delegate is not None:
            self.delegate.update()
        return False

    def num_children(self):
        return self.delegate.num_children() if self.delegate else 0

    def has_children(self):
        return self.delegate.has_children() if self.delegate else False

    def get_child_index(self, name):
        return self.delegate.get_child_index(name) if self.delegate else -1

    def get_child_at_index(self, index):
        return self.delegate.get_child_at_index(index) if self.delegate else None


# =============================================================================
# Registration.
# =============================================================================
#
# We register against the runtime's Rust type names (what lldb actually sees in the
# DWARF emitted by the linked lin-runtime static lib) AND the bare struct names, so
# the formatters apply whether lldb reports a qualified or unqualified type. Once
# Phase 3 emits DILocalVariable/DIType for Lin locals as TaggedVal, those locals pick
# these up automatically; until then they are usable via expressions / casts (see the
# Testing recipe in editors/vscode/README or the worktree report).

_MODULE = "lin_formatters"

_TAGGEDVAL_TYPES = [
    "lin_runtime::tagged::TaggedVal",
    "TaggedVal",
]
_LINARRAY_TYPES = [
    "lin_runtime::array::LinArray",
    "LinArray",
]
_LINSTRING_TYPES = [
    "lin_runtime::string::LinString",
    "LinString",
]
_LINOBJECT_TYPES = [
    "lin_runtime::object::LinObject",
    "LinObject",
]


def __lldb_init_module(debugger, internal_dict):
    def add_summary(fn, types):
        for ty in types:
            debugger.HandleCommand(
                'type summary add -w lin -F %s.%s "%s"' % (_MODULE, fn, ty)
            )

    def add_synth(cls, types):
        for ty in types:
            debugger.HandleCommand(
                'type synthetic add -w lin -l %s.%s "%s"' % (_MODULE, cls, ty)
            )

    add_summary("taggedval_summary", _TAGGEDVAL_TYPES)
    add_summary("linstring_summary", _LINSTRING_TYPES)
    add_summary("linarray_summary", _LINARRAY_TYPES)
    add_summary("linobject_summary", _LINOBJECT_TYPES)

    add_synth("TaggedValSynth", _TAGGEDVAL_TYPES)
    add_synth("LinArraySynth", _LINARRAY_TYPES)
    add_synth("LinObjectSynth", _LINOBJECT_TYPES)

    # Enable the category so the formatters take effect.
    debugger.HandleCommand("type category enable lin")
