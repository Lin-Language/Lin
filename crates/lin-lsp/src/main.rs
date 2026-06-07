use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

use lin_check::typed_ir::{TypedExpr, TypedModule, TypedStmt};
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
                // Inlay type hints on bindings whose type is inferred (no explicit annotation).
                inlay_hint_provider: Some(OneOf::Left(true)),
                // Quick-fixes derived from existing diagnostics (unused imports, did-you-mean).
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                // Signature help inside a call's argument list, triggered by `(` and `,`.
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".into(), ",".into()]),
                    retrigger_characters: Some(vec![",".into()]),
                    work_done_progress_options: Default::default(),
                }),
                // Type-aware semantic highlighting (full-document only; the TextMate grammar stays
                // the fallback for incremental edits).
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                        legend: semantic_tokens_legend(),
                        full: Some(SemanticTokensFullOptions::Bool(true)),
                        range: Some(false),
                        work_done_progress_options: Default::default(),
                    }),
                ),
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

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());

        // Only emit hints whose anchor falls inside the requested range — clients re-request as the
        // viewport scrolls, so honouring the range keeps the response small.
        let range = params.range;
        let hints: Vec<InlayHint> = inlay_hints(&source, &analysis)
            .into_iter()
            .filter(|h| position_in_range(h.position, range))
            .collect();
        Ok(Some(hints))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = &params.text_document.uri;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());

        let data = semantic_tokens(&source, &analysis);
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());
        let offset = position_to_offset(&source, pos);

        Ok(signature_help(&source, &analysis, offset))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };

        let actions = code_actions(&source, uri, &params);
        if actions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(actions))
        }
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
    /// The parsed surface AST. Retained so inlay-hint/signature-help handlers can walk binding
    /// patterns and call expressions without re-parsing.
    module: lin_parse::ast::Module,
    /// The type-checked module, when checking succeeded. `None` on a hard check error. Used by the
    /// inlay-hint handler to look up inferred binding types and by semantic tokens.
    typed: Option<TypedModule>,
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

    let typed = match checker.check_module(&module) {
        Ok(typed) => Some(typed),
        Err(check_diags) => {
            diags.extend(check_diags.iter().map(|d| lsp_diagnostic(source, d)));
            None
        }
    };

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
        module,
        typed,
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

// ── semantic tokens ────────────────────────────────────────────────────────────

// Token-type indices into the legend below. Kept as `u32` constants so the encoder and the legend
// can't drift out of step. The order here IS the legend order the client uses to decode.
const ST_VARIABLE: u32 = 0;
const ST_PARAMETER: u32 = 1;
const ST_FUNCTION: u32 = 2;
const ST_TYPE: u32 = 3;
const ST_NAMESPACE: u32 = 4;

/// The legend advertised in `initialize`. The index of each token type in this list is the number
/// the delta-encoded token stream refers to (see `ST_*` constants).
fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::VARIABLE,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::TYPE,
            // `namespace` highlights imported symbols (the editor can dim/colour them distinctly).
            SemanticTokenType::NAMESPACE,
        ],
        token_modifiers: vec![],
    }
}

/// Classify every identifier span in the document and emit LSP semantic tokens (delta-encoded).
///
/// Sources, in precedence order:
///   1. AST function parameters → `parameter` (definition sites + every use that resolves to one).
///   2. Type-annotation identifiers (`TypeExpr::Named`/`Generic`) → `type`.
///   3. Import-binding module use-sites whose resolved type is a namespace-like value are not
///      separable from functions here, so imported names are classified by their type like any use.
///   4. Remaining `span_type_map` use-sites → `function` when the type renders as a function
///      (`=>`), else `variable`.
///
/// Multi-line spans are skipped entirely (LSP forbids a token crossing a line). The result is
/// sorted by (line, char) and delta-encoded per the spec.
fn semantic_tokens(source: &str, analysis: &Analysis) -> Vec<SemanticToken> {
    // (start -> (start, end, token_type)). A span may be produced by more than one source; the
    // FIRST inserted for a given start offset wins (parameters/types take precedence over the
    // generic use-site pass).
    let mut by_start: HashMap<u32, (u32, u32, u32)> = HashMap::new();

    // 1. Parameter definition sites, and the set of param def-spans (to classify their uses).
    let mut param_defs: HashSet<(u32, u32)> = HashSet::new();
    collect_param_spans(&analysis.module.statements, &mut param_defs);
    for (start, end) in &param_defs {
        by_start.entry(*start).or_insert((*start, *end, ST_PARAMETER));
    }

    // 2. Type-annotation identifiers.
    let mut type_spans: Vec<lin_common::Span> = Vec::new();
    collect_type_spans(&analysis.module.statements, &mut type_spans);
    for span in &type_spans {
        by_start.entry(span.start).or_insert((span.start, span.end, ST_TYPE));
    }

    // 3. Imported local names — used to flag a use-site as a `namespace` (an imported symbol).
    let imported: HashSet<&str> = analysis
        .imported_names
        .iter()
        .map(|i| i.name.as_str())
        .collect();

    // 4. Generic identifier use-sites from the type map. Precedence: a use whose def_span is a
    // parameter is a `parameter`; an imported name is a `namespace`; a function-typed use is a
    // `function`; everything else is a `variable`.
    for (use_span, ty_str, def_span) in &analysis.span_type_map {
        let text = source
            .get(use_span.start as usize..use_span.end as usize)
            .unwrap_or("");
        let kind = if def_span
            .map(|d| param_defs.contains(&(d.start, d.end)))
            .unwrap_or(false)
        {
            ST_PARAMETER
        } else if imported.contains(text) {
            ST_NAMESPACE
        } else if ty_str.contains("=>") {
            ST_FUNCTION
        } else {
            ST_VARIABLE
        };
        by_start
            .entry(use_span.start)
            .or_insert((use_span.start, use_span.end, kind));
    }

    // Drop anything that crosses a line boundary, convert to positions, sort.
    // Track the source end-offset alongside so we can drop overlapping tokens after sorting
    // (a `Generic` head span, say `Foo<Bar>`, contains its argument spans — LSP tokens must not
    // overlap, so the containing token is dropped in favour of the more specific inner ones).
    let mut tokens: Vec<(u32, u32, u32, u32, u32, u32)> = Vec::new(); // (line, char, len, type, start_off, end_off)
    for (start_off, end_off, kind) in by_start.values() {
        let start = offset_to_position(source, *start_off as usize);
        let end = offset_to_position(source, *end_off as usize);
        if start.line != end.line {
            continue; // LSP: a semantic token cannot span lines.
        }
        let len = end.character.saturating_sub(start.character);
        if len == 0 {
            continue;
        }
        tokens.push((start.line, start.character, len, *kind, *start_off, *end_off));
    }
    tokens.sort_by_key(|(line, ch, _, _, _, _)| (*line, *ch));

    // Delta-encode per the LSP spec, skipping any token whose source range overlaps the previous
    // emitted token (keeps the stream non-overlapping as LSP requires).
    let mut data = Vec::with_capacity(tokens.len());
    let mut prev_line = 0u32;
    let mut prev_char = 0u32;
    let mut prev_end_off = 0u32;
    for (line, ch, len, kind, start_off, end_off) in tokens {
        if start_off < prev_end_off {
            continue; // overlaps the previously-emitted token; drop it.
        }
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 { ch - prev_char } else { ch };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length: len,
            token_type: kind,
            token_modifiers_bitset: 0,
        });
        prev_line = line;
        prev_char = ch;
        prev_end_off = end_off;
    }
    data
}

/// Collect parameter identifier spans from every function literal in the module (recursively).
fn collect_param_spans(stmts: &[Stmt], out: &mut HashSet<(u32, u32)>) {
    for stmt in stmts {
        match stmt {
            Stmt::Val { value, .. } | Stmt::Var { value, .. } => {
                collect_param_spans_in_expr(value, out);
            }
            Stmt::Expr(e) => collect_param_spans_in_expr(e, out),
            _ => {}
        }
    }
}

fn collect_param_spans_in_expr(expr: &lin_parse::ast::Expr, out: &mut HashSet<(u32, u32)>) {
    use lin_parse::ast::Expr as E;
    match expr {
        E::Function { params, body, .. } => {
            for p in params {
                if let Some((_, span)) = pattern_ident(&p.pattern) {
                    out.insert((span.start, span.end));
                }
            }
            collect_param_spans_in_expr(body, out);
        }
        E::Block(stmts, tail, _) => {
            collect_param_spans(stmts, out);
            collect_param_spans_in_expr(tail, out);
        }
        E::If { condition, then_branch, else_branch, .. } => {
            collect_param_spans_in_expr(condition, out);
            collect_param_spans_in_expr(then_branch, out);
            collect_param_spans_in_expr(else_branch, out);
        }
        E::Match { scrutinee, arms, .. } => {
            collect_param_spans_in_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_param_spans_in_expr(g, out);
                }
                collect_param_spans_in_expr(&arm.body, out);
            }
        }
        E::Call { func, args, .. } => {
            collect_param_spans_in_expr(func, out);
            for a in args {
                collect_param_spans_in_expr(a, out);
            }
        }
        E::DotCall { receiver, args, .. } => {
            collect_param_spans_in_expr(receiver, out);
            if let Some(args) = args {
                for a in args {
                    collect_param_spans_in_expr(a, out);
                }
            }
        }
        E::BinaryOp { left, right, .. } => {
            collect_param_spans_in_expr(left, out);
            collect_param_spans_in_expr(right, out);
        }
        E::UnaryOp { operand, .. } => collect_param_spans_in_expr(operand, out),
        E::Assign { value, .. } => collect_param_spans_in_expr(value, out),
        _ => {}
    }
}

/// Collect the spans of identifiers that appear in TYPE position (annotations, return types,
/// generic arguments) so they can be highlighted as `type`.
fn collect_type_spans(stmts: &[Stmt], out: &mut Vec<lin_common::Span>) {
    for stmt in stmts {
        match stmt {
            Stmt::Val { type_ann, value, .. } => {
                if let Some(t) = type_ann {
                    collect_type_spans_in_type(t, out);
                }
                collect_type_spans_in_expr(value, out);
            }
            Stmt::Var { type_ann, value, .. } => {
                if let Some(t) = type_ann {
                    collect_type_spans_in_type(t, out);
                }
                collect_type_spans_in_expr(value, out);
            }
            Stmt::TypeDecl { body, .. } => collect_type_spans_in_type(body, out),
            Stmt::Expr(e) => collect_type_spans_in_expr(e, out),
            _ => {}
        }
    }
}

fn collect_type_spans_in_expr(expr: &lin_parse::ast::Expr, out: &mut Vec<lin_common::Span>) {
    use lin_parse::ast::Expr as E;
    match expr {
        E::Function { params, return_type, body, .. } => {
            for p in params {
                if let Some(t) = &p.type_ann {
                    collect_type_spans_in_type(t, out);
                }
            }
            if let Some(rt) = return_type {
                collect_type_spans_in_type(rt, out);
            }
            collect_type_spans_in_expr(body, out);
        }
        E::Block(stmts, tail, _) => {
            collect_type_spans(stmts, out);
            collect_type_spans_in_expr(tail, out);
        }
        E::If { condition, then_branch, else_branch, .. } => {
            collect_type_spans_in_expr(condition, out);
            collect_type_spans_in_expr(then_branch, out);
            collect_type_spans_in_expr(else_branch, out);
        }
        E::Match { scrutinee, arms, .. } => {
            collect_type_spans_in_expr(scrutinee, out);
            for arm in arms {
                collect_type_spans_in_expr(&arm.body, out);
            }
        }
        E::Call { func, args, .. } => {
            collect_type_spans_in_expr(func, out);
            for a in args {
                collect_type_spans_in_expr(a, out);
            }
        }
        E::DotCall { receiver, args, .. } => {
            collect_type_spans_in_expr(receiver, out);
            if let Some(args) = args {
                for a in args {
                    collect_type_spans_in_expr(a, out);
                }
            }
        }
        E::BinaryOp { left, right, .. } => {
            collect_type_spans_in_expr(left, out);
            collect_type_spans_in_expr(right, out);
        }
        E::UnaryOp { operand, .. } => collect_type_spans_in_expr(operand, out),
        E::Assign { value, .. } => collect_type_spans_in_expr(value, out),
        _ => {}
    }
}

fn collect_type_spans_in_type(ty: &lin_parse::ast::TypeExpr, out: &mut Vec<lin_common::Span>) {
    use lin_parse::ast::TypeExpr as T;
    match ty {
        T::Named(_, span) => out.push(*span),
        T::Generic(_, args, span) => {
            out.push(*span);
            for a in args {
                collect_type_spans_in_type(a, out);
            }
        }
        T::Array(inner, _) => collect_type_spans_in_type(inner, out),
        T::FixedArray(items, _) => items.iter().for_each(|t| collect_type_spans_in_type(t, out)),
        T::Union(items, _)
        | T::Intersection(items, _)
        | T::TaggedUnion(items, _) => items.iter().for_each(|t| collect_type_spans_in_type(t, out)),
        T::Function(params, ret, _) => {
            params.iter().for_each(|t| collect_type_spans_in_type(t, out));
            collect_type_spans_in_type(ret, out);
        }
        T::Object(fields, _) => fields.iter().for_each(|(_, t)| collect_type_spans_in_type(t, out)),
        T::IndexSig(inner, _) => collect_type_spans_in_type(inner, out),
        T::StringLit(_, _) => {}
    }
}

// ── code actions ───────────────────────────────────────────────────────────────

/// Turn the diagnostics the client supplied (in `params.context.diagnostics`) into quick-fixes:
///   - "imported but never used" → "Remove unused import" (deletes the line span).
///   - any diagnostic carrying a parsed did-you-mean `suggestion` (in its `data`) → "Change to `X`"
///     replacing the offending range.
///
/// Only diagnostics that overlap the requested `params.range` produce an action. Pure over
/// `(source, uri, params)` so it's unit-testable.
fn code_actions(source: &str, uri: &Url, params: &CodeActionParams) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();
    let requested = params.range;
    for diag in &params.context.diagnostics {
        if !ranges_overlap(diag.range, requested) {
            continue;
        }

        // Unused-import fix: delete the whole line the diagnostic sits on.
        if diag.message.contains("imported but never used") {
            let line_range = full_line_range(source, diag.range.start.line);
            let edit = TextEdit { range: line_range, new_text: String::new() };
            actions.push(CodeActionOrCommand::CodeAction(quick_fix(
                "Remove unused import".to_string(),
                uri.clone(),
                vec![edit],
                diag.clone(),
            )));
            continue;
        }

        // Did-you-mean fix: replace the offending span with the suggested identifier.
        if let Some(suggestion) = diag
            .data
            .as_ref()
            .and_then(|d| d.get("suggestion"))
            .and_then(|s| s.as_str())
        {
            let edit = TextEdit { range: diag.range, new_text: suggestion.to_string() };
            actions.push(CodeActionOrCommand::CodeAction(quick_fix(
                format!("Change to `{}`", suggestion),
                uri.clone(),
                vec![edit],
                diag.clone(),
            )));
        }
    }
    actions
}

/// Build a `QuickFix` code action with a single-document edit, attaching the source diagnostic.
fn quick_fix(title: String, uri: Url, edits: Vec<TextEdit>, diagnostic: Diagnostic) -> CodeAction {
    let mut changes = HashMap::new();
    changes.insert(uri, edits);
    CodeAction {
        title,
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic]),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            document_changes: None,
            change_annotations: None,
        }),
        command: None,
        is_preferred: Some(true),
        disabled: None,
        data: None,
    }
}

/// The full-line `Range` for `line`, including its trailing newline so deleting it removes the line
/// entirely (collapsing to the start of the next line).
fn full_line_range(source: &str, line: u32) -> Range {
    let start = Position { line, character: 0 };
    // End at the start of the next line so the newline is consumed by the deletion.
    let end = Position { line: line + 1, character: 0 };
    // Clamp the end to EOF when this is the last line (no trailing newline to span into).
    let line_count = source.lines().count() as u32;
    if line + 1 >= line_count && !source.ends_with('\n') {
        let eof = offset_to_position(source, source.len());
        return Range { start, end: eof };
    }
    Range { start, end }
}

/// True when two LSP ranges overlap (touching counts as overlap, matching client expectations for
/// "diagnostic under the cursor").
fn ranges_overlap(a: Range, b: Range) -> bool {
    let a_start = (a.start.line, a.start.character);
    let a_end = (a.end.line, a.end.character);
    let b_start = (b.start.line, b.start.character);
    let b_end = (b.end.line, b.end.character);
    a_start <= b_end && b_start <= a_end
}

// ── signature help ─────────────────────────────────────────────────────────────

/// Build signature help when `offset` sits inside a `f(…)` call's argument list. Returns `None`
/// when the cursor is not inside a resolvable call (e.g. dot-calls, or callees whose function type
/// can't be looked up) — per the no-guessing rule.
fn signature_help(source: &str, analysis: &Analysis, offset: usize) -> Option<SignatureHelp> {
    // Find the innermost plain `Call` whose argument region (between `(` and the closing `)`)
    // contains the cursor, plus the byte offset just after its opening `(`.
    let (callee_span, paren_after) = find_enclosing_call(&analysis.module.statements, source, offset)?;

    // Resolve the callee's type via the type map (the callee is an identifier use-site).
    let ty_str = tightest_span(&analysis.span_type_map, callee_span.start as usize)
        .map(|(_, s, _)| s.clone())?;
    // Only function-typed callees produce a signature.
    let params = function_param_types(&ty_str)?;

    // Active parameter = number of top-level commas between the opening paren and the cursor.
    let active = top_level_commas(&source[paren_after..offset.min(source.len())]);
    let active = (active as usize).min(params.len().saturating_sub(1)) as u32;

    // Render `(P1, P2, …) => R` exactly as the type string, with one ParameterInformation per
    // top-level parameter so the client can bold the active one. Param labels use the bare type
    // text (the function type carries no parameter names).
    let label = ty_str.clone();
    let parameters: Vec<ParameterInformation> = params
        .iter()
        .map(|p| ParameterInformation {
            label: ParameterLabel::Simple(p.clone()),
            documentation: None,
        })
        .collect();

    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: None,
            parameters: Some(parameters),
            active_parameter: Some(active),
        }],
        active_signature: Some(0),
        active_parameter: Some(active),
    })
}

/// Walk the AST for the INNERMOST plain `Call` whose argument list contains `offset`. Returns the
/// callee identifier span and the byte offset just after the call's opening `(`. `DotCall`s are
/// intentionally not resolved (their method type isn't in the use-site map).
fn find_enclosing_call(
    stmts: &[Stmt],
    source: &str,
    offset: usize,
) -> Option<(lin_common::Span, usize)> {
    let mut best: Option<(lin_common::Span, usize)> = None;
    for stmt in stmts {
        match stmt {
            Stmt::Val { value, .. } | Stmt::Var { value, .. } => {
                find_enclosing_call_in_expr(value, source, offset, &mut best);
            }
            Stmt::Expr(e) => find_enclosing_call_in_expr(e, source, offset, &mut best),
            _ => {}
        }
    }
    best
}

fn find_enclosing_call_in_expr(
    expr: &lin_parse::ast::Expr,
    source: &str,
    offset: usize,
    best: &mut Option<(lin_common::Span, usize)>,
) {
    use lin_parse::ast::Expr as E;
    // Recurse first so the innermost matching call wins (children update `best` last).
    match expr {
        E::Call { func, args, span, .. } => {
            for a in args {
                find_enclosing_call_in_expr(a, source, offset, best);
            }
            find_enclosing_call_in_expr(func, source, offset, best);
            // Lin's `Call` span is just the `(` token, so the argument region is found by scanning
            // from the opening paren to its matching close in source. `span.start` is the `(`.
            let open = span.start as usize;
            if source.as_bytes().get(open) == Some(&b'(') {
                let close = matching_paren(&source[open..]).map(|c| open + c);
                let paren_after = open + 1;
                // Inside the parens: after `(` and at/before the `)` (or to EOF when unclosed,
                // which is the common case while the user is still typing arguments).
                let end_bound = close.unwrap_or(source.len());
                if offset >= paren_after && offset <= end_bound {
                    // Only resolvable when the callee is a bare identifier (its type is a use-site).
                    if let E::Ident(_, ident_span) = func.as_ref() {
                        *best = Some((*ident_span, paren_after));
                    }
                }
            }
        }
        E::DotCall { receiver, args, .. } => {
            find_enclosing_call_in_expr(receiver, source, offset, best);
            if let Some(args) = args {
                for a in args {
                    find_enclosing_call_in_expr(a, source, offset, best);
                }
            }
        }
        E::Block(stmts, tail, _) => {
            if let Some(r) = find_enclosing_call(stmts, source, offset) {
                *best = Some(r);
            }
            find_enclosing_call_in_expr(tail, source, offset, best);
        }
        E::Function { body, .. } => find_enclosing_call_in_expr(body, source, offset, best),
        E::If { condition, then_branch, else_branch, .. } => {
            find_enclosing_call_in_expr(condition, source, offset, best);
            find_enclosing_call_in_expr(then_branch, source, offset, best);
            find_enclosing_call_in_expr(else_branch, source, offset, best);
        }
        E::Match { scrutinee, arms, .. } => {
            find_enclosing_call_in_expr(scrutinee, source, offset, best);
            for arm in arms {
                find_enclosing_call_in_expr(&arm.body, source, offset, best);
            }
        }
        E::BinaryOp { left, right, .. } => {
            find_enclosing_call_in_expr(left, source, offset, best);
            find_enclosing_call_in_expr(right, source, offset, best);
        }
        E::UnaryOp { operand, .. } => find_enclosing_call_in_expr(operand, source, offset, best),
        E::Assign { value, .. } => find_enclosing_call_in_expr(value, source, offset, best),
        E::Index { object, key, .. } => {
            find_enclosing_call_in_expr(object, source, offset, best);
            find_enclosing_call_in_expr(key, source, offset, best);
        }
        _ => {}
    }
}

/// Split a rendered function-type string `(P1, P2, …) => R` into its top-level parameter type
/// texts. Returns `None` when the string isn't a function type. Mirrors `first_param_category`'s
/// top-level split but keeps every parameter.
fn function_param_types(sig: &str) -> Option<Vec<String>> {
    if !sig.starts_with('(') {
        return None;
    }
    // The `(...) => R` form must contain `=>`; without it this isn't a function type.
    if !sig.contains("=>") {
        return None;
    }
    let close = matching_paren(sig)?;
    let params = &sig[1..close];
    if params.trim().is_empty() {
        return Some(Vec::new());
    }
    Some(split_top_level(params))
}

/// Index of the `)` that closes the `(` at byte 0 of `s` (depth-balanced over `()[]{}<>`).
fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split `s` on top-level commas (commas at bracket depth 0), trimming each piece.
fn split_top_level(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim().to_string());
    out
}

/// Count top-level commas (depth 0) in `s`. Used to pick the active parameter from the text between
/// a call's `(` and the cursor.
fn top_level_commas(s: &str) -> u32 {
    let mut depth = 0i32;
    let mut count = 0u32;
    for c in s.chars() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    count
}

// ── inlay hints ────────────────────────────────────────────────────────────────

/// Build inline type-annotation hints for every `val`/`var` binding whose type was INFERRED
/// (no explicit annotation in source). Each hint is positioned at the end of the bound identifier
/// and labelled `: <Type>`, where the type comes from the type-checked module.
///
/// Pure over `(source, analysis)` so it's unit-testable. Returns an empty vec when checking failed
/// (no typed module) — we never guess a type from a broken parse.
fn inlay_hints(source: &str, analysis: &Analysis) -> Vec<InlayHint> {
    let Some(typed) = analysis.typed.as_ref() else {
        return Vec::new();
    };
    // Map every `val`/`var` binding's STATEMENT span to its inferred type. The AST and typed
    // statements share stmt spans (the checker copies them through), so we join on span.
    let mut ty_by_stmt: HashMap<(u32, u32), String> = HashMap::new();
    collect_binding_types(&typed.statements, &mut ty_by_stmt);

    // Walk the AST for unannotated `val`/`var` bindings and pair each with the inferred type.
    let mut anchors: Vec<(lin_common::Span, String)> = Vec::new();
    collect_unannotated_bindings(&analysis.module.statements, &ty_by_stmt, &mut anchors);

    let mut hints = Vec::new();
    for (name_span, ty_str) in anchors {
        // Don't emit a hint when the type couldn't be rendered usefully (unsolved inference var).
        if ty_str.starts_with("?T") {
            continue;
        }
        let position = offset_to_position(source, name_span.end as usize);
        hints.push(InlayHint {
            position,
            label: InlayHintLabel::String(format!(": {}", ty_str)),
            kind: Some(InlayHintKind::TYPE),
            text_edits: None,
            tooltip: None,
            padding_left: Some(false),
            padding_right: Some(false),
            data: None,
        });
    }
    hints
}

/// Recursively collect `(stmt_span_tuple -> type_string)` for every `val`/`var` in the typed
/// module, descending into function bodies and blocks so nested bindings get hints too.
fn collect_binding_types(stmts: &[TypedStmt], out: &mut HashMap<(u32, u32), String>) {
    for stmt in stmts {
        match stmt {
            TypedStmt::Val { ty, span, value, .. } => {
                out.insert((span.start, span.end), ty.to_string());
                collect_binding_types_in_expr(value, out);
            }
            TypedStmt::Var { ty, span, value, .. } => {
                out.insert((span.start, span.end), ty.to_string());
                collect_binding_types_in_expr(value, out);
            }
            TypedStmt::Expr(e) => collect_binding_types_in_expr(e, out),
            _ => {}
        }
    }
}

/// Descend a typed expression looking for nested `val`/`var` bindings (in blocks / function
/// bodies / branches) so their inferred types are available to the inlay-hint join.
fn collect_binding_types_in_expr(expr: &TypedExpr, out: &mut HashMap<(u32, u32), String>) {
    use lin_check::typed_ir::TypedExpr as E;
    match expr {
        E::Block { stmts, expr, .. } => {
            collect_binding_types(stmts, out);
            collect_binding_types_in_expr(expr, out);
        }
        E::Function { body, .. } => collect_binding_types_in_expr(body, out),
        E::If { cond, then_br, else_br, .. } => {
            collect_binding_types_in_expr(cond, out);
            collect_binding_types_in_expr(then_br, out);
            collect_binding_types_in_expr(else_br, out);
        }
        E::Match { scrutinee, arms, .. } => {
            collect_binding_types_in_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_binding_types_in_expr(g, out);
                }
                collect_binding_types_in_expr(&arm.body, out);
            }
        }
        E::Call { func, args, .. } => {
            collect_binding_types_in_expr(func, out);
            for a in args {
                collect_binding_types_in_expr(a, out);
            }
        }
        E::BinaryOp { left, right, .. } => {
            collect_binding_types_in_expr(left, out);
            collect_binding_types_in_expr(right, out);
        }
        E::UnaryOp { operand, .. } => collect_binding_types_in_expr(operand, out),
        E::Coerce { expr, .. } => collect_binding_types_in_expr(expr, out),
        E::LocalSet { value, .. } => collect_binding_types_in_expr(value, out),
        _ => {}
    }
}

/// Walk the surface AST for `val`/`var` bindings that have NO explicit type annotation and bind a
/// single identifier (destructuring patterns are skipped — they bind several names with no single
/// anchor). For each, look up the inferred type by the binding's statement span and, when found,
/// record `(identifier_span, type_string)`. Recurses into function bodies/blocks/branches.
fn collect_unannotated_bindings(
    stmts: &[Stmt],
    ty_by_stmt: &HashMap<(u32, u32), String>,
    out: &mut Vec<(lin_common::Span, String)>,
) {
    for stmt in stmts {
        match stmt {
            Stmt::Val { pattern, type_ann: None, value, span, .. } => {
                if let Some((_, name_span)) = pattern_ident(pattern) {
                    if let Some(ty) = ty_by_stmt.get(&(span.start, span.end)) {
                        out.push((name_span, ty.clone()));
                    }
                }
                collect_unannotated_bindings_in_expr(value, ty_by_stmt, out);
            }
            Stmt::Var { name_span, type_ann: None, value, span, .. } => {
                if let Some(ty) = ty_by_stmt.get(&(span.start, span.end)) {
                    out.push((*name_span, ty.clone()));
                }
                collect_unannotated_bindings_in_expr(value, ty_by_stmt, out);
            }
            Stmt::Val { value, .. } | Stmt::Var { value, .. } => {
                collect_unannotated_bindings_in_expr(value, ty_by_stmt, out);
            }
            Stmt::Expr(e) => collect_unannotated_bindings_in_expr(e, ty_by_stmt, out),
            _ => {}
        }
    }
}

/// Descend a surface expression mirroring `collect_binding_types_in_expr`, finding nested
/// unannotated `val`/`var` bindings.
fn collect_unannotated_bindings_in_expr(
    expr: &lin_parse::ast::Expr,
    ty_by_stmt: &HashMap<(u32, u32), String>,
    out: &mut Vec<(lin_common::Span, String)>,
) {
    use lin_parse::ast::Expr as E;
    match expr {
        E::Block(stmts, tail, _) => {
            collect_unannotated_bindings(stmts, ty_by_stmt, out);
            collect_unannotated_bindings_in_expr(tail, ty_by_stmt, out);
        }
        E::Function { body, .. } => collect_unannotated_bindings_in_expr(body, ty_by_stmt, out),
        E::If { condition, then_branch, else_branch, .. } => {
            collect_unannotated_bindings_in_expr(condition, ty_by_stmt, out);
            collect_unannotated_bindings_in_expr(then_branch, ty_by_stmt, out);
            collect_unannotated_bindings_in_expr(else_branch, ty_by_stmt, out);
        }
        E::Match { scrutinee, arms, .. } => {
            collect_unannotated_bindings_in_expr(scrutinee, ty_by_stmt, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_unannotated_bindings_in_expr(g, ty_by_stmt, out);
                }
                collect_unannotated_bindings_in_expr(&arm.body, ty_by_stmt, out);
            }
        }
        E::Call { func, args, .. } => {
            collect_unannotated_bindings_in_expr(func, ty_by_stmt, out);
            for a in args {
                collect_unannotated_bindings_in_expr(a, ty_by_stmt, out);
            }
        }
        E::DotCall { receiver, args, .. } => {
            collect_unannotated_bindings_in_expr(receiver, ty_by_stmt, out);
            if let Some(args) = args {
                for a in args {
                    collect_unannotated_bindings_in_expr(a, ty_by_stmt, out);
                }
            }
        }
        E::BinaryOp { left, right, .. } => {
            collect_unannotated_bindings_in_expr(left, ty_by_stmt, out);
            collect_unannotated_bindings_in_expr(right, ty_by_stmt, out);
        }
        E::UnaryOp { operand, .. } => collect_unannotated_bindings_in_expr(operand, ty_by_stmt, out),
        E::Assign { value, .. } => collect_unannotated_bindings_in_expr(value, ty_by_stmt, out),
        _ => {}
    }
}

/// True when `pos` lies within the half-open LSP `range` (inclusive of both ends, which is fine for
/// the inlay viewport filter — clients tolerate a hint exactly on the boundary).
fn position_in_range(pos: Position, range: Range) -> bool {
    let after_start = (pos.line, pos.character) >= (range.start.line, range.start.character);
    let before_end = (pos.line, pos.character) <= (range.end.line, range.end.character);
    after_start && before_end
}

fn lsp_diagnostic(source: &str, d: &lin_common::Diagnostic) -> Diagnostic {
    let start = offset_to_position(source, d.span.start as usize);
    let end_offset = (d.span.end as usize).max(d.span.start as usize + 1);
    let end = offset_to_position(source, end_offset);
    // Stash a parsed "did you mean" suggestion into `data` so it round-trips back to the
    // code-action handler (the LSP `message` stays human-readable; the structured suggestion lets
    // us offer a one-click "Change to X" fix).
    let data = d
        .help
        .as_deref()
        .and_then(suggestion_from_help)
        .map(|s| serde_json::json!({ "suggestion": s }));
    Diagnostic {
        range: Range { start, end },
        severity: Some(match d.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
        }),
        message: d.message.clone(),
        source: Some("lin".to_string()),
        data,
        ..Default::default()
    }
}

/// Extract the suggested replacement identifier from a "did you mean 'X'?" / "did you mean \"X\"?"
/// help string. Returns `None` when the help isn't a did-you-mean suggestion.
fn suggestion_from_help(help: &str) -> Option<String> {
    let lower = help.to_ascii_lowercase();
    if !lower.contains("did you mean") {
        return None;
    }
    // The suggestion is the text between the first pair of quotes (either ' or ").
    let bytes = help.as_bytes();
    let open = help.find(['\'', '"'])?;
    let quote = bytes[open];
    let rest = &help[open + 1..];
    let close = rest.find(quote as char)?;
    Some(rest[..close].to_string())
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

    // ── Tier-2: inlay hints ─────────────────────────────────────────────────────

    /// A `val` binding with NO explicit annotation gets a type hint; a binding WITH an annotation
    /// gets none.
    #[test]
    fn inlay_hints_only_for_inferred_bindings() {
        let src = "val a = 1\nval b: Int32 = 2\n";
        let analysis = analyse(src, None);
        let hints = inlay_hints(src, &analysis);
        let labels: Vec<String> = hints
            .iter()
            .map(|h| match &h.label {
                InlayHintLabel::String(s) => s.clone(),
                _ => String::new(),
            })
            .collect();
        // `a` is inferred -> one hint; `b` is annotated -> no hint.
        assert_eq!(hints.len(), 1, "expected exactly one hint, got {:?}", labels);
        assert_eq!(labels[0], ": Int32");
        assert_eq!(hints[0].kind, Some(InlayHintKind::TYPE));
        // The hint anchors at the end of the identifier `a` (line 0).
        assert_eq!(hints[0].position.line, 0);
        assert_eq!(hints[0].position.character, "val a".len() as u32);
    }

    /// Nested `val` bindings inside a function body also get inferred-type hints.
    #[test]
    fn inlay_hints_descend_into_function_bodies() {
        let src = "val f = () =>\n  val inner = \"hi\"\n  inner\n";
        let analysis = analyse(src, None);
        let hints = inlay_hints(src, &analysis);
        let labels: Vec<String> = hints
            .iter()
            .filter_map(|h| match &h.label {
                InlayHintLabel::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(
            labels.iter().any(|l| l == ": String"),
            "expected a String hint for the nested binding, got {:?}",
            labels
        );
    }

    // ── Tier-2: semantic tokens ─────────────────────────────────────────────────

    /// Decode the delta-encoded token stream back into absolute `(line, char, len, type)` tuples.
    fn decode_semantic(tokens: &[SemanticToken]) -> Vec<(u32, u32, u32, u32)> {
        let mut out = Vec::new();
        let mut line = 0u32;
        let mut ch = 0u32;
        for t in tokens {
            if t.delta_line == 0 {
                ch += t.delta_start;
            } else {
                line += t.delta_line;
                ch = t.delta_start;
            }
            out.push((line, ch, t.length, t.token_type));
        }
        out
    }

    /// A parameter use is classified `parameter`; a function-typed binding use is `function`; an
    /// ordinary value use is `variable`.
    #[test]
    fn semantic_tokens_classify_identifiers_by_role() {
        // `n` is a parameter, used in the body; `g` is a function-typed val, called in `h`.
        let src = "val g = (n: Int32) => n + 1\nval h = () => g(2)\n";
        let analysis = analyse(src, None);
        let toks = decode_semantic(&semantic_tokens(src, &analysis));

        // Locate the body use of `n` (after `=>`).
        let arrow = src.find("=>").unwrap();
        let n_use = arrow + src[arrow..].find('n').unwrap();
        let n_pos = offset_to_position(src, n_use);
        let n_tok = toks
            .iter()
            .find(|(l, c, _, _)| *l == n_pos.line && *c == n_pos.character)
            .expect("expected a token at the body use of `n`");
        assert_eq!(n_tok.3, ST_PARAMETER, "param use should be `parameter`: {:?}", n_tok);

        // Locate the use of `g` inside `h` (line 1).
        let line1 = src.find('\n').unwrap() + 1;
        let g_use = line1 + src[line1..].find('g').unwrap();
        let g_pos = offset_to_position(src, g_use);
        let g_tok = toks
            .iter()
            .find(|(l, c, _, _)| *l == g_pos.line && *c == g_pos.character)
            .expect("expected a token at the use of `g`");
        assert_eq!(g_tok.3, ST_FUNCTION, "function-typed use should be `function`: {:?}", g_tok);

        // No token may overlap the next (sorted, non-overlapping invariant).
        let mut sorted = toks.clone();
        sorted.sort_by_key(|(l, c, _, _)| (*l, *c));
        for w in sorted.windows(2) {
            let (l0, c0, len0, _) = w[0];
            let (l1, c1, _, _) = w[1];
            if l0 == l1 {
                assert!(c0 + len0 <= c1, "tokens overlap: {:?} then {:?}", w[0], w[1]);
            }
        }
    }

    // ── Tier-2: signature help ──────────────────────────────────────────────────

    /// The active parameter is the count of top-level commas before the cursor.
    #[test]
    fn signature_help_active_parameter_from_comma_count() {
        assert_eq!(top_level_commas(""), 0);
        assert_eq!(top_level_commas("1"), 0);
        assert_eq!(top_level_commas("1, 2"), 1);
        assert_eq!(top_level_commas("1, 2, 3"), 2);
        // Commas nested inside brackets don't advance the active parameter.
        assert_eq!(top_level_commas("[1, 2], 3"), 1);
        assert_eq!(top_level_commas("f(a, b), "), 1);
    }

    #[test]
    fn function_param_types_splits_signature() {
        assert_eq!(
            function_param_types("(Int32, String) => Boolean"),
            Some(vec!["Int32".to_string(), "String".to_string()])
        );
        assert_eq!(function_param_types("() => Int32"), Some(vec![]));
        // Not a function type.
        assert_eq!(function_param_types("Int32"), None);
        assert_eq!(function_param_types("(Int32)"), None);
    }

    /// End to end: cursor inside `add(1, │2)` resolves the callee's signature and marks the SECOND
    /// parameter active.
    #[test]
    fn signature_help_resolves_call_and_active_param() {
        let src = "val add = (a: Int32, b: Int32) => a + b\nval r = add(1, 2)\n";
        let analysis = analyse(src, None);
        // Cursor right before the `2`.
        let call_open = src.rfind('(').unwrap();
        let cursor = call_open + src[call_open..].find("2").unwrap();
        let help = signature_help(src, &analysis, cursor).expect("expected signature help");
        assert_eq!(help.signatures.len(), 1);
        let sig = &help.signatures[0];
        assert!(sig.label.contains("=>"), "label should be a function type: {}", sig.label);
        assert_eq!(sig.parameters.as_ref().unwrap().len(), 2);
        // Second parameter active (one comma before the cursor).
        assert_eq!(sig.active_parameter, Some(1), "active param should be index 1");
    }

    /// Cursor outside any call → no signature help.
    #[test]
    fn signature_help_none_outside_call() {
        let src = "val add = (a: Int32, b: Int32) => a + b\nval r = add(1, 2)\n";
        let analysis = analyse(src, None);
        // Offset 0 (`v` of `val`) is not inside any call.
        assert!(signature_help(src, &analysis, 0).is_none());
    }

    // ── Tier-2: code actions ────────────────────────────────────────────────────

    fn dummy_uri() -> Url {
        Url::parse("file:///tmp/lsp_test.lin").unwrap()
    }

    /// Build `CodeActionParams` requesting actions over the whole document, with the freshly
    /// analysed diagnostics supplied as context (what a real client would echo back).
    fn code_action_params(src: &str, diags: Vec<Diagnostic>) -> CodeActionParams {
        let end = offset_to_position(src, src.len());
        CodeActionParams {
            text_document: TextDocumentIdentifier { uri: dummy_uri() },
            range: Range { start: Position { line: 0, character: 0 }, end },
            context: CodeActionContext {
                diagnostics: diags,
                only: None,
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    fn action_titles(actions: &[CodeActionOrCommand]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|a| match a {
                CodeActionOrCommand::CodeAction(ca) => Some(ca.title.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn code_action_removes_unused_import() {
        let src = "import { print } from \"std/io\"\nval x = 1\n";
        let analysis = analyse(src, None);
        // The unused-import warning must be present.
        assert!(
            analysis.diagnostics.iter().any(|d| d.message.contains("imported but never used")),
            "expected an unused-import diagnostic, got {:?}",
            analysis.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let params = code_action_params(src, analysis.diagnostics.clone());
        let actions = code_actions(src, &dummy_uri(), &params);
        let titles = action_titles(&actions);
        assert!(
            titles.iter().any(|t| t == "Remove unused import"),
            "expected a remove-unused-import action, got {:?}",
            titles
        );
        // The edit must delete the import line (line 0).
        let ca = actions.iter().find_map(|a| match a {
            CodeActionOrCommand::CodeAction(ca) if ca.title == "Remove unused import" => Some(ca),
            _ => None,
        }).unwrap();
        let edits = ca.edit.as_ref().unwrap().changes.as_ref().unwrap().get(&dummy_uri()).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].range.start.line, 0);
        assert_eq!(edits[0].new_text, "");
        assert_eq!(ca.kind, Some(CodeActionKind::QUICKFIX));
    }

    #[test]
    fn code_action_offers_did_you_mean_change() {
        // `lenght` is a typo for `length` — but use a local binding so the suggestion is stable.
        let src = "val length = 1\nval y = lenght\n";
        let analysis = analyse(src, None);
        // Some diagnostic must carry a suggestion (did-you-mean).
        let has_suggestion = analysis.diagnostics.iter().any(|d| {
            d.data.as_ref().and_then(|j| j.get("suggestion")).is_some()
        });
        assert!(
            has_suggestion,
            "expected a did-you-mean suggestion in diagnostics: {:?}",
            analysis.diagnostics.iter().map(|d| (&d.message, &d.data)).collect::<Vec<_>>()
        );
        let params = code_action_params(src, analysis.diagnostics.clone());
        let actions = code_actions(src, &dummy_uri(), &params);
        let titles = action_titles(&actions);
        assert!(
            titles.iter().any(|t| t == "Change to `length`"),
            "expected a change-to-length action, got {:?}",
            titles
        );
    }

    #[test]
    fn suggestion_from_help_parses_quoted_name() {
        assert_eq!(suggestion_from_help("did you mean 'length'?").as_deref(), Some("length"));
        assert_eq!(suggestion_from_help("did you mean \"foo\"?").as_deref(), Some("foo"));
        assert_eq!(suggestion_from_help("type defined here"), None);
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
