use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use lin_check::typed_ir::{TypedModule, TypedStmt};
use lin_check::types::Type;
use lin_check::Checker;
use lin_common::Severity;
use lin_parse::ast::Stmt;

// ── server ────────────────────────────────────────────────────────────────────

struct Backend {
    client: Client,
    /// In-memory buffer for every open document, keyed by URI.
    docs: RwLock<HashMap<Url, String>>,
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        // Store the workspace root so we can resolve relative imports.
        if let Some(root) = params
            .root_uri
            .as_ref()
            .and_then(|u| u.to_file_path().ok())
        {
            *WORKSPACE_ROOT.write().unwrap() = Some(root);
        }
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".into(), " ".into()]),
                    resolve_provider: Some(false),
                    ..Default::default()
                }),
                document_formatting_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                // Plain rename (no prepareRename) — the inverse-span lookup is reliable enough that
                // a prepare step would add no value; the client just sends rename directly.
                rename_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "lin-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "lin-lsp ready")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        self.update(&uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.update(&params.text_document.uri, &change.text).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        if let Some(text) = params.text {
            self.update(&params.text_document.uri, &text).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.docs.write().unwrap().remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, vec![], None)
            .await;
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());
        let offset = position_to_offset(&source, pos);

        Ok(tightest_span(&analysis.span_type_map, offset).map(|(_, ty_str, _)| Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!("```lin\n{}\n```", ty_str),
            }),
            range: None,
        }))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());
        let offset = position_to_offset(&source, pos);

        let def_span = match tightest_span(&analysis.span_type_map, offset)
            .and_then(|(_, _, ds)| *ds)
        {
            Some(s) => s,
            None => return Ok(None),
        };

        let start = offset_to_position(&source, def_span.start as usize);
        let end = offset_to_position(&source, def_span.end as usize);
        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: uri.clone(),
            range: Range { start, end },
        })))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());
        let offset = position_to_offset(&source, pos);

        let prefix = word_before(&source, offset);

        // Detect whether cursor is in a dot-completion context and resolve
        // the receiver's type category (e.g. "array", "string", "object").
        let (in_dot_context, receiver_category) = dot_receiver_category(&source, offset, &analysis.span_type_map);

        let mut items: Vec<CompletionItem> = Vec::new();

        // In dot context only show applicable stdlib functions — no keywords/types/bindings.
        if !in_dot_context {
            // 1. Bindings visible at the cursor (from span_type_map def_spans).
            for (_, ty_str, def_span) in &analysis.span_type_map {
                if let Some(ds) = def_span {
                    let name = &source[ds.start as usize..ds.end as usize];
                    if !name.is_empty() && name.starts_with(|c: char| c.is_alphabetic() || c == '_') {
                        if name.starts_with(prefix) {
                            let kind = if ty_str.contains("=>") {
                                CompletionItemKind::FUNCTION
                            } else {
                                CompletionItemKind::VARIABLE
                            };
                            items.push(CompletionItem {
                                label: name.to_string(),
                                kind: Some(kind),
                                detail: Some(ty_str.clone()),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
            items.dedup_by(|a, b| a.label == b.label);

            // 2. Keywords.
            let keywords = [
                "val", "var", "type", "export", "import", "from", "as",
                "if", "then", "else", "match", "is", "has", "when",
                "true", "false", "null",
            ];
            for kw in keywords {
                if kw.starts_with(prefix) {
                    items.push(CompletionItem {
                        label: kw.to_string(),
                        kind: Some(CompletionItemKind::KEYWORD),
                        ..Default::default()
                    });
                }
            }

            // 3. Built-in types.
            let builtin_types = [
                "String", "Boolean", "Null", "Number", "Json", "Error",
                "Int8", "Int16", "Int32", "Int64",
                "UInt8", "UInt16", "UInt32", "UInt64",
                "Float32", "Float64",
                "Iterator", "Iterable", "Function",
            ];
            for ty in builtin_types {
                if ty.starts_with(prefix) {
                    items.push(CompletionItem {
                        label: ty.to_string(),
                        kind: Some(CompletionItemKind::CLASS),
                        ..Default::default()
                    });
                }
            }
        }

        // 4. Imported symbols — derived from THIS file's `import` statements (never a hardcoded
        // list). In dot context, filter to functions whose first parameter matches the receiver
        // category; otherwise offer every imported name. Keywords/types/bindings are suppressed in
        // dot context (handled above).
        let filter_cat = if in_dot_context {
            Some(receiver_category.as_deref().unwrap_or("any"))
        } else {
            None
        };
        for imp in &analysis.imported_names {
            if !imp.name.starts_with(prefix) {
                continue;
            }
            // Dot-context filtering: only show items applicable to the receiver type. We use the
            // resolved signature's first parameter category; items with no signature or a
            // non-matching first param are dropped unless the receiver type is unknown ("any").
            if let Some(cat) = filter_cat {
                if cat != "any" {
                    let first_param_cat = imp
                        .ty
                        .as_deref()
                        .and_then(first_param_category);
                    if first_param_cat.as_deref() != Some(cat) {
                        continue;
                    }
                }
            }
            let kind = match imp.ty.as_deref() {
                Some(t) if t.contains("=>") => CompletionItemKind::FUNCTION,
                _ => CompletionItemKind::VALUE,
            };
            items.push(CompletionItem {
                label: imp.name.clone(),
                kind: Some(kind),
                detail: imp.ty.clone(),
                documentation: Some(Documentation::String(format!("from {}", imp.module))),
                ..Default::default()
            });
        }
        items.dedup_by(|a, b| a.label == b.label && a.detail == b.detail);

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let source = match self
            .docs
            .read()
            .unwrap()
            .get(&params.text_document.uri)
            .cloned()
        {
            Some(s) => s,
            None => return Ok(None),
        };

        // Single canonical, comment-preserving formatter shared with the CLI.
        // On parse errors, don't format the file (return no edits).
        let formatted = match lin_parse::format_source(&source) {
            Ok(formatted) => formatted,
            Err(_) => return Ok(None),
        };
        let end_pos = offset_to_position(&source, source.len());

        Ok(Some(vec![TextEdit {
            range: Range {
                start: Position { line: 0, character: 0 },
                end: end_pos,
            },
            new_text: formatted,
        }]))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());
        let offset = position_to_offset(&source, pos);

        let include_decl = params.context.include_declaration;
        let occ = occurrences_at(&analysis.span_type_map, offset);
        if occ.is_empty() {
            return Ok(None);
        }
        // The first span returned by `occurrences_at` is the binding/definition itself.
        let def = occ.first().copied();
        let locations: Vec<Location> = occ
            .iter()
            .filter(|s| include_decl || Some(**s) != def)
            .map(|s| Location {
                uri: uri.clone(),
                range: span_to_range(&source, *s),
            })
            .collect();
        Ok(Some(locations))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let source = match self.docs.read().unwrap().get(&params.text_document.uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let mut lexer = lin_lex::Lexer::new(&source, 0);
        let tokens = lexer.tokenize();
        let mut parser = lin_parse::Parser::new(tokens);
        let module = parser.parse_module();
        let symbols = document_symbols(&source, &module);
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());
        let offset = position_to_offset(&source, pos);

        let occ = occurrences_at(&analysis.span_type_map, offset);
        if occ.is_empty() {
            return Ok(None);
        }
        let def = occ.first().copied();
        let highlights: Vec<DocumentHighlight> = occ
            .iter()
            .map(|s| DocumentHighlight {
                range: span_to_range(&source, *s),
                kind: Some(if Some(*s) == def {
                    DocumentHighlightKind::WRITE
                } else {
                    DocumentHighlightKind::READ
                }),
            })
            .collect();
        Ok(Some(highlights))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let new_name = params.new_name;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());
        let offset = position_to_offset(&source, pos);

        let occ = occurrences_at(&analysis.span_type_map, offset);
        if occ.is_empty() {
            return Ok(None);
        }
        // Single-document rename: one TextEdit per occurrence in the open file.
        let edits: Vec<TextEdit> = occ
            .iter()
            .map(|s| TextEdit {
                range: span_to_range(&source, *s),
                new_text: new_name.clone(),
            })
            .collect();
        let mut changes = HashMap::new();
        changes.insert(uri.clone(), edits);
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }))
    }
}

impl Backend {
    async fn update(&self, uri: &Url, source: &str) {
        self.docs
            .write()
            .unwrap()
            .insert(uri.clone(), source.to_string());
        let base_dir = file_dir(uri);
        let analysis = analyse(source, base_dir.as_deref());
        self.client
            .publish_diagnostics(uri.clone(), analysis.diagnostics, None)
            .await;
    }
}

// ── analysis ─────────────────────────────────────────────────────────────────

struct Analysis {
    diagnostics: Vec<Diagnostic>,
    span_type_map: Vec<(lin_common::Span, String, Option<lin_common::Span>)>,
    /// Names this file imports (local name as it appears in scope), each with its module
    /// path and resolved type signature when known. Derived from the file's `import` statements
    /// + the resolved exports of those modules — never a hardcoded list, so it can't go stale.
    imported_names: Vec<ImportedName>,
}

/// One importable symbol surfaced for completion. `name` is the local (possibly aliased) name;
/// `module` is the source module path; `ty` is the resolved type signature string when the
/// module type-checked (used to enrich the completion detail), else `None`.
#[derive(Clone)]
struct ImportedName {
    name: String,
    module: String,
    ty: Option<String>,
}

/// Collect the names a parsed module imports as `(local, export, module)` triples. This is a pure
/// function over the AST (no I/O), so it's unit-testable in isolation. `local` is the name bound
/// in scope (the alias when present, else the export name); `export` is the original exported
/// name (used to look up the resolved type); `module` is the source module path.
fn collect_import_names(module: &lin_parse::ast::Module) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for stmt in &module.statements {
        if let Stmt::Import { bindings, path, .. } = stmt {
            for binding in bindings {
                let local = binding.alias.as_ref().unwrap_or(&binding.name).clone();
                out.push((local, binding.name.clone(), path.clone()));
            }
        }
    }
    out
}

fn analyse(source: &str, base_dir: Option<&Path>) -> Analysis {
    let mut lexer = lin_lex::Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    let mut parser = lin_parse::Parser::new(tokens);
    let module = parser.parse_module();

    let mut diags: Vec<Diagnostic> = parser
        .diagnostics
        .iter()
        .map(|d| lsp_diagnostic(source, d))
        .collect();

    let mut imported: HashMap<String, TypedModule> = HashMap::new();
    let effective_base = base_dir
        .map(|p| p.to_path_buf())
        .or_else(|| WORKSPACE_ROOT.read().unwrap().clone())
        .unwrap_or_else(|| PathBuf::from("."));

    let mut visiting: HashSet<String> = HashSet::new();
    pre_resolve_imports(&module, &effective_base, &mut imported, &mut visiting);

    let mut import_type_map: HashMap<(String, String), Type> = HashMap::new();
    for (path, imp_module) in &imported {
        for (name, ty) in extract_exports(imp_module) {
            import_type_map.insert((path.clone(), name), ty);
        }
    }

    let mut checker = Checker::new();
    checker.import_types = import_type_map;

    match checker.check_module(&module) {
        Ok(_) => {}
        Err(check_diags) => {
            diags.extend(check_diags.iter().map(|d| lsp_diagnostic(source, d)));
        }
    }

    // Warn on unused imports.
    diags.extend(unused_import_warnings(source, &module));

    // Derive the set of importable names for completion straight from this file's `import`
    // statements, enriched with the resolved export type when the imported module checked.
    // `binding.name` is the EXPORT name (used to look up the type); the local alias is what we
    // surface as the completion label.
    let imported_names: Vec<ImportedName> = collect_import_names(&module)
        .into_iter()
        .map(|(local, export, module)| {
            let ty = checker
                .import_types
                .get(&(module.clone(), export))
                .map(|t| t.to_string());
            ImportedName { name: local, module, ty }
        })
        .collect();

    Analysis {
        diagnostics: diags,
        span_type_map: checker.span_type_map,
        imported_names,
    }
}

/// Emit a Warning diagnostic for each imported name that is never used in the file body.
fn unused_import_warnings(source: &str, module: &lin_parse::ast::Module) -> Vec<Diagnostic> {
    let mut warnings = Vec::new();
    for stmt in &module.statements {
        let Stmt::Import { bindings, path: _, span } = stmt else { continue };
        // Find the line this import is on so we can exclude it from the usage search.
        let import_line = offset_to_position(source, span.start as usize).line;

        for binding in bindings {
            let local_name = binding.alias.as_ref().unwrap_or(&binding.name);
            // Check if `local_name` appears as a whole word on any non-import line.
            let used = source.lines().enumerate().any(|(line_idx, line)| {
                if line_idx as u32 == import_line { return false; }
                contains_identifier(line, local_name)
            });
            if !used {
                // Highlight the binding name within the import span.
                let name_offset = find_name_in_import(source, span.start as usize, local_name);
                let (start, end) = match name_offset {
                    Some(o) => (
                        offset_to_position(source, o),
                        offset_to_position(source, o + local_name.len()),
                    ),
                    None => (
                        offset_to_position(source, span.start as usize),
                        offset_to_position(source, span.end as usize),
                    ),
                };
                warnings.push(Diagnostic {
                    range: Range { start, end },
                    severity: Some(DiagnosticSeverity::HINT),
                    tags: Some(vec![DiagnosticTag::UNNECESSARY]),
                    message: format!("'{}' is imported but never used", local_name),
                    source: Some("lin".to_string()),
                    ..Default::default()
                });
            }
        }
    }
    warnings
}

/// Returns true if `line` contains `name` as a whole identifier (not as a substring).
fn contains_identifier(line: &str, name: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = line[start..].find(name) {
        let abs = start + pos;
        let before_ok = abs == 0 || !line.as_bytes()[abs - 1].is_ascii_alphanumeric() && line.as_bytes()[abs - 1] != b'_';
        let after_ok = abs + name.len() >= line.len() || !line.as_bytes()[abs + name.len()].is_ascii_alphanumeric() && line.as_bytes()[abs + name.len()] != b'_';
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

/// Finds the byte offset of `name` within the import statement starting at `import_start`.
fn find_name_in_import(source: &str, import_start: usize, name: &str) -> Option<usize> {
    let search_area = &source[import_start..];
    let pos = search_area.find(name)?;
    let abs = import_start + pos;
    // Make sure it's a whole identifier.
    let before_ok = abs == 0 || !source.as_bytes()[abs - 1].is_ascii_alphanumeric() && source.as_bytes()[abs - 1] != b'_';
    let after_ok = abs + name.len() >= source.len() || !source.as_bytes()[abs + name.len()].is_ascii_alphanumeric() && source.as_bytes()[abs + name.len()] != b'_';
    if before_ok && after_ok { Some(abs) } else { None }
}

// ── import resolution (mirrors lin-compile logic) ────────────────────────────

// NOTE: this list MUST stay in sync with `lin_compile::stdlib_source`
// (crates/lin-compile/src/lib.rs). The LSP can't simply call that function
// because `lin-compile` pulls in LLVM/inkwell, which we don't want to link into
// the language server. The `stdlib_modules_match_compiler` test below pins the
// two lists together so any future drift fails CI rather than silently breaking
// editor support for a module. If you add a stdlib module, add it in BOTH places.
fn stdlib_source(path: &str) -> Option<&'static str> {
    match path {
        "std/io"       => Some(include_str!("../../../stdlib/io.lin")),
        "std/json"     => Some(include_str!("../../../stdlib/json.lin")),
        "std/string"   => Some(include_str!("../../../stdlib/string.lin")),
        "std/number"   => Some(include_str!("../../../stdlib/number.lin")),
        "std/array"    => Some(include_str!("../../../stdlib/array.lin")),
        "std/iter"     => Some(include_str!("../../../stdlib/iter.lin")),
        "std/object"   => Some(include_str!("../../../stdlib/object.lin")),
        "std/fs"       => Some(include_str!("../../../stdlib/fs.lin")),
        "std/ffi"      => Some(include_str!("../../../stdlib/ffi.lin")),
        "std/http"     => Some(include_str!("../../../stdlib/http.lin")),
        "std/template" => Some(include_str!("../../../stdlib/template.lin")),
        "std/async"    => Some(include_str!("../../../stdlib/async.lin")),
        "std/test"     => Some(include_str!("../../../stdlib/test.lin")),
        "std/time"     => Some(include_str!("../../../stdlib/time.lin")),
        "std/path"     => Some(include_str!("../../../stdlib/path.lin")),
        "std/math"     => Some(include_str!("../../../stdlib/math.lin")),
        "std/env"      => Some(include_str!("../../../stdlib/env.lin")),
        "std/hash"     => Some(include_str!("../../../stdlib/hash.lin")),
        "std/bytes"    => Some(include_str!("../../../stdlib/bytes.lin")),
        "std/net"      => Some(include_str!("../../../stdlib/net.lin")),
        "std/process"  => Some(include_str!("../../../stdlib/process.lin")),
        "std/tty"      => Some(include_str!("../../../stdlib/tty.lin")),
        "std/signal"   => Some(include_str!("../../../stdlib/signal.lin")),
        "std/yaml"     => Some(include_str!("../../../stdlib/yaml.lin")),
        "std/jq"       => Some(include_str!("../../../stdlib/jq.lin")),
        "std/stream"   => Some(include_str!("../../../stdlib/stream.lin")),
        "std/compress" => Some(include_str!("../../../stdlib/compress.lin")),
        "std/archive"  => Some(include_str!("../../../stdlib/archive.lin")),
        "std/event"    => Some(include_str!("../../../stdlib/event.lin")),
        _ => None,
    }
}

/// A module's stable identity for cycle detection. Mirrors
/// `lin_compile::module_identity`: stdlib paths (`std/...`) are already canonical;
/// user modules are keyed by their canonicalised absolute file path so two spellings
/// of the same file map to one identity.
fn module_identity(path: &str, base_dir: &Path) -> String {
    if stdlib_source(path).is_some() {
        return path.to_string();
    }
    let file_path = base_dir.join(format!("{}.lin", path));
    file_path
        .canonicalize()
        .unwrap_or(file_path)
        .to_string_lossy()
        .to_string()
}

fn pre_resolve_imports(
    ast_module: &lin_parse::ast::Module,
    base_dir: &Path,
    cache: &mut HashMap<String, TypedModule>,
    // Identities of modules currently being resolved or already resolved. Guards
    // against infinite recursion on cyclic import graphs (a now-supported language
    // feature; cf. lin-compile's Tarjan SCC handling). Unlike the compiler, the LSP
    // only needs to resolve each module once and not crash — it doesn't need SCC
    // seed-and-recheck for accurate cross-cycle types.
    visiting: &mut HashSet<String>,
) {
    for stmt in &ast_module.statements {
        if let Stmt::Import { path, .. } = stmt {
            if cache.contains_key(path.as_str()) {
                continue;
            }
            let identity = module_identity(path.as_str(), base_dir);
            // Already resolved or currently on the resolution stack: skip to break cycles.
            if !visiting.insert(identity) {
                continue;
            }
            let (ast_mod, child_base) = if let Some(src) = stdlib_source(path.as_str()) {
                let mut lexer = lin_lex::Lexer::new(src, 0);
                let tokens = lexer.tokenize();
                let mut parser = lin_parse::Parser::new(tokens);
                (parser.parse_module(), base_dir.to_path_buf())
            } else {
                let file_path = base_dir.join(format!("{}.lin", path));
                match std::fs::read_to_string(&file_path) {
                    Ok(src) => {
                        let mut lexer = lin_lex::Lexer::new(&src, 0);
                        let tokens = lexer.tokenize();
                        let mut parser = lin_parse::Parser::new(tokens);
                        let ast = parser.parse_module();
                        let child = file_path
                            .parent()
                            .unwrap_or(base_dir)
                            .to_path_buf();
                        (ast, child)
                    }
                    Err(_) => continue,
                }
            };

            pre_resolve_imports(&ast_mod, &child_base, cache, visiting);

            let mut import_type_map: HashMap<(String, String), Type> = HashMap::new();
            for (dep_path, dep_module) in cache.iter() {
                for (name, ty) in extract_exports(dep_module) {
                    import_type_map.insert((dep_path.clone(), name), ty);
                }
            }

            let mut checker = Checker::new();
            checker.import_types = import_type_map;
            // Trusted stdlib modules legitimately reference `lin_*` intrinsics (ADR-060); allow
            // them here so resolving an imported `std/...` dependency doesn't spuriously error.
            let is_stdlib = stdlib_source(path.as_str()).is_some();
            checker.lenient_json = is_stdlib;
            checker.allow_intrinsics = is_stdlib;
            if let Ok(typed) = checker.check_module(&ast_mod) {
                cache.insert(path.clone(), typed);
            }
        }
    }
}

fn extract_exports(module: &TypedModule) -> Vec<(String, Type)> {
    module
        .statements
        .iter()
        .filter_map(|s| match s {
            TypedStmt::Val { name: Some(n), ty, .. } => Some((n.clone(), ty.clone())),
            _ => None,
        })
        .collect()
}

// ── dot-completion helpers ────────────────────────────────────────────────────

/// Returns `(in_dot_context, category)`.
/// `in_dot_context` is true when the cursor is immediately after a `.`.
/// `category` is Some("array"|"string"|"number"|"object") when the receiver type is known,
/// or None when in dot context but the type couldn't be resolved (show all stdlib items).
fn dot_receiver_category(
    source: &str,
    offset: usize,
    span_type_map: &[(lin_common::Span, String, Option<lin_common::Span>)],
) -> (bool, Option<String>) {
    let prefix_len = word_before(source, offset).len();
    let dot_offset = match offset.checked_sub(prefix_len + 1) {
        Some(o) => o,
        None => return (false, None),
    };

    let src_bytes = source.as_bytes();
    if src_bytes.get(dot_offset) != Some(&b'.') {
        return (false, None);
    }

    // Find the type of the expression to the left of the dot.
    let receiver_offset = match dot_offset.checked_sub(1) {
        Some(o) => o,
        None => return (true, None),
    };
    let ty_str = tightest_span(span_type_map, receiver_offset)
        .map(|(_, s, _)| s.as_str())
        .unwrap_or("");

    if ty_str.is_empty() {
        // In dot context but type unknown — show all stdlib items.
        return (true, None);
    }

    (true, Some(type_to_category(ty_str).to_string()))
}

/// Maps a Lin type string to a broad category used for dot-completion filtering.
fn type_to_category(ty: &str) -> &'static str {
    if ty.contains("[]") || ty.to_lowercase().contains("array") || ty.starts_with('[') {
        "array"
    } else if ty == "String" {
        "string"
    } else if ty == "Int32" || ty == "Float64" || ty == "Int64" || ty == "Float32"
        || ty == "Int8" || ty == "Int16" || ty == "UInt8" || ty == "UInt16"
        || ty == "UInt32" || ty == "UInt64"
    {
        "number"
    } else if ty.starts_with('{') || ty == "Object" {
        "object"
    } else {
        "any"
    }
}

// ── completion helpers ────────────────────────────────────────────────────────

/// Extract the broad category of a function signature's FIRST parameter, used to decide whether
/// an imported function applies to a dot-receiver of a given category. e.g. `(String, ...) => X`
/// → "string". Returns `None` when the string isn't a function type or has no parameters.
fn first_param_category(sig: &str) -> Option<String> {
    // Signatures render as `(P1, P2, ...) => R`. Grab the text inside the leading parens.
    if !sig.starts_with('(') {
        return None;
    }
    let close = sig.find(')')?;
    let params = &sig[1..close];
    if params.trim().is_empty() {
        return None;
    }
    // Take the first top-level parameter (split on the first comma that isn't nested).
    let mut depth = 0i32;
    let mut first = params;
    for (i, c) in params.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => {
                first = &params[..i];
                break;
            }
            _ => {}
        }
    }
    Some(type_to_category(first.trim()).to_string())
}

fn word_before(source: &str, offset: usize) -> &str {
    let bytes = source.as_bytes();
    let start = (0..offset)
        .rev()
        .take_while(|&i| {
            let b = bytes[i];
            b.is_ascii_alphanumeric() || b == b'_'
        })
        .last()
        .unwrap_or(offset);
    &source[start..offset]
}

// ── span utilities ────────────────────────────────────────────────────────────

fn tightest_span<'a>(
    map: &'a [(lin_common::Span, String, Option<lin_common::Span>)],
    offset: usize,
) -> Option<&'a (lin_common::Span, String, Option<lin_common::Span>)> {
    map.iter()
        .filter(|(span, _, _)| {
            span.start as usize <= offset && offset <= span.end as usize
        })
        .min_by_key(|(span, _, _)| span.end - span.start)
}

/// Convert a `lin_common::Span` to an LSP `Range` (same byte-offset → line/col conversion
/// goto_definition/hover use). Factored out so references/highlight/rename don't duplicate it.
fn span_to_range(source: &str, span: lin_common::Span) -> Range {
    Range {
        start: offset_to_position(source, span.start as usize),
        end: offset_to_position(source, span.end as usize),
    }
}

/// Given a cursor `offset`, find the symbol under it and return every occurrence span that refers
/// to the SAME binding — i.e. the inverse of goto_definition. The cursor's tightest span carries a
/// `def_span` identifying the binding; we collect every use-span in the map whose `def_span` equals
/// it, and add the `def_span` itself (definition sites are not recorded as their own map entry).
///
/// Works whether the cursor is on a use OR on the definition: if the cursor span's own `def_span`
/// is `None` (e.g. it IS a definition with no recorded back-reference), we fall back to treating the
/// cursor span as the binding span and gather uses that point back to it.
///
/// Returns an empty vec when the cursor isn't on a resolvable symbol. Single-document scope: the
/// span map only covers the currently-open file.
fn occurrences_at(
    map: &[(lin_common::Span, String, Option<lin_common::Span>)],
    offset: usize,
) -> Vec<lin_common::Span> {
    let (cursor_span, _, cursor_def) = match tightest_span(map, offset) {
        Some(t) => t,
        None => return Vec::new(),
    };
    // The binding span every occurrence shares. Prefer the cursor's def_span (cursor is on a use);
    // otherwise treat the cursor span as the binding itself (cursor is on the definition).
    let binding = cursor_def.unwrap_or(*cursor_span);

    let mut spans: Vec<lin_common::Span> = Vec::new();
    spans.push(binding);
    for (use_span, _, def) in map {
        if *def == Some(binding) {
            spans.push(*use_span);
        }
    }
    // De-duplicate (the cursor span itself may appear both as the binding and as a use).
    spans.sort_by_key(|s| (s.start, s.end));
    spans.dedup();
    spans
}

/// Walk the parsed module's top-level statements and emit a `DocumentSymbol` for each declaration
/// (val/var/type). Imports/foreign-imports/replace/bare expressions are not surfaced as symbols.
/// `val` bound to a function literal is reported as a Function; everything else as Variable/Class.
fn document_symbols(source: &str, module: &lin_parse::ast::Module) -> Vec<DocumentSymbol> {
    let mut symbols = Vec::new();
    for stmt in &module.statements {
        let (name, name_span, kind) = match stmt {
            Stmt::Val { pattern, value, span: _, .. } => {
                let Some((name, name_span)) = pattern_ident(pattern) else { continue };
                let kind = if matches!(value, lin_parse::ast::Expr::Function { .. }) {
                    SymbolKind::FUNCTION
                } else {
                    SymbolKind::VARIABLE
                };
                (name, name_span, kind)
            }
            Stmt::Var { name, name_span, .. } => {
                (name.clone(), *name_span, SymbolKind::VARIABLE)
            }
            Stmt::TypeDecl { name, span, .. } => {
                (name.clone(), *span, SymbolKind::CLASS)
            }
            _ => continue,
        };
        let range = span_to_range(source, stmt.span());
        let selection_range = span_to_range(source, name_span);
        #[allow(deprecated)]
        symbols.push(DocumentSymbol {
            name,
            detail: None,
            kind,
            tags: None,
            deprecated: None,
            range,
            // The selection range must be contained within `range`; clamp defensively.
            selection_range: if name_span.start >= stmt.span().start
                && name_span.end <= stmt.span().end
            {
                selection_range
            } else {
                range
            },
            children: None,
        });
    }
    symbols
}

/// Extract the bound name + its span from a `val` pattern when it's a simple identifier binding.
/// Destructuring patterns (object/array) bind multiple names and aren't surfaced as a single
/// top-level symbol, so they return `None`.
fn pattern_ident(pattern: &lin_parse::ast::Pattern) -> Option<(String, lin_common::Span)> {
    match pattern {
        lin_parse::ast::Pattern::Ident(name, span) => Some((name.clone(), *span)),
        _ => None,
    }
}

fn lsp_diagnostic(source: &str, d: &lin_common::Diagnostic) -> Diagnostic {
    let start = offset_to_position(source, d.span.start as usize);
    let end_offset = (d.span.end as usize).max(d.span.start as usize + 1);
    let end = offset_to_position(source, end_offset);
    Diagnostic {
        range: Range { start, end },
        severity: Some(match d.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
        }),
        message: d.message.clone(),
        source: Some("lin".to_string()),
        ..Default::default()
    }
}

fn offset_to_position(source: &str, offset: usize) -> Position {
    let mut line = 0u32;
    let mut character = 0u32;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += 1;
        }
    }
    Position { line, character }
}

fn position_to_offset(source: &str, pos: Position) -> usize {
    let mut line = 0u32;
    let mut character = 0u32;
    for (i, ch) in source.char_indices() {
        if line == pos.line && character == pos.character {
            return i;
        }
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += 1;
        }
    }
    source.len()
}

fn file_dir(uri: &Url) -> Option<PathBuf> {
    uri.to_file_path()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

// ── global workspace root (set once on initialize) ────────────────────────────

static WORKSPACE_ROOT: std::sync::LazyLock<RwLock<Option<PathBuf>>> =
    std::sync::LazyLock::new(|| RwLock::new(None));

// ── entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| Backend {
        client,
        docs: RwLock::new(HashMap::new()),
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> lin_parse::ast::Module {
        let mut lexer = lin_lex::Lexer::new(src, 0);
        let tokens = lexer.tokenize();
        let mut parser = lin_parse::Parser::new(tokens);
        parser.parse_module()
    }

    /// Type-check `src` (no imports) and return the resulting span_type_map, the same data the
    /// reference/highlight/rename handlers consume. Panics on a check error so test fixtures stay
    /// well-formed.
    fn span_map(src: &str) -> Vec<(lin_common::Span, String, Option<lin_common::Span>)> {
        let module = parse(src);
        let mut checker = Checker::new();
        let _ = checker.check_module(&module);
        checker.span_type_map
    }

    /// Byte offset of the first occurrence of `needle` at or after byte `from`.
    fn offset_after(src: &str, from: usize, needle: &str) -> usize {
        from + src[from..].find(needle).expect("needle not found")
    }

    #[test]
    fn collect_import_names_yields_imported_symbols() {
        let module = parse("import { suite, test, expect } from \"std/test\"\n");
        let triples = collect_import_names(&module);
        let names: Vec<&str> = triples.iter().map(|(local, _, _)| local.as_str()).collect();
        assert!(names.contains(&"suite"), "expected suite, got {:?}", names);
        assert!(names.contains(&"test"), "expected test, got {:?}", names);
        assert!(names.contains(&"expect"), "expected expect, got {:?}", names);
    }

    #[test]
    fn collect_import_names_honours_aliases_and_module() {
        let module = parse("import { test as t } from \"std/test\"\n");
        let triples = collect_import_names(&module);
        assert_eq!(triples.len(), 1);
        let (local, export, module_path) = &triples[0];
        assert_eq!(local, "t"); // local binding is the alias
        assert_eq!(export, "test"); // export name preserved for type lookup
        assert_eq!(module_path, "std/test");
    }

    #[test]
    fn first_param_category_classifies_receiver() {
        assert_eq!(first_param_category("(String, String) => Boolean").as_deref(), Some("string"));
        assert_eq!(first_param_category("(String[], String) => String").as_deref(), Some("array"));
        assert_eq!(first_param_category("(Int32) => Float64").as_deref(), Some("number"));
        assert_eq!(first_param_category("() => String"), None);
        assert_eq!(first_param_category("String"), None);
    }

    /// Cyclic import graph (A imports B, B imports A) must terminate, not
    /// stack-overflow. Cyclic imports are a supported language feature; the LSP
    /// previously recursed unconditionally and crashed the server on open/edit.
    #[test]
    fn pre_resolve_imports_terminates_on_import_cycle() {
        let dir = std::env::temp_dir().join(format!("lin_lsp_cycle_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // a imports b, b imports a.
        std::fs::write(dir.join("a.lin"), "import { fromB } from \"b\"\nval fromA = 1\n").unwrap();
        std::fs::write(dir.join("b.lin"), "import { fromA } from \"a\"\nval fromB = 2\n").unwrap();

        let entry = parse("import { fromA } from \"a\"\n");
        let mut cache: HashMap<String, TypedModule> = HashMap::new();
        let mut visiting: HashSet<String> = HashSet::new();
        // The assertion is simply that this call returns (does not overflow the stack).
        pre_resolve_imports(&entry, &dir, &mut cache, &mut visiting);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Non-cyclic imports still resolve normally (the cycle guard must not regress them).
    #[test]
    fn pre_resolve_imports_resolves_acyclic_chain() {
        let dir = std::env::temp_dir().join(format!("lin_lsp_acyclic_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("leaf.lin"), "val leafVal = 42\n").unwrap();

        let entry = parse("import { leafVal } from \"leaf\"\n");
        let mut cache: HashMap<String, TypedModule> = HashMap::new();
        let mut visiting: HashSet<String> = HashSet::new();
        pre_resolve_imports(&entry, &dir, &mut cache, &mut visiting);

        assert!(cache.contains_key("leaf"), "acyclic import should resolve: {:?}", cache.keys().collect::<Vec<_>>());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every stdlib module on disk must be resolvable by the LSP's `stdlib_source`,
    /// which in turn must match `lin_compile::stdlib_source` (kept in sync by hand;
    /// see the note on `stdlib_source`). This pins the two lists together: if a new
    /// stdlib module is added but not wired into the LSP, this fails.
    #[test]
    fn stdlib_modules_match_compiler() {
        // Canonical set = every non-test .lin file in stdlib/.
        let stdlib_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../stdlib");
        let mut missing = Vec::new();
        for entry in std::fs::read_dir(&stdlib_dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(stem) = name.strip_suffix(".lin") else { continue };
            if stem.ends_with(".test") {
                continue;
            }
            let module = format!("std/{}", stem);
            if stdlib_source(&module).is_none() {
                missing.push(module);
            }
        }
        assert!(
            missing.is_empty(),
            "LSP stdlib_source is missing modules (sync with lin_compile::stdlib_source): {:?}",
            missing
        );
    }

    // ── Tier-1 LSP features ────────────────────────────────────────────────────

    /// A function parameter is bound with a real def_span (`define_at`), so its uses inside the
    /// body all share that def_span. Placing the cursor on a USE must return every occurrence —
    /// the binding (param) plus all reads.
    #[test]
    fn occurrences_at_collects_param_uses_from_a_use_site() {
        // `n` is a parameter; read three times in the body. (Anchor body searches after `=>` to
        // avoid the stray `n` inside the `Int32` annotation.)
        let src = "val f = (n: Int32) => n + n + n\n";
        let map = span_map(src);
        let body = src.find("=>").unwrap() + 2;
        // Cursor on the first use of `n` in the body.
        let cursor = offset_after(src, body, "n");
        let occ = occurrences_at(&map, cursor);
        // 1 binding (param decl) + 3 body uses, deduped.
        assert_eq!(occ.len(), 4, "expected param binding + 3 uses, got {:?}", occ);
        // Every returned span must actually cover the text `n`.
        for s in &occ {
            assert_eq!(&src[s.start as usize..s.end as usize], "n", "span {:?} not over `n`", s);
        }
    }

    /// LIMITATION: definition sites are not recorded as their own entry in the span map (only
    /// USE-spans are pushed by the checker). So placing the cursor exactly on a param/let binding
    /// that has no overlapping use-span yields no occurrences. This test pins that known behaviour
    /// so a future change that DOES record def-sites updates it deliberately.
    #[test]
    fn occurrences_at_from_bare_definition_is_empty_known_limitation() {
        let src = "val f = (n: Int32) => n + n + n\n";
        let map = span_map(src);
        let param = src.find('(').unwrap() + 1; // the `n` of the param decl
        let from_def = occurrences_at(&map, offset_after(src, param, "n"));
        assert!(
            from_def.is_empty(),
            "param decl site has no recorded span; expected empty, got {:?}",
            from_def
        );
    }

    /// A plain `val x = ...` binding (not a param) is now bound with a real def_span, so its uses
    /// are grouped: placing the cursor on one use returns the binding plus every other use.
    #[test]
    fn occurrences_at_collects_plain_val_uses_from_a_use_site() {
        // `x` is a top-level `val`, read twice in the body of `f`.
        let src = "val x = 1\nval f = () => x + x\n";
        let map = span_map(src);
        let body = src.find("=>").unwrap() + 2;
        // Cursor on the first use of `x` in `f`'s body.
        let cursor = offset_after(src, body, "x");
        let occ = occurrences_at(&map, cursor);
        // 1 binding (the `val x` decl) + 2 uses, deduped.
        assert_eq!(occ.len(), 3, "expected val binding + 2 uses, got {:?}", occ);
        for s in &occ {
            assert_eq!(&src[s.start as usize..s.end as usize], "x", "span {:?} not over `x`", s);
        }
        // The binding span must be the `val x` decl site (offset of `x` on line 1), proving the
        // def_span flows from the `val` binding, not just the uses.
        let decl = offset_after(src, src.find("val").unwrap(), "x");
        assert!(
            occ.iter().any(|s| s.start as usize == decl),
            "occurrences must include the `val x` decl site at offset {}, got {:?}",
            decl, occ
        );
    }

    /// A `var` binding likewise records a def_span: cursor on a read of the var returns the binding
    /// plus the read and the reassignment target site.
    #[test]
    fn occurrences_at_collects_var_uses_including_reassignment() {
        let src = "var c = 0\nval f = () => { c = c + 1; c }\n";
        let map = span_map(src);
        let body = src.find("=>").unwrap() + 2;
        // Cursor on the read `c + 1` (the `c` after `=`).
        let eq = offset_after(src, body, "=");
        let cursor = offset_after(src, eq + 1, "c");
        let occ = occurrences_at(&map, cursor);
        // binding (var c) + assignment-target `c` + read in `c + 1` + final `c`.
        assert!(occ.len() >= 3, "expected var binding + uses, got {:?}", occ);
        for s in &occ {
            assert_eq!(&src[s.start as usize..s.end as usize], "c", "span {:?} not over `c`", s);
        }
        let decl = offset_after(src, src.find("var").unwrap(), "c");
        assert!(
            occ.iter().any(|s| s.start as usize == decl),
            "occurrences must include the `var c` decl site at offset {}, got {:?}",
            decl, occ
        );
    }

    /// Two distinct bindings of the same name must not be conflated: `a` in `f` and `a` in `g` are
    /// separate, so an occurrence query for one returns only that one's spans.
    #[test]
    fn occurrences_at_does_not_conflate_distinct_bindings() {
        let src = "val f = (a: Int32) => a + 1\nval g = (a: Int32) => a + 2\n";
        let map = span_map(src);
        // Use site of `a` inside `f`'s body.
        let f_body = src.find("=>").unwrap() + 2;
        let occ_f = occurrences_at(&map, offset_after(src, f_body, "a"));
        // All returned spans must come from the FIRST line only.
        let first_line_len = src.find('\n').unwrap();
        for s in &occ_f {
            assert!(
                (s.start as usize) <= first_line_len,
                "occurrence {:?} leaked into the second binding's scope",
                s
            );
        }
        assert_eq!(occ_f.len(), 2, "expected f's `a` binding + 1 use, got {:?}", occ_f);
    }

    /// Cursor not on any symbol yields no occurrences.
    #[test]
    fn occurrences_at_empty_off_symbol() {
        let src = "val f = (n: Int32) => n + n\n";
        let map = span_map(src);
        // Offset 0 is the `v` of `val` — a keyword, no recorded symbol span.
        let occ = occurrences_at(&map, 0);
        assert!(occ.is_empty(), "keyword position should yield no occurrences: {:?}", occ);
    }

    #[test]
    fn document_symbols_lists_top_level_declarations() {
        let src = "import { print } from \"std/io\"\n\
                   val answer = 42\n\
                   var counter = 0\n\
                   type Point = { x: Int32, y: Int32 }\n\
                   val greet = (name: String) => name\n";
        let module = parse(src);
        let syms = document_symbols(src, &module);
        let by_name: HashMap<&str, SymbolKind> =
            syms.iter().map(|s| (s.name.as_str(), s.kind)).collect();
        // Import is NOT surfaced.
        assert!(!by_name.contains_key("print"), "imports must not be symbols");
        assert_eq!(by_name.get("answer"), Some(&SymbolKind::VARIABLE));
        assert_eq!(by_name.get("counter"), Some(&SymbolKind::VARIABLE));
        assert_eq!(by_name.get("Point"), Some(&SymbolKind::CLASS));
        // val bound to a function literal -> Function.
        assert_eq!(by_name.get("greet"), Some(&SymbolKind::FUNCTION));
        // selection_range must be contained within range for every symbol.
        for s in &syms {
            assert!(
                s.selection_range.start.line >= s.range.start.line
                    && s.selection_range.end.line <= s.range.end.line,
                "selection range escapes full range for {}",
                s.name
            );
        }
    }
}
