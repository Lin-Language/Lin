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
        // Store the workspace root so we can resolve relative imports. Prefer the
        // (deprecated but widely sent) `root_uri`; fall back to the first workspace
        // folder when only the multi-root form is provided.
        let root = params
            .root_uri
            .as_ref()
            .and_then(|u| u.to_file_path().ok())
            .or_else(|| {
                params
                    .workspace_folders
                    .as_ref()
                    .and_then(|fs| fs.first())
                    .and_then(|f| f.uri.to_file_path().ok())
            });
        if let Some(root) = &root {
            *WORKSPACE_ROOT.write().unwrap() = Some(root.clone());
        }

        // Build the cross-file index: seed stdlib, then enumerate + index every
        // `*.lin` file under the workspace root. Files are re-indexed on edit, and
        // open DIRECT dependents are re-checked when an imported file changes (see
        // the `update` / `recheck_open_dependents` handlers).
        {
            let mut index = WORKSPACE_INDEX.write().unwrap();
            *index = WorkspaceIndex::default();
            seed_stdlib_index(&mut index);
            if let Some(root) = &root {
                for path in collect_lin_files(root) {
                    if let Ok(src) = std::fs::read_to_string(&path) {
                        index.insert_user_file(&path, &src);
                    }
                }
            }
        }
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    // `"` and `/` additionally trigger import-path completion inside a `from "…"`.
                    trigger_characters: Some(vec![".".into(), " ".into(), "\"".into(), "/".into()]),
                    resolve_provider: Some(false),
                    ..Default::default()
                }),
                document_formatting_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                // Ctrl+T fuzzy search over every top-level declaration in the workspace.
                workspace_symbol_provider: Some(OneOf::Left(true)),
                // Jump from a value to the `type` declaration naming its type (cross-file).
                type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
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
                // Run-test CodeLenses above each `test(...)`/`withFixture(...)` declaration.
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                // Fold function bodies, object/array literals, match expressions, import runs.
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                // Smart-expand selection (innermost span outward) from the AST span nesting.
                selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
                // Clickable import paths that resolve to the target `.lin` file.
                document_link_provider: Some(DocumentLinkOptions {
                    resolve_provider: Some(false),
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

        // Import-path completion: inside a `from "…"` / `import foreign "…"` string, complete
        // module paths (stdlib ids + sibling `.lin` files) and short-circuit the rest.
        if let Some(typed) = import_string_prefix(&source, offset) {
            let base_dir = file_dir(uri);
            let items = import_path_completions(&typed, base_dir.as_deref());
            return Ok(Some(CompletionResponse::Array(items)));
        }

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

        // Cross-file first: if the cursor is on a top-level exported/imported symbol,
        // gather occurrences across the whole workspace index.
        if let Ok(path) = uri.to_file_path() {
            let module_id = canonical_id(&path);
            let index = WORKSPACE_INDEX.read().unwrap();
            if let Some((owner, name)) = index.resolve_symbol(&module_id, offset) {
                let occ = index.occurrences(&owner, &name);
                if !occ.is_empty() {
                    let decl = decl_span(&index, &owner, &name);
                    let locations: Vec<Location> = occ
                        .iter()
                        .filter(|(mid, s)| include_decl || !(mid == &owner && Some(*s) == decl))
                        .filter_map(|(mid, s)| {
                            let f = index.files.get(mid)?;
                            let u = module_id_to_uri(mid)?;
                            Some(Location { uri: u, range: span_to_range(&f.source, *s) })
                        })
                        .collect();
                    return Ok(Some(locations));
                }
            }
        }

        // Intra-file fallback (locals, parameters): the first span is the binding.
        let occ = occurrences_at(&analysis.span_type_map, offset);
        if occ.is_empty() {
            return Ok(None);
        }
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

        // Cross-file first: a top-level exported/imported symbol renames everywhere.
        if let Ok(path) = uri.to_file_path() {
            let module_id = canonical_id(&path);
            let index = WORKSPACE_INDEX.read().unwrap();
            if let Some((owner, name)) = index.resolve_symbol(&module_id, offset) {
                // `rename_edits` returns None for stdlib-owned symbols (read-only) —
                // decline the rename rather than emit an unsound/partial edit.
                match index.rename_edits(&owner, &name) {
                    Some(edits) if !edits.is_empty() => {
                        // Group spans by file URI into per-document TextEdit lists.
                        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
                        for (mid, span) in edits {
                            let Some(file) = index.files.get(&mid) else { continue };
                            let Some(file_uri) = module_id_to_uri(&mid) else { continue };
                            changes.entry(file_uri).or_default().push(TextEdit {
                                range: span_to_range(&file.source, span),
                                new_text: new_name.clone(),
                            });
                        }
                        if !changes.is_empty() {
                            return Ok(Some(WorkspaceEdit {
                                changes: Some(changes),
                                document_changes: None,
                                change_annotations: None,
                            }));
                        }
                    }
                    // stdlib symbol or nothing renameable cross-file: fall through to
                    // the intra-file path (which itself yields nothing for stdlib).
                    _ => {}
                }
            }
        }

        // Intra-file fallback (locals, parameters): one TextEdit per occurrence.
        let occ = occurrences_at(&analysis.span_type_map, offset);
        if occ.is_empty() {
            return Ok(None);
        }
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

        let base_dir = file_dir(uri);
        let index = WORKSPACE_INDEX.read().unwrap();
        let actions = code_actions(&source, uri, &params, &index, base_dir.as_deref());
        if actions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(actions))
        }
    }

    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        let uri = &params.text_document.uri;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let mut lexer = lin_lex::Lexer::new(&source, 0);
        let tokens = lexer.tokenize();
        let mut parser = lin_parse::Parser::new(tokens);
        let module = parser.parse_module();
        Ok(Some(test_code_lenses(&source, uri, &module)))
    }

    async fn folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> Result<Option<Vec<FoldingRange>>> {
        let uri = &params.text_document.uri;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let mut lexer = lin_lex::Lexer::new(&source, 0);
        let tokens = lexer.tokenize();
        let mut parser = lin_parse::Parser::new(tokens);
        let module = parser.parse_module();
        Ok(Some(folding_ranges(&source, &module)))
    }

    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        let uri = &params.text_document.uri;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());
        let ranges = params
            .positions
            .iter()
            .map(|pos| {
                let offset = position_to_offset(&source, *pos);
                selection_range_at(&source, &analysis.module, offset)
            })
            .collect();
        Ok(Some(ranges))
    }

    async fn document_link(
        &self,
        params: DocumentLinkParams,
    ) -> Result<Option<Vec<DocumentLink>>> {
        let uri = &params.text_document.uri;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let mut lexer = lin_lex::Lexer::new(&source, 0);
        let tokens = lexer.tokenize();
        let mut parser = lin_parse::Parser::new(tokens);
        let module = parser.parse_module();
        Ok(Some(import_document_links(&source, &module, base_dir.as_deref())))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let query = &params.query;
        let index = WORKSPACE_INDEX.read().unwrap();
        let mut symbols = Vec::new();
        for (mod_id, name, span) in index.workspace_symbols(query) {
            let Some(file) = index.files.get(&mod_id) else { continue };
            // stdlib symbols have no on-disk URI; skip them in the symbol list (they
            // aren't navigable as files and would confuse the client's open).
            let Some(uri) = module_id_to_uri(&mod_id) else { continue };
            #[allow(deprecated)]
            symbols.push(SymbolInformation {
                name,
                kind: SymbolKind::VARIABLE,
                tags: None,
                deprecated: None,
                location: Location {
                    uri,
                    range: span_to_range(&file.source, span),
                },
                container_name: None,
            });
        }
        Ok(Some(symbols))
    }

    async fn goto_type_definition(
        &self,
        params: request::GotoTypeDefinitionParams,
    ) -> Result<Option<request::GotoTypeDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.docs.read().unwrap().get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());
        let offset = position_to_offset(&source, pos);

        // The value's rendered type at the cursor (e.g. `Point` or `Point[]`).
        let ty_str = match tightest_span(&analysis.span_type_map, offset) {
            Some((_, s, _)) => s.clone(),
            None => return Ok(None),
        };
        // Extract the leading type *name* (strip array brackets / generic args). We
        // only resolve a single named type — unions/objects have no single decl.
        let type_name = match leading_type_name(&ty_str) {
            Some(n) => n,
            None => return Ok(None),
        };

        // Find a `type <name>` declaration: same file first, then anywhere in the
        // workspace index (cross-file type definitions).
        let index = WORKSPACE_INDEX.read().unwrap();
        if let Ok(path) = uri.to_file_path() {
            let module_id = canonical_id(&path);
            if let Some(file) = index.files.get(&module_id) {
                if let Some((_, span)) = file.type_decls.iter().find(|(n, _)| n == &type_name) {
                    return Ok(Some(request::GotoTypeDefinitionResponse::Scalar(Location {
                        uri: uri.clone(),
                        range: span_to_range(&source, *span),
                    })));
                }
            }
        }
        for (mod_id, file) in &index.files {
            if let Some((_, span)) = file.type_decls.iter().find(|(n, _)| n == &type_name) {
                if let Some(file_uri) = module_id_to_uri(mod_id) {
                    return Ok(Some(request::GotoTypeDefinitionResponse::Scalar(Location {
                        uri: file_uri,
                        range: span_to_range(&file.source, *span),
                    })));
                }
            }
        }
        Ok(None)
    }
}

impl Backend {
    async fn update(&self, uri: &Url, source: &str) {
        self.docs
            .write()
            .unwrap()
            .insert(uri.clone(), source.to_string());
        // Re-index this file's symbol/import table so cross-file
        // references/symbols/rename stay current. This also refreshes the export
        // signatures dependents read through `analyse`'s `pre_resolve_imports`.
        if let Ok(path) = uri.to_file_path() {
            WORKSPACE_INDEX
                .write()
                .unwrap()
                .insert_user_file(&path, source);
        }
        let base_dir = file_dir(uri);
        let analysis = analyse(source, base_dir.as_deref());
        self.client
            .publish_diagnostics(uri.clone(), analysis.diagnostics, None)
            .await;

        // Dependent re-check: when F changes, any OPEN file B that imports from F
        // can have stale inferred types (hover/inlay/diagnostics) for symbols it
        // pulls from F, because B's analysis cached F's *old* exports. Re-analyse
        // those dependents and re-publish their diagnostics so they reflect F's new
        // exports.
        //
        // POLICY (documented per the task brief):
        //   - DIRECT dependents only — files whose `ImportRef.module_id` is F. We do
        //     NOT walk transitively: a transitive dependent only sees F's types via
        //     an intermediate module's re-exported signature, which is rare, and a
        //     full transitive sweep on every keystroke does not scale. Direct-only is
        //     the sound, bounded default.
        //   - OPEN documents only — the client renders diagnostics/hover only for
        //     open docs, so re-checking closed files would be wasted work.
        //   - Runs on every `update` (did_open / did_change / did_save). Re-analysing
        //     a handful of direct open dependents is cheap (parse + check of small
        //     files); bounding to direct + open keeps it so even on each keystroke,
        //     which is the freshest behaviour without a workspace-wide sweep.
        //
        // No infinite-loop risk: `dependents_of` never returns F itself, we only
        // re-analyse (we do NOT recursively call `update`, which would re-trigger),
        // and we skip F's own URI defensively. A cyclic graph (A↔B) is therefore
        // safe — editing A re-checks B once and stops.
        self.recheck_open_dependents(uri).await;
    }

    /// Re-analyse and re-publish diagnostics for every currently-open document that
    /// DIRECTLY imports from `changed_uri`. See the policy note in `update`.
    async fn recheck_open_dependents(&self, changed_uri: &Url) {
        let Ok(changed_path) = changed_uri.to_file_path() else { return };
        let changed_id = canonical_id(&changed_path);

        // Compute the direct dependents under the index read lock, then drop it
        // before any await (we must not hold a std `RwLock` guard across `.await`).
        let dependent_ids: Vec<String> = {
            let index = WORKSPACE_INDEX.read().unwrap();
            index.dependents_of(&changed_id)
        };
        if dependent_ids.is_empty() {
            return;
        }

        // Snapshot the open docs (uri + source) for the dependents we care about,
        // again releasing the lock before awaiting. A dependent that isn't currently
        // open is skipped — the client only renders open documents.
        let to_recheck: Vec<(Url, String)> = {
            let docs = self.docs.read().unwrap();
            docs.iter()
                .filter(|(dep_uri, _)| *dep_uri != changed_uri)
                .filter(|(dep_uri, _)| {
                    dep_uri
                        .to_file_path()
                        .map(|p| dependent_ids.contains(&canonical_id(&p)))
                        .unwrap_or(false)
                })
                .map(|(u, s)| (u.clone(), s.clone()))
                .collect()
        };

        for (dep_uri, dep_source) in to_recheck {
            let base_dir = file_dir(&dep_uri);
            let analysis = analyse(&dep_source, base_dir.as_deref());
            self.client
                .publish_diagnostics(dep_uri, analysis.diagnostics, None)
                .await;
        }
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

/// Detect whether `offset` sits inside the quoted path of an `import` statement and, if
/// so, return the path text already typed up to the cursor. Conservative + line-scoped:
/// matches `... from "<typed>` and `import foreign "<typed>` on the cursor's line, with
/// the cursor positioned after the opening quote and before any closing quote.
///
/// LIMITATION: detection is single-line and textual (no multi-line import strings, which
/// the grammar doesn't produce anyway). Returns `None` outside an import-string context.
fn import_string_prefix(source: &str, offset: usize) -> Option<String> {
    let offset = offset.min(source.len());
    // Start of the current line.
    let line_start = source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = &source[line_start..offset];
    // Find the opening quote we're inside: the last `"` on the line before the cursor.
    let quote = line.rfind('"')?;
    let before_quote = line[..quote].trim_end();
    // Only an import context: `... from` or `import foreign`.
    let is_from = before_quote.ends_with("from");
    let is_foreign = before_quote.ends_with("import foreign") || before_quote.ends_with("foreign");
    if !(is_from || is_foreign) {
        return None;
    }
    // The text between the opening quote and the cursor is what's been typed so far.
    let typed = &line[quote + 1..];
    // If the user already closed the string before the cursor, we're not inside it.
    if typed.contains('"') {
        return None;
    }
    Some(typed.to_string())
}

/// Build import-path completion items for a partially-typed module path `typed`:
///   - every `std/*` stdlib module id (always offered);
///   - sibling `.lin` files in the importing file's directory (path stems), when known.
/// Items are filtered to those starting with `typed` so `/` retriggers narrow the list.
fn import_path_completions(typed: &str, base_dir: Option<&Path>) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    // stdlib modules.
    for id in STDLIB_MODULE_IDS {
        if id.starts_with(typed) {
            items.push(CompletionItem {
                label: id.to_string(),
                kind: Some(CompletionItemKind::MODULE),
                detail: Some("stdlib module".to_string()),
                ..Default::default()
            });
        }
    }
    // Sibling `.lin` files (excluding `.test.lin`), as bare stems.
    if let Some(dir) = base_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let Some(stem) = name.strip_suffix(".lin") else { continue };
                if stem.ends_with(".test") || stem.is_empty() {
                    continue;
                }
                if stem.starts_with(typed) {
                    items.push(CompletionItem {
                        label: stem.to_string(),
                        kind: Some(CompletionItemKind::FILE),
                        detail: Some("local module".to_string()),
                        ..Default::default()
                    });
                }
            }
        }
    }
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items.dedup_by(|a, b| a.label == b.label);
    items
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
        E::Block(stmts, tail, _, _) => {
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
        E::Block(stmts, tail, _, _) => {
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
fn code_actions(
    source: &str,
    uri: &Url,
    params: &CodeActionParams,
    index: &WorkspaceIndex,
    base_dir: Option<&Path>,
) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();
    let requested = params.range;
    for diag in &params.context.diagnostics {
        if !ranges_overlap(diag.range, requested) {
            continue;
        }

        // Auto-import fix: an undefined name that some workspace/stdlib module exports
        // can be imported with one click. Offers one action per exporting module.
        if let Some(name) = undefined_name(&diag.message) {
            for module in index.modules_exporting(&name, base_dir) {
                if let Some(edit) = auto_import_edit(source, &name, &module) {
                    actions.push(CodeActionOrCommand::CodeAction(quick_fix(
                        format!("Import `{}` from \"{}\"", name, module),
                        uri.clone(),
                        vec![edit],
                        diag.clone(),
                    )));
                }
            }
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

/// Extract the offending identifier from an "Undefined variable 'X'" / "Undefined
/// function 'X'" diagnostic message. Returns `None` for any other diagnostic.
fn undefined_name(message: &str) -> Option<String> {
    for prefix in ["Undefined variable '", "Undefined function '"] {
        if let Some(rest) = message.strip_prefix(prefix) {
            if let Some(close) = rest.find('\'') {
                return Some(rest[..close].to_string());
            }
        }
    }
    None
}

/// Build the `TextEdit` that imports `name` from `module`. If the file already imports
/// from `module` via `import { ... } from "module"`, merge `name` into that brace list;
/// otherwise insert a fresh `import { name } from "module"` line after the last existing
/// import (or at the very top when there are none). Returns `None` when `name` is already
/// imported from `module`.
fn auto_import_edit(source: &str, name: &str, module: &str) -> Option<TextEdit> {
    let mut lexer = lin_lex::Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    let mut parser = lin_parse::Parser::new(tokens);
    let parsed = parser.parse_module();

    let mut last_import_line: Option<u32> = None;
    for stmt in &parsed.statements {
        if let Stmt::Import { bindings, path, span } = stmt {
            let line = offset_to_position(source, span.start as usize).line;
            last_import_line = Some(line);
            if path == module {
                // Already importing from this module — merge into its brace list (unless
                // the name is already present).
                if bindings.iter().any(|b| b.name == name || b.alias.as_deref() == Some(name)) {
                    return None;
                }
                // Insert `, name` just before the closing `}` of this import's brace list.
                // The stmt span covers only the `import` keyword, so scan to the line end.
                let start = span.start as usize;
                let line_end = source[start..].find('\n').map(|i| start + i).unwrap_or(source.len());
                let stmt_src = source.get(start..line_end)?;
                let brace = stmt_src.find('}')?;
                let insert_at = start + brace;
                let pos = offset_to_position(source, insert_at);
                return Some(TextEdit {
                    range: Range { start: pos, end: pos },
                    new_text: format!(", {}", name),
                });
            }
        }
    }

    // No existing import from `module`: add a new line. Place it after the last import,
    // else at the top of the file.
    let new_line = format!("import {{ {} }} from \"{}\"\n", name, module);
    let line = last_import_line.map(|l| l + 1).unwrap_or(0);
    let pos = Position { line, character: 0 };
    Some(TextEdit {
        range: Range { start: pos, end: pos },
        new_text: new_line,
    })
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

// ── code lens (run-test) ───────────────────────────────────────────────────────

/// Build the run-test CodeLenses for a file: one `▶ Run Test` lens above each
/// `test("name", ...)` / `withFixture(..., "name", ...)` call (mirroring the VSCode
/// Test Explorer's `TEST_DECL_RE` / `WITHFIXTURE_DECL_RE` discovery, but driven off
/// the parsed AST), plus a single `▶ Run File Tests` lens at the top when any test
/// exists. Lens commands use the fixed `lin.runTest(uri, name)` / `lin.testFile(uri)`
/// contract the extension wires against.
fn test_code_lenses(source: &str, uri: &Url, module: &lin_parse::ast::Module) -> Vec<CodeLens> {
    let mut tests: Vec<(String, lin_common::Span)> = Vec::new();
    for stmt in &module.statements {
        collect_test_calls_in_stmt(stmt, &mut tests);
    }
    // De-duplicate by (name, anchor) so a test referenced once yields one lens.
    tests.sort_by_key(|(_, s)| (s.start, s.end));
    tests.dedup();

    let mut lenses = Vec::new();
    if !tests.is_empty() {
        // `▶ Run File Tests` at the very top of the file.
        lenses.push(CodeLens {
            range: span_to_range(source, lin_common::Span::new(0, 0, 0)),
            command: Some(Command {
                title: "▶ Run File Tests".to_string(),
                command: "lin.testFile".to_string(),
                arguments: Some(vec![serde_json::json!(uri.to_string())]),
            }),
            data: None,
        });
    }
    for (name, anchor) in tests {
        lenses.push(CodeLens {
            range: span_to_range(source, anchor),
            command: Some(Command {
                title: "▶ Run Test".to_string(),
                command: "lin.runTest".to_string(),
                arguments: Some(vec![
                    serde_json::json!(uri.to_string()),
                    serde_json::json!(name),
                ]),
            }),
            data: None,
        });
    }
    lenses
}

fn collect_test_calls_in_stmt(stmt: &Stmt, out: &mut Vec<(String, lin_common::Span)>) {
    match stmt {
        Stmt::Val { value, .. } | Stmt::Var { value, .. } => {
            collect_test_calls_in_expr(value, out);
        }
        Stmt::Replace { value, .. } => collect_test_calls_in_expr(value, out),
        Stmt::Expr(e) => collect_test_calls_in_expr(e, out),
        _ => {}
    }
}

/// Walk an expression for `test("name", ...)` / `withFixture(..., "name", ...)` calls,
/// recording `(name, anchor_span)`. The anchor is the callee identifier span so the lens
/// renders on the line of the `test(`/`withFixture(` token.
fn collect_test_calls_in_expr(expr: &lin_parse::ast::Expr, out: &mut Vec<(String, lin_common::Span)>) {
    use lin_parse::ast::Expr as E;
    if let E::Call { func, args, .. } = expr {
        if let E::Ident(name, ident_span) = func.as_ref() {
            match name.as_str() {
                // `test("name", ...)` — first string-literal arg is the name.
                "test" => {
                    if let Some(E::StringLit(s, _)) = args.first() {
                        out.push((s.clone(), *ident_span));
                    }
                }
                // `withFixture(setup, "name", ...)` — name is the SECOND string-literal arg
                // (mirrors `WITHFIXTURE_DECL_RE`, which skips the first two `(`-args).
                "withFixture" => {
                    let name = args.iter().filter_map(|a| match a {
                        E::StringLit(s, _) => Some(s.clone()),
                        _ => None,
                    }).next();
                    if let Some(name) = name {
                        out.push((name, *ident_span));
                    }
                }
                _ => {}
            }
        }
    }
    // Recurse into every sub-expression so nested test() calls (e.g. inside a
    // `suite("...", [ test(...), test(...) ])` array) are discovered.
    walk_child_exprs(expr, &mut |child| collect_test_calls_in_expr(child, out));
}

/// Invoke `f` on every immediate child expression of `expr`. Centralises the AST
/// structural recursion shared by code-lens discovery (and any future AST walkers).
fn walk_child_exprs(expr: &lin_parse::ast::Expr, f: &mut dyn FnMut(&lin_parse::ast::Expr)) {
    use lin_parse::ast::{Expr as E, ObjectField, StringPart};
    match expr {
        E::BinaryOp { left, right, .. } => { f(left); f(right); }
        E::UnaryOp { operand, .. } => f(operand),
        E::Call { func, args, .. } => { f(func); for a in args { f(a); } }
        E::DotCall { receiver, args, .. } => {
            f(receiver);
            if let Some(args) = args { for a in args { f(a); } }
        }
        E::Index { object, key, .. } => { f(object); f(key); }
        E::If { condition, then_branch, else_branch, .. } => {
            f(condition); f(then_branch); f(else_branch);
        }
        E::Match { scrutinee, arms, .. } => {
            f(scrutinee);
            for arm in arms { f(&arm.body); }
        }
        E::Block(stmts, tail, _, _) => {
            for s in stmts {
                match s {
                    Stmt::Val { value, .. } | Stmt::Var { value, .. } => f(value),
                    Stmt::Replace { value, .. } => f(value),
                    Stmt::Expr(e) => f(e),
                    _ => {}
                }
            }
            f(tail);
        }
        E::Function { body, .. } => f(body),
        E::Object(fields, _, _) => {
            for field in fields {
                match field {
                    ObjectField::Pair(k, v) => { f(k); f(v); }
                    ObjectField::Spread(e) => f(e),
                }
            }
        }
        E::Array(items, _, _) => { for it in items { f(it); } }
        E::Assign { value, .. } => f(value),
        E::IndexAssign { object, key, value, .. } => { f(object); f(key); f(value); }
        E::Is { expr, .. } | E::Has { expr, .. } => f(expr),
        E::StringInterp(parts, _) => {
            for part in parts {
                if let StringPart::Expr(e) = part { f(e); }
            }
        }
        E::TupleArgs(items, _) => { for it in items { f(it); } }
        _ => {}
    }
}

// ── AST extent walkers (folding + selection) ─────────────────────────────────────

/// Invoke `f` on `expr` and, recursively, on every sub-expression. Built on the shared
/// `walk_child_exprs` immediate-child traversal so it stays in sync with the AST shape.
fn walk_exprs_deep(expr: &lin_parse::ast::Expr, f: &mut dyn FnMut(&lin_parse::ast::Expr)) {
    f(expr);
    walk_child_exprs(expr, &mut |child| walk_exprs_deep(child, f));
}

/// Invoke `f` on the root expression(s) carried by a statement (and, transitively, all of
/// their sub-expressions). Covers `val`/`var`/`replace` initialisers and bare expression
/// statements; declaration-only statements (imports, type decls) carry no expressions.
fn walk_exprs_in_stmt(stmt: &Stmt, f: &mut dyn FnMut(&lin_parse::ast::Expr)) {
    match stmt {
        Stmt::Val { value, .. } | Stmt::Var { value, .. } | Stmt::Replace { value, .. } => {
            walk_exprs_deep(value, f)
        }
        Stmt::Expr(e) => walk_exprs_deep(e, f),
        _ => {}
    }
}

/// True for the compound expression kinds whose `full_span()` covers a real multi-token
/// extent worth offering as a fold / selection region. Leaf and operator nodes (whose
/// `full_span()` is just `span()`) are excluded so we don't emit degenerate ranges.
fn is_extent_node(expr: &lin_parse::ast::Expr) -> bool {
    use lin_parse::ast::Expr as E;
    matches!(
        expr,
        E::Call { .. }
            | E::DotCall { .. }
            | E::Index { .. }
            | E::IndexAssign { .. }
            | E::Object(..)
            | E::Array(..)
            | E::Block(..)
            | E::If { .. }
            | E::Match { .. }
            | E::Function { .. }
    )
}

// ── folding ranges ─────────────────────────────────────────────────────────────

/// Emit folding ranges for multi-line constructs. Two sources:
///   - consecutive `import` statement runs (collapse into one `Imports` region),
///     driven off the parsed import statements' start lines;
///   - every compound expression (function bodies, object/array literals, blocks,
///     calls, `if`/`match`) whose AST `full_span()` covers more than one source line.
///
/// The compound-node extents come from the additive `Expr::full_span()` (opening token ..
/// closing delimiter / last child), which is AST-precise — the opening-token `span()` stays
/// unchanged for the formatter/coverage consumers. When the buffer fails to parse the AST is
/// empty, so we fall back to a balanced-delimiter text scan (`delimiter_folds`).
fn folding_ranges(source: &str, module: &lin_parse::ast::Module) -> Vec<FoldingRange> {
    let mut out = Vec::new();

    // Consecutive `import` statement runs collapse into one region. Each import's span
    // covers only the `import` keyword, but its start line is all we need here.
    let mut run_start_line: Option<u32> = None;
    let mut run_end_line: Option<u32> = None;
    let flush = |out: &mut Vec<FoldingRange>, s: Option<u32>, e: Option<u32>| {
        if let (Some(sl), Some(el)) = (s, e) {
            if el > sl {
                out.push(FoldingRange {
                    start_line: sl,
                    start_character: None,
                    end_line: el,
                    end_character: None,
                    kind: Some(FoldingRangeKind::Imports),
                    collapsed_text: None,
                });
            }
        }
    };
    for stmt in &module.statements {
        match stmt {
            Stmt::Import { span, .. } | Stmt::ForeignImport { span, .. } => {
                let line = offset_to_position(source, span.start as usize).line;
                if run_start_line.is_none() {
                    run_start_line = Some(line);
                }
                run_end_line = Some(line);
            }
            _ => {
                flush(&mut out, run_start_line.take(), run_end_line.take());
                run_end_line = None;
            }
        }
    }
    flush(&mut out, run_start_line.take(), run_end_line.take());

    // AST-precise region folds: one per compound node whose full extent spans >1 line.
    // De-duplicated by (start_line, end_line) so co-terminating nodes (e.g. a call whose
    // sole argument is the object literal it wraps) don't emit overlapping duplicates.
    let mut seen: HashSet<(u32, u32)> = HashSet::new();
    let mut push_region = |out: &mut Vec<FoldingRange>, sp: lin_common::Span| {
        let sl = offset_to_position(source, sp.start as usize).line;
        let el = offset_to_position(source, sp.end as usize).line;
        if el > sl && seen.insert((sl, el)) {
            out.push(FoldingRange {
                start_line: sl,
                start_character: None,
                end_line: el,
                end_character: None,
                kind: Some(FoldingRangeKind::Region),
                collapsed_text: None,
            });
        }
    };
    let mut had_ast = false;
    for stmt in &module.statements {
        walk_exprs_in_stmt(stmt, &mut |e| {
            had_ast = true;
            if is_extent_node(e) {
                push_region(&mut out, e.full_span());
            }
        });
    }

    // Fallback: if the buffer produced no expressions (parse failure / empty module), recover
    // region folds from a balanced-delimiter text scan so folding still works on broken input.
    if !had_ast {
        out.extend(delimiter_folds(source));
    }
    out
}

/// Fold every balanced `{}` / `[]` / `(...)` region that spans more than one line.
/// String literals are skipped so braces inside strings don't unbalance the scan. Used
/// only as a fallback when the AST is empty (the buffer didn't parse).
fn delimiter_folds(source: &str) -> Vec<FoldingRange> {
    let bytes = source.as_bytes();
    let mut stack: Vec<usize> = Vec::new();
    let mut out = Vec::new();
    let mut in_str = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' | b'[' | b'(' => stack.push(i),
            b'}' | b']' | b')' => {
                if let Some(open) = stack.pop() {
                    let sl = offset_to_position(source, open).line;
                    let el = offset_to_position(source, i).line;
                    if el > sl {
                        out.push(FoldingRange {
                            start_line: sl,
                            start_character: None,
                            end_line: el,
                            end_character: None,
                            kind: Some(FoldingRangeKind::Region),
                            collapsed_text: None,
                        });
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

// ── selection ranges ───────────────────────────────────────────────────────────

/// Build the smart-expand selection hierarchy at `offset`, innermost → outermost:
///   1. the identifier/word under the cursor (when on one);
///   2. each enclosing AST expression, by its `full_span()` (innermost first) — so a
///      cursor in `add(1, 2)` expands to the argument, then the whole call, etc.;
///   3. the cursor's source line;
///   4. the whole document.
///
/// The expression extents come from the additive `Expr::full_span()` (opening token ..
/// closing delimiter / last child), making the expansion AST-precise. When the buffer
/// fails to parse, the AST is empty, so we fall back to balanced-delimiter nesting
/// (`enclosing_bracket_pairs`).
fn selection_range_at(source: &str, module: &lin_parse::ast::Module, offset: usize) -> SelectionRange {
    let offset = offset.min(source.len());
    // Ordered innermost → outermost list of (start, end) byte ranges.
    let mut ranges: Vec<(usize, usize)> = Vec::new();

    // 1. Word under the cursor.
    let word = word_at(source, offset);
    if !word.is_empty() {
        // Recover the word's byte range (word_at expands around offset).
        let bytes = source.as_bytes();
        let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut start = offset;
        let mut end = offset;
        while start > 0 && is_word(bytes[start - 1]) {
            start -= 1;
        }
        while end < bytes.len() && is_word(bytes[end]) {
            end += 1;
        }
        ranges.push((start, end));
    }

    // 2. Enclosing AST expression extents (full_span), from the AST. Every expression whose
    //    full extent contains the cursor is a candidate; sorting by width yields the
    //    innermost-first nesting. Falls back to balanced-delimiter pairs when the AST is empty
    //    (parse failure) so selection still works on broken input.
    let mut ast_spans: Vec<(usize, usize)> = Vec::new();
    for stmt in &module.statements {
        // Statement-level extent (e.g. a whole `val x = ...`) so expansion reaches the stmt.
        let ss = stmt.span();
        if (ss.start as usize) <= offset && offset <= (ss.end as usize) {
            ast_spans.push((ss.start as usize, ss.end as usize));
        }
        walk_exprs_in_stmt(stmt, &mut |e| {
            let fs = e.full_span();
            let (s, en) = (fs.start as usize, fs.end as usize);
            if s <= offset && offset <= en {
                ast_spans.push((s, en));
            }
        });
    }
    if !ast_spans.is_empty() {
        // Innermost (narrowest) first.
        ast_spans.sort_by_key(|(s, e)| e - s);
        ranges.extend(ast_spans);
    } else {
        for (open, close) in enclosing_bracket_pairs(source, offset) {
            // Inner content (between the delimiters).
            if close > open + 1 {
                ranges.push((open + 1, close));
            }
            // The region including its delimiters.
            ranges.push((open, close + 1));
        }
    }

    // 3. The cursor's line.
    let line_start = source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = source[offset..].find('\n').map(|i| offset + i).unwrap_or(source.len());
    ranges.push((line_start, line_end));

    // 4. Whole document.
    ranges.push((0, source.len()));

    // De-duplicate while preserving order; keep only ranges that contain the cursor and
    // strictly grow (so each parent properly contains its child).
    let mut chain: Vec<(usize, usize)> = Vec::new();
    for (s, e) in ranges {
        if s > offset || e < offset {
            continue;
        }
        match chain.last() {
            Some(&(ps, pe)) if ps == s && pe == e => continue, // duplicate
            Some(&(ps, pe)) if s >= ps && e <= pe => continue,  // not strictly larger
            _ => chain.push((s, e)),
        }
    }
    if chain.is_empty() {
        chain.push((offset, offset));
    }

    // Build the nested chain from outermost down so the head is innermost.
    let mut current: Option<SelectionRange> = None;
    for &(s, e) in chain.iter().rev() {
        current = Some(SelectionRange {
            range: Range {
                start: offset_to_position(source, s),
                end: offset_to_position(source, e),
            },
            parent: current.map(Box::new),
        });
    }
    current.unwrap()
}

/// All balanced `{}`/`[]`/`()` pairs that enclose `offset`, as `(open_idx, close_idx)`
/// byte indices, ordered innermost → outermost. String literals are skipped so braces
/// inside strings don't unbalance the scan.
fn enclosing_bracket_pairs(source: &str, offset: usize) -> Vec<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut stack: Vec<usize> = Vec::new();
    let mut enclosing: Vec<(usize, usize)> = Vec::new();
    let mut in_str = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' | b'[' | b'(' => stack.push(i),
            b'}' | b']' | b')' => {
                if let Some(open) = stack.pop() {
                    // This pair encloses the cursor when open < offset <= close.
                    if open < offset && offset <= i {
                        enclosing.push((open, i));
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    // Sort innermost (smallest) → outermost.
    enclosing.sort_by_key(|(o, c)| c - o);
    enclosing
}

// ── document links (import paths) ──────────────────────────────────────────────

/// Make each `import ... from "PATH"` (and `import foreign "PATH"`) path string a
/// clickable `DocumentLink` to the resolved target file. `std/...` and any path that
/// doesn't resolve to a real on-disk file are skipped (no navigable target). Reuses
/// the same relative-path resolution as the import resolver (`base_dir/PATH.lin`).
fn import_document_links(
    source: &str,
    module: &lin_parse::ast::Module,
    base_dir: Option<&Path>,
) -> Vec<DocumentLink> {
    let mut out = Vec::new();
    let base = match base_dir {
        Some(b) => b,
        None => return out,
    };
    for stmt in &module.statements {
        let (path, stmt_span) = match stmt {
            Stmt::Import { path, span, .. } => (path, span),
            Stmt::ForeignImport { path, span, .. } => (path, span),
            _ => continue,
        };
        // stdlib has no on-disk file; skip.
        if stdlib_source(path).is_some() || path.starts_with("std/") {
            continue;
        }
        let target = base.join(format!("{}.lin", path));
        if !target.is_file() {
            continue;
        }
        let Ok(target_uri) = Url::from_file_path(&target) else { continue };
        // Locate the quoted PATH string within the statement source so only the path
        // (not the whole `import` keyword) is the clickable link.
        let Some(range) = quoted_path_range(source, *stmt_span, path) else { continue };
        out.push(DocumentLink {
            range,
            target: Some(target_uri),
            tooltip: None,
            data: None,
        });
    }
    out
}

/// Range of the quoted `"path"` token inside the import statement that begins at
/// `stmt_span`. The parser records only the `import` keyword's span, so the search
/// runs from the statement start to the end of its line. Returns the range covering
/// just the path characters between the quotes.
fn quoted_path_range(source: &str, stmt_span: lin_common::Span, path: &str) -> Option<Range> {
    let start = stmt_span.start as usize;
    // Scan to the end of the statement's line (import statements are single-line).
    let line_end = source[start..].find('\n').map(|i| start + i).unwrap_or(source.len());
    let hay = source.get(start..line_end)?;
    let needle = format!("\"{}\"", path);
    let rel = hay.find(&needle)?;
    let abs = start + rel + 1; // +1 to skip the opening quote
    Some(Range {
        start: offset_to_position(source, abs),
        end: offset_to_position(source, abs + path.len()),
    })
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
    let param_types = function_param_types(&ty_str)?;

    // Active parameter = number of top-level commas between the opening paren and the cursor.
    let active = top_level_commas(&source[paren_after..offset.min(source.len())]);
    let active = (active as usize).min(param_types.len().saturating_sub(1)) as u32;

    // Recover parameter NAMES (non-invasively) from the callee's binding AST when the callee is a
    // same-file `val`/`var` bound to a function literal. The function `Type` carries no names, so
    // we read them from the surface params. Only used when the name count matches the type-derived
    // param count (so positional bolding via `active` stays correct); otherwise fall back to
    // types-only labels.
    let callee_name = source.get(callee_span.start as usize..callee_span.end as usize).unwrap_or("");
    let names = callee_param_names(&analysis.module, callee_name)
        .filter(|n| n.len() == param_types.len());

    // Render one ParameterInformation per parameter as `name: Type` when names are known, else the
    // bare type. The overall signature label is rebuilt to match (so the client highlights the
    // right slice of the label string for the active parameter).
    let rendered: Vec<String> = param_types
        .iter()
        .enumerate()
        .map(|(i, ty)| match names.as_ref().and_then(|n| n.get(i)) {
            Some(name) => format!("{}: {}", name, ty),
            None => ty.clone(),
        })
        .collect();

    // Reconstruct `(p0, p1, …) => R` from the rendered params + the return type tail of `ty_str`.
    let return_tail = ty_str.find("=>").map(|i| &ty_str[i..]).unwrap_or("");
    let label = format!("({}) {}", rendered.join(", "), return_tail).trim_end().to_string();

    let parameters: Vec<ParameterInformation> = rendered
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

/// Recover the parameter names of `callee_name` when it's a top-level `val`/`var` in `module` bound
/// to a function literal. Each name comes from the param's binding pattern (`Pattern::Ident`);
/// destructured/wildcard params render as `_`. Returns `None` when no such function binding exists.
/// Non-invasive: reads only the surface AST — the checker's function `Type` is unchanged.
fn callee_param_names(module: &lin_parse::ast::Module, callee_name: &str) -> Option<Vec<String>> {
    use lin_parse::ast::Expr as E;
    if callee_name.is_empty() {
        return None;
    }
    for stmt in &module.statements {
        let (matches, value) = match stmt {
            Stmt::Val { pattern, value, .. } => {
                (pattern_ident(pattern).map(|(n, _)| n).as_deref() == Some(callee_name), value)
            }
            Stmt::Var { name, value, .. } => (name == callee_name, value),
            _ => continue,
        };
        if !matches {
            continue;
        }
        if let E::Function { params, .. } = value {
            return Some(
                params
                    .iter()
                    .map(|p| pattern_ident(&p.pattern).map(|(n, _)| n).unwrap_or_else(|| "_".to_string()))
                    .collect(),
            );
        }
    }
    None
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
        E::Block(stmts, tail, _, _) => {
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
        E::Block(stmts, tail, _, _) => {
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

/// Convert a byte offset into the source to an LSP `Position`. The `character` field is a
/// **UTF-16 code-unit** column (the LSP default `positionEncoding`), NOT a `char`/codepoint count:
/// a character outside the BMP (e.g. an emoji) is two UTF-16 units, so columns after it must be
/// advanced by `ch.len_utf16()`. Counting one-per-`char` (as a naive implementation does) misaligns
/// every range a client decodes once an astral codepoint appears on the line.
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
            character += ch.len_utf16() as u32;
        }
    }
    Position { line, character }
}

/// Convert an LSP `Position` (line + UTF-16 code-unit column) back to a byte offset into the source.
/// Mirrors `offset_to_position`: we accumulate UTF-16 code units per `char` (`ch.len_utf16()`) until
/// the target column is reached. A malformed `pos.character` that lands in the MIDDLE of a surrogate
/// pair (only possible if the client sends a bad position) is rounded to the nearest char boundary —
/// we stop at the char whose UTF-16 span would cross the target, returning a valid byte offset and
/// never panicking.
fn position_to_offset(source: &str, pos: Position) -> usize {
    let mut line = 0u32;
    let mut character = 0u32;
    for (i, ch) in source.char_indices() {
        if line == pos.line && character >= pos.character {
            return i;
        }
        if ch == '\n' {
            // Reached the end of the requested line before the requested column: clamp to the
            // newline's byte offset (the line is shorter than the client's column).
            if line == pos.line {
                return i;
            }
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16() as u32;
        }
    }
    source.len()
}

fn file_dir(uri: &Url) -> Option<PathBuf> {
    uri.to_file_path()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

// ── cross-file workspace index (Tier 3) ───────────────────────────────────────

// Cross-file references / rename / workspace-symbols are built on a *name-based*
// symbol index rather than the intra-file def_span machinery. The reason is
// structural: imported names are bound in the checker with `env.define` (no
// def_span — see lin-check `Stmt::Import` handling), so a use of an imported
// symbol carries `def_span = None` in `span_type_map`. `occurrences_at` therefore
// cannot link an import use back to its export across files. Instead we resolve a
// click to `(owner_module, export_name)` via each file's export/import tables and
// then collect occurrences of that name in every file that participates.
//
// The unit of cross-file resolution is the *top-level exported declaration*
// (`export val|var|type`). Local (non-exported) bindings and function parameters
// remain intra-file only — they're handled by the existing single-document
// `references`/`rename` path and are never reached here.

/// One file's contribution to the workspace index. Pure data derived from the
/// file's source + parsed AST; no I/O. `module_id` keys this entry in the index.
#[derive(Clone)]
struct FileSymbols {
    /// Full source text (used for text-scanning occurrences + position conversion).
    source: String,
    /// Top-level *exported* declarations: `(name, name_span)`. Only `export`-ed
    /// `val`/`var`/`type` are cross-file-visible.
    exports: Vec<(String, lin_common::Span)>,
    /// Import bindings this file declares (one per `{ a, b as c }` entry).
    imports: Vec<ImportRef>,
    /// Every top-level `type` declaration (exported OR local), `(name, name_span)`.
    /// Used only by go-to-type-definition, which must reach unexported type aliases.
    type_decls: Vec<(String, lin_common::Span)>,
    /// True for embedded stdlib modules — they are read-only and must NEVER receive
    /// rename edits (they're not real files on disk).
    is_stdlib: bool,
}

/// A single imported binding within a file.
#[derive(Clone)]
struct ImportRef {
    /// Resolved module identity of the *source* module (a canonical file path for
    /// user modules, or `std/...` for stdlib). Matches `module_identity`.
    module_id: String,
    /// The original exported name in the source module.
    export_name: String,
    /// The name bound in *this* file's scope (the alias when present, else `export_name`).
    local_name: String,
    /// Span of the whole `import` statement (used to locate the binding name in source).
    stmt_span: lin_common::Span,
}

/// Build a `FileSymbols` from a file's source. `base_dir` is the directory the file
/// lives in (used to resolve relative import paths to canonical module ids). Pure
/// over its inputs so the whole index is unit-testable without touching the FS.
fn file_symbols(source: &str, base_dir: &Path) -> FileSymbols {
    let mut lexer = lin_lex::Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    let mut parser = lin_parse::Parser::new(tokens);
    let module = parser.parse_module();

    let mut exports = Vec::new();
    let mut imports = Vec::new();
    let mut type_decls = Vec::new();
    for stmt in &module.statements {
        match stmt {
            Stmt::Val { pattern, exported: true, .. } => {
                if let Some((name, name_span)) = pattern_ident(pattern) {
                    exports.push((name, name_span));
                }
            }
            Stmt::Var { name, name_span, exported: true, .. } => {
                exports.push((name.clone(), *name_span));
            }
            Stmt::TypeDecl { name, exported, span, .. } => {
                type_decls.push((name.clone(), *span));
                if *exported {
                    exports.push((name.clone(), *span));
                }
            }
            Stmt::Import { bindings, path, span } => {
                let module_id = module_identity(path, base_dir);
                for binding in bindings {
                    let local = binding.alias.as_ref().unwrap_or(&binding.name).clone();
                    imports.push(ImportRef {
                        module_id: module_id.clone(),
                        export_name: binding.name.clone(),
                        local_name: local,
                        stmt_span: *span,
                    });
                }
            }
            _ => {}
        }
    }

    FileSymbols {
        source: source.to_string(),
        exports,
        imports,
        type_decls,
        is_stdlib: false,
    }
}

/// The cross-file index: `module_id -> FileSymbols`. Built from an in-memory
/// `path -> source` map (so the core logic is FS-independent and unit-testable);
/// the handlers populate it from open buffers + on-disk `*.lin` files.
#[derive(Default, Clone)]
struct WorkspaceIndex {
    files: HashMap<String, FileSymbols>,
}

impl WorkspaceIndex {
    /// Insert/replace a user file's entry, keyed by its canonical path id.
    fn insert_user_file(&mut self, path: &Path, source: &str) {
        let id = canonical_id(path);
        let base = path.parent().unwrap_or(path);
        let mut syms = file_symbols(source, base);
        syms.is_stdlib = false;
        self.files.insert(id, syms);
    }

    /// Insert an embedded stdlib module's entry (read-only; never renamed).
    fn insert_stdlib(&mut self, module_id: &str, source: &str) {
        // stdlib modules import other `std/...` modules; resolve those relative to
        // an arbitrary base (stdlib ids are absolute `std/...`, so base is unused).
        let mut syms = file_symbols(source, Path::new("."));
        syms.is_stdlib = true;
        self.files.insert(module_id.to_string(), syms);
    }

    /// Resolve a click at `offset` in the file identified by `module_id` to the
    /// symbol it refers to: `(owner_module_id, export_name)`. Returns `None` when
    /// the cursor isn't on a cross-file-resolvable top-level symbol (e.g. a local
    /// binding or parameter — those are handled intra-file by the caller).
    fn resolve_symbol(&self, module_id: &str, offset: usize) -> Option<(String, String)> {
        let file = self.files.get(module_id)?;
        let word = word_at(&file.source, offset);
        if word.is_empty() {
            return None;
        }
        // 1. The cursor is on this file's own exported declaration (or a use of it):
        //    owner is this file.
        if file.exports.iter().any(|(n, _)| n == word) {
            // Confirm the cursor actually sits on an occurrence of `word`, not some
            // unrelated token that merely shares a prefix — `word_at` already gives
            // the exact identifier under the cursor, so an equal name is sufficient.
            return Some((module_id.to_string(), word.to_string()));
        }
        // 2. The cursor is on an imported binding (or a use of it): owner is the
        //    source module + the original export name.
        if let Some(imp) = file.imports.iter().find(|i| i.local_name == word) {
            return Some((imp.module_id.clone(), imp.export_name.clone()));
        }
        None
    }

    /// All cross-file occurrences of the symbol `(owner_module, export_name)`, as
    /// `(module_id, span)` pairs. Includes:
    ///   (a) the export declaration site + every whole-word use in the owner file;
    ///   (b) in each file importing the symbol: the import-binding name + every
    ///       whole-word use of the *local* (possibly aliased) name.
    ///
    /// LIMITATION (alias-correct, conservative): occurrences are matched by whole
    /// identifier text within the relevant file. A local binding that *shadows* an
    /// imported/exported name in an inner scope would be over-matched; the language's
    /// top-level-export model makes this rare, and we prefer over-reporting reads
    /// (references) while gating *rename* on the same conservative set (below).
    fn occurrences(&self, owner_module: &str, export_name: &str) -> Vec<(String, lin_common::Span)> {
        let mut out: Vec<(String, lin_common::Span)> = Vec::new();

        // (a) Owner file: declaration site + uses of the export name.
        if let Some(owner) = self.files.get(owner_module) {
            for span in whole_word_spans(&owner.source, export_name) {
                out.push((owner_module.to_string(), span));
            }
        }

        // (b) Importing files: the import binding + local uses of the bound name.
        for (mod_id, file) in &self.files {
            if mod_id == owner_module {
                continue;
            }
            for imp in &file.imports {
                if imp.module_id == owner_module && imp.export_name == export_name {
                    // Local name occurrences (covers the import binding itself, which
                    // appears in the `import { ... }` clause, plus every body use).
                    for span in whole_word_spans(&file.source, &imp.local_name) {
                        out.push((mod_id.clone(), span));
                    }
                }
            }
        }

        out.sort_by(|a, b| (a.0.as_str(), a.1.start, a.1.end).cmp(&(b.0.as_str(), b.1.start, b.1.end)));
        out.dedup();
        out
    }

    /// Compute the SOUND set of rename edits for `(owner_module, export_name)` when
    /// renaming it to `new_name`, as `(module_id, span)` pairs. Differs from
    /// `occurrences` in two ways that matter for correctness:
    ///   1. stdlib owner / stdlib import targets are REFUSED entirely (returns
    ///      `None`) — stdlib is read-only and we must never emit edits into it.
    ///   2. Import-alias handling: for `import { foo as bar }`, only the EXPORT-side
    ///      token (`foo`) in the import clause is renamed; the alias `bar` and its
    ///      body uses are a distinct local binding and are left untouched. For a
    ///      non-aliased `import { foo }`, the clause token + all body uses rename
    ///      (local name == export name).
    ///
    /// Returns `None` when the rename is unsound/unsupported (stdlib symbol), so the
    /// handler can decline rather than produce a partial or wrong edit.
    fn rename_edits(
        &self,
        owner_module: &str,
        export_name: &str,
    ) -> Option<Vec<(String, lin_common::Span)>> {
        // Refuse to rename a symbol owned by a read-only stdlib module.
        if self.files.get(owner_module).map(|f| f.is_stdlib).unwrap_or(true) {
            return None;
        }

        let mut out: Vec<(String, lin_common::Span)> = Vec::new();

        // Owner file: decl + every use of the export name.
        if let Some(owner) = self.files.get(owner_module) {
            for span in whole_word_spans(&owner.source, export_name) {
                out.push((owner_module.to_string(), span));
            }
        }

        // Importing files.
        for (mod_id, file) in &self.files {
            if mod_id == owner_module {
                continue;
            }
            // A stdlib file can never import a user module, but guard anyway: never
            // edit stdlib sources.
            if file.is_stdlib {
                continue;
            }
            for imp in &file.imports {
                if imp.module_id != owner_module || imp.export_name != export_name {
                    continue;
                }
                if imp.local_name == imp.export_name {
                    // No alias: rename the clause token + every body use.
                    for span in whole_word_spans(&file.source, &imp.local_name) {
                        out.push((mod_id.clone(), span));
                    }
                } else {
                    // Aliased: rename ONLY the export-side token inside the import
                    // statement; leave the alias + its body uses alone.
                    if let Some(off) =
                        find_name_in_import(&file.source, imp.stmt_span.start as usize, &imp.export_name)
                    {
                        out.push((
                            mod_id.clone(),
                            lin_common::Span::new(0, off as u32, (off + imp.export_name.len()) as u32),
                        ));
                    }
                }
            }
        }

        out.sort_by(|a, b| (a.0.as_str(), a.1.start, a.1.end).cmp(&(b.0.as_str(), b.1.start, b.1.end)));
        out.dedup();
        Some(out)
    }

    /// Every module that exports `name`, as an import-path string suitable for an
    /// `import { name } from "<path>"` line. stdlib modules surface as their `std/...`
    /// id; user modules are returned as a path relative to `from_dir` (so the inserted
    /// import resolves the same way the import resolver would). The result is sorted
    /// (stdlib first, then alphabetical) and de-duplicated.
    fn modules_exporting(&self, name: &str, from_dir: Option<&Path>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (mod_id, file) in &self.files {
            if !file.exports.iter().any(|(n, _)| n == name) {
                continue;
            }
            if mod_id.starts_with("std/") {
                out.push(mod_id.clone());
            } else if let Some(rel) = import_path_for(mod_id, from_dir) {
                out.push(rel);
            }
        }
        out.sort_by(|a, b| {
            let a_std = a.starts_with("std/");
            let b_std = b.starts_with("std/");
            b_std.cmp(&a_std).then_with(|| a.cmp(b))
        });
        out.dedup();
        out
    }

    /// Every file that DIRECTLY imports from `module_id`, as a set of module ids.
    /// A file B depends on `module_id` when B has an `ImportRef` whose resolved
    /// `module_id` equals it (i.e. `import { ... } from "<module_id>"`). The owner
    /// itself is never returned, so this is safe to drive a re-check loop with even
    /// on a self/cyclic import graph (A→B, B→A): asking for A's dependents yields B
    /// and asking for B's yields A, but neither result contains the queried module,
    /// and the caller re-checks each dependent exactly once (no transitive chase) —
    /// so there is no recursion to bound.
    ///
    /// DIRECT only by design: transitive dependents are intentionally NOT walked
    /// here (see the `update` handler's re-check policy note for the rationale).
    /// Pure over the index, so it's unit-testable without the async handler.
    fn dependents_of(&self, module_id: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .files
            .iter()
            .filter(|(mid, file)| {
                mid.as_str() != module_id
                    && file.imports.iter().any(|imp| imp.module_id == module_id)
            })
            .map(|(mid, _)| mid.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Fuzzy-search every top-level declaration in the index for `query`,
    /// returning `(module_id, name, name_span, kind)` matches. An empty query
    /// matches everything (the client filters further). stdlib symbols are
    /// included so the user can locate stdlib definitions.
    fn workspace_symbols(&self, query: &str) -> Vec<(String, String, lin_common::Span)> {
        let mut out = Vec::new();
        for (mod_id, file) in &self.files {
            for (name, span) in &file.exports {
                if fuzzy_match(query, name) {
                    out.push((mod_id.clone(), name.clone(), *span));
                }
            }
        }
        out.sort_by(|a, b| (a.1.as_str(), a.0.as_str()).cmp(&(b.1.as_str(), b.0.as_str())));
        out
    }
}

/// Case-insensitive subsequence fuzzy match (the classic Ctrl+T behaviour): every
/// char of `query` appears in `name` in order. An empty query matches everything.
fn fuzzy_match(query: &str, name: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let mut q = query.chars().map(|c| c.to_ascii_lowercase()).peekable();
    for nc in name.chars().map(|c| c.to_ascii_lowercase()) {
        if let Some(&qc) = q.peek() {
            if nc == qc {
                q.next();
            }
        } else {
            break;
        }
    }
    q.peek().is_none()
}

/// The declaration name-span of `(owner_module, export_name)` when the owner file
/// is indexed and declares that export, else `None` (e.g. resolving into a stdlib
/// export whose decl we still want to *navigate* to but not rename).
fn decl_span(index: &WorkspaceIndex, owner_module: &str, export_name: &str) -> Option<lin_common::Span> {
    index
        .files
        .get(owner_module)?
        .exports
        .iter()
        .find(|(n, _)| n == export_name)
        .map(|(_, s)| *s)
}

/// Extract the leading named type from a rendered type string for go-to-type-def.
/// `Point` → `Point`; `Point[]` → `Point`; `Foo<Bar>` → `Foo`. Returns `None` for
/// anything that isn't a single leading identifier (unions, object literals,
/// function types, builtins like `Int32`/`String` we don't navigate to).
fn leading_type_name(ty: &str) -> Option<String> {
    let ty = ty.trim();
    // First identifier run at the start of the string.
    let name: String = ty
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    // The remainder must be only array brackets / generic args — i.e. the leading
    // name spans a single type, not e.g. `Int32 | String` where `Int32` is one
    // alternative of a union (which has no single decl to jump to).
    let rest = ty[name.len()..].trim_start();
    let is_single = rest.is_empty() || rest.starts_with('[') || rest.starts_with('<');
    if !is_single {
        return None;
    }
    // Don't bother navigating builtin primitive names (no user `type` decl).
    const BUILTINS: &[&str] = &[
        "String", "Boolean", "Null", "Number", "Json", "Error", "Object",
        "Int8", "Int16", "Int32", "Int64", "UInt8", "UInt16", "UInt32", "UInt64",
        "Float32", "Float64", "Iterator", "Iterable", "Function",
    ];
    if BUILTINS.contains(&name.as_str()) {
        return None;
    }
    Some(name)
}

/// Derive the `import`-statement path string that resolves to user module `module_id`
/// (a canonical absolute `.lin` file path) from a file in `from_dir`. Returns the path
/// relative to `from_dir` with the `.lin` extension stripped and forward-slash
/// separators (matching the `base_dir.join("{path}.lin")` resolver). Falls back to the
/// file stem when no relative base is available or the relative path can't be computed.
fn import_path_for(module_id: &str, from_dir: Option<&Path>) -> Option<String> {
    let target = Path::new(module_id);
    let stem_path = target.with_extension("");
    let rel = match from_dir {
        Some(dir) => pathdiff_relative(&stem_path, dir).unwrap_or(stem_path.clone()),
        None => stem_path.clone(),
    };
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        return target.file_stem().map(|s| s.to_string_lossy().to_string());
    }
    Some(s)
}

/// Compute `target` relative to `base` (both absolute) without touching the FS, e.g.
/// `("/a/b/leaf", "/a/c")` → `../b/leaf`. Returns `None` when either path isn't
/// absolute (the relative form would be meaningless).
fn pathdiff_relative(target: &Path, base: &Path) -> Option<PathBuf> {
    use std::path::Component;
    if !target.is_absolute() || !base.is_absolute() {
        return None;
    }
    let mut ta = target.components().peekable();
    let mut ba = base.components().peekable();
    // Skip the shared prefix.
    while let (Some(t), Some(b)) = (ta.peek(), ba.peek()) {
        if t == b {
            ta.next();
            ba.next();
        } else {
            break;
        }
    }
    let mut result = PathBuf::new();
    for c in ba {
        if let Component::Normal(_) = c {
            result.push("..");
        }
    }
    for c in ta {
        result.push(c.as_os_str());
    }
    Some(result)
}

/// Convert an index `module_id` back to a file `Url`. stdlib ids (`std/...`) have no
/// on-disk file and return `None` — that's how stdlib is kept out of edit results.
fn module_id_to_uri(module_id: &str) -> Option<Url> {
    if module_id.starts_with("std/") {
        return None;
    }
    Url::from_file_path(Path::new(module_id)).ok()
}

/// The exact identifier under `offset` (empty when the cursor is not on an
/// identifier character). Expands left and right over `[A-Za-z0-9_]`.
fn word_at(source: &str, offset: usize) -> &str {
    let bytes = source.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = offset.min(source.len());
    let mut end = offset.min(source.len());
    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }
    while end < bytes.len() && is_word(bytes[end]) {
        end += 1;
    }
    &source[start..end]
}

/// Every whole-identifier occurrence of `name` in `source`, as byte spans. A
/// match is whole-word when neither neighbouring byte is `[A-Za-z0-9_]`. This is
/// the cross-file occurrence primitive (the checker's def_span linkage is
/// unavailable for imported names — see the module note above).
fn whole_word_spans(source: &str, name: &str) -> Vec<lin_common::Span> {
    let mut out = Vec::new();
    if name.is_empty() {
        return out;
    }
    let bytes = source.as_bytes();
    let mut start = 0usize;
    while let Some(pos) = source[start..].find(name) {
        let abs = start + pos;
        let before_ok = abs == 0 || !(bytes[abs - 1].is_ascii_alphanumeric() || bytes[abs - 1] == b'_');
        let after_idx = abs + name.len();
        let after_ok = after_idx >= bytes.len()
            || !(bytes[after_idx].is_ascii_alphanumeric() || bytes[after_idx] == b'_');
        if before_ok && after_ok {
            out.push(lin_common::Span::new(0, abs as u32, after_idx as u32));
        }
        start = abs + 1;
    }
    out
}

/// Canonical id for a user file path: the canonicalised absolute path as a string
/// (falls back to the lexical path when the file doesn't exist on disk yet, e.g.
/// an unsaved in-memory buffer in tests). Matches `module_identity`'s user-module key.
fn canonical_id(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

/// Recursively collect `*.lin` files under `root`, skipping hidden directories,
/// `target/`, `node_modules/`, and `.lin-cache/`. Bounded depth guards against
/// pathological trees. Returns absolute paths.
fn collect_lin_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 32 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
                walk(&path, depth + 1, out);
            } else if name.ends_with(".lin") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out
}

// ── global workspace root + index (set on initialize, maintained on edits) ─────

static WORKSPACE_ROOT: std::sync::LazyLock<RwLock<Option<PathBuf>>> =
    std::sync::LazyLock::new(|| RwLock::new(None));

/// The live cross-file index. Built on `initialize`, refreshed per-file on
/// `did_open`/`did_change`/`did_save`. stdlib modules are seeded once.
static WORKSPACE_INDEX: std::sync::LazyLock<RwLock<WorkspaceIndex>> =
    std::sync::LazyLock::new(|| RwLock::new(WorkspaceIndex::default()));

/// Seed the index with every embedded stdlib module so go-to/references can see
/// stdlib exports (they're flagged read-only so rename never edits them).
fn seed_stdlib_index(index: &mut WorkspaceIndex) {
    for module_id in STDLIB_MODULE_IDS {
        if let Some(src) = stdlib_source(module_id) {
            index.insert_stdlib(module_id, src);
        }
    }
}

/// The full set of stdlib module ids (kept in sync with `stdlib_source` by the
/// `stdlib_modules_match_compiler` test, which enumerates the on-disk set).
const STDLIB_MODULE_IDS: &[&str] = &[
    "std/io", "std/json", "std/string", "std/number", "std/array", "std/iter",
    "std/object", "std/fs", "std/ffi", "std/http", "std/template", "std/async",
    "std/test", "std/time", "std/path", "std/math", "std/env", "std/hash",
    "std/bytes", "std/net", "std/process", "std/tty", "std/signal", "std/yaml",
    "std/jq", "std/stream", "std/compress", "std/archive", "std/event",
];

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
        let actions = code_actions(src, &dummy_uri(), &params, &WorkspaceIndex::default(), None);
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
        let actions = code_actions(src, &dummy_uri(), &params, &WorkspaceIndex::default(), None);
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

    // ── Tier-3: cross-file index ────────────────────────────────────────────────

    /// Build a `WorkspaceIndex` from an in-memory `(absolute_path, source)` map. Uses
    /// lexical (non-existent) paths so `canonical_id`/`module_identity` agree via their
    /// fall-back-to-lexical-path branch — no FS access, so the core logic is fully
    /// unit-testable. The same `WorkspaceIndex` is wired to the real FS in the handlers.
    fn index_from(files: &[(&str, &str)]) -> WorkspaceIndex {
        let mut index = WorkspaceIndex::default();
        seed_stdlib_index(&mut index);
        for (path, src) in files {
            index.insert_user_file(Path::new(path), src);
        }
        index
    }

    /// The canonical module id the index uses for a (non-existent) absolute path —
    /// matches what the handlers compute via `canonical_id` for a file URI.
    fn id_of(path: &str) -> String {
        canonical_id(Path::new(path))
    }

    /// Cross-file references: `foo` exported from a.lin and used in b.lin. A query
    /// from EITHER file returns occurrences in BOTH files.
    #[test]
    fn cross_file_references_span_both_files() {
        let a = "export val foo = 1\n";
        let b = "import { foo } from \"a\"\nval x = foo + foo\n";
        let index = index_from(&[("/ws/a.lin", a), ("/ws/b.lin", b)]);
        let a_id = id_of("/ws/a.lin");
        let b_id = id_of("/ws/b.lin");

        // Resolve from the export decl site in a.lin.
        let decl_off = a.find("foo").unwrap();
        let (owner, name) = index
            .resolve_symbol(&a_id, decl_off)
            .expect("should resolve the exported symbol");
        assert_eq!(owner, a_id);
        assert_eq!(name, "foo");

        let occ = index.occurrences(&owner, &name);
        let in_a = occ.iter().filter(|(m, _)| m == &a_id).count();
        let in_b = occ.iter().filter(|(m, _)| m == &b_id).count();
        // a.lin: the `export val foo` decl (1). b.lin: import binding `foo` + two uses (3).
        assert_eq!(in_a, 1, "expected 1 occurrence in a.lin, got {:?}", occ);
        assert_eq!(in_b, 3, "expected 3 occurrences in b.lin, got {:?}", occ);

        // Resolve from a USE site in b.lin → same symbol, same occurrence set.
        let use_off = b.rfind("foo").unwrap();
        let (owner2, name2) = index
            .resolve_symbol(&b_id, use_off)
            .expect("should resolve the imported symbol from its use");
        assert_eq!((owner2, name2), (owner, name));
    }

    /// Dependency computation for the on-edit re-check: when A changes, the set of
    /// files to re-check is exactly the DIRECT importers of A. Here B imports a
    /// symbol from A (so B is a dependent) and C is unrelated (so C is excluded).
    /// A itself is never in its own dependent set, which is what makes driving a
    /// re-check loop with this safe on cyclic graphs.
    #[test]
    fn dependents_of_includes_direct_importers_only() {
        let a = "export val foo = 1\n";
        let b = "import { foo } from \"a\"\nval x = foo\n";
        let c = "val unrelated = 99\n";
        let index = index_from(&[("/ws/a.lin", a), ("/ws/b.lin", b), ("/ws/c.lin", c)]);
        let a_id = id_of("/ws/a.lin");
        let b_id = id_of("/ws/b.lin");
        let c_id = id_of("/ws/c.lin");

        let deps = index.dependents_of(&a_id);
        assert!(deps.contains(&b_id), "B imports from A → must be a dependent: {:?}", deps);
        assert!(!deps.contains(&c_id), "C is unrelated → must be excluded: {:?}", deps);
        assert!(!deps.contains(&a_id), "A is never its own dependent: {:?}", deps);

        // C imports nothing → it has no dependents either.
        assert!(index.dependents_of(&c_id).is_empty());
    }

    /// Cyclic import graphs are safe to drive the re-check loop with: A↔B each list
    /// the OTHER as a dependent but never themselves, so editing A re-checks B once
    /// (and vice-versa) with no transitive chase and no recursion to bound.
    #[test]
    fn dependents_of_cyclic_graph_excludes_self_no_recursion() {
        let a = "import { b } from \"b\"\nexport val a = 1\n";
        let b = "import { a } from \"a\"\nexport val b = 2\n";
        let index = index_from(&[("/ws/a.lin", a), ("/ws/b.lin", b)]);
        let a_id = id_of("/ws/a.lin");
        let b_id = id_of("/ws/b.lin");

        let a_deps = index.dependents_of(&a_id);
        assert_eq!(a_deps, vec![b_id.clone()], "A's only dependent is B: {:?}", a_deps);
        assert!(!a_deps.contains(&a_id), "A must not depend on itself");

        let b_deps = index.dependents_of(&b_id);
        assert_eq!(b_deps, vec![a_id.clone()], "B's only dependent is A: {:?}", b_deps);
        assert!(!b_deps.contains(&b_id), "B must not depend on itself");
    }

    /// Cross-file rename emits a multi-file edit set keyed per file; stdlib is never edited.
    #[test]
    fn cross_file_rename_multi_file_and_never_stdlib() {
        let a = "export val foo = 1\n";
        let b = "import { foo } from \"a\"\nval x = foo\n";
        let index = index_from(&[("/ws/a.lin", a), ("/ws/b.lin", b)]);
        let a_id = id_of("/ws/a.lin");
        let b_id = id_of("/ws/b.lin");

        let edits = index
            .rename_edits(&a_id, "foo")
            .expect("renaming a user-owned export must be allowed");
        let in_a = edits.iter().filter(|(m, _)| m == &a_id).count();
        let in_b = edits.iter().filter(|(m, _)| m == &b_id).count();
        assert_eq!(in_a, 1, "a.lin decl renamed once: {:?}", edits);
        assert_eq!(in_b, 2, "b.lin import binding + 1 use renamed: {:?}", edits);
        // Every edit span must cover the text `foo`.
        for (m, s) in &edits {
            let src = if m == &a_id { a } else { b };
            assert_eq!(&src[s.start as usize..s.end as usize], "foo");
        }

        // Renaming a stdlib-owned export is refused (read-only) — no edits emitted.
        assert!(
            index.rename_edits("std/io", "print").is_none(),
            "stdlib symbols must never be renamed"
        );
        // And no edit set produced by any user rename targets a stdlib file: the
        // `std/...` ids have no on-disk URI.
        assert!(
            edits.iter().all(|(m, _)| module_id_to_uri(m).is_some()),
            "rename edits must only target real files, never stdlib: {:?}",
            edits
        );
    }

    /// Import-alias rename is conservative + correct: renaming the export `foo`
    /// rewrites the EXPORT-side token in `import { foo as bar }` but leaves the alias
    /// `bar` (and its body uses) untouched.
    #[test]
    fn cross_file_rename_respects_import_alias() {
        let a = "export val foo = 1\n";
        let b = "import { foo as bar } from \"a\"\nval x = bar + bar\n";
        let index = index_from(&[("/ws/a.lin", a), ("/ws/b.lin", b)]);
        let a_id = id_of("/ws/a.lin");
        let b_id = id_of("/ws/b.lin");

        // `rename_edits` takes the EXISTING symbol name; the new name is applied by
        // the handler when building TextEdits, so pass the current export name here.
        let edits = index.rename_edits(&a_id, "foo").expect("user export renameable");
        let a_edits: Vec<_> = edits.iter().filter(|(m, _)| m == &a_id).collect();
        let b_edits: Vec<_> = edits.iter().filter(|(m, _)| m == &b_id).collect();
        assert_eq!(a_edits.len(), 1);
        assert_eq!(b_edits.len(), 1, "only the export-side token renames: {:?}", edits);
        // The single b.lin edit must cover the `foo` in the import clause, NOT `bar`.
        let (_, span) = b_edits[0];
        assert_eq!(&b[span.start as usize..span.end as usize], "foo");
        // It must sit inside the import statement (line 0), not the body.
        assert!((span.start as usize) < b.find('\n').unwrap());
    }

    /// Workspace-symbol fuzzy filter aggregates top-level exports across files and
    /// honours the query; unexported decls are never symbols.
    #[test]
    fn workspace_symbol_fuzzy_filter() {
        let a = "export val parseHeader = 1\nexport val parseBody = 2\nval helper = 3\n";
        let b = "export val render = 4\n";
        let index = index_from(&[("/ws/a.lin", a), ("/ws/b.lin", b)]);

        // Fuzzy "pb" matches parseBody (subsequence p..b) but not parseHeader/render.
        let hits = index.workspace_symbols("pb");
        let names: Vec<&str> = hits.iter().map(|(_, n, _)| n.as_str()).collect();
        assert!(names.contains(&"parseBody"), "expected parseBody, got {:?}", names);
        assert!(!names.contains(&"parseHeader"), "pb must not match parseHeader: {:?}", names);
        assert!(!names.contains(&"render"), "pb must not match render: {:?}", names);

        // Non-exported `helper` is never a workspace symbol.
        let all = index.workspace_symbols("");
        let all_names: Vec<&str> = all.iter().map(|(_, n, _)| n.as_str()).collect();
        assert!(!all_names.contains(&"helper"), "unexported decls are not symbols: {:?}", all_names);
        assert!(all_names.contains(&"parseHeader") && all_names.contains(&"render"));
    }

    /// `leading_type_name` strips array/generic suffixes and refuses builtins/unions.
    #[test]
    fn leading_type_name_extracts_navigable_name() {
        assert_eq!(leading_type_name("Point").as_deref(), Some("Point"));
        assert_eq!(leading_type_name("Point[]").as_deref(), Some("Point"));
        assert_eq!(leading_type_name("Box<Int32>").as_deref(), Some("Box"));
        // Builtins have no user `type` decl to jump to.
        assert_eq!(leading_type_name("Int32"), None);
        assert_eq!(leading_type_name("String"), None);
        // A union's leading alternative is not a single navigable type.
        assert_eq!(leading_type_name("Foo | Bar"), None);
        // Object literal type — no single decl.
        assert_eq!(leading_type_name("{ x: Int32 }"), None);
    }

    /// go-to-type-definition data source: a value's named type resolves to its
    /// (local OR exported) `type` declaration via the index `type_decls` table.
    #[test]
    fn type_decls_capture_local_and_exported_types() {
        let a = "type Point = { x: Int32, y: Int32 }\nexport val origin = { x: 0, y: 0 }\n";
        let index = index_from(&[("/ws/a.lin", a)]);
        let a_id = id_of("/ws/a.lin");
        let file = index.files.get(&a_id).unwrap();
        // Local (unexported) type decl is captured for type navigation.
        assert!(
            file.type_decls.iter().any(|(n, _)| n == "Point"),
            "local type decl must be indexed for go-to-type-definition"
        );
        // It is NOT a workspace symbol (only exports are).
        assert!(!file.exports.iter().any(|(n, _)| n == "Point"));
    }

    /// The hand-maintained `STDLIB_MODULE_IDS` seed list must cover exactly the
    /// modules `stdlib_source` knows — drift would silently drop stdlib from the index.
    #[test]
    fn stdlib_module_ids_match_stdlib_source() {
        for id in STDLIB_MODULE_IDS {
            assert!(
                stdlib_source(id).is_some(),
                "STDLIB_MODULE_IDS has `{}` which stdlib_source doesn't know",
                id
            );
        }
        let stdlib_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../stdlib");
        for entry in std::fs::read_dir(&stdlib_dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(stem) = name.strip_suffix(".lin") else { continue };
            if stem.ends_with(".test") {
                continue;
            }
            let id = format!("std/{}", stem);
            assert!(
                STDLIB_MODULE_IDS.contains(&id.as_str()),
                "stdlib module `{}` missing from STDLIB_MODULE_IDS seed list",
                id
            );
        }
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

    // ── code lens (run-test) ────────────────────────────────────────────────────

    /// CodeLens discovery finds both `test("name", ...)` and `withFixture(..., "name", ...)`
    /// declarations (including ones nested inside a `suite(...)` array) and emits the fixed
    /// `lin.runTest(uri, name)` command, plus a single `lin.testFile` lens at the top.
    #[test]
    fn code_lens_discovers_tests() {
        let src = "import { suite, test, withFixture } from \"std/test\"\n\
                   val s = suite(\"x\", [\n\
                     test(\"alpha\", () => []),\n\
                     test(\"beta\", () => []),\n\
                   ])\n\
                   val w = withFixture(setup, \"gamma\", (f) => [])\n";
        let module = parse(src);
        let uri = dummy_uri();
        let lenses = test_code_lenses(src, &uri, &module);

        // One file-level lens + three test lenses.
        let run_tests: Vec<(&str, &str)> = lenses
            .iter()
            .filter_map(|l| {
                let c = l.command.as_ref()?;
                if c.command == "lin.runTest" {
                    let args = c.arguments.as_ref()?;
                    Some((args[1].as_str().unwrap(), c.title.as_str()))
                } else {
                    None
                }
            })
            .collect();
        let names: Vec<&str> = run_tests.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"alpha"), "missing alpha: {:?}", names);
        assert!(names.contains(&"beta"), "missing beta: {:?}", names);
        assert!(names.contains(&"gamma"), "missing gamma (withFixture): {:?}", names);
        // Title + command contract.
        assert!(run_tests.iter().all(|(_, t)| *t == "▶ Run Test"));
        // First arg is the document URI string.
        let first = lenses.iter().find(|l| l.command.as_ref().map(|c| c.command == "lin.runTest").unwrap_or(false)).unwrap();
        assert_eq!(
            first.command.as_ref().unwrap().arguments.as_ref().unwrap()[0].as_str().unwrap(),
            uri.to_string()
        );
        // Exactly one file-level "Run File Tests" lens.
        let file_lenses = lenses.iter().filter(|l| l.command.as_ref().map(|c| c.command == "lin.testFile").unwrap_or(false)).count();
        assert_eq!(file_lenses, 1, "expected one Run File Tests lens");
    }

    /// No tests in a file → no lenses (not even the file-level one).
    #[test]
    fn code_lens_empty_when_no_tests() {
        let src = "val x = 1\n";
        let module = parse(src);
        let lenses = test_code_lenses(src, &dummy_uri(), &module);
        assert!(lenses.is_empty(), "expected no lenses, got {:?}", lenses.len());
    }

    // ── folding ranges ──────────────────────────────────────────────────────────

    /// Folding ranges are AST-precise: an import-run region for the consecutive imports,
    /// plus one `Region` per multi-line compound node (the function body and the array
    /// literal), each spanning its full source extent (opening token .. closing delimiter).
    #[test]
    fn folding_range_covers_multiline_blocks_and_imports() {
        let src = "import { a } from \"std/io\"\n\
                   import { b } from \"std/array\"\n\
                   val f = (x: Int32) => {\n\
                     val y = x + 1\n\
                     y\n\
                   }\n\
                   val arr = [\n\
                     1,\n\
                     2,\n\
                   ]\n";
        let module = parse(src);
        let folds = folding_ranges(src, &module);
        // Import-run fold over the two import lines (0..1).
        assert!(
            folds.iter().any(|f| f.kind == Some(FoldingRangeKind::Imports)
                && f.start_line == 0
                && f.end_line == 1),
            "expected an import-run fold 0..1, got {:?}",
            folds
        );
        // The function literal's full extent: `(x: Int32) => {` (line 2) .. closing `}` (line 5).
        assert!(
            folds.iter().any(|f| f.kind == Some(FoldingRangeKind::Region)
                && f.start_line == 2
                && f.end_line == 5),
            "expected a function-body region fold 2..5, got {:?}",
            folds
        );
        // The array literal's full extent: opening `[` (line 6) .. closing `]` (line 9).
        assert!(
            folds.iter().any(|f| f.kind == Some(FoldingRangeKind::Region)
                && f.start_line == 6
                && f.end_line == 9),
            "expected an array-literal region fold 6..9, got {:?}",
            folds
        );
    }

    // ── selection ranges ─────────────────────────────────────────────────────────

    /// Smart-expand is AST-precise: a cursor on the inner `1` literal expands through the
    /// enclosing call's full extent (`add(1, 2)`), then the whole `val` statement, then the
    /// document — each range strictly containing the previous, driven by `Expr::full_span()`.
    #[test]
    fn selection_range_nests_innermost_to_outermost() {
        let src = "val r = add(1, 2)\n";
        let analysis = analyse(src, None);
        let off = src.find('1').unwrap();
        let sel = selection_range_at(src, &analysis.module, off);

        // Collect the innermost→outermost chain as the source text each range covers.
        let mut texts = Vec::new();
        let mut node = &sel;
        loop {
            let s = position_to_offset(src, node.range.start);
            let e = position_to_offset(src, node.range.end);
            texts.push(src[s..e].to_string());
            match &node.parent {
                Some(p) => node = p,
                None => break,
            }
        }
        // AST-precise expansion: the `1` literal, the enclosing call's full extent, the whole
        // statement, then the document.
        assert_eq!(
            texts,
            vec![
                "1".to_string(),
                "add(1, 2)".to_string(),
                "val r = add(1, 2)".to_string(),
                "val r = add(1, 2)\n".to_string(),
            ],
            "AST-precise selection chain"
        );

        // Each parent must still strictly contain its child.
        let mut node = &sel;
        while let Some(parent) = &node.parent {
            assert!(
                parent.range.start <= node.range.start && node.range.end <= parent.range.end,
                "child range escapes parent"
            );
            node = parent;
        }
    }

    // ── document links (import paths) ─────────────────────────────────────────────

    /// A relative `import ... from "PATH"` whose target file exists yields a DocumentLink
    /// pointing at that file; `std/...` imports are skipped (no on-disk file).
    #[test]
    fn document_link_resolves_import() {
        let dir = std::env::temp_dir().join(format!("lin_lsp_doclink_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("leaf.lin"), "export val leafVal = 1\n").unwrap();

        let src = "import { print } from \"std/io\"\nimport { leafVal } from \"leaf\"\n";
        let module = parse(src);
        let links = import_document_links(src, &module, Some(&dir));

        // Exactly one link (the relative `leaf` import); the stdlib one is skipped.
        assert_eq!(links.len(), 1, "expected one link, got {:?}", links);
        let link = &links[0];
        let target = link.target.as_ref().unwrap();
        assert!(target.to_string().ends_with("leaf.lin"), "link should target leaf.lin: {}", target);
        // The range covers the `leaf` text between the quotes (not the whole statement).
        let span_text = {
            let start = position_to_offset(src, link.range.start);
            let end = position_to_offset(src, link.range.end);
            &src[start..end]
        };
        assert_eq!(span_text, "leaf", "link range should be just the path text");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── auto-import code action ───────────────────────────────────────────────────

    /// An undefined name that a stdlib module exports offers an "Import ... from ..." quick fix
    /// whose edit inserts the import line.
    #[test]
    fn auto_import_action_offers_stdlib_import() {
        // `print` is undefined here (never imported) but exported by std/io.
        let src = "val x = print(\"hi\")\n";
        let analysis = analyse(src, None);
        // There must be an undefined-name diagnostic for `print`.
        assert!(
            analysis.diagnostics.iter().any(|d| undefined_name(&d.message).as_deref() == Some("print")),
            "expected an undefined `print` diagnostic, got {:?}",
            analysis.diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        let index = index_from(&[]); // stdlib is seeded.
        let params = code_action_params(src, analysis.diagnostics.clone());
        let actions = code_actions(src, &dummy_uri(), &params, &index, None);
        let titles = action_titles(&actions);
        assert!(
            titles.iter().any(|t| t == "Import `print` from \"std/io\""),
            "expected an auto-import action for print, got {:?}",
            titles
        );
        // The edit must insert an import line.
        let ca = actions.iter().find_map(|a| match a {
            CodeActionOrCommand::CodeAction(ca) if ca.title.contains("Import `print`") => Some(ca),
            _ => None,
        }).unwrap();
        let edits = ca.edit.as_ref().unwrap().changes.as_ref().unwrap().get(&dummy_uri()).unwrap();
        assert!(edits[0].new_text.contains("import { print } from \"std/io\""));
    }

    /// Auto-import merges into an existing import from the SAME module rather than adding a
    /// second `import ... from "std/io"` line.
    #[test]
    fn auto_import_edit_merges_existing_module_import() {
        let src = "import { print } from \"std/io\"\nval x = printErr(\"e\")\n";
        let edit = auto_import_edit(src, "printErr", "std/io").expect("expected a merge edit");
        // A merge inserts `, printErr` (not a whole new import line).
        assert_eq!(edit.new_text, ", printErr");
        // Inserted on line 0 (the existing import line).
        assert_eq!(edit.range.start.line, 0);
    }

    /// Already-imported name from the module → no edit (nothing to add).
    #[test]
    fn auto_import_edit_none_when_already_imported() {
        let src = "import { print } from \"std/io\"\nval x = 1\n";
        assert!(auto_import_edit(src, "print", "std/io").is_none());
    }

    // ── import-path completion ────────────────────────────────────────────────────

    /// Inside a `from "…"` string the prefix is detected and stdlib paths are completed.
    #[test]
    fn import_string_prefix_detects_from_context() {
        let src = "import { x } from \"std/ar\n";
        let off = src.find("std/ar").unwrap() + "std/ar".len();
        assert_eq!(import_string_prefix(src, off).as_deref(), Some("std/ar"));
        // Outside any import string → None.
        let plain = "val x = \"hi\"\n";
        let off2 = plain.find("hi").unwrap();
        assert!(import_string_prefix(plain, off2).is_none());
    }

    #[test]
    fn import_path_completions_offers_stdlib_modules() {
        let items = import_path_completions("std/ar", None);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"std/array"), "expected std/array in {:?}", labels);
        // No non-matching modules.
        assert!(labels.iter().all(|l| l.starts_with("std/ar")));
    }

    // ── signature help with parameter names (feature 13) ──────────────────────────

    /// Same-file callee bound to a function literal: signature help renders `name: Type`
    /// for each parameter (names pulled from the binding AST, no checker change).
    #[test]
    fn signature_help_includes_param_names() {
        let src = "val add = (a: Int32, b: Int32) => a + b\nval r = add(1, 2)\n";
        let analysis = analyse(src, None);
        let call_open = src.rfind('(').unwrap();
        let cursor = call_open + 1;
        let help = signature_help(src, &analysis, cursor).expect("expected signature help");
        let sig = &help.signatures[0];
        let params = sig.parameters.as_ref().unwrap();
        // Labels carry the parameter names.
        let labels: Vec<&str> = params.iter().filter_map(|p| match &p.label {
            ParameterLabel::Simple(s) => Some(s.as_str()),
            _ => None,
        }).collect();
        assert_eq!(labels, vec!["a: Int32", "b: Int32"], "param labels should be name: Type");
        // The signature label reflects names too.
        assert!(sig.label.contains("a: Int32"), "label should include names: {}", sig.label);
    }

    // ── UTF-16 position encoding (LSP default) ────────────────────────────────────

    /// A non-BMP character (an emoji, 2 UTF-16 code units / 4 bytes / 1 `char`) must advance the LSP
    /// column by 2, not 1. We assert offset↔position round-trips in UTF-16 units and that a position
    /// just after the emoji maps back to the byte offset just after it.
    #[test]
    fn position_encoding_is_utf16_for_astral_char() {
        // `val x = "😀ab"` — the emoji is U+1F600 (4 bytes, 2 UTF-16 units).
        let src = "val x = \"😀ab\"\n";
        let emoji_byte = src.find('😀').unwrap();
        let emoji_len = '😀'.len_utf8(); // 4
        assert_eq!('😀'.len_utf16(), 2);

        // Column at the emoji's start (it's preceded by `val x = "` = 9 bytes, all ASCII → col 9).
        let pos_emoji = offset_to_position(src, emoji_byte);
        assert_eq!(pos_emoji, Position { line: 0, character: 9 });

        // The byte right after the emoji is `a`. Its UTF-16 column must be 9 + 2 = 11 (NOT 10).
        let a_byte = emoji_byte + emoji_len;
        let pos_a = offset_to_position(src, a_byte);
        assert_eq!(pos_a, Position { line: 0, character: 11 }, "emoji must count as 2 UTF-16 units");

        // Round-trip: column 11 maps back to the byte offset of `a`.
        assert_eq!(position_to_offset(src, pos_a), a_byte);
        // And column 9 maps back to the emoji's byte start.
        assert_eq!(position_to_offset(src, pos_emoji), emoji_byte);

        // A position landing inside the surrogate pair (col 10, only sendable by a malformed client)
        // must NOT panic and must round to a char boundary — here the emoji's start byte.
        let mid = position_to_offset(src, Position { line: 0, character: 10 });
        assert_eq!(mid, a_byte, "mid-surrogate column rounds forward to the next char boundary");
    }

    /// CJK characters are in the BMP (1 UTF-16 unit, 3 bytes, 1 `char`): they advance the column by
    /// 1, and a combining char likewise stays 1 unit. This pins that BMP-multibyte text is unaffected
    /// by the UTF-16 fix (only astral codepoints differ from a naive char count).
    #[test]
    fn position_encoding_bmp_multibyte_is_one_unit() {
        // `日本` — each is 3 bytes, 1 UTF-16 unit.
        let src = "val s = \"日本\"\n";
        let first = src.find('日').unwrap(); // col 9
        assert_eq!(offset_to_position(src, first), Position { line: 0, character: 9 });
        let second = first + '日'.len_utf8();
        // Second CJK char is at col 10 (one UTF-16 unit past the first).
        assert_eq!(offset_to_position(src, second), Position { line: 0, character: 10 });
        assert_eq!(position_to_offset(src, Position { line: 0, character: 10 }), second);
    }
}
