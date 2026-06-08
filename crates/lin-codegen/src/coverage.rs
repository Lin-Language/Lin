//! LLVM source-based coverage instrumentation.
//!
//! Emits the globals required for `llvm-profdata merge` + `llvm-cov export --format=lcov`:
//!   __profc_<fn>        [n x i64] counter array  (__llvm_prf_cnts)
//!   __profd_<fn>        profile data struct       (__llvm_prf_data)
//!   __covrec_<hash>     function coverage record  (__llvm_covfun)
//!   __llvm_coverage_mapping  filenames + header   (__llvm_covmap)
//!   __llvm_prf_nm       compressed function names (__llvm_prf_names)
//!
//! Reference: LLVM Coverage Mapping Format (version 6, LLVM 13+).

use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::values::GlobalValue;
use inkwell::AddressSpace;
use std::io::Write as IoWrite;

/// Convert a byte `offset` in `src` to a 1-indexed (line, col). Shared by the coverage region
/// emitter and the `--debug` DWARF line-table emitter (so byte-offset → line mapping is identical
/// in both). Column counts Unicode scalar values, matching the existing coverage behaviour.
pub fn offset_to_line_col(src: &str, offset: u32) -> (u32, u32) {
    let offset = offset as usize;
    let mut line = 1u32;
    let mut col = 1u32;
    for (i, ch) in src.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// A single coverage region: a source span mapped to one profile counter.
pub struct Region {
    /// Index of the counter (in this function's `__profc` array) that backs this region.
    pub counter: u32,
    /// Region start line (1-indexed).
    pub start_line: u32,
    /// Region start column (1-indexed).
    pub start_col: u32,
    /// Region end line (1-indexed).
    pub end_line: u32,
    /// Region end column (1-indexed).
    pub end_col: u32,
}

/// Per-function information needed to emit coverage globals.
pub struct FnCovInfo {
    pub name: String,
    /// Index into the source files list (for multi-file coverage).
    pub file_idx: u32,
    /// Coverage regions for this function, one profile counter each.
    pub regions: Vec<Region>,
}

/// State accumulated while compiling a coverage-instrumented module.
pub struct CoverageEmitter<'ctx> {
    /// All source files tracked in this module (absolute paths).
    /// Index 0 is the main source file.
    pub source_files: Vec<String>,
    /// Source texts corresponding to source_files (for converting byte offsets to line/col).
    pub source_texts: Vec<String>,
    functions: Vec<(FnCovInfo, GlobalValue<'ctx>)>, // (info, profc_global)
    /// Globals to add to llvm.compiler.used (profd entries).
    compiler_used: Vec<GlobalValue<'ctx>>,
}

impl<'ctx> CoverageEmitter<'ctx> {
    pub fn new(source_path: String) -> Self {
        Self {
            source_files: vec![source_path],
            source_texts: vec![String::new()],
            functions: Vec::new(),
            compiler_used: Vec::new(),
        }
    }

    /// Add an additional source file and return its index.
    /// If the file is already tracked, returns its existing index.
    pub fn add_source_file(&mut self, path: &str, text: &str) -> u32 {
        if let Some(idx) = self.source_files.iter().position(|p| p == path) {
            return idx as u32;
        }
        let idx = self.source_files.len() as u32;
        self.source_files.push(path.to_string());
        self.source_texts.push(text.to_string());
        idx
    }

    /// Look up an already-tracked source file's index by absolute path. Used to attribute a
    /// cross-module monomorphized specialization's regions to its generic-definition source file
    /// (which was registered when that module was compiled as an import, before the importer).
    pub fn file_idx_of(&self, path: &str) -> Option<u32> {
        self.source_files.iter().position(|p| p == path).map(|i| i as u32)
    }

    /// Convert a byte offset in the given source file to (line, col), both 1-indexed.
    pub fn offset_to_line_col_in(&self, file_idx: usize, offset: u32) -> (u32, u32) {
        let src = self.source_texts.get(file_idx).map(|s| s.as_str()).unwrap_or("");
        offset_to_line_col(src, offset)
    }

    /// Emit `__profc_<name>` and `__profd_<name>` globals for a function with N regions.
    /// Returns the `__profc` global so the caller can emit counter increments, or `None`
    /// if the function has no coverage regions (in which case no globals are emitted).
    pub fn emit_function_globals(
        &mut self,
        context: &'ctx Context,
        module: &Module<'ctx>,
        info: FnCovInfo,
    ) -> Option<GlobalValue<'ctx>> {
        let n = info.regions.len() as u32;
        if n == 0 {
            return None;
        }

        let i64_type = context.i64_type();
        let i32_type = context.i32_type();
        let i16_type = context.i16_type();
        let ptr_type = context.ptr_type(AddressSpace::default());

        // __profc_<name> = [n x i64] zeroinitializer, section "__llvm_prf_cnts", comdat, align 8
        let profc_name = format!("__profc_{}", info.name);
        let counter_type = i64_type.array_type(n);
        let profc = module.add_global(counter_type, None, &profc_name);
        profc.set_initializer(&counter_type.const_zero());
        profc.set_section(Some("__llvm_prf_cnts"));
        profc.set_alignment(8);
        profc.set_linkage(Linkage::Private);
        {
            use inkwell::comdat::ComdatSelectionKind;
            let comdat = module.get_or_insert_comdat(&profc_name);
            comdat.set_selection_kind(ComdatSelectionKind::NoDuplicates);
            profc.set_comdat(comdat);
        }

        // __profd_<name>: { i64 fn_hash, i64 cfg_hash, i64 counter_delta, i64 0,
        //                   ptr null, ptr null, i32 num_counters, [3 x i16] zeros, i32 0 }
        let fn_hash = md5_first_8_le(&info.name);
        let cfg_hash: i64 = 24; // fixed value used by clang for simple single-block functions

        let profd_name = format!("__profd_{}", info.name);
        let profd_struct_type = context.struct_type(
            &[
                i64_type.into(), // fn_hash
                i64_type.into(), // cfg_hash
                i64_type.into(), // counter_delta (ptrtoint(profc) - ptrtoint(profd))
                i64_type.into(), // bitmap_delta (0)
                ptr_type.into(), // name_ptr (null)
                ptr_type.into(), // value_sites_ptr (null)
                i32_type.into(), // num_counters
                i16_type.array_type(3).into(), // num_value_kinds (zeros)
                i32_type.into(), // padding
            ],
            false,
        );

        let profd = module.add_global(profd_struct_type, None, &profd_name);
        profd.set_section(Some("__llvm_prf_data"));
        profd.set_alignment(8);
        profd.set_linkage(Linkage::Private);
        {
            use inkwell::comdat::ComdatSelectionKind;
            let comdat = module.get_or_insert_comdat(&profc_name);
            comdat.set_selection_kind(ComdatSelectionKind::NoDuplicates);
            profd.set_comdat(comdat);
        }

        // counter_delta = ptrtoint(profc) - ptrtoint(profd)
        let profc_ptr = profc.as_pointer_value();
        let profd_ptr = profd.as_pointer_value();
        let profc_int = inkwell::values::PointerValue::const_to_int(profc_ptr, i64_type);
        let profd_int = inkwell::values::PointerValue::const_to_int(profd_ptr, i64_type);
        let counter_delta = inkwell::values::IntValue::const_sub(profc_int, profd_int);

        let profd_init = profd_struct_type.const_named_struct(&[
            i64_type.const_int(fn_hash as u64, false).into(),
            i64_type.const_int(cfg_hash as u64, false).into(),
            counter_delta.into(),
            i64_type.const_int(0, false).into(),
            ptr_type.const_null().into(),
            ptr_type.const_null().into(),
            i32_type.const_int(n as u64, false).into(), // num_counters = n
            i16_type.array_type(3).const_zero().into(),
            i32_type.const_int(0, false).into(),
        ]);
        profd.set_initializer(&profd_init);

        self.compiler_used.push(profd);
        self.functions.push((info, profc));
        Some(profc)
    }

    /// After all functions are compiled, emit the module-level coverage globals.
    pub fn finalize(self, context: &'ctx Context, module: &Module<'ctx>) {
        if self.functions.is_empty() {
            return;
        }

        let i64_type = context.i64_type();
        let i32_type = context.i32_type();

        // 1. Build and emit the filenames section.
        let filenames_bytes = encode_filenames(&self.source_files);
        let filenames_hash = md5_first_8_le_bytes(&filenames_bytes);
        let filenames_len = filenames_bytes.len() as u32;

        // 2. For each function, emit __covrec_<hash>.
        let mut covrec_globals: Vec<GlobalValue<'ctx>> = Vec::new();
        for (info, _profc) in &self.functions {
            let fn_hash = md5_first_8_le(&info.name);
            let covmap_bytes = encode_fn_covmap(
                info.file_idx, // global filename index for this function
                &info.regions,
            );
            let covmap_len = covmap_bytes.len() as u32;

            let fn_hash_unsigned = fn_hash as u64;
            let covrec_name = format!("__covrec_{:X}u", fn_hash_unsigned);

            let bytes_array_type = context.i8_type().array_type(covmap_len);
            // CovMapFunctionRecordV3 layout (LLVM v3+ / format version 6, PACKED):
            //   NameRef (i64)       = MD5 first 8 bytes LE of the function name
            //   DataSize (u32)      = byte length of CoverageMapping data
            //   FuncHash (i64)      = CFG hash (24 for single-BB functions)
            //   FilenamesRef (i64)  = MD5 first 8 bytes LE of the encoded filenames section
            //   CoverageMapping     = coverage data bytes
            // MUST be packed: i64 follows i32 at offset 12 (unaligned) per LLVM spec.
            let covrec_struct_type = context.struct_type(
                &[
                    i64_type.into(),          // NameRef
                    i32_type.into(),          // DataSize
                    i64_type.into(),          // FuncHash (cfg_hash)
                    i64_type.into(),          // FilenamesRef
                    bytes_array_type.into(),  // CoverageMapping bytes
                ],
                true, // packed — no padding between DataSize (i32) and FuncHash (i64)
            );

            let covrec_bytes_arr = context.const_string(&covmap_bytes, false);

            let cfg_hash: u64 = 24; // fixed CFG hash for simple single-BB functions

            let covrec_init = covrec_struct_type.const_named_struct(&[
                i64_type.const_int(fn_hash_unsigned, false).into(),
                i32_type.const_int(covmap_len as u64, false).into(),
                i64_type.const_int(cfg_hash, false).into(),
                i64_type.const_int(filenames_hash as u64, false).into(),
                covrec_bytes_arr.into(),
            ]);

            let covrec = module.add_global(covrec_struct_type, None, &covrec_name);
            covrec.set_initializer(&covrec_init);
            covrec.set_section(Some("__llvm_covfun"));
            covrec.set_alignment(8);
            covrec.set_linkage(Linkage::LinkOnceODR);
            covrec.set_visibility(inkwell::GlobalVisibility::Hidden);
            {
                use inkwell::comdat::ComdatSelectionKind;
                let comdat = module.get_or_insert_comdat(&covrec_name);
                comdat.set_selection_kind(ComdatSelectionKind::Any);
                covrec.set_comdat(comdat);
            }

            covrec_globals.push(covrec);
        }

        // 3. Emit __llvm_coverage_mapping.
        let header_type = context.struct_type(
            &[i32_type.into(), i32_type.into(), i32_type.into(), i32_type.into()],
            false,
        );
        let header_init = header_type.const_named_struct(&[
            i32_type.const_int(0, false).into(),
            i32_type.const_int(filenames_len as u64, false).into(),
            i32_type.const_int(0, false).into(),
            i32_type.const_int(6, false).into(), // format version 6
        ]);
        let filenames_array_type = context.i8_type().array_type(filenames_len);
        let filenames_arr = context.const_string(&filenames_bytes, false);
        let covmap_struct_type = context.struct_type(
            &[header_type.into(), filenames_array_type.into()],
            false,
        );
        let covmap_init = covmap_struct_type.const_named_struct(&[
            header_init.into(),
            filenames_arr.into(),
        ]);
        let covmap_global = module.add_global(covmap_struct_type, None, "__llvm_coverage_mapping");
        covmap_global.set_initializer(&covmap_init);
        covmap_global.set_section(Some("__llvm_covmap"));
        covmap_global.set_alignment(8);
        covmap_global.set_linkage(Linkage::Private);

        // 4. Emit __llvm_prf_nm.
        let prf_nm_bytes = encode_prf_names(self.functions.iter().map(|(i, _)| i.name.as_str()));
        let nm_len = prf_nm_bytes.len() as u32;
        let nm_arr = context.const_string(&prf_nm_bytes, false);
        let nm_arr_type = context.i8_type().array_type(nm_len);
        let prf_nm = module.add_global(nm_arr_type, None, "__llvm_prf_nm");
        prf_nm.set_initializer(&nm_arr);
        prf_nm.set_section(Some("__llvm_prf_names"));
        prf_nm.set_alignment(1);
        prf_nm.set_linkage(Linkage::Private);

        // Emit llvm.used: covmap + covrecs + prf_nm
        let mut llvm_used: Vec<GlobalValue<'ctx>> = Vec::new();
        llvm_used.extend(covrec_globals);
        llvm_used.push(covmap_global);
        llvm_used.push(prf_nm);
        emit_metadata_global(context, module, "llvm.used", "llvm.metadata", &llvm_used);

        // Emit llvm.compiler.used: profd entries
        if !self.compiler_used.is_empty() {
            emit_metadata_global(context, module, "llvm.compiler.used", "llvm.metadata", &self.compiler_used);
        }
    }
}

// ---------------------------------------------------------------------------
// Encoding helpers
// ---------------------------------------------------------------------------

/// LLVM coverage format function hash: first 8 bytes of MD5(name), little-endian i64.
pub fn md5_first_8_le(name: &str) -> i64 {
    md5_first_8_le_bytes(name.as_bytes())
}

pub fn md5_first_8_le_bytes(data: &[u8]) -> i64 {
    let digest = md5::compute(data);
    i64::from_le_bytes(digest.0[..8].try_into().unwrap())
}

fn write_uleb128(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (n & 0x7F) as u8;
        n >>= 7;
        if n != 0 {
            out.push(byte | 0x80);
        } else {
            out.push(byte);
            break;
        }
    }
    out
}

/// Encode the filenames section bytes for the coverage map.
fn encode_filenames(filenames: &[String]) -> Vec<u8> {
    let mut raw = Vec::new();
    for name in filenames {
        let b = name.as_bytes();
        raw.extend_from_slice(&write_uleb128(b.len() as u64));
        raw.extend_from_slice(b);
    }

    let mut compressed = Vec::new();
    {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        let mut enc = ZlibEncoder::new(&mut compressed, Compression::best());
        enc.write_all(&raw).expect("zlib compress");
        enc.finish().expect("zlib finish");
    }

    let mut out = Vec::new();
    out.extend_from_slice(&write_uleb128(filenames.len() as u64));
    out.extend_from_slice(&write_uleb128(raw.len() as u64)); // uncompressed length (required by LLVM)
    out.extend_from_slice(&write_uleb128(compressed.len() as u64));
    out.extend_from_slice(&compressed);
    out
}

/// Encode the coverage mapping bytes for a single function with N counter regions.
/// Format: [num_vfiles][vfile_0_global_idx][num_expressions][num_regions_for_file_0]
///         then per region (sorted by start line/col, with line deltas relative to the
///         previous region): [counter_encoded][delta_start_line][start_col][delta_end_line][end_col]
pub fn encode_fn_covmap(
    global_filename_idx: u32,
    regions: &[Region],
) -> Vec<u8> {
    // Regions must be emitted in source order (by start line, then column).
    let mut sorted: Vec<&Region> = regions.iter().collect();
    sorted.sort_by_key(|r| (r.start_line, r.start_col));

    let mut out = Vec::new();
    out.extend_from_slice(&write_uleb128(1));                          // num_virtual_file_ids = 1
    out.extend_from_slice(&write_uleb128(global_filename_idx as u64)); // vfile[0] = global filename index
    out.extend_from_slice(&write_uleb128(0));                          // num_counter_expressions = 0
    out.extend_from_slice(&write_uleb128(sorted.len() as u64));        // num_regions for file 0

    let mut prev_line: u32 = 0;
    for r in sorted {
        // counter = local index `counter`, kind=1: (counter << 2) | 1
        out.extend_from_slice(&write_uleb128(((r.counter << 2) | 1) as u64));
        out.extend_from_slice(&write_uleb128((r.start_line - prev_line) as u64)); // delta from prev region's start line
        out.extend_from_slice(&write_uleb128(r.start_col as u64));
        let delta_end = r.end_line.saturating_sub(r.start_line);
        out.extend_from_slice(&write_uleb128(delta_end as u64));
        out.extend_from_slice(&write_uleb128(r.end_col as u64));
        prev_line = r.start_line;
    }
    out
}

/// Encode `__llvm_prf_nm` bytes: compressed function names joined by `\x01`.
fn encode_prf_names<'a>(names: impl Iterator<Item = &'a str>) -> Vec<u8> {
    let names_vec: Vec<&str> = names.collect();
    if names_vec.is_empty() {
        return Vec::new();
    }
    let raw: Vec<u8> = names_vec.join("\x01").into_bytes();
    let mut compressed = Vec::new();
    {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        let mut enc = ZlibEncoder::new(&mut compressed, Compression::best());
        enc.write_all(&raw).expect("zlib compress names");
        enc.finish().expect("zlib finish names");
    }
    let mut out = Vec::new();
    out.extend_from_slice(&write_uleb128(raw.len() as u64));
    out.extend_from_slice(&write_uleb128(compressed.len() as u64));
    out.extend_from_slice(&compressed);
    out
}

// ---------------------------------------------------------------------------
// LLVM metadata helpers
// ---------------------------------------------------------------------------

/// Emit an appending global (e.g. `llvm.used`) containing pointers to `globals`.
fn emit_metadata_global<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    global_name: &str,
    section: &str,
    globals: &[GlobalValue<'ctx>],
) {
    if globals.is_empty() {
        return;
    }
    let ptr_type = context.ptr_type(AddressSpace::default());
    let ptrs: Vec<inkwell::values::PointerValue<'ctx>> = globals
        .iter()
        .map(|g| g.as_pointer_value())
        .collect();
    let count = ptrs.len() as u32;
    let arr_init = ptr_type.const_array(&ptrs);
    let arr_type = ptr_type.array_type(count);
    let g = module.add_global(arr_type, None, global_name);
    g.set_initializer(&arr_init);
    g.set_section(Some(section));
    g.set_linkage(Linkage::Appending);
}
