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
use lin_parse::ast::{Stmt, TypeExpr};

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
            *WORKSPACE_ROOT.write().unwrap_or_else(|e| e.into_inner()) = Some(root.clone());
        }

        // Build the cross-file index: seed stdlib, then enumerate + index every
        // `*.lin` file under the workspace root. Files are re-indexed on edit, and
        // open DIRECT dependents are re-checked when an imported file changes (see
        // the `update` / `recheck_open_dependents` handlers).
        {
            let mut index = WORKSPACE_INDEX.write().unwrap_or_else(|e| e.into_inner());
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
                    // Docs are filled lazily in `completion_resolve` for the selected item only (the
                    // offered set can be large — every imported stdlib symbol — and extracting a doc
                    // re-lexes the owner module source, so eager resolution would be a hot-path cost).
                    resolve_provider: Some(true),
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
        self.docs.write().unwrap_or_else(|e| e.into_inner()).remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, vec![], None)
            .await;
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());
        let offset = position_to_offset(&source, pos);

        let Some((_, ty_str, def_span)) = tightest_span(&analysis.span_type_map, offset).cloned()
        else {
            return Ok(None);
        };

        // Resolve a doc comment for the hovered symbol, if it has one. Two sources, in order:
        //   1. a LOCAL binding in this file — its `def_span` points at the declaration's name in
        //      THIS source, so extract its leading doc block directly;
        //   2. an IMPORTED symbol (or this file's own export) — resolve it through the cross-file
        //      index to the owner module's source + decl span (the same path goto-def uses).
        let doc = hover_doc(&source, uri, def_span, offset);

        // Signature on top (```lin fence), docs below (Markdown) — rust-analyzer/TS style.
        // Clean raw solver type-var ids (`?T9004`) into readable generic names before display.
        let mut value = format!("```lin\n{}\n```", clean_type_string(&ty_str));
        if let Some(doc) = doc {
            let rendered = render_doc_markdown(&doc);
            if !rendered.is_empty() {
                value.push_str("\n\n---\n\n");
                value.push_str(&rendered);
            }
        }

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
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
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());
        let offset = position_to_offset(&source, pos);

        // Same-file first: a local/parameter use carries a `def_span` pointing at its binding in
        // THIS file. That's the most precise target, so prefer it.
        if let Some(def_span) = tightest_span(&analysis.span_type_map, offset).and_then(|(_, _, ds)| *ds) {
            let start = offset_to_position(&source, def_span.start as usize);
            let end = offset_to_position(&source, def_span.end as usize);
            return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range: Range { start, end },
            })));
        }

        // Cross-file fallback: an IMPORTED name has no intra-file `def_span` (imported bindings are
        // `env.define`d without one — see the cross-file index note). Resolve the symbol under the
        // cursor to its owner module via the SAME path references/rename use, then jump to the owner
        // file's export declaration span. stdlib owners have no on-disk URI (`module_id_to_uri`
        // returns `None`), so goto into stdlib yields nothing — acceptable.
        if let Ok(path) = uri.to_file_path() {
            let module_id = canonical_id(&path);
            let index = WORKSPACE_INDEX.read().unwrap_or_else(|e| e.into_inner());
            if let Some((owner, name)) = index.resolve_symbol(&module_id, offset) {
                if let (Some(decl), Some(owner_uri), Some(owner_file)) = (
                    decl_span(&index, &owner, &name),
                    module_id_to_uri(&owner),
                    index.files.get(&owner),
                ) {
                    // Convert the decl span using the OWNER file's source (offsets index into it),
                    // not the current file's.
                    return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                        uri: owner_uri,
                        range: span_to_range(&owner_file.source, decl),
                    })));
                }
            }
        }

        Ok(None)
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
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

        // Suppress completion inside a NORMAL string literal or a line comment. Import strings were
        // already handled above (they have their own path-completion), so any string we detect here
        // is an ordinary string where identifier/keyword completion would be noise. We return an
        // empty list rather than `None` so the client doesn't fall back to its own word-based list.
        if in_string_or_comment(&source, offset) {
            return Ok(Some(CompletionResponse::Array(Vec::new())));
        }

        let prefix = word_before(&source, offset);

        // Detect whether cursor is in a dot-completion context and resolve
        // the receiver's type category (e.g. "array", "string", "object").
        let (in_dot_context, receiver_category) = dot_receiver_category(&source, offset, &analysis.span_type_map);

        let mut items: Vec<CompletionItem> = Vec::new();

        // Classify the cursor's position so each completion source is offered only where it makes
        // sense (the user's report: bindings in expression position, no `import`/`from`/`as` there,
        // types only in type-annotation position). Dot context is its own path (handled below).
        let ctx = classify_completion_context(&analysis.module, &source, offset);

        // Binding-NAME position (`val ▮` / `var ▮`): the user is typing a fresh name — there is
        // nothing to complete. Return an empty list (not `None`, so the client doesn't fall back to
        // its own word list), gating off EVERY source below (bindings/keywords/types/imports). A dot
        // context can't co-occur with this (a `val name` line has no receiver), so this is safe.
        if ctx == CompletionContext::BindingName {
            return Ok(Some(CompletionResponse::Array(Vec::new())));
        }

        // In dot context only show applicable stdlib functions — no keywords/types/bindings.
        if !in_dot_context {
            // 1. In-scope bindings — offered in Expression / StatementStart, NOT in a type position.
            //    Two sources, deduped by name: (a) referenced bindings with an inferred type from
            //    `span_type_map`, and (b) ALL binders structurally in lexical scope at the cursor
            //    (so a defined-but-unreferenced lambda param like `item` is offered — the report's
            //    core fix). (a) is preferred for its richer inferred type detail.
            if matches!(ctx, CompletionContext::Expression | CompletionContext::StatementStart) {
                let mut seen: HashSet<String> = HashSet::new();
                // (a) referenced bindings (rich inferred type).
                for (_, ty_str, def_span) in &analysis.span_type_map {
                    if let Some(ds) = def_span {
                        // `.get` (not raw index) so a stale/multi-byte-misaligned def_span can't panic.
                        let name = source.get(ds.start as usize..ds.end as usize).unwrap_or("");
                        if name.is_empty() || !name.starts_with(|c: char| c.is_alphabetic() || c == '_') {
                            continue;
                        }
                        if !name.starts_with(prefix) || !seen.insert(name.to_string()) {
                            continue;
                        }
                        let kind = if ty_str.contains("=>") {
                            CompletionItemKind::FUNCTION
                        } else {
                            CompletionItemKind::VARIABLE
                        };
                        items.push(CompletionItem {
                            label: name.to_string(),
                            kind: Some(kind),
                            detail: Some(clean_type_string(ty_str)),
                            // Stash a resolve key so `completion_resolve` can fill the doc
                            // (lazily, for the selected item only) from this file's own decl.
                            data: completion_resolve_data(uri, name),
                            ..Default::default()
                        });
                    }
                }
                // (b) structural scope binders (covers unreferenced names absent from span_type_map).
                for b in collect_scope_bindings(&analysis.module, offset) {
                    if b.name.is_empty() || !b.name.starts_with(prefix) || !seen.insert(b.name.clone()) {
                        continue;
                    }
                    // Prefer the inferred type recorded for this binder's def_span (if it WAS used
                    // somewhere); else fall back to its AST annotation; else offer with no detail.
                    let detail = b
                        .def_span
                        .and_then(|ds| {
                            analysis
                                .span_type_map
                                .iter()
                                .find(|(_, _, d)| *d == Some(ds))
                                .map(|(_, ty, _)| clean_type_string(ty))
                        })
                        .or(b.annotated_ty.clone());
                    let kind = if b.is_function {
                        CompletionItemKind::FUNCTION
                    } else {
                        CompletionItemKind::VARIABLE
                    };
                    items.push(CompletionItem {
                        label: b.name.clone(),
                        kind: Some(kind),
                        detail,
                        data: completion_resolve_data(uri, &b.name),
                        ..Default::default()
                    });
                }
            }

            // 2. Keywords, gated by context (the report: NO `import`/`from`/`as`/`export` in an
            //    expression). Selection delegated to `keywords_for_context` so it's unit-testable.
            for kw in keywords_for_context(ctx) {
                if kw.starts_with(prefix) {
                    items.push(CompletionItem {
                        label: kw.to_string(),
                        kind: Some(CompletionItemKind::KEYWORD),
                        ..Default::default()
                    });
                }
            }

            // 3. Types — built-ins + user-defined `type` names + in-scope generic params. Offered
            //    ONLY in a type-annotation position (`val x: ▮`, param/return types, `type T = ▮`).
            if ctx == CompletionContext::TypeAnnotation {
                let builtin_types = [
                    "String", "Boolean", "Null", "Number", "Json", "Error",
                    "Int8", "Int16", "Int32", "Int64",
                    "UInt8", "UInt16", "UInt32", "UInt64",
                    "Float32", "Float64",
                    "Iterator", "Iterable", "Function",
                ];
                let mut seen_types: HashSet<String> = HashSet::new();
                for ty in builtin_types {
                    if ty.starts_with(prefix) && seen_types.insert(ty.to_string()) {
                        items.push(CompletionItem {
                            label: ty.to_string(),
                            kind: Some(CompletionItemKind::CLASS),
                            ..Default::default()
                        });
                    }
                }
                // User-defined type names + generic type params in scope.
                for ty in collect_type_names(&analysis.module, offset) {
                    if ty.starts_with(prefix) && seen_types.insert(ty.clone()) {
                        items.push(CompletionItem {
                            label: ty.clone(),
                            kind: Some(CompletionItemKind::CLASS),
                            ..Default::default()
                        });
                    }
                }
            }
        }

        // 4. Imported symbols — derived from THIS file's `import` statements (never a hardcoded
        // list). In dot context, filter to functions whose first parameter matches the receiver
        // category; otherwise offer every imported name. Keywords/types/bindings are suppressed in
        // dot context (handled above). Imported VALUES are not offered in a TypeAnnotation position
        // (a type is expected there, not a value), but the dot-context path is unconditional.
        let filter_cat = if in_dot_context {
            Some(receiver_category.as_deref().unwrap_or("any"))
        } else {
            None
        };
        let suppress_imports_for_type = !in_dot_context && ctx == CompletionContext::TypeAnnotation;
        for imp in &analysis.imported_names {
            if suppress_imports_for_type {
                break;
            }
            if !imp.name.starts_with(prefix) {
                continue;
            }
            // Dot-context filtering: only show items applicable to the receiver type. The decision
            // is delegated to `dot_item_applies` so it can be unit-tested directly. The key change
            // from the old logic: a RECEIVER-POLYMORPHIC combinator (first-param category "any",
            // e.g. `map`/`filter`/`reduce` whose first param renders as generic/`Json`/union) is now
            // OFFERED on any receiver instead of being dropped — that idiom is the whole point of
            // `xs.map(...)`.
            if let Some(cat) = filter_cat {
                let first_param_cat = imp.ty.as_deref().and_then(first_param_category);
                if !dot_item_applies(cat, first_param_cat.as_deref()) {
                    continue;
                }
            }
            let kind = match imp.ty.as_deref() {
                Some(t) if t.contains("=>") => CompletionItemKind::FUNCTION,
                _ => CompletionItemKind::VALUE,
            };
            items.push(CompletionItem {
                label: imp.name.clone(),
                kind: Some(kind),
                detail: imp.ty.as_deref().map(clean_type_string),
                // Fallback documentation (the source module) shown until — and if — `completion_resolve`
                // upgrades it to the symbol's rendered doc comment. The resolve key is the importing
                // file + the local name, so the resolver walks the SAME cross-file path as hover.
                documentation: Some(Documentation::String(format!("from {}", imp.module))),
                data: completion_resolve_data(uri, &imp.name),
                ..Default::default()
            });
        }

        // 5. UNIMPORTED stdlib combinators/methods in dot-context (FIX B). In addition to the
        // already-imported symbols above, offer applicable stdlib exports the file hasn't imported
        // yet — so `xs.map`/`xs.for`/`xs.filter` complete and inserting one ALSO adds its import.
        // Gated by `dot_item_applies` against the receiver category (so a `.` doesn't dump the whole
        // stdlib), deduped against names already offered (imported symbols + earlier candidates), and
        // resolved lazily: the import `additionalTextEdits` + doc are filled in `completion_resolve`
        // from the `{ module, name }` stash, keeping the offered list cheap.
        if in_dot_context {
            let already: HashSet<String> = items.iter().map(|i| i.label.clone()).collect();
            items.extend(stdlib_dot_completion_items(
                STDLIB_DOT_CANDIDATES.iter(),
                receiver_category.as_deref().unwrap_or("any"),
                prefix,
                &already,
                uri,
            ));
        }

        items.dedup_by(|a, b| a.label == b.label && a.detail == b.detail);

        Ok(Some(CompletionResponse::Array(items)))
    }

    /// Lazily fill a completion item's `documentation` with the symbol's rendered doc comment, for
    /// the SELECTED item only (advertised via `resolve_provider: true`). The item carries a
    /// `{ uri, name }` key in `data` (set by the `completion` handler); we re-resolve the doc from
    /// that file's current buffer (local decl first, then the cross-file index) — the same path hover
    /// uses — and replace the placeholder "from <module>" string with rich Markdown when found. When
    /// there's no doc (or no resolvable key), the item is returned unchanged.
    async fn completion_resolve(&self, mut item: CompletionItem) -> Result<CompletionItem> {
        let Some((uri, name)) = parse_completion_resolve_data(item.data.as_ref()) else {
            return Ok(item);
        };
        let Some(source) = self.docs.read().unwrap_or_else(|e| e.into_inner()).get(&uri).cloned()
        else {
            return Ok(item);
        };
        let base_dir = file_dir(&uri);
        let analysis = analyse(&source, base_dir.as_deref());

        // Unimported stdlib candidate (FIX B): the item carries an owner `module`. Attach the
        // import edit (computed by the SAME `auto_import_edit` the auto-import code action uses, so
        // they produce identical edits — merge into an existing import from the module, else a new
        // line) and resolve the doc from the OWNER module rather than this file.
        let candidate_module = parse_completion_resolve_module(item.data.as_ref());
        let doc = if let Some(module) = candidate_module.as_deref() {
            if let Some(edit) = auto_import_edit(&source, &name, module) {
                item.additional_text_edits = Some(vec![edit]);
            }
            let index = WORKSPACE_INDEX.read().unwrap_or_else(|e| e.into_inner());
            resolve_doc_via_index(&index, module, &name).filter(|d| !d.is_empty())
        } else {
            // Local declaration in this file first; then imports / own exports via the index.
            let mut doc = local_decl_name_span(&analysis.module, &name)
                .and_then(|span| extract_doc(&source, span))
                .filter(|d| !d.is_empty());
            if doc.is_none() {
                if let Ok(path) = uri.to_file_path() {
                    let module_id = canonical_id(&path);
                    let index = WORKSPACE_INDEX.read().unwrap_or_else(|e| e.into_inner());
                    doc = resolve_doc_via_index(&index, &module_id, &name).filter(|d| !d.is_empty());
                }
            }
            doc
        };

        if let Some(doc) = doc {
            let rendered = render_doc_markdown(&doc);
            if !rendered.is_empty() {
                item.documentation = Some(Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: rendered,
                }));
            }
        }
        Ok(item)
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
        // `offset_to_position` takes a CHAR offset, so the end-of-document offset is the char
        // count, not the byte length.
        let end_pos = offset_to_position(&source, source.chars().count());

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
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
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
            let index = WORKSPACE_INDEX.read().unwrap_or_else(|e| e.into_inner());
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
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(&params.text_document.uri).cloned() {
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
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
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
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());
        let offset = position_to_offset(&source, pos);

        // Cross-file first: a top-level exported/imported symbol renames everywhere.
        if let Ok(path) = uri.to_file_path() {
            let module_id = canonical_id(&path);
            let index = WORKSPACE_INDEX.read().unwrap_or_else(|e| e.into_inner());
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
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
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
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
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
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };
        let base_dir = file_dir(uri);
        let analysis = analyse(&source, base_dir.as_deref());
        let offset = position_to_offset(&source, pos);

        // Resolve a callee's doc comment by NAME: a local declaration in this file first (its leading
        // block lives in `source`), then the cross-file index (imports + own exports).
        let module_id = uri.to_file_path().ok().map(|p| canonical_id(&p));
        let index = WORKSPACE_INDEX.read().unwrap_or_else(|e| e.into_inner()).clone();
        let resolve = |name: &str| -> Option<DocComment> {
            if let Some(span) = local_decl_name_span(&analysis.module, name) {
                if let Some(doc) = extract_doc(&source, span) {
                    if !doc.is_empty() {
                        return Some(doc);
                    }
                }
            }
            let mid = module_id.as_deref()?;
            resolve_doc_via_index(&index, mid, name).filter(|d| !d.is_empty())
        };

        Ok(signature_help(&source, &analysis, offset, resolve))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
            Some(s) => s,
            None => return Ok(None),
        };

        let base_dir = file_dir(uri);
        // Snapshot the index under the read guard, then DROP it before the (re-lex/parse + loop)
        // work in `code_actions` — mirroring `recheck_open_dependents`. Holding the guard across that
        // work would let one panic-capable code path poison the lock for the rest of the session.
        let index = {
            let guard = WORKSPACE_INDEX.read().unwrap_or_else(|e| e.into_inner());
            guard.clone()
        };
        let actions = code_actions(&source, uri, &params, &index, base_dir.as_deref());
        if actions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(actions))
        }
    }

    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        let uri = &params.text_document.uri;
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
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
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
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
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
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
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
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
        let index = WORKSPACE_INDEX.read().unwrap_or_else(|e| e.into_inner());
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
        let source = match self.docs.read().unwrap_or_else(|e| e.into_inner()).get(uri).cloned() {
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
        let index = WORKSPACE_INDEX.read().unwrap_or_else(|e| e.into_inner());
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
            let index = WORKSPACE_INDEX.read().unwrap_or_else(|e| e.into_inner());
            index.dependents_of(&changed_id)
        };
        if dependent_ids.is_empty() {
            return;
        }

        // Snapshot the open docs (uri + source) for the dependents we care about,
        // again releasing the lock before awaiting. A dependent that isn't currently
        // open is skipped — the client only renders open documents.
        let to_recheck: Vec<(Url, String)> = {
            let docs = self.docs.read().unwrap_or_else(|e| e.into_inner());
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
        .or_else(|| WORKSPACE_ROOT.read().unwrap_or_else(|e| e.into_inner()).clone())
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
    // `import_start` is a CHAR offset (an import statement's `span.start`); convert to a byte index
    // to slice/search the UTF-8 source. `.get` so a stale/out-of-range start can't panic the slice.
    let import_byte = char_offset_to_byte(source, import_start);
    let search_area = source.get(import_byte..)?;
    let pos = search_area.find(name)?;
    let abs = import_byte + pos; // byte index of the match
    // Make sure it's a whole identifier (the neighbouring bytes aren't word characters).
    let before_ok = abs == 0 || !source.as_bytes()[abs - 1].is_ascii_alphanumeric() && source.as_bytes()[abs - 1] != b'_';
    let after_ok = abs + name.len() >= source.len() || !source.as_bytes()[abs + name.len()].is_ascii_alphanumeric() && source.as_bytes()[abs + name.len()] != b'_';
    // Return a CHAR offset (callers feed it to `offset_to_position`, which expects char offsets).
    if before_ok && after_ok { Some(byte_offset_to_char(source, abs)) } else { None }
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
        "std/bytes"    => Some(include_str!("../../../stdlib/bytes.lin")),
        "std/regex"       => Some(include_str!("../../../stdlib/regex.lin")),
        "std/crypto"      => Some(include_str!("../../../stdlib/crypto.lin")),
        "std/csv"         => Some(include_str!("../../../stdlib/csv.lin")),
        "std/encoding"    => Some(include_str!("../../../stdlib/encoding.lin")),
        "std/random"      => Some(include_str!("../../../stdlib/random.lin")),
        "std/bignum"      => Some(include_str!("../../../stdlib/bignum.lin")),
        "std/decimal"     => Some(include_str!("../../../stdlib/decimal.lin")),
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

/// Export name→type map for an imported module, used to enrich completion detail / signature for
/// imported symbols.
///
/// LIMITATION (FIX 5, intentionally not implemented): only `val` exports are surfaced. `var` and
/// `type` exports are NOT, because they cannot be recovered from the typed IR here: `TypedStmt::Var`
/// carries only a `slot` (no `name`), and `type` declarations are ERASED entirely (there is no
/// `TypedStmt::TypeDecl`). So adding arms for them is not the trivial, obviously-correct change the
/// task scoped FIX 5 to — surfacing them would require either a name on `TypedStmt::Var` or reading
/// the surface AST and re-deriving types, neither of which is low-risk. The user-visible effect is
/// minor: completion of an IMPORTED `var`/`type` shows no inferred-type detail (the name still
/// completes via the cross-file index; hover/goto on the export site itself are unaffected).
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

/// The set of top-level names declared with `export` in a PARSED module. We need this because the
/// `export` flag is DROPPED when the AST is lowered to a `TypedModule` (`TypedStmt::Val` has no
/// `exported` field), so `extract_exports(&typed)` returns ALL top-level `val`s — including private
/// helpers like `_bisect` (declared `val _bisect`, not `export val`). Callers that must honour the
/// real export surface (e.g. the stdlib dot-completion candidate builder) intersect the typed
/// name→type map with this set. The flag lives on `Stmt::Val { exported }` / `Stmt::Var { exported }`
/// (verified in `lin-parse/src/ast.rs`); we read it from the AST, then join by name to the typed
/// types. `val`-binding names come from the binding `Pattern` (a simple `export val foo = …` is a
/// `Pattern::Ident`); destructuring exports are uncommon at stdlib top level and need no special case
/// here (each bound name is still surfaced via the typed module — they just won't appear in this set,
/// which is the conservative/correct behaviour for an export filter).
fn ast_exported_names(module: &lin_parse::ast::Module) -> HashSet<String> {
    use lin_parse::ast::Pattern;
    let mut out = HashSet::new();
    for stmt in &module.statements {
        match stmt {
            Stmt::Val { exported: true, pattern, .. } => {
                collect_pattern_names(pattern, &mut out);
            }
            Stmt::Var { exported: true, name, .. } => {
                out.insert(name.clone());
            }
            _ => {}
        }
    }
    // Helper that walks a binding pattern, collecting the simple/identifier binders it introduces.
    fn collect_pattern_names(pat: &Pattern, out: &mut HashSet<String>) {
        match pat {
            Pattern::Ident(n, _) => {
                out.insert(n.clone());
            }
            Pattern::Object(fields, rest, _) => {
                for f in fields {
                    collect_pattern_names(&f.pattern, out);
                }
                if let Some(r) = rest {
                    out.insert(r.clone());
                }
            }
            Pattern::Array(items, rest, _) => {
                for p in items {
                    collect_pattern_names(p, out);
                }
                if let Some(r) = rest {
                    out.insert(r.clone());
                }
            }
            // TypeName/Literal/Wildcard introduce no value binder.
            _ => {}
        }
    }
    out
}

// ── type-string display cleaning ──────────────────────────────────────────────

/// Decimal text of `u32::MAX`, the `TypeVar` id the checker uses as the `Json` marker
/// (`Type::is_json`). It Displays as `?T4294967295`, which we surface as `Json`.
const JSON_TYPEVAR_DECIMAL: &str = "4294967295";

/// Turn a rendered type string into a clean, user-facing one for display surfaces (completion
/// `detail`, hover, signature help). The checker's `Type::Display` renders an unsolved/generic
/// `TypeVar(id)` as the raw solver token `?T<id>` (and the `Json` marker — `TypeVar(u32::MAX)` —
/// as `?T4294967295`). Those leak ugly internal ids like `?T9004` into the UI.
///
/// This rewrites every `?T<id>` token in the string:
///   - the `Json` marker (`?T4294967295`) → `Json`;
///   - any other id → a stable generic NAME `T`, `U`, `V`, … assigned in order of FIRST appearance,
///     so the SAME id always maps to the SAME letter within one string and DISTINCT ids get distinct
///     letters. After `Z` it falls back to `T1`, `T2`, … so it never collides or panics.
///
/// The mapping is per-string and deterministic in the id's textual position, NOT in the solver's
/// id values — so the same signature renders identically regardless of solver run-order, and the
/// transform is idempotent (its own output contains no `?T` tokens to rewrite again).
fn clean_type_string(s: &str) -> String {
    // Fast path: nothing to do when there's no `?T` token at all.
    if !s.contains("?T") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    // id-text → assigned display name, so repeats of the same var reuse the same letter.
    let mut assigned: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut next_index: usize = 0;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Recognise the `?T<digits>` token shape.
        if bytes[i] == b'?' && i + 1 < bytes.len() && bytes[i + 1] == b'T' {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 2 {
                // We matched at least one digit → it's a TypeVar token.
                let id_text = &s[i + 2..j];
                if id_text == JSON_TYPEVAR_DECIMAL {
                    out.push_str("Json");
                } else {
                    let name = assigned.entry(id_text.to_string()).or_insert_with(|| {
                        let n = generic_name(next_index);
                        next_index += 1;
                        n
                    });
                    out.push_str(name);
                }
                i = j;
                continue;
            }
        }
        // Not a TypeVar token — copy the byte through. `s` is valid UTF-8 and we only ever advance
        // past whole tokens or single bytes that are not the start of a multi-byte sequence here
        // (`?` is ASCII), so pushing the char at `i` is safe.
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The `n`-th generic display name: `T, U, V, W, X, Y, Z`, then `T1, T2, …` past `Z`.
fn generic_name(n: usize) -> String {
    // Single letters T..Z (7 names) cover essentially every real signature.
    const LETTERS: &[u8] = b"TUVWXYZ";
    if n < LETTERS.len() {
        (LETTERS[n] as char).to_string()
    } else {
        format!("T{}", n - LETTERS.len() + 1)
    }
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
    // `offset` and the offsets derived from it here are CHAR offsets. The prefix word is ASCII
    // (`[A-Za-z0-9_]`), so its byte length equals its char count — usable directly in char space.
    let prefix_len = word_before(source, offset).len();
    let dot_offset = match offset.checked_sub(prefix_len + 1) {
        Some(o) => o,
        None => return (false, None),
    };

    // Index the source BY BYTE to read the `.` char: convert the char offset to a byte index first.
    let dot_byte = char_offset_to_byte(source, dot_offset);
    if source.as_bytes().get(dot_byte) != Some(&b'.') {
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

/// Decide whether an imported function should appear in `xs.` dot-completion, given the receiver's
/// category (`receiver_cat`) and the function's FIRST-parameter category (`first_param_cat`, `None`
/// when the item has no resolvable function signature).
///
/// An item APPLIES when any of the following hold:
///   - the receiver category is unknown (`"any"`) — we can't filter, so offer everything;
///   - the first-param category is `"any"` — a RECEIVER-POLYMORPHIC combinator (generic/`Json`/
///     union first param, e.g. `map`/`filter`/`reduce`). These are the core dot-applied idiom and
///     must be offered on every receiver; the previous exact-match filter wrongly dropped them;
///   - the item has no signature at all (`None`) — we can't prove it inapplicable, so keep it;
///   - the first-param category EQUALS the receiver category (a concrete, matching method).
///
/// It is EXCLUDED only when both categories are concrete AND differ (e.g. a `string`-typed first
/// param on an `array` receiver).
fn dot_item_applies(receiver_cat: &str, first_param_cat: Option<&str>) -> bool {
    if receiver_cat == "any" {
        return true;
    }
    match first_param_cat {
        None | Some("any") => true,
        Some(fc) => fc == receiver_cat,
    }
}

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

// ── completion-context classification (scope/position awareness) ──────────────

/// Where the cursor sits, used to gate which completion sources are offered. Classified by
/// `classify_completion_context` from the parsed AST + a light backward char scan. The variants:
///
///   - `TypeAnnotation` — a type is expected: after a `:` governing a `val`/`var`/param/field
///     binding, after a function `): ` return type, inside a `type Name = …` RHS, or with the
///     cursor already inside a parsed `TypeExpr`. Offers built-in + user type names; NOT bindings.
///   - `ImportStmt` — the cursor is inside an `import … from "…"` statement (outside its path
///     string, which has its own handler). The only place `from`/`as` are offered.
///   - `StatementStart` — the cursor begins a fresh statement (only whitespace precedes it on the
///     line, at top level or block level). Offers declaration keywords `val`/`var`/`type`/`import`/
///     `export` in addition to the expression sources.
///   - `Expression` — the common fallback: a function/lambda body, a `val`/`var` RHS, a call
///     argument, etc. Offers in-scope bindings, imported symbols, expression keywords + literals.
///
/// Anything the classifier can't confidently place falls back to `Expression` (the safe default —
/// it offers bindings, which is what the user usually wants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionContext {
    TypeAnnotation,
    ImportStmt,
    StatementStart,
    Expression,
    /// The cursor sits right after a `val`/`var` keyword in the binding-NAME position
    /// (`val ▮` / `var ▮`, possibly mid-typing the name), before any `:` or `=` on the line.
    /// The user is about to TYPE A NEW NAME, so there is nothing to complete — every completion
    /// source is gated off and the handler returns an empty list (like `in_string_or_comment`).
    BindingName,
}

/// The keyword set offered in a given completion context. Pure (no I/O) so the gating policy is
/// unit-testable. The split enforces the report's requirements:
///   - declaration keywords (`val`/`var`/`type`/`export`/`import`) only at a `StatementStart`;
///   - `from`/`as` only inside an `ImportStmt` (never in expression position — the user's complaint);
///   - expression-level keywords (`if`/`then`/`else`/`match`/`is`/`has`/`when`) + value literals
///     (`true`/`false`/`null`) in `Expression` (and at a `StatementStart`, which can begin with an
///     expression statement);
///   - nothing in a `TypeAnnotation` (types are offered there instead, see the handler).
fn keywords_for_context(ctx: CompletionContext) -> &'static [&'static str] {
    match ctx {
        CompletionContext::StatementStart => &[
            "val", "var", "type", "export", "import",
            "if", "then", "else", "match", "is", "has", "when",
            "true", "false", "null",
        ],
        CompletionContext::Expression => &[
            "if", "then", "else", "match", "is", "has", "when",
            "true", "false", "null",
        ],
        CompletionContext::ImportStmt => &["from", "as"],
        CompletionContext::TypeAnnotation => &[],
        // Binding-NAME position (`val ▮`): the user is typing a fresh name — offer nothing.
        CompletionContext::BindingName => &[],
    }
}

/// Classify the completion cursor position. Combines a structural AST signal (cursor inside a
/// parsed `TypeExpr` ⇒ TypeAnnotation; inside an `Import` statement span ⇒ ImportStmt) with a light
/// BACKWARD char scan over the current line/preceding tokens to catch the fine cases the AST can't
/// (a `:` just typed before any type text exists ⇒ TypeAnnotation; an empty line at block level ⇒
/// StatementStart). `offset` is a CHAR offset (the canonical Lin span space). Falls back to
/// `Expression` whenever no stronger signal applies.
fn classify_completion_context(
    module: &lin_parse::ast::Module,
    source: &str,
    offset: usize,
) -> CompletionContext {
    // 1. Inside an `import … from "…"` statement (outside the path string — that's handled earlier).
    //    This is the ONLY place `from`/`as` are offered.
    for stmt in &module.statements {
        if let Stmt::Import { span, .. } = stmt {
            if (span.start as usize) <= offset && offset <= (span.end as usize) {
                return CompletionContext::ImportStmt;
            }
        }
    }

    // 2. Inside a parsed type expression ⇒ TypeAnnotation (covers `val x: Int▮`, union/array/generic
    //    arg positions where a `TypeExpr` node already exists). Reuses the same type-position span
    //    collector the semantic-tokens highlighter uses, so it stays in sync with the AST shape.
    let mut type_spans = Vec::new();
    collect_type_spans(&module.statements, &mut type_spans);
    for span in type_spans {
        if (span.start as usize) <= offset && offset <= (span.end as usize) {
            return CompletionContext::TypeAnnotation;
        }
    }

    // 3. Backward char scan to disambiguate the cases the AST can't yet see.
    // Binding-NAME position (`val ▮` / `var ▮`, no `:`/`=` yet) FIRST: the user is typing a fresh
    // name, so nothing is offered. This must precede the statement-start/expression fallbacks (a
    // bare `val` line trims non-empty, so `backward_scan_is_statement_start` wouldn't catch it and
    // it would otherwise fall through to `Expression` and dump in-scope bindings).
    if backward_scan_is_binding_name_position(source, offset) {
        return CompletionContext::BindingName;
    }
    if backward_scan_is_type_position(source, offset) {
        return CompletionContext::TypeAnnotation;
    }
    if backward_scan_is_import_clause(source, offset) {
        return CompletionContext::ImportStmt;
    }
    if backward_scan_is_statement_start(source, offset) {
        return CompletionContext::StatementStart;
    }

    CompletionContext::Expression
}

/// Backward char scan: is the cursor in a TYPE-annotation position? True when, scanning back over
/// the current line (skipping whitespace), we hit a `:` that governs a binding/param/return type
/// (e.g. `val x: ▮`, `(x: ▮`, `): ▮`) or a `type Name = ▮` RHS, BEFORE hitting a token that would
/// end such a context (a newline that starts a fresh statement, `;`, `(` for an arg, etc.). This
/// fires for the "colon typed, no type text yet" case where no `TypeExpr` node exists. It is
/// intentionally conservative: a `:` inside a JSON object LITERAL (`{ k: ▮ }`) is NOT a type
/// position, so an unbalanced `{` before the `:` on the line disqualifies it.
fn backward_scan_is_type_position(source: &str, offset: usize) -> bool {
    let byte_off = char_offset_to_byte(source, offset);
    let line_start = source[..byte_off].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = &source[line_start..byte_off];

    // Walk back over the partial type identifier the user may be typing, then whitespace.
    let trimmed = line.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
    let trimmed = trimmed.trim_end();
    let last = match trimmed.chars().last() {
        Some(c) => c,
        None => return false,
    };

    // After `:` — but exclude object-literal `key:` and ternary/`?:`-style uses by requiring the
    // line to look like a binding/param/return. We check for an unbalanced `{` (object literal).
    if last == ':' {
        // `::` isn't a Lin token, but guard anyway.
        let before_colon = &trimmed[..trimmed.len() - 1];
        // A `{` opened on this line and not closed before the `:` ⇒ object literal field, not a type.
        let opens = before_colon.matches('{').count();
        let closes = before_colon.matches('}').count();
        if opens > closes {
            return false;
        }
        return true;
    }

    // `type Name = ▮` — the RHS of a type declaration is a type position. Match a line that begins
    // (after optional `export`) with `type` and has an `=` before the cursor.
    let line_trim = line.trim_start();
    let body = line_trim.strip_prefix("export ").unwrap_or(line_trim).trim_start();
    if body.starts_with("type ") && body.contains('=') {
        return true;
    }

    false
}

/// Backward scan: is the cursor inside an `import` clause (the keyword-bearing part, not the path
/// string)? True when the current line — after optional `export` — begins with `import`. Used so
/// `from`/`as` are offered only here.
fn backward_scan_is_import_clause(source: &str, offset: usize) -> bool {
    let byte_off = char_offset_to_byte(source, offset);
    let line_start = source[..byte_off].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = &source[line_start..byte_off];
    let body = line.trim_start();
    body.starts_with("import ") || body == "import" || body.starts_with("import")
}

/// Backward scan: is the cursor at the START of a statement? True when only whitespace precedes the
/// cursor on its line (the user is at the first token of a new line) AND the line isn't a
/// CONTINUATION of the previous line's expression. Declaration keywords (`val`/`var`/`type`/`import`/
/// `export`) are offered here. A previous line ending in a continuation token (`=`, `=>`, a binary
/// operator, an open delimiter, `,`, `|`, `&`, etc.) means the cursor continues that expression, so
/// we do NOT treat it as a statement start (avoids offering `import` inside a wrapped `val` RHS).
fn backward_scan_is_statement_start(source: &str, offset: usize) -> bool {
    let byte_off = char_offset_to_byte(source, offset);
    let line_start = source[..byte_off].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = &source[line_start..byte_off];
    // The text before the cursor on this line, minus any partial identifier being typed.
    let before_word = line.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_');
    // Statement start requires nothing but indentation before the typed word.
    if !before_word.trim().is_empty() {
        return false;
    }
    // Look at the nearest preceding non-empty line. If it ends in a continuation token, the cursor
    // is a wrapped expression, not a fresh statement.
    let mut prev = &source[..line_start];
    loop {
        let pstart = prev.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let pline = prev[pstart..].trim_end();
        if pline.trim().is_empty() {
            if pstart == 0 {
                break;
            }
            prev = &source[..pstart - 1];
            continue;
        }
        // Strip a trailing `//` comment so the real last token is examined.
        let code = pline.split("//").next().unwrap_or(pline).trim_end();
        let cont = code.ends_with('=')
            || code.ends_with("=>")
            || code.ends_with('(')
            || code.ends_with('[')
            || code.ends_with('{')
            || code.ends_with(',')
            || code.ends_with('|')
            || code.ends_with('&')
            || code.ends_with('+')
            || code.ends_with('-')
            || code.ends_with('*')
            || code.ends_with('/')
            || code.ends_with('<')
            || code.ends_with('>')
            || code.ends_with("&&")
            || code.ends_with("||");
        if cont {
            return false;
        }
        break;
    }
    true
}

/// Backward scan: is the cursor in the binding-NAME position of a `val`/`var` declaration — i.e.
/// the user has typed `val `/`var ` (plus optional indentation) and is now TYPING THE NAME, before
/// any `:` (type annotation) or `=` (RHS)? In that position there is nothing to complete (the name
/// is brand new), so the classifier returns `BindingName` and the handler offers an empty list.
///
/// True when the text from the current line's start up to the cursor matches
/// `^\s*(val|var)\s+[A-Za-z0-9_]*$` — only the keyword + whitespace + the partial name. The trailing
/// `\s+` requires at least one space after the keyword, so a still-being-typed keyword like `va▮`
/// (which has no following space) is NOT caught (that stays a StatementStart). Once a `:` or `=`
/// appears the regex no longer matches, so `val x: ▮` (TypeAnnotation) and `val x = ▮` (Expression)
/// are unaffected. `offset` is a CHAR offset (the canonical Lin span space), converted to a byte
/// index for the line slice exactly like the sibling backward scans.
fn backward_scan_is_binding_name_position(source: &str, offset: usize) -> bool {
    let byte_off = char_offset_to_byte(source, offset);
    let line_start = source[..byte_off].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = &source[line_start..byte_off];
    // Strip leading indentation; the keyword must be the first token on the line.
    let body = line.trim_start();
    // Match `val`/`var` followed by at least one space, then only name chars to the cursor.
    let rest = match body.strip_prefix("val ").or_else(|| body.strip_prefix("var ")) {
        Some(r) => r,
        None => return false,
    };
    // After the keyword+space: only further whitespace + an optional partial identifier, nothing
    // else (no `:`, no `=`, no `(` etc. — any of those means we're past the name position).
    let name = rest.trim_start();
    name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

// ── in-scope binding collection (scope-aware completion) ──────────────────────

/// One binding visible at the cursor, collected structurally from the AST so that even a
/// defined-but-never-referenced name (e.g. a lambda param the user hasn't used yet) is offered.
#[derive(Debug, Clone)]
struct ScopeBinding {
    name: String,
    /// The binder's defining identifier span, used to look its inferred type up in `span_type_map`
    /// (which keys types by use-site but records the `def_span`) and to dedupe against it.
    def_span: Option<lin_common::Span>,
    /// A type annotation rendered from the AST when present (params/`val x: T`), used as a fallback
    /// detail when the binding was never referenced (so `span_type_map` has no inferred type).
    annotated_ty: Option<String>,
    is_function: bool,
}

/// Collect every binding in lexical scope at `offset`, walking the module and descending ONLY into
/// the scopes that enclose the cursor. Surfaces: top-level + block `val`/`var` names whose scope
/// reaches the cursor, parameters of any lambda/function whose body contains the cursor (this is
/// what surfaces an unreferenced `item`), and destructuring-bound names. Respects lexical scope —
/// a binding in a sibling scope that doesn't enclose the cursor is NOT collected.
fn collect_scope_bindings(
    module: &lin_parse::ast::Module,
    offset: usize,
) -> Vec<ScopeBinding> {
    let mut out = Vec::new();
    let off = offset as u32;
    // Top-level statements form the outermost scope: every top-level binding is in scope for the
    // whole module (Lin top-level `val`/fns are mutually visible — ADR-012 forward-decl), so we
    // don't gate them on textual order.
    for stmt in &module.statements {
        collect_bindings_from_stmt(stmt, off, &mut out);
    }
    out
}

/// Collect binders introduced directly by a statement (the binding it declares), then descend into
/// its initialiser expression looking for enclosing-lambda params / nested blocks.
fn collect_bindings_from_stmt(stmt: &Stmt, off: u32, out: &mut Vec<ScopeBinding>) {
    match stmt {
        Stmt::Val { pattern, type_ann, value, .. } => {
            let is_fn = matches!(value, lin_parse::ast::Expr::Function { .. });
            collect_binders_from_pattern(pattern, type_ann.as_ref(), is_fn, out);
            collect_bindings_from_expr(value, off, out);
        }
        Stmt::Var { name, name_span, type_ann, value, .. } => {
            out.push(ScopeBinding {
                name: name.clone(),
                def_span: Some(*name_span),
                annotated_ty: type_ann.as_ref().map(render_type_expr),
                is_function: matches!(value, lin_parse::ast::Expr::Function { .. }),
            });
            collect_bindings_from_expr(value, off, out);
        }
        Stmt::Replace { value, .. } => collect_bindings_from_expr(value, off, out),
        Stmt::Expr(e) => collect_bindings_from_expr(e, off, out),
        _ => {}
    }
}

/// Add the names a binding `pattern` introduces (plain ident or destructuring) to `out`. `type_ann`
/// is the optional annotation on the binding; `is_function` flags a function-valued binding so it
/// renders with a function icon (params are always `false`).
fn collect_binders_from_pattern(
    pattern: &lin_parse::ast::Pattern,
    type_ann: Option<&TypeExpr>,
    is_function: bool,
    out: &mut Vec<ScopeBinding>,
) {
    use lin_parse::ast::Pattern as P;
    match pattern {
        P::Ident(name, span) => out.push(ScopeBinding {
            name: name.clone(),
            def_span: Some(*span),
            annotated_ty: type_ann.map(render_type_expr),
            is_function,
        }),
        P::Object(fields, rest, _) => {
            for f in fields {
                collect_binders_from_subpattern(&f.pattern, out);
            }
            if let Some(r) = rest {
                out.push(ScopeBinding { name: r.clone(), def_span: None, annotated_ty: None, is_function: false });
            }
        }
        P::Array(elems, rest, _) => {
            for e in elems {
                collect_binders_from_subpattern(e, out);
            }
            if let Some(r) = rest {
                out.push(ScopeBinding { name: r.clone(), def_span: None, annotated_ty: None, is_function: false });
            }
        }
        P::TypeName(..) | P::Literal(..) | P::Wildcard(..) => {}
    }
}

/// Recurse into a destructuring sub-pattern, collecting plain-ident binders (no type annotation
/// available at this depth).
fn collect_binders_from_subpattern(pattern: &lin_parse::ast::Pattern, out: &mut Vec<ScopeBinding>) {
    use lin_parse::ast::Pattern as P;
    match pattern {
        P::Ident(name, span) => out.push(ScopeBinding {
            name: name.clone(),
            def_span: Some(*span),
            annotated_ty: None,
            is_function: false,
        }),
        P::Object(fields, rest, _) => {
            for f in fields {
                collect_binders_from_subpattern(&f.pattern, out);
            }
            if let Some(r) = rest {
                out.push(ScopeBinding { name: r.clone(), def_span: None, annotated_ty: None, is_function: false });
            }
        }
        P::Array(elems, rest, _) => {
            for e in elems {
                collect_binders_from_subpattern(e, out);
            }
            if let Some(r) = rest {
                out.push(ScopeBinding { name: r.clone(), def_span: None, annotated_ty: None, is_function: false });
            }
        }
        P::TypeName(..) | P::Literal(..) | P::Wildcard(..) => {}
    }
}

/// Descend into an expression looking for enclosing scopes of `off`: a `Function` whose BODY
/// contains the cursor contributes its params (the `item` case), a `Block` whose extent contains
/// the cursor contributes its `val`/`var`s, and a `Match` arm whose body contains the cursor
/// contributes its pattern bindings. Only scopes that ENCLOSE the cursor are descended (lexical
/// scoping), so a sibling lambda's params never leak in.
fn collect_bindings_from_expr(expr: &lin_parse::ast::Expr, off: u32, out: &mut Vec<ScopeBinding>) {
    use lin_parse::ast::Expr as E;
    match expr {
        E::Function { params, body, full_span, .. } => {
            // Only contribute params + descend when the cursor is within this function's extent.
            if full_span.start <= off && off <= full_span.end {
                for p in params {
                    collect_binders_from_pattern(&p.pattern, p.type_ann.as_ref(), false, out);
                }
                collect_bindings_from_expr(body, off, out);
            }
        }
        E::Block(stmts, tail, _, full_span) => {
            if full_span.start <= off && off <= full_span.end {
                for s in stmts {
                    collect_bindings_from_stmt(s, off, out);
                }
                collect_bindings_from_expr(tail, off, out);
            }
        }
        E::Match { scrutinee, arms, full_span, .. } => {
            if full_span.start <= off && off <= full_span.end {
                collect_bindings_from_expr(scrutinee, off, out);
                for arm in arms {
                    let bspan = arm.body.full_span();
                    if bspan.start <= off && off <= bspan.end {
                        if let lin_parse::ast::MatchPattern::Is(p) | lin_parse::ast::MatchPattern::Has(p) = &arm.pattern {
                            collect_binders_from_subpattern(p, out);
                        }
                    }
                    collect_bindings_from_expr(&arm.body, off, out);
                }
            }
        }
        _ => walk_child_exprs(expr, &mut |child| collect_bindings_from_expr(child, off, out)),
    }
}

/// Render a `TypeExpr` to a short display string for completion `detail` when no inferred type is
/// available. Best-effort and read-only — covers the common shapes; falls back to a generic label.
fn render_type_expr(ty: &TypeExpr) -> String {
    use TypeExpr as T;
    match ty {
        T::Named(n, _) => n.clone(),
        T::Generic(n, args, _) => format!("{}<{}>", n, args.iter().map(render_type_expr).collect::<Vec<_>>().join(", ")),
        T::Array(inner, _) => format!("{}[]", render_type_expr(inner)),
        T::Union(parts, _) => parts.iter().map(render_type_expr).collect::<Vec<_>>().join(" | "),
        T::Intersection(parts, _) => parts.iter().map(render_type_expr).collect::<Vec<_>>().join(" & "),
        T::Function(params, ret, _) => format!(
            "({}) => {}",
            params.iter().map(render_type_expr).collect::<Vec<_>>().join(", "),
            render_type_expr(ret)
        ),
        T::StringLit(s, _) => format!("\"{}\"", s),
        T::Object(..) | T::IndexSig(..) | T::FixedArray(..) | T::TaggedUnion(..) => "{ … }".to_string(),
    }
}

// ── user-defined type names (for TypeAnnotation completion) ───────────────────

/// Collect the names of `type` declarations and generic type params in scope at `offset`, offered
/// alongside the built-in types in a `TypeAnnotation` position. Top-level `type` decls are module-
/// global; generic `<T>` params are added when the cursor is inside the declaring function.
fn collect_type_names(module: &lin_parse::ast::Module, offset: usize) -> Vec<String> {
    let mut out = Vec::new();
    let off = offset as u32;
    for stmt in &module.statements {
        match stmt {
            Stmt::TypeDecl { name, params, .. } => {
                out.push(name.clone());
                out.extend(params.iter().cloned());
            }
            Stmt::Val { value, .. } | Stmt::Var { value, .. } => {
                collect_type_params_from_expr(value, off, &mut out);
            }
            Stmt::Expr(e) => collect_type_params_from_expr(e, off, &mut out),
            _ => {}
        }
    }
    out
}

/// Add a function's `<T, …>` generic params when the cursor is within its extent (so they're
/// offered in its param/return annotations + body type positions).
fn collect_type_params_from_expr(expr: &lin_parse::ast::Expr, off: u32, out: &mut Vec<String>) {
    use lin_parse::ast::Expr as E;
    if let E::Function { type_params, full_span, .. } = expr {
        if full_span.start <= off && off <= full_span.end {
            out.extend(type_params.iter().cloned());
        }
    }
    walk_child_exprs(expr, &mut |child| collect_type_params_from_expr(child, off, out));
}

// ── unimported stdlib dot-completion candidates (FIX B) ───────────────────────

/// One stdlib export offered in dot-completion even when NOT yet imported. The completion handler
/// turns it into an item whose accept inserts the bare method NAME plus an `import { name } from
/// "module"` edit (computed by the shared `auto_import_edit`).
#[derive(Clone)]
struct StdlibCandidate {
    /// Bare export name (the completion label, e.g. `map`).
    name: String,
    /// Owner stdlib module id (e.g. `std/iter`) — used for the import edit and doc resolution.
    module: String,
    /// Rendered (raw) type signature; cleaned for `detail` at item-build time via `clean_type_string`.
    ty: String,
}

/// The stdlib modules whose exports we offer as unimported dot-completion candidates. SCOPE
/// (deliberately narrow so a dot doesn't dump the whole stdlib): the combinator/method-bearing
/// modules a user reaches for via `xs.method(...)` — receiver-polymorphic iterable combinators
/// (`std/iter`: map/filter/reduce/for/range…), array ops (`std/array`: push/length/slice/sort…),
/// string ops (`std/string`), and object ops (`std/object`: keys). Each candidate is still gated by
/// `dot_item_applies` against the receiver category at offer time, so only relevant ones appear.
const DOT_CANDIDATE_MODULES: &[&str] =
    &["std/iter", "std/array", "std/string", "std/object"];

/// All offered stdlib dot-completion candidates, computed ONCE from the embedded stdlib sources
/// (which are static, so this never goes stale) and memoised. Type-checks each candidate module via
/// the same `pre_resolve_imports` + `extract_exports` path the import-type map uses, so the rendered
/// signatures match what completion/hover show for the already-imported case.
static STDLIB_DOT_CANDIDATES: std::sync::LazyLock<Vec<StdlibCandidate>> =
    std::sync::LazyLock::new(build_stdlib_dot_candidates);

fn build_stdlib_dot_candidates() -> Vec<StdlibCandidate> {
    let mut out = Vec::new();
    for &module in DOT_CANDIDATE_MODULES {
        for (name, ty) in stdlib_module_exports(module) {
            // Defensive secondary filter: never offer a `_`-prefixed name. `stdlib_module_exports`
            // already drops non-`export`ed helpers, but `_` is the stdlib's private-helper naming
            // convention — even an accidentally-exported `_foo` is conventionally internal.
            if name.starts_with('_') {
                continue;
            }
            out.push(StdlibCandidate { name, module: module.to_string(), ty: ty.to_string() });
        }
    }
    out
}

/// Build the unimported-stdlib dot-completion items (FIX B) for the given `receiver_cat` from a set
/// of `candidates`. Pure over its inputs (no I/O), so the offer/gate/dedupe logic is unit-testable.
///
/// A candidate is offered when ALL hold:
///   - its name starts with the typed `prefix`;
///   - its name is NOT already offered (`already`, which the caller seeds with imported symbols +
///     local bindings) — so an already-imported `map` isn't listed twice;
///   - it is function-shaped (`=>` in the signature) — a bare value isn't a dot-method;
///   - `dot_item_applies(receiver_cat, first_param_category)` — gating by receiver category so a
///     `.` doesn't dump the whole stdlib (e.g. a `string`-first-param method is dropped on an array).
///
/// The label is the bare NAME (plain identifier insert, no arg snippet). The `detail` uses the FIX A
/// clean renderer (no raw `?T` ids). The import edit + doc are attached lazily in `completion_resolve`
/// from the `{ module, name }` stash, so building the list stays cheap.
fn stdlib_dot_completion_items<'a>(
    candidates: impl Iterator<Item = &'a StdlibCandidate>,
    receiver_cat: &str,
    prefix: &str,
    already: &HashSet<String>,
    uri: &Url,
) -> Vec<CompletionItem> {
    let mut out = Vec::new();
    for cand in candidates {
        if !cand.name.starts_with(prefix) || already.contains(&cand.name) {
            continue;
        }
        if !cand.ty.contains("=>") {
            continue;
        }
        let first_param_cat = first_param_category(&cand.ty);
        if !dot_item_applies(receiver_cat, first_param_cat.as_deref()) {
            continue;
        }
        out.push(CompletionItem {
            label: cand.name.clone(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some(format!("{}  (from {})", clean_type_string(&cand.ty), cand.module)),
            documentation: Some(Documentation::String(format!("from {}", cand.module))),
            data: completion_resolve_data_stdlib(uri, &cand.name, &cand.module),
            ..Default::default()
        });
    }
    out
}

/// Type-check a single stdlib module (resolving its own imports first) and return its genuinely
/// `export`ed `val`s as `(name, Type)`. The export flag is read from the PARSED AST (the typed
/// module drops it), so private helpers like `_bisect` — declared `val`, not `export val` — are NOT
/// returned even though they're top-level `val`s; their types still come from the typed module.
/// Returns empty on a parse/check failure (degrades gracefully — the dot-completion just won't offer
/// that module's symbols).
fn stdlib_module_exports(module_id: &str) -> Vec<(String, Type)> {
    let Some(src) = stdlib_source(module_id) else {
        return Vec::new();
    };
    let mut lexer = lin_lex::Lexer::new(src, 0);
    let tokens = lexer.tokenize();
    let mut parser = lin_parse::Parser::new(tokens);
    let ast = parser.parse_module();

    // Resolve the module's own imports (e.g. std/iter -> intrinsics) so it type-checks. stdlib ids
    // are absolute, so the base dir is unused for resolution.
    let base = PathBuf::from(".");
    let mut cache: HashMap<String, TypedModule> = HashMap::new();
    let mut visiting: HashSet<String> = HashSet::new();
    pre_resolve_imports(&ast, &base, &mut cache, &mut visiting);

    let mut import_type_map: HashMap<(String, String), Type> = HashMap::new();
    for (dep_path, dep_module) in cache.iter() {
        for (name, ty) in extract_exports(dep_module) {
            import_type_map.insert((dep_path.clone(), name), ty);
        }
    }

    // The genuinely-`export`ed names, read from the AST (the typed module drops the flag — see
    // `ast_exported_names`). We INTERSECT the typed name→type pairs with this set so only real
    // exports become dot-completion candidates; private helpers like `_bisect` (declared `val`, not
    // `export val`) are filtered out while their TYPES still come from the typed module.
    let exported = ast_exported_names(&ast);

    let mut checker = Checker::new();
    checker.import_types = import_type_map;
    // Trusted stdlib: it legitimately references `lin_*` intrinsics and forwards Json (ADR-060).
    checker.lenient_json = true;
    checker.allow_intrinsics = true;
    match checker.check_module(&ast) {
        Ok(typed) => extract_exports(&typed)
            .into_iter()
            .filter(|(name, _)| exported.contains(name))
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// True when `offset` sits inside a string literal or a `//` line comment, so identifier/keyword
/// completion should be suppressed there. Line-scoped + lightweight (no full lex): we scan the
/// current line from its start up to the cursor, tracking string state (honouring `\"` escapes) and
/// the `//` comment marker.
///
/// Lin string literals are single-line, so a line-local scan is sufficient and robust against a
/// partial/unparseable buffer mid-edit. Import strings are handled separately (their own
/// path-completion runs BEFORE this check), so a string detected here is always an ordinary string.
/// `${...}` interpolation: the cursor inside a `${ ... }` IS code position, so we treat an open `${`
/// as leaving the string (completion stays enabled inside the interpolation hole).
fn in_string_or_comment(source: &str, offset: usize) -> bool {
    // `offset` is a char offset; convert to a byte index for the `&str` slices below.
    let offset = char_offset_to_byte(source, offset);
    let line_start = source[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = &source[line_start..offset];

    let mut in_str = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            match c {
                // `\X` escapes the next char (e.g. `\"` does not close the string).
                '\\' => {
                    chars.next();
                }
                // `${` opens an interpolation hole — code position resumes until the matching `}`.
                '$' if chars.peek() == Some(&'{') => {
                    chars.next(); // consume `{`
                    // Skip to the closing `}` (interpolations don't nest deeply in practice); if it
                    // isn't closed before the cursor, the cursor is inside the code hole → not a string.
                    let mut closed = false;
                    for ic in chars.by_ref() {
                        if ic == '}' {
                            closed = true;
                            break;
                        }
                    }
                    if !closed {
                        return false; // cursor is inside the `${ … }` code hole.
                    }
                    // Otherwise we consumed the hole and are back in the string body.
                }
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            // `//` begins a line comment — everything to the cursor (and EOL) is comment.
            '/' if chars.peek() == Some(&'/') => return true,
            _ => {}
        }
    }
    in_str
}

/// Detect whether `offset` sits inside the quoted path of an `import` statement and, if
/// so, return the path text already typed up to the cursor. Conservative + line-scoped:
/// matches `... from "<typed>` and `import foreign "<typed>` on the cursor's line, with
/// the cursor positioned after the opening quote and before any closing quote.
///
/// LIMITATION: detection is single-line and textual (no multi-line import strings, which
/// the grammar doesn't produce anyway). Returns `None` outside an import-string context.
fn import_string_prefix(source: &str, offset: usize) -> Option<String> {
    // `offset` is a char offset; convert to a byte index for the `&str` slices below.
    let offset = char_offset_to_byte(source, offset);
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
    // `offset` is a char offset; the word characters are ASCII (`[A-Za-z0-9_]`), so we scan bytes
    // from the byte index of `offset` leftwards. Identifier bytes are all single-byte, so a byte
    // walk back over them lands on a char boundary.
    let byte_off = char_offset_to_byte(source, offset);
    let bytes = source.as_bytes();
    let start = (0..byte_off)
        .rev()
        .take_while(|&i| {
            let b = bytes[i];
            b.is_ascii_alphanumeric() || b == b'_'
        })
        .last()
        .unwrap_or(byte_off);
    &source[start..byte_off]
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
                // Insert `, name` right after the last existing binding in this import's brace
                // list. The stmt span covers only the `import` keyword, so scan to the line end.
                // `span.start` is a CHAR offset; convert to a byte index to slice the source, then
                // lift the insertion byte position back to a CHAR offset for `offset_to_position`.
                let start = char_offset_to_byte(source, span.start as usize);
                let line_end = source[start..].find('\n').map(|i| start + i).unwrap_or(source.len());
                let stmt_src = source.get(start..line_end)?;
                let brace = stmt_src.find('}')?;
                // Insert before the whitespace run that precedes `}`, not at the `}` itself, so the
                // formatter's trailing space before `}` is preserved (`{ a, b }` stays
                // `{ a, b, name }`, not `{ a, b , name}`).
                let after_last_binding = stmt_src[..brace].trim_end().len();
                let insert_at = start + after_last_binding; // byte offset just past last binding
                let pos = offset_to_position(source, byte_offset_to_char(source, insert_at));
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
        // `offset_to_position` takes a CHAR offset → end-of-document is the char count.
        let eof = offset_to_position(source, source.chars().count());
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
    // Scan by `char` tracking CHAR indices so the offsets fed to `offset_to_position` (which now
    // expects char offsets) are correct even when the source contains multibyte characters.
    let mut stack: Vec<usize> = Vec::new();
    let mut out = Vec::new();
    let mut in_str = false;
    let mut chars = source.chars().enumerate().peekable();
    while let Some((i, c)) = chars.next() {
        if in_str {
            if c == '\\' {
                chars.next(); // skip the escaped char
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' | '[' | '(' => stack.push(i),
            '}' | ']' | ')' => {
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
    // All ranges here are CHAR offsets (to match the AST `full_span()` ranges and feed
    // `offset_to_position`). `offset` is a char offset; clamp it to the total char count.
    let total_chars = source.chars().count();
    let offset = offset.min(total_chars);
    // Ordered innermost → outermost list of (start, end) CHAR ranges.
    let mut ranges: Vec<(usize, usize)> = Vec::new();

    // 1. Word under the cursor. Expand over BYTES (word chars are ASCII) from the cursor's byte
    //    index, then lift the byte range back to char offsets to stay in char space.
    let word = word_at(source, offset);
    if !word.is_empty() {
        let bytes = source.as_bytes();
        let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let byte_off = char_offset_to_byte(source, offset);
        let mut start = byte_off;
        let mut end = byte_off;
        while start > 0 && is_word(bytes[start - 1]) {
            start -= 1;
        }
        while end < bytes.len() && is_word(bytes[end]) {
            end += 1;
        }
        ranges.push((byte_offset_to_char(source, start), byte_offset_to_char(source, end)));
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

    // 3. The cursor's line. Compute the byte bounds, then lift to char offsets.
    let byte_off = char_offset_to_byte(source, offset);
    let line_start_byte = source[..byte_off].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end_byte = source[byte_off..].find('\n').map(|i| byte_off + i).unwrap_or(source.len());
    ranges.push((
        byte_offset_to_char(source, line_start_byte),
        byte_offset_to_char(source, line_end_byte),
    ));

    // 4. Whole document (char-offset extent).
    ranges.push((0, total_chars));

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
    // `offset` is a CHAR offset; we scan by `char` and track CHAR indices so the returned pairs
    // and the cursor comparison stay in char space. ASCII brackets, but the string body between
    // them may contain multibyte chars, so a byte scan would misplace the indices.
    let mut stack: Vec<usize> = Vec::new();
    let mut enclosing: Vec<(usize, usize)> = Vec::new();
    let mut in_str = false;
    let mut chars = source.chars().enumerate().peekable();
    while let Some((i, c)) = chars.next() {
        if in_str {
            if c == '\\' {
                chars.next(); // skip the escaped char
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' | '[' | '(' => stack.push(i),
            '}' | ']' | ')' => {
                if let Some(open) = stack.pop() {
                    // This pair encloses the cursor when open < offset <= close.
                    if open < offset && offset <= i {
                        enclosing.push((open, i));
                    }
                }
            }
            _ => {}
        }
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
    // `stmt_span.start` is a CHAR offset; convert to a byte index to slice/search the source, then
    // lift the located byte positions back to CHAR offsets for `offset_to_position`.
    let start = char_offset_to_byte(source, stmt_span.start as usize);
    // Scan to the end of the statement's line (import statements are single-line).
    let line_end = source[start..].find('\n').map(|i| start + i).unwrap_or(source.len());
    let hay = source.get(start..line_end)?;
    let needle = format!("\"{}\"", path);
    let rel = hay.find(&needle)?;
    let abs = start + rel + 1; // byte offset, +1 to skip the opening quote
    let abs_char = byte_offset_to_char(source, abs);
    Some(Range {
        start: offset_to_position(source, abs_char),
        // `path` is the byte length of the path text; advance by its char count to reach the
        // closing quote. (Path strings can contain multibyte chars.)
        end: offset_to_position(source, abs_char + path.chars().count()),
    })
}

// ── doc comments (JSDoc-like `//` blocks) ──────────────────────────────────────
//
// The stdlib (and user code) documents declarations with a contiguous block of own-line `//`
// comments directly above an `export val`/`export type` (or any `val`/`var`/`type`). Tags follow a
// JSDoc-like convention:
//   - bare prose lines           → free-form description (joined into a paragraph);
//   - `@param <name>  <desc>`    → one parameter entry (ordered);
//   - `@returns <desc>`          → the return description;
//   - `@example <code>`          → an example snippet (rendered in a ```lin fence);
//   - `// ── Title ──` banners    → decorative section headers; NOT part of a decl's doc, so they
//                                    terminate a leading block (a banner is never a doc line).
//
// This is the SINGLE extract + render path used by hover, completion, and signature help. The raw
// extraction (`extract_doc`) mirrors the formatter's leading-comment attachment rule (own-line
// comments immediately preceding the decl, no blank-line gap) and gen-stdlib's "stop at the first
// blank/code/banner line" behaviour; the parse + render (`DocComment` / `render_doc_markdown`)
// mirror gen-stdlib's `renderDocLine` markdown conventions so editor hovers match the docs site.

/// A parsed JSDoc-like doc comment, separated into its constituent pieces. Any piece may be empty:
/// a block with no `@`-tags is just `description`; a block with no prose has an empty `description`.
#[derive(Debug, Default, Clone, PartialEq)]
struct DocComment {
    /// Free-form leading prose lines (in order), each already stripped of its `//` prefix.
    description: Vec<String>,
    /// `@param <name> <desc>` entries, in source order.
    params: Vec<(String, String)>,
    /// `@returns <desc>` (the LAST one wins if repeated; rare).
    returns: Option<String>,
    /// `@example <code>` snippets, in source order.
    examples: Vec<String>,
}

impl DocComment {
    /// True when nothing was captured — used by callers to fall back to non-doc behaviour.
    fn is_empty(&self) -> bool {
        self.description.iter().all(|l| l.trim().is_empty())
            && self.params.is_empty()
            && self.returns.is_none()
            && self.examples.is_empty()
    }

    /// The `@param` description for `name`, if any (used by signature help).
    fn param_doc(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(n, _)| n == name).map(|(_, d)| d.as_str())
    }

    /// The description joined into a single paragraph string (blank lines separate paragraphs).
    fn description_text(&self) -> String {
        let mut paras: Vec<String> = Vec::new();
        let mut cur: Vec<&str> = Vec::new();
        for line in &self.description {
            if line.trim().is_empty() {
                if !cur.is_empty() {
                    paras.push(cur.join(" "));
                    cur.clear();
                }
            } else {
                cur.push(line.trim());
            }
        }
        if !cur.is_empty() {
            paras.push(cur.join(" "));
        }
        paras.join("\n\n")
    }
}

/// Strip the leading `//` (and at most one following space) from a captured comment's `text`.
/// `lin_lex::Comment.text` retains the `//` prefix (right-trimmed), so e.g. `"// foo"` → `"foo"`,
/// `"//foo"` → `"foo"`, `"//"` → `""`.
fn strip_comment_prefix(text: &str) -> &str {
    let t = text.trim_start();
    let rest = t.strip_prefix("//").unwrap_or(t);
    rest.strip_prefix(' ').unwrap_or(rest)
}

/// True when a (already-`//`-stripped) doc line is a decorative section banner, e.g.
/// `── Arithmetic ──` or `--- shared tables ---`. Mirrors gen-stdlib's `isBanner`. Banners are not
/// a declaration's documentation, so they terminate a leading block.
fn is_doc_banner(text: &str) -> bool {
    let t = text.trim();
    t.contains('─') || t.starts_with("---") || t.starts_with("==")
}

/// Extract the contiguous leading block of own-line `//` comments immediately preceding the
/// declaration whose NAME span is `decl_name_span`, lexing `source` for comments. The block is the
/// run of own-line comment lines that end on the line just above the decl (no blank-line gap, no
/// intervening code) — the formatter's leading-comment rule. A decorative `── … ──` banner line
/// terminates the block from above (it documents a section, not this decl). Returns `None` when no
/// leading comment block exists.
fn extract_doc(source: &str, decl_name_span: lin_common::Span) -> Option<DocComment> {
    let mut lexer = lin_lex::Lexer::new(source, 0);
    let _ = lexer.tokenize();
    let comments: Vec<lin_lex::Comment> =
        lexer.comments().iter().filter(|c| c.own_line).cloned().collect();
    if comments.is_empty() {
        return None;
    }

    // IMPORTANT: lexer/parser `Span` offsets are CHAR offsets into the source (the lexer scans a
    // `Vec<char>`), NOT byte offsets — so we must count lines in char-offset space here, not via the
    // byte-based `offset_to_position`. Comment spans and the decl span are both char offsets, so they
    // stay consistent with one another. (stdlib files carry multibyte chars in their banners/prose,
    // where a byte/char mix-up would mis-locate the block.)
    let line_of_char_offset = |char_off: usize| -> u32 {
        source.chars().take(char_off).filter(|&c| c == '\n').count() as u32
    };

    // The decl's source line (0-based). The leading block must end on the line directly above it.
    let decl_line = line_of_char_offset(decl_name_span.start as usize);

    // Pair each own-line comment with its source line, in source order.
    let mut commented_lines: Vec<(u32, &str)> = comments
        .iter()
        .map(|c| {
            let line = line_of_char_offset(c.span.start as usize);
            (line, c.text.as_str())
        })
        .collect();
    commented_lines.sort_by_key(|(l, _)| *l);

    // Walk UP from `decl_line - 1`: collect comment lines that are contiguous (each exactly one line
    // above the previous), stopping at the first gap (blank line or code). A banner line also stops
    // the block (and is not included).
    let mut block: Vec<&str> = Vec::new();
    let mut expected = decl_line.checked_sub(1)?;
    loop {
        // The comment that sits exactly on `expected`, if any.
        let Some((_, text)) = commented_lines.iter().rev().find(|(l, _)| *l == expected) else {
            break; // gap (blank/code): the contiguous run ends here.
        };
        let stripped = strip_comment_prefix(text);
        if is_doc_banner(stripped) {
            break; // a section banner above is not this decl's documentation.
        }
        block.push(text);
        if expected == 0 {
            break;
        }
        expected -= 1;
    }
    if block.is_empty() {
        return None;
    }
    // We collected bottom-up; reverse to source order.
    block.reverse();
    let stripped: Vec<String> = block.iter().map(|t| strip_comment_prefix(t).to_string()).collect();
    Some(parse_doc_block(&stripped))
}

/// Parse a block of (already-`//`-stripped) doc lines into a `DocComment`. `@param`/`@returns`/
/// `@example` tags are recognised at the START of a line; everything else is description prose.
/// Robust to missing pieces and to a compact mid-line `@returns` on a prose/param line (mirrors
/// gen-stdlib's split behaviour).
fn parse_doc_block(lines: &[String]) -> DocComment {
    let mut doc = DocComment::default();
    for raw in lines {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("@param") {
            let rest = rest.trim_start();
            // Name runs up to the first whitespace; the remainder is the description.
            let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let name = rest[..name_end].trim().to_string();
            let mut desc = rest[name_end..].trim().to_string();
            // Compact style: a `@param` line may carry a trailing `@returns ...` — split it out.
            if let Some(cut) = desc.find("@returns") {
                let after = desc[cut..].trim_start_matches("@returns").trim().to_string();
                desc = desc[..cut].trim().to_string();
                if !after.is_empty() {
                    doc.returns = Some(after);
                }
            }
            if !name.is_empty() {
                doc.params.push((name, desc));
            }
        } else if let Some(rest) = trimmed.strip_prefix("@returns") {
            doc.returns = Some(rest.trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("@example") {
            doc.examples.push(rest.trim().to_string());
        } else if let Some(cut) = trimmed.find("@returns ") {
            // A compact one-liner like "Sum of a and b. @returns a + b." — split prose from tag.
            let prose = trimmed[..cut].trim();
            let ret = trimmed[cut..].trim_start_matches("@returns").trim();
            if !prose.is_empty() {
                doc.description.push(prose.to_string());
            }
            if !ret.is_empty() {
                doc.returns = Some(ret.to_string());
            }
        } else {
            doc.description.push(line.to_string());
        }
    }
    doc
}

/// Render a `DocComment` to LSP Markdown, matching the docs-site (`gen-stdlib`) conventions:
///   - description prose as paragraphs;
///   - a **Parameters** bullet list (`` - `name` — desc ``);
///   - a **Returns** line;
///   - each `@example` as a fenced ```lin code block.
/// Sections are separated by blank lines; absent sections are omitted. Returns an empty string when
/// the doc is empty (callers treat that as "no doc").
fn render_doc_markdown(doc: &DocComment) -> String {
    if doc.is_empty() {
        return String::new();
    }
    let mut sections: Vec<String> = Vec::new();

    let desc = doc.description_text();
    if !desc.trim().is_empty() {
        sections.push(desc);
    }

    if !doc.params.is_empty() {
        let mut block = String::from("**Parameters**\n");
        for (name, d) in &doc.params {
            if d.trim().is_empty() {
                block.push_str(&format!("- `{}`\n", name));
            } else {
                block.push_str(&format!("- `{}` — {}\n", name, d.trim()));
            }
        }
        sections.push(block.trim_end().to_string());
    }

    if let Some(ret) = &doc.returns {
        if !ret.trim().is_empty() {
            sections.push(format!("**Returns** {}", ret.trim()));
        }
    }

    for ex in &doc.examples {
        if !ex.trim().is_empty() {
            sections.push(format!("**Example**\n```lin\n{}\n```", ex.trim()));
        }
    }

    sections.join("\n\n")
}

/// Resolve the doc comment for the symbol named `word` as seen from the file `module_id`, using the
/// cross-file index: the symbol is resolved to its owner module + export name (the same path
/// goto-definition/references use), then the owner's source + declaration name-span yield the
/// leading doc block. Returns `None` for symbols with no resolvable owner/decl or no doc block.
///
/// Covers IMPORTED symbols (owner is another module, incl. stdlib) AND a file's OWN exports (owner
/// is this file). Local non-exported bindings are not in the index — those are handled separately by
/// the hover/completion handlers, which already hold the current file's source + decl span.
fn resolve_doc_via_index(index: &WorkspaceIndex, module_id: &str, word: &str) -> Option<DocComment> {
    let (owner, export) = index.resolve_symbol_by_name(module_id, word)?;
    let owner_file = index.files.get(&owner)?;
    let decl = decl_span(index, &owner, &export)?;
    extract_doc(&owner_file.source, decl)
}

/// The name-span of a top-level `val`/`var`/`type` declaration named `name` in this module's surface
/// AST (the local-symbol counterpart of `decl_span`, which only covers cross-file EXPORTS). Used to
/// extract the doc block of a callee that is a local, non-exported declaration in the current file.
fn local_decl_name_span(module: &lin_parse::ast::Module, name: &str) -> Option<lin_common::Span> {
    for stmt in &module.statements {
        match stmt {
            Stmt::Val { pattern, .. } => {
                if let Some((n, span)) = pattern_ident(pattern) {
                    if n == name {
                        return Some(span);
                    }
                }
            }
            Stmt::Var { name: vn, name_span, .. } if vn == name => return Some(*name_span),
            Stmt::TypeDecl { name: tn, span, .. } if tn == name => return Some(*span),
            _ => {}
        }
    }
    None
}

/// Resolve the doc comment to surface in hover for the symbol at `offset` in the file `uri`.
/// Prefers a LOCAL binding's leading doc block (the cursor's `def_span` points into THIS file's
/// source), then falls back to the cross-file index for an imported symbol or this file's own
/// export. Returns `None` when no doc block is found (the hover then shows just the type).
fn hover_doc(
    source: &str,
    uri: &Url,
    def_span: Option<lin_common::Span>,
    offset: usize,
) -> Option<DocComment> {
    // 1. Local binding: its def_span is the declaration's name span in this very file.
    if let Some(ds) = def_span {
        if let Some(doc) = extract_doc(source, ds) {
            if !doc.is_empty() {
                return Some(doc);
            }
        }
    }
    // 2. Imported symbol / own export: resolve via the cross-file index.
    let path = uri.to_file_path().ok()?;
    let module_id = canonical_id(&path);
    let word = word_at(source, offset);
    if word.is_empty() {
        return None;
    }
    let index = WORKSPACE_INDEX.read().unwrap_or_else(|e| e.into_inner());
    resolve_doc_via_index(&index, &module_id, word).filter(|d| !d.is_empty())
}

/// Build the `CompletionItem.data` payload that lets `completion_resolve` lazily fetch a doc for an
/// item: the originating file URI + the (local) symbol name. Kept as a tiny JSON object so it
/// round-trips losslessly through the client back to the resolve handler.
fn completion_resolve_data(uri: &Url, name: &str) -> Option<serde_json::Value> {
    Some(serde_json::json!({ "uri": uri.to_string(), "name": name }))
}

/// `data` payload for an UNIMPORTED stdlib dot-completion candidate (FIX B): the originating file
/// URI, the export name, and the owner module. `completion_resolve` uses `module` to (a) compute the
/// `additionalTextEdits` import edit via the shared `auto_import_edit` and (b) resolve the doc from
/// the owner module rather than this file.
fn completion_resolve_data_stdlib(uri: &Url, name: &str, module: &str) -> Option<serde_json::Value> {
    Some(serde_json::json!({ "uri": uri.to_string(), "name": name, "module": module }))
}

/// Parse the `{ uri, name }` key stashed by `completion_resolve_data` back into `(Url, name)`.
/// Returns `None` for an absent/malformed payload (the item then resolves to itself unchanged).
fn parse_completion_resolve_data(data: Option<&serde_json::Value>) -> Option<(Url, String)> {
    let data = data?;
    let uri = data.get("uri")?.as_str()?;
    let name = data.get("name")?.as_str()?;
    let uri = Url::parse(uri).ok()?;
    Some((uri, name.to_string()))
}

/// Read the owner `module` from a stdlib-candidate `data` payload (set by
/// `completion_resolve_data_stdlib`). `None` for non-candidate items (the ordinary local/imported
/// resolve path is taken instead).
fn parse_completion_resolve_module(data: Option<&serde_json::Value>) -> Option<String> {
    data?.get("module")?.as_str().map(|s| s.to_string())
}

// ── signature help ─────────────────────────────────────────────────────────────

/// Build signature help when `offset` sits inside a `f(…)` call's argument list. Returns `None`
/// when the cursor is not inside a resolvable call (e.g. dot-calls, or callees whose function type
/// can't be looked up) — per the no-guessing rule.
///
/// `resolve_doc` is invoked with the callee's bare name and returns its parsed `DocComment` (if the
/// callee is a documented import / own export); the handler supplies the index-backed resolver while
/// unit tests can pass a closure directly. The function's description becomes the signature
/// documentation and each `@param`'s description is attached to the matching parameter.
fn signature_help(
    source: &str,
    analysis: &Analysis,
    offset: usize,
    resolve_doc: impl Fn(&str) -> Option<DocComment>,
) -> Option<SignatureHelp> {
    // Find the innermost plain `Call` whose argument region (between `(` and the closing `)`)
    // contains the cursor, plus the byte offset just after its opening `(`.
    let (callee_span, paren_after) = find_enclosing_call(&analysis.module.statements, source, offset)?;

    // Resolve the callee's type via the type map (the callee is an identifier use-site).
    // Clean the WHOLE signature once up front so raw solver type-var ids (`?T9004`) become readable
    // generic names (`T`/`U`/…) consistently across both the params and the return tail below.
    let ty_str = tightest_span(&analysis.span_type_map, callee_span.start as usize)
        .map(|(_, s, _)| clean_type_string(s))?;
    // Only function-typed callees produce a signature.
    let param_types = function_param_types(&ty_str)?;

    // Active parameter = number of top-level commas between the opening paren and the cursor.
    // `offset` and `paren_after` are CHAR offsets; convert to byte indices to slice the source.
    // `.get` with a normalised (non-reversed, in-bounds) range so a stale paren/cursor offset can't
    // panic the slice.
    let arg_hi = char_offset_to_byte(source, offset);
    let arg_lo = char_offset_to_byte(source, paren_after).min(arg_hi);
    let arg_text = source.get(arg_lo..arg_hi).unwrap_or("");
    let active = top_level_commas(arg_text);
    let active = (active as usize).min(param_types.len().saturating_sub(1)) as u32;

    // Recover parameter NAMES (non-invasively) from the callee's binding AST when the callee is a
    // same-file `val`/`var` bound to a function literal. The function `Type` carries no names, so
    // we read them from the surface params. Only used when the name count matches the type-derived
    // param count (so positional bolding via `active` stays correct); otherwise fall back to
    // types-only labels.
    // `callee_span` is in CHAR offsets; convert to byte indices to slice the source text.
    let callee_lo = char_offset_to_byte(source, callee_span.start as usize);
    let callee_hi = char_offset_to_byte(source, callee_span.end as usize);
    let callee_name = source.get(callee_lo..callee_hi).unwrap_or("");
    let names = callee_param_names(&analysis.module, callee_name)
        .filter(|n| n.len() == param_types.len());

    // The callee's doc comment, if any — its description annotates the signature and its `@param`
    // entries annotate the matching parameters (matched by name).
    let doc = resolve_doc(callee_name).filter(|d| !d.is_empty());

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

    // Attach each param's `@param` description (matched by NAME) as its documentation. Params with
    // no name recovered, or no matching `@param` entry, get no doc (left empty per the brief).
    let parameters: Vec<ParameterInformation> = rendered
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let param_doc = names
                .as_ref()
                .and_then(|n| n.get(i))
                .and_then(|name| doc.as_ref().and_then(|d| d.param_doc(name)))
                .filter(|d| !d.trim().is_empty())
                .map(|d| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: d.trim().to_string(),
                    })
                });
            ParameterInformation {
                label: ParameterLabel::Simple(p.clone()),
                documentation: param_doc,
            }
        })
        .collect();

    // The function's description (prose + Returns) annotates the whole signature.
    let signature_doc = doc.as_ref().and_then(|d| {
        let desc = d.description_text();
        let mut parts: Vec<String> = Vec::new();
        if !desc.trim().is_empty() {
            parts.push(desc);
        }
        if let Some(ret) = &d.returns {
            if !ret.trim().is_empty() {
                parts.push(format!("**Returns** {}", ret.trim()));
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: parts.join("\n\n"),
            }))
        }
    });

    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: signature_doc,
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
            // `open` is a CHAR offset (all span offsets are); convert to a byte index to read/slice
            // the source, and lift the matching-close byte distance back to a CHAR offset so the
            // `offset` comparison below stays in char space.
            let open = span.start as usize;
            let open_byte = char_offset_to_byte(source, open);
            if source.as_bytes().get(open_byte) == Some(&b'(') {
                // `.get` so a stale span start can't panic. Use the SOURCE matcher (not
                // `matching_paren`, which treats `<>` as brackets) — in real source `<`/`>` are
                // comparison operators and `=>` is a lambda arrow, so an argument like
                // `f(1 > 0, 2)` must not unbalance the paren scan.
                let close = source
                    .get(open_byte..)
                    .and_then(matching_paren_in_source)
                    .map(|c| byte_offset_to_char(source, open_byte + c));
                let paren_after = open + 1; // `(` is ASCII, so +1 char == +1 byte
                // Inside the parens: after `(` and at/before the `)` (or to EOF when unclosed,
                // which is the common case while the user is still typing arguments).
                let end_bound = close.unwrap_or_else(|| source.chars().count());
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
///
/// Scans a RENDERED TYPE string, where `<>` legitimately delimit generic arguments (`Foo<Bar>`). The
/// one ambiguity is the function arrow `=>`: its `>` is NOT a closing angle bracket, so a function
/// type like `(Int32) => Int32` would otherwise have its depth driven negative by the arrow. We skip
/// the `>` of a `=>` (a `>` immediately preceded by `=`) so function-typed parameters split correctly.
fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut prev = '\0';
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            // `=>` arrow: the `>` is part of the arrow, not a closing angle bracket.
            '>' if prev == '=' => {}
            ')' | ']' | '}' | '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        prev = c;
    }
    None
}

/// Index of the `)` that closes the `(` at byte 0 of `s`, scanning over USER SOURCE. Depth is
/// balanced over `()[]{}` only (NOT `<>`, which are comparison operators in source) and string
/// literals are skipped (honouring `\"` escapes) so a `)` inside a string can't close the call.
/// This is the source-text counterpart of `matching_paren` (which scans rendered type strings).
fn matching_paren_in_source(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if in_str {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
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

/// Split a RENDERED TYPE string `s` on top-level commas (commas at bracket depth 0), trimming each
/// piece. Depth balances `()[]{}<>`; the `>` of a function arrow `=>` is skipped (it is part of the
/// arrow, not a closing angle bracket) so a function-typed parameter like `(Int32) => Int32` is kept
/// as a single piece rather than split at its arrow.
fn split_top_level(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut prev = '\0';
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            '>' if prev == '=' => {}
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
        prev = c;
    }
    out.push(s[start..].trim().to_string());
    out
}

/// Count top-level commas (bracket-depth 0) in USER SOURCE argument text. Used to pick the active
/// parameter from the text between a call's `(` and the cursor.
///
/// Unlike `split_top_level`/`matching_paren` (which scan RENDERED TYPE strings, where `<>` legitimately
/// delimit generic arguments), this runs over real source where `<`/`>` are the comparison operators
/// and `=>` is the lambda arrow — so it must NOT treat them as bracket depth. Doing so previously
/// skewed the comma count for argument text like `f(x > 0, y)` or `f(x => x, y)`, mis-bolding the
/// active parameter. Depth is tracked using only `()`, `[]`, `{}`. Commas inside string literals
/// (`"..."`, honouring `\"` escapes) are skipped; `${...}` interpolation spans inside a string are
/// likewise ignored along with the rest of the string body.
fn top_level_commas(s: &str) -> u32 {
    let mut depth = 0i32;
    let mut count = 0u32;
    let mut in_str = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            match c {
                // Skip the escaped character (e.g. `\"` does not close the string).
                '\\' => {
                    chars.next();
                }
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
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
        // Render raw solver type-var ids into readable generic names (`?T9004` → `T`, the `Json`
        // marker → `Json`) so an inferred generic/Json binding shows a clean hint instead of being
        // suppressed. `clean_type_string` is a no-op on already-concrete types.
        let ty_str = clean_type_string(&ty_str);
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

// ── offset spaces (READ THIS before touching the conversions below) ───────────────
//
// THREE offset spaces meet in this server; mixing them silently corrupts every range
// once a multibyte character appears on a line:
//
//   1. CHAR offset  — a count of Unicode scalar values (`char`s). EVERY `lin_common::Span`
//      the lexer/parser produces is in this space: the lexer scans a `Vec<char>` and
//      `self.pos` indexes that char vector (`lin-lex/src/lexer.rs`). So `span.start`/
//      `span.end` are char offsets, NOT byte offsets. This is the CANONICAL offset space
//      on the Lin side — every `offset: usize` parameter in THIS file is a char offset.
//   2. BYTE offset  — an index into the UTF-8 `&str`. Needed ONLY when slicing/indexing
//      `source` (Rust strings slice by byte). Convert a char offset to a byte offset with
//      `char_offset_to_byte` at the exact slice site, and convert a byte result back with
//      `byte_offset_to_char` if it re-enters char space.
//   3. UTF-16 col   — the `Position.character` field on the LSP wire (the default
//      `positionEncoding`). An astral codepoint (e.g. an emoji) is TWO UTF-16 units.
//
// `offset_to_position` / `position_to_offset` are the ONLY bridges between (1) and (3).
// Keep the Lin side uniformly in char offsets; touch bytes only at a slice, and UTF-16
// only at the wire.

/// Byte index of the char at char offset `char_off` (clamped to the end of the string when
/// `char_off` is past the last char). Use this immediately before slicing/indexing `source`,
/// since Rust `&str` slicing is by byte. Never panics and always returns a char boundary.
fn char_offset_to_byte(source: &str, char_off: usize) -> usize {
    source
        .char_indices()
        .nth(char_off)
        .map(|(b, _)| b)
        .unwrap_or(source.len())
}

/// Char offset of the char that begins at (or, for a non-boundary `byte_off`, contains) byte
/// index `byte_off`. The inverse of `char_offset_to_byte`, used to lift a byte index produced
/// by a `&str` search back into the canonical char-offset space. Clamps to the total char count
/// when `byte_off` is at/after the end.
fn byte_offset_to_char(source: &str, byte_off: usize) -> usize {
    source
        .char_indices()
        .take_while(|(b, _)| *b < byte_off)
        .count()
}

/// Convert a CHAR offset into the source to an LSP `Position`. The INPUT is a char offset (the
/// space all `lin_common::Span` offsets live in — see the offset-spaces note above). The OUTPUT
/// `character` field is a **UTF-16 code-unit** column (the LSP default `positionEncoding`), NOT a
/// `char`/codepoint count: a character outside the BMP (e.g. an emoji) is two UTF-16 units, so
/// columns after it advance by `ch.len_utf16()`. Loop over `chars().enumerate()` so the counter is
/// the CHAR index, matching the input space; comparing a byte index against a char offset (as a
/// naive implementation does) stops too early once a multibyte char precedes the target.
fn offset_to_position(source: &str, offset: usize) -> Position {
    let mut line = 0u32;
    let mut character = 0u32;
    for (char_index, ch) in source.chars().enumerate() {
        if char_index >= offset {
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

/// Convert an LSP `Position` (line + UTF-16 code-unit column) back to a CHAR offset into the source
/// (the canonical Lin offset space — see the offset-spaces note above; callers compare the result
/// against `lin_common::Span` offsets or pass it to char-offset helpers). Mirrors
/// `offset_to_position`: we accumulate UTF-16 code units per `char` (`ch.len_utf16()`) until the
/// target column is reached, RETURNING the count of chars consumed (a char offset), NOT the byte
/// index. A malformed `pos.character` that lands in the MIDDLE of a surrogate pair (only possible if
/// the client sends a bad position) is rounded forward to the next char boundary. Position past EOF
/// clamps to the total char count.
fn position_to_offset(source: &str, pos: Position) -> usize {
    let mut line = 0u32;
    let mut character = 0u32;
    for (char_index, ch) in source.chars().enumerate() {
        if line == pos.line && character >= pos.character {
            return char_index;
        }
        if ch == '\n' {
            // Reached the end of the requested line before the requested column: clamp to the
            // newline's char offset (the line is shorter than the client's column).
            if line == pos.line {
                return char_index;
            }
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16() as u32;
        }
    }
    source.chars().count()
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

    /// Resolve a bare symbol `name` as seen from the file `module_id` to its owner module +
    /// export name, WITHOUT a cursor offset. Mirrors `resolve_symbol`'s logic (own export first,
    /// then an imported binding) but keys off a name the caller already has (completion item /
    /// signature-help callee). Returns `None` when the file isn't indexed or the name is neither a
    /// local export nor an imported binding.
    fn resolve_symbol_by_name(&self, module_id: &str, name: &str) -> Option<(String, String)> {
        let file = self.files.get(module_id)?;
        if file.exports.iter().any(|(n, _)| n == name) {
            return Some((module_id.to_string(), name.to_string()));
        }
        if let Some(imp) = file.imports.iter().find(|i| i.local_name == name) {
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
    ///
    /// SOUNDNESS (the whole point of this function vs. `occurrences`): rename edits are derived from
    /// IDENTIFIER TOKENS (`identifier_token_spans`), NOT a raw text scan. That excludes matches inside
    /// comments and string literals, which a text scan would wrongly rewrite. For the SHADOWING case
    /// — an importing file that locally re-binds the same name (`val foo`, a param `foo`, a
    /// destructuring `{ foo }`, etc.) — a flat token scan still cannot tell the import's uses from
    /// the shadow's uses, so we refuse to touch the body: only the import-clause token is renamed and
    /// the body uses are left to the user. Partial-but-sound beats complete-but-wrong.
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

        // Owner file: decl + every use of the export name — identifier tokens only (no
        // comment/string over-match). The owner file can locally shadow its own export inside a
        // nested scope; if it does, fall back to renaming only the export's declaration site.
        if let Some(owner) = self.files.get(owner_module) {
            if file_rebinds_name_excluding_decl(&owner.source, export_name) {
                if let Some(span) = decl_span(self, owner_module, export_name) {
                    out.push((owner_module.to_string(), span));
                }
            } else {
                for span in identifier_token_spans(&owner.source, export_name) {
                    out.push((owner_module.to_string(), span));
                }
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
                    // No alias. If this file locally re-binds the name (a shadowing `val`/`var`/param/
                    // destructuring), we cannot soundly distinguish import uses from shadow uses with
                    // a flat token scan — rename ONLY the import-clause token and leave body uses
                    // alone. Otherwise rename the clause token + every identifier-token body use.
                    if file_rebinds_name(&file.source, &imp.local_name) {
                        if let Some(off) = find_name_in_import(
                            &file.source,
                            imp.stmt_span.start as usize,
                            &imp.local_name,
                        ) {
                            out.push((
                                mod_id.clone(),
                                lin_common::Span::new(0, off as u32, (off + imp.local_name.len()) as u32),
                            ));
                        }
                    } else {
                        for span in identifier_token_spans(&file.source, &imp.local_name) {
                            out.push((mod_id.clone(), span));
                        }
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
    // `offset` is a char offset; word characters are ASCII, so we expand over BYTES from its byte
    // index (each word byte is single-byte, so the bounds stay on char boundaries).
    let bytes = source.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let byte_off = char_offset_to_byte(source, offset);
    let mut start = byte_off;
    let mut end = byte_off;
    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }
    while end < bytes.len() && is_word(bytes[end]) {
        end += 1;
    }
    &source[start..end]
}

/// Every whole-identifier occurrence of `name` in `source`, as CHAR-offset spans (the canonical
/// Lin span space — these go to `span_to_range`, which expects char offsets). The text scan runs
/// over BYTES (Rust `&str::find`), so the located byte positions are lifted to char offsets before
/// being stored. A match is whole-word when neither neighbouring byte is `[A-Za-z0-9_]`. This is
/// the cross-file occurrence primitive (the checker's def_span linkage is unavailable for imported
/// names — see the module note above).
fn whole_word_spans(source: &str, name: &str) -> Vec<lin_common::Span> {
    let mut out = Vec::new();
    if name.is_empty() {
        return out;
    }
    let bytes = source.as_bytes();
    let mut start = 0usize;
    while let Some(pos) = source[start..].find(name) {
        let abs = start + pos; // byte offset of the match
        let before_ok = abs == 0 || !(bytes[abs - 1].is_ascii_alphanumeric() || bytes[abs - 1] == b'_');
        let after_idx = abs + name.len();
        let after_ok = after_idx >= bytes.len()
            || !(bytes[after_idx].is_ascii_alphanumeric() || bytes[after_idx] == b'_');
        if before_ok && after_ok {
            let start_char = byte_offset_to_char(source, abs) as u32;
            let end_char = byte_offset_to_char(source, after_idx) as u32;
            out.push(lin_common::Span::new(0, start_char, end_char));
        }
        start = abs + 1;
    }
    out
}

/// Every IDENTIFIER-token occurrence of `name` in `source`, as byte spans — the SOUND occurrence
/// primitive for rename. Unlike `whole_word_spans` (a raw `source.find` text scan), this lexes the
/// file and keeps only `TokenKind::Ident` spans, so matches inside COMMENTS and STRING LITERALS are
/// excluded (the lexer emits those as `Comment`/`StringLit`, never `Ident`). Identifiers inside
/// `${...}` string interpolations ARE real uses and are collected by recursing into the
/// interpolation's sub-token-stream (ADR-004). It does NOT do scope resolution, so a same-named
/// local that shadows the symbol is still matched — callers that need shadow-safety must guard with
/// `file_rebinds_name` first.
fn identifier_token_spans(source: &str, name: &str) -> Vec<lin_common::Span> {
    let mut out = Vec::new();
    if name.is_empty() {
        return out;
    }
    let mut lexer = lin_lex::Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    collect_ident_spans_in_tokens(&tokens, name, &mut out);
    out.sort_by_key(|s| (s.start, s.end));
    out.dedup();
    out
}

/// Recurse a token stream pushing every `Ident(name)` span, descending into `InterpString` expr
/// parts so identifiers used inside `${...}` interpolations count as real uses.
fn collect_ident_spans_in_tokens(tokens: &[lin_lex::Token], name: &str, out: &mut Vec<lin_common::Span>) {
    use lin_lex::{InterpPart, TokenKind};
    for tok in tokens {
        match &tok.kind {
            TokenKind::Ident(n) if n == name => out.push(tok.span),
            TokenKind::InterpString(parts) => {
                for part in parts {
                    if let InterpPart::Expr(sub) = part {
                        collect_ident_spans_in_tokens(sub, name, out);
                    }
                }
            }
            _ => {}
        }
    }
}

/// True when the file's AST introduces a LOCAL binding named `name` anywhere (a `val`/`var`, a
/// function parameter, or a destructuring/match pattern element) — i.e. a binding that could SHADOW
/// an imported symbol of the same name. When this holds, a flat identifier-token scan cannot tell an
/// import use from a shadow use, so rename must NOT rewrite body uses (it would corrupt the shadow).
/// Conservative: the top-level `import` binding itself is excluded (it's the symbol we're renaming,
/// not a shadow), but any other introduction of `name` returns true.
fn file_rebinds_name(source: &str, name: &str) -> bool {
    let mut lexer = lin_lex::Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    let mut parser = lin_parse::Parser::new(tokens);
    let module = parser.parse_module();
    module.statements.iter().any(|s| stmt_binds_name(s, name, true))
}

/// Like `file_rebinds_name`, but for the OWNER file: the symbol's own top-level `export val|var`
/// declaration is NOT a shadow (it's the declaration we're renaming). Returns true only when a
/// NESTED scope (function body, block, match arm) re-binds the name — in which case the owner file's
/// body uses can't be soundly distinguished from the shadow, so the caller renames only the decl
/// site.
fn file_rebinds_name_excluding_decl(source: &str, name: &str) -> bool {
    let mut lexer = lin_lex::Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    let mut parser = lin_parse::Parser::new(tokens);
    let module = parser.parse_module();
    module.statements.iter().any(|s| match s {
        // The export's own top-level binding is the declaration, not a shadow — but a nested
        // re-binding inside its initializer expression still counts.
        Stmt::Val { pattern, value, .. } if pattern_binds_name(pattern, name) => {
            expr_binds_name(value, name)
        }
        Stmt::Var { name: vn, value, .. } if vn == name => expr_binds_name(value, name),
        other => stmt_binds_name(other, name, true),
    })
}

/// Whether `stmt` (recursively, into expressions) introduces a binding named `name`. When
/// `top_level` is true the statement is a module-level statement and a top-level `import` of `name`
/// is NOT counted as a shadow (it's the import we're tracking); a top-level `val`/`var` of `name`
/// IS a shadow (re-binding the imported name at module scope).
fn stmt_binds_name(stmt: &Stmt, name: &str, top_level: bool) -> bool {
    match stmt {
        Stmt::Val { pattern, value, .. } => {
            pattern_binds_name(pattern, name) || expr_binds_name(value, name)
        }
        Stmt::Var { name: vn, value, .. } => vn == name || expr_binds_name(value, name),
        Stmt::Replace { name: rn, value, .. } => rn == name || expr_binds_name(value, name),
        Stmt::Expr(e) => expr_binds_name(e, name),
        // A top-level import of `name` is the binding we're renaming, not a shadow.
        Stmt::Import { .. } if top_level => false,
        _ => false,
    }
}

/// Whether a binding pattern (incl. destructuring) introduces `name`.
fn pattern_binds_name(pattern: &lin_parse::ast::Pattern, name: &str) -> bool {
    use lin_parse::ast::Pattern as P;
    match pattern {
        P::Ident(n, _) => n == name,
        P::Object(fields, rest, _) => {
            rest.as_deref() == Some(name)
                || fields.iter().any(|f| pattern_binds_name(&f.pattern, name))
        }
        P::Array(items, rest, _) => {
            rest.as_deref() == Some(name) || items.iter().any(|p| pattern_binds_name(p, name))
        }
        _ => false,
    }
}

/// Whether an expression introduces a binding named `name` in some nested scope: a function
/// parameter, or a `val`/`var`/match-pattern inside a block/branch. Use-sites are irrelevant here —
/// we only look for binding INTRODUCTIONS that would shadow the import.
fn expr_binds_name(expr: &lin_parse::ast::Expr, name: &str) -> bool {
    use lin_parse::ast::Expr as E;
    match expr {
        E::Function { params, body, .. } => {
            params.iter().any(|p| pattern_binds_name(&p.pattern, name)
                || p.default.as_ref().map(|d| expr_binds_name(d, name)).unwrap_or(false))
                || expr_binds_name(body, name)
        }
        E::Block(stmts, tail, _, _) => {
            stmts.iter().any(|s| stmt_binds_name(s, name, false)) || expr_binds_name(tail, name)
        }
        E::If { condition, then_branch, else_branch, .. } => {
            expr_binds_name(condition, name)
                || expr_binds_name(then_branch, name)
                || expr_binds_name(else_branch, name)
        }
        E::Match { scrutinee, arms, .. } => {
            use lin_parse::ast::MatchPattern;
            expr_binds_name(scrutinee, name)
                || arms.iter().any(|a| {
                    let pat_binds = match &a.pattern {
                        MatchPattern::Is(p) | MatchPattern::Has(p) => pattern_binds_name(p, name),
                        MatchPattern::Else => false,
                    };
                    pat_binds
                        || a.guard.as_ref().map(|g| expr_binds_name(g, name)).unwrap_or(false)
                        || expr_binds_name(&a.body, name)
                })
        }
        E::Call { func, args, .. } => {
            expr_binds_name(func, name) || args.iter().any(|a| expr_binds_name(a, name))
        }
        E::DotCall { receiver, args, .. } => {
            expr_binds_name(receiver, name)
                || args.as_ref().map(|a| a.iter().any(|x| expr_binds_name(x, name))).unwrap_or(false)
        }
        E::BinaryOp { left, right, .. } => {
            expr_binds_name(left, name) || expr_binds_name(right, name)
        }
        E::UnaryOp { operand, .. } => expr_binds_name(operand, name),
        E::Assign { value, .. } => expr_binds_name(value, name),
        E::IndexAssign { value, .. } => expr_binds_name(value, name),
        E::Index { object, key, .. } => {
            expr_binds_name(object, name) || expr_binds_name(key, name)
        }
        E::Array(items, _, _) => items.iter().any(|i| expr_binds_name(i, name)),
        E::Object(fields, _, _) => fields.iter().any(|f| {
            use lin_parse::ast::ObjectField;
            match f {
                ObjectField::Pair(k, v) => expr_binds_name(k, name) || expr_binds_name(v, name),
                ObjectField::Spread(e) => expr_binds_name(e, name),
            }
        }),
        E::Is { expr, pattern, .. } | E::Has { expr, pattern, .. } => {
            expr_binds_name(expr, name) || pattern_binds_name(pattern, name)
        }
        E::StringInterp(parts, _) => parts.iter().any(|p| {
            use lin_parse::ast::StringPart;
            matches!(p, StringPart::Expr(e) if expr_binds_name(e, name))
        }),
        E::TupleArgs(items, _) => items.iter().any(|i| expr_binds_name(i, name)),
        _ => false,
    }
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
    "std/test", "std/time", "std/path", "std/math", "std/env",
    "std/bytes", "std/regex", "std/crypto", "std/csv", "std/encoding", "std/random", "std/bignum", "std/decimal", "std/net", "std/process", "std/tty", "std/signal", "std/yaml",
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
        // Polymorphic first params render as "any" (generic/Json/union).
        assert_eq!(first_param_category("(T, (T) => U) => U[]").as_deref(), Some("any"));
        assert_eq!(first_param_category("(Json) => String").as_deref(), Some("any"));
        assert_eq!(first_param_category("(Int32 | String) => Boolean").as_deref(), Some("any"));
    }

    /// Dot-completion applicability: a polymorphic (`any`) first param matches ANY receiver (the
    /// `map`/`filter`/`reduce` idiom), no-signature items are kept, and a concretely-typed mismatch
    /// is excluded.
    #[test]
    fn dot_item_applies_includes_polymorphic_combinators() {
        // Polymorphic combinator first param ("any") applies to every concrete receiver.
        assert!(dot_item_applies("array", Some("any")));
        assert!(dot_item_applies("string", Some("any")));
        assert!(dot_item_applies("object", Some("any")));
        // Exact concrete match still applies.
        assert!(dot_item_applies("array", Some("array")));
        assert!(dot_item_applies("string", Some("string")));
        // No signature → can't prove inapplicable → kept.
        assert!(dot_item_applies("array", None));
        // Unknown receiver → everything applies.
        assert!(dot_item_applies("any", Some("string")));
        // Concrete mismatch is the ONLY exclusion.
        assert!(!dot_item_applies("array", Some("string")));
        assert!(!dot_item_applies("string", Some("number")));
    }

    /// End-to-end: an imported fn with a GENERIC first param is offered on an array receiver (the
    /// `xs.map(...)` idiom), while a concretely string-typed method is excluded on an array receiver.
    #[test]
    fn completion_offers_polymorphic_combinator_on_array_receiver() {
        // `mapper` has a generic first param (receiver-polymorphic); `upper` is string-only.
        let src = concat!(
            "val mapper = <T, U>(xs: T[], f: (T) => U) => xs\n",
            "val upper = (s: String) => s\n",
            "export val mapper2 = mapper\n",
        );
        // Build the imported-names list the completion handler filters: simulate the two functions
        // as imported names with their rendered first-param categories.
        let poly = ImportedName {
            name: "map".to_string(),
            module: "std/iter".to_string(),
            ty: Some("(T, (T) => U) => U[]".to_string()),
        };
        let stronly = ImportedName {
            name: "upper".to_string(),
            module: "std/string".to_string(),
            ty: Some("(String) => String".to_string()),
        };
        let _ = src; // documentation of the shapes; the filter is driven off the rendered types.

        // Array receiver: polymorphic `map` applies, string-only `upper` does not.
        assert!(dot_item_applies("array", poly.ty.as_deref().and_then(first_param_category).as_deref()));
        assert!(!dot_item_applies("array", stronly.ty.as_deref().and_then(first_param_category).as_deref()));
        // String receiver: BOTH apply (poly always, upper because its first param is string).
        assert!(dot_item_applies("string", poly.ty.as_deref().and_then(first_param_category).as_deref()));
        assert!(dot_item_applies("string", stronly.ty.as_deref().and_then(first_param_category).as_deref()));
    }

    // ── context-aware completion (scope/position awareness) ──────────────────

    /// Classify the cursor at the `▮` marker in `src` (the marker is stripped before parsing). Char
    /// offset of the marker is its position in the marker-free source.
    fn classify_at(src_with_marker: &str) -> CompletionContext {
        let offset = src_with_marker.find('▮').expect("missing ▮ cursor marker");
        // The marker is a 3-byte char; the char offset of the cursor equals the char count before it.
        let char_off = src_with_marker[..offset].chars().count();
        let src = src_with_marker.replace('▮', "");
        let module = parse(&src);
        classify_completion_context(&module, &src, char_off)
    }

    /// Collect the in-scope binding NAMES at the `▮` marker in `src`.
    fn scope_names_at(src_with_marker: &str) -> Vec<String> {
        let offset = src_with_marker.find('▮').expect("missing ▮ cursor marker");
        let char_off = src_with_marker[..offset].chars().count();
        let src = src_with_marker.replace('▮', "");
        let module = parse(&src);
        collect_scope_bindings(&module, char_off)
            .into_iter()
            .map(|b| b.name)
            .collect()
    }

    #[test]
    fn classify_expression_inside_lambda_body_and_val_rhs() {
        // Inside a lambda body passed to `.for(...)` — expression position.
        assert_eq!(
            classify_at("val nums = [1, 2, 3]\nnums.for(item =>\n  print(it▮)\n)\n"),
            CompletionContext::Expression
        );
        // A `val` RHS is an expression position.
        assert_eq!(
            classify_at("val x = fo▮\n"),
            CompletionContext::Expression
        );
    }

    #[test]
    fn classify_type_annotation_positions() {
        // After `val x: ` (no type text yet) — backward scan fires.
        assert_eq!(classify_at("val x: ▮\n"), CompletionContext::TypeAnnotation);
        // Partially-typed annotation — the parsed TypeExpr span brackets the cursor.
        assert_eq!(classify_at("val x: Int▮\n"), CompletionContext::TypeAnnotation);
        // A function PARAM annotation `(x: ▮`.
        assert_eq!(classify_at("val f = (x: ▮) => x\n"), CompletionContext::TypeAnnotation);
        // A function RETURN type `): ▮`.
        assert_eq!(classify_at("val f = (x: Int32): ▮ => x\n"), CompletionContext::TypeAnnotation);
        // A `type T = ▮` declaration RHS.
        assert_eq!(classify_at("type Id = ▮\n"), CompletionContext::TypeAnnotation);
        // An object-LITERAL `key:` is NOT a type position (it's a value).
        assert_eq!(classify_at("val o = { k: ▮ }\n"), CompletionContext::Expression);
    }

    /// FIX 1: the binding-NAME position (`val ▮` / `var ▮`, before any `:`/`=`) offers NOTHING.
    /// Boundaries: once a `:` appears it's a TypeAnnotation; once a `=` appears the RHS is an
    /// Expression. The bare-name position (mid-block, indented, name partially typed) is suppressed.
    #[test]
    fn classify_binding_name_position_offers_nothing() {
        // Cursor right after `val ` (top level), no name yet.
        assert_eq!(classify_at("val ▮\n"), CompletionContext::BindingName);
        // `var` too.
        assert_eq!(classify_at("var ▮\n"), CompletionContext::BindingName);
        // Mid-typing the name is still the name position.
        assert_eq!(classify_at("val fo▮\n"), CompletionContext::BindingName);
        // The report's exact shape: indented, inside a lambda body passed to `.for(...)`.
        assert_eq!(
            classify_at("val nums = [1, 2, 3]\nnums.for(item =>\n  print(item)\n  val ▮\n)\n"),
            CompletionContext::BindingName
        );
        // BOUNDARY: a `:` makes it a TYPE position again (types, not nothing).
        assert_eq!(classify_at("val x:▮\n"), CompletionContext::TypeAnnotation);
        assert_eq!(classify_at("val x: ▮\n"), CompletionContext::TypeAnnotation);
        // BOUNDARY: a `=` makes the RHS an EXPRESSION.
        assert_eq!(classify_at("val x =▮\n"), CompletionContext::Expression);
        assert_eq!(classify_at("val x = ▮\n"), CompletionContext::Expression);
        // A still-being-typed `va`/`var` keyword (no following space) stays StatementStart, not
        // BindingName — there's no binding yet, the user may be typing `val`/`var`/`var`.
        assert_eq!(classify_at("va▮\n"), CompletionContext::StatementStart);
    }

    #[test]
    fn classify_statement_start_and_import() {
        // A fresh top-level line is a statement start.
        assert_eq!(classify_at("val a = 1\n▮\n"), CompletionContext::StatementStart);
        // Partially-typed declaration keyword at line start.
        assert_eq!(classify_at("val a = 1\nva▮\n"), CompletionContext::StatementStart);
        // Inside an import clause (before the `from "…"` string).
        assert_eq!(classify_at("import { foo▮ } from \"std/io\"\n"), CompletionContext::ImportStmt);
        // A wrapped `val` RHS continuation is NOT a statement start (prev line ends with `=`).
        assert_eq!(classify_at("val x =\n  ▮\n"), CompletionContext::Expression);
    }

    /// THE regression test for the report: the user's exact snippet, cursor after `it`. The in-scope
    /// set must include the lambda param `item` (even though it's never referenced) AND the outer
    /// `nums`, and must NOT include an out-of-scope sibling binding.
    #[test]
    fn scope_collection_surfaces_unreferenced_lambda_param() {
        let names = scope_names_at(
            "val nums = [1, 2, 3]\nval other = 99\nnums.for(item =>\n  print(it▮)\n)\n",
        );
        assert!(names.contains(&"item".to_string()), "expected lambda param `item`, got {names:?}");
        assert!(names.contains(&"nums".to_string()), "expected outer `nums`, got {names:?}");
        // `other` is a sibling top-level binding — also in scope (module-global), so present.
        assert!(names.contains(&"other".to_string()), "expected top-level `other`, got {names:?}");
    }

    #[test]
    fn scope_collection_respects_lexical_scope_for_lambda_params() {
        // Cursor is in `f`'s body (after a complete reference); `g`'s param `b` is a sibling scope
        // that does NOT enclose it.
        let names = scope_names_at(
            "val f = (a: Int32) => print(a▮)\nval g = (b: Int32) => b\n",
        );
        assert!(names.contains(&"a".to_string()), "expected own param `a`, got {names:?}");
        assert!(!names.contains(&"b".to_string()), "sibling param `b` leaked: {names:?}");
    }

    #[test]
    fn scope_collection_includes_destructured_binders() {
        let names = scope_names_at("val { x, y } = pt\nprint(▮)\n");
        assert!(names.contains(&"x".to_string()), "expected destructured `x`, got {names:?}");
        assert!(names.contains(&"y".to_string()), "expected destructured `y`, got {names:?}");
    }

    /// Keyword gating: the report's hard requirement — no `import`/`from`/`as`/`export`/`val`/`var`/
    /// `type` in expression position; declaration keywords + `import` at a statement start; `from`/
    /// `as` only inside an import clause.
    #[test]
    fn keyword_gating_by_context() {
        let expr = keywords_for_context(CompletionContext::Expression);
        for banned in ["import", "from", "as", "export", "val", "var", "type"] {
            assert!(!expr.contains(&banned), "`{banned}` must NOT be offered in expression position");
        }
        // Expression-level keywords + literals ARE offered.
        for kw in ["if", "match", "is", "true", "null"] {
            assert!(expr.contains(&kw), "`{kw}` expected in expression position");
        }

        let stmt = keywords_for_context(CompletionContext::StatementStart);
        assert!(stmt.contains(&"val"), "`val` expected at statement start");
        assert!(stmt.contains(&"import"), "`import` expected at statement start");
        assert!(!stmt.contains(&"from"), "`from` must not be at statement start");

        let imp = keywords_for_context(CompletionContext::ImportStmt);
        assert!(imp.contains(&"from") && imp.contains(&"as"), "from/as expected inside import clause");
        assert!(!imp.contains(&"val"), "`val` must not be inside import clause");

        // No keywords in a type-annotation position (types are offered there instead).
        assert!(keywords_for_context(CompletionContext::TypeAnnotation).is_empty());
    }

    /// Type gating: built-in + user `type` names are offered ONLY in a TypeAnnotation position; user
    /// type names are collected from the module's `type` decls + in-scope generic params.
    #[test]
    fn type_name_collection_and_gating() {
        // User type decl + a generic param inside the declaring fn are collected at a body cursor.
        let names = {
            let src_with_marker = "type Point = { x: Int32 }\nval id = <T>(v: T): T => ▮\n";
            let offset = src_with_marker.find('▮').unwrap();
            let char_off = src_with_marker[..offset].chars().count();
            let src = src_with_marker.replace('▮', "");
            let module = parse(&src);
            collect_type_names(&module, char_off)
        };
        assert!(names.contains(&"Point".to_string()), "expected user type `Point`, got {names:?}");
        assert!(names.contains(&"T".to_string()), "expected generic param `T`, got {names:?}");

        // Type keywords/builtins are gated by context: types only in TypeAnnotation; keywords (which
        // exclude types) carry the value-side. The handler offers builtins only when
        // ctx == TypeAnnotation — assert the classification drives that:
        assert_eq!(classify_at("val x: ▮\n"), CompletionContext::TypeAnnotation);
        assert_eq!(classify_at("val x = ▮\n"), CompletionContext::Expression);
    }

    /// End-to-end gate documentation: in Expression position the in-scope binder set + the keyword
    /// set together must surface `item`/`nums` and exclude statement keywords. This composes the two
    /// pure pieces the async handler wires together.
    #[test]
    fn expression_position_offers_bindings_not_statement_keywords() {
        let src_with_marker = "val nums = [1, 2, 3]\nnums.for(item =>\n  print(it▮)\n)\n";
        let ctx = classify_at(src_with_marker);
        assert_eq!(ctx, CompletionContext::Expression);
        let names = scope_names_at(src_with_marker);
        assert!(names.contains(&"item".to_string()));
        assert!(names.contains(&"nums".to_string()));
        let kws = keywords_for_context(ctx);
        assert!(!kws.contains(&"import"), "import leaked into expression completion");
    }

    /// FIX A: raw solver type-var ids (`?T9004`) render as clean, stable generic names. The same
    /// id repeated maps to the same letter; distinct ids get distinct letters in first-appearance
    /// order; the `Json` marker (`?T4294967295`) renders as `Json`; and the transform is idempotent.
    #[test]
    fn clean_type_string_renders_generic_names() {
        // Distinct vars → distinct letters in first-appearance order; same var → same letter.
        let sig = "(?T9004[] | Iterator | Stream, (?T9004, Int32) => ?T9005) => ?T9005[]";
        let cleaned = clean_type_string(sig);
        assert_eq!(cleaned, "(T[] | Iterator | Stream, (T, Int32) => U) => U[]");
        // No raw `?T` token or solver id digits survive.
        assert!(!cleaned.contains("?T"), "raw type-var token leaked: {cleaned}");
        assert!(!cleaned.contains("9004") && !cleaned.contains("9005"));
        // Stable: the assignment is positional, not dependent on the (arbitrary) id VALUES, so a
        // higher id appearing FIRST still becomes `T`.
        assert_eq!(clean_type_string("(?T42, ?T7) => ?T7"), "(T, U) => U");
        // Idempotent: re-cleaning the output is a no-op.
        assert_eq!(clean_type_string(&cleaned), cleaned);
        // The Json marker (`TypeVar(u32::MAX)`) renders as `Json`, not a giant letter index.
        assert_eq!(clean_type_string("(?T4294967295) => String"), "(Json) => String");
        // A concrete type is passed through untouched (fast path, no `?T`).
        assert_eq!(clean_type_string("(String, Int32) => Boolean"), "(String, Int32) => Boolean");
        // A real checker-rendered generic `Type` cleans to letters too (end-to-end via Display).
        let ty = Type::Function {
            params: vec![Type::TypeVar(9004), Type::TypeVar(9005)],
            ret: Box::new(Type::Array(Box::new(Type::TypeVar(9004)))),
            required: 2,
        };
        assert_eq!(clean_type_string(&ty.to_string()), "(T, U) => T[]");
    }

    // ── FIX B: unimported stdlib combinators in dot-completion ───────────────────

    /// Test helpers: a tiny URI and a `StdlibCandidate` builder so the offer/gate/dedupe path can be
    /// driven directly (the real list comes from `STDLIB_DOT_CANDIDATES`, exercised below).
    fn fixb_uri() -> Url {
        Url::parse("file:///tmp/fixb.lin").unwrap()
    }
    fn cand(name: &str, module: &str, ty: &str) -> StdlibCandidate {
        StdlibCandidate { name: name.to_string(), module: module.to_string(), ty: ty.to_string() }
    }
    /// Find the candidate offered (by label) in a built item list, if any.
    fn item_named<'a>(items: &'a [CompletionItem], name: &str) -> Option<&'a CompletionItem> {
        items.iter().find(|i| i.label == name)
    }

    /// `map` IS offered on an array receiver, labelled with the bare NAME (a plain identifier insert,
    /// no `insert_text`/snippet), with a clean `detail` (no raw `?T`) noting its source module, and a
    /// `data` payload carrying the owner module so `completion_resolve` can attach the import edit.
    #[test]
    fn stdlib_dot_offers_map_on_array_receiver() {
        // The real `map` signature (first param `?T[] | Iterator | Stream`) → category "array".
        let cands = vec![cand(
            "map",
            "std/iter",
            "(?T9002[] | Iterator<?T4294967295> | Stream<?T4294967295>, (?T9002, Int32) => ?T9003) => ?T9003[]",
        )];
        let already = HashSet::new();
        let items = stdlib_dot_completion_items(cands.iter(), "array", "", &already, &fixb_uri());
        let map = item_named(&items, "map").expect("`map` must be offered on an array receiver");
        // Plain identifier insert — no snippet/insert_text.
        assert!(map.insert_text.is_none(), "must insert the bare NAME, not a snippet");
        assert_eq!(map.kind, Some(CompletionItemKind::FUNCTION));
        // Clean detail (FIX A): no raw solver ids leak; the source module is shown.
        let detail = map.detail.as_deref().unwrap();
        assert!(!detail.contains("?T"), "raw type-var token leaked into detail: {detail}");
        assert!(detail.contains("(from std/iter)"), "detail should note the source module: {detail}");
        // The resolve payload carries the owner module (so the import edit can be attached lazily).
        assert_eq!(parse_completion_resolve_module(map.data.as_ref()).as_deref(), Some("std/iter"));
    }

    /// Accepting an offered candidate yields the import edit via the SHARED `auto_import_edit` (the
    /// same fn the auto-import code action uses). With no existing std/iter import, it adds a new line.
    #[test]
    fn stdlib_dot_import_edit_adds_new_line_when_module_unimported() {
        let src = "val xs = [1, 2, 3]\n";
        let edit = auto_import_edit(src, "map", "std/iter").expect("expected a new-import edit");
        assert!(
            edit.new_text.contains("import { map } from \"std/iter\""),
            "expected a new std/iter import line, got: {:?}",
            edit.new_text
        );
    }

    /// The import edit MERGES into an existing `import { ... } from "std/iter"` rather than adding a
    /// second line — `, map` is inserted into the existing brace list (shared `auto_import_edit`).
    #[test]
    fn stdlib_dot_import_edit_merges_into_existing_iter_import() {
        let src = "import { filter } from \"std/iter\"\nval xs = [1, 2, 3]\n";
        let edit = auto_import_edit(src, "map", "std/iter").expect("expected a merge edit");
        // A merge inserts `, map` (an append into the existing brace list), NOT a whole new line.
        assert_eq!(edit.new_text, ", map");
        // …on the existing import's line (line 0), so no duplicate `import ... std/iter` line is added.
        assert_eq!(edit.range.start.line, 0);
        assert!(!edit.new_text.contains("import"), "merge must not emit a second import line");
    }

    /// Dedupe: a candidate whose name is ALREADY offered (imported symbol / earlier candidate, seeded
    /// into `already`) is NOT listed a second time.
    #[test]
    fn stdlib_dot_dedupes_already_offered() {
        let cands = vec![cand("map", "std/iter", "(?T9002[], (?T9002, Int32) => ?T9003) => ?T9003[]")];
        let mut already = HashSet::new();
        already.insert("map".to_string());
        let items = stdlib_dot_completion_items(cands.iter(), "array", "", &already, &fixb_uri());
        assert!(item_named(&items, "map").is_none(), "an already-offered `map` must not be re-offered");
    }

    /// Gating: a candidate whose first-param category doesn't match the receiver is NOT offered, so a
    /// `.` doesn't dump the whole stdlib. `range` (`(Int32, Int32) => …`, category "number") is dropped
    /// on an array receiver; `map` (first param category "array") is dropped on a string receiver.
    #[test]
    fn stdlib_dot_gates_by_receiver_category() {
        let cands = vec![
            cand("range", "std/iter", "(Int32, Int32) => Iterator<Int32>"),
            cand("map", "std/iter", "(?T9002[] | Iterator<?T4294967295> | Stream<?T4294967295>, (?T9002, Int32) => ?T9003) => ?T9003[]"),
        ];
        let already = HashSet::new();
        // Array receiver: `range` (number first param) is gated OUT; `map` (array) is kept.
        let on_array = stdlib_dot_completion_items(cands.iter(), "array", "", &already, &fixb_uri());
        assert!(item_named(&on_array, "range").is_none(), "`range` must be gated out on an array receiver");
        assert!(item_named(&on_array, "map").is_some(), "`map` should be offered on an array receiver");
        // String receiver: `map` (array first param) is gated OUT.
        let on_string = stdlib_dot_completion_items(cands.iter(), "string", "", &already, &fixb_uri());
        assert!(item_named(&on_string, "map").is_none(), "`map` (array first param) must be gated out on a string receiver");
    }

    /// The user's reported failure: `for` IS offered on an array receiver. `for`'s first param renders
    /// as the `Json` marker → category "any" → applies to every receiver (the `xs.for(...)` idiom).
    #[test]
    fn stdlib_dot_offers_for_on_array_receiver() {
        let cands = vec![cand(
            "for",
            "std/iter",
            "(?T4294967295, (?T4294967295, Int32) => ?T4294967295) => Null",
        )];
        let already = HashSet::new();
        let items = stdlib_dot_completion_items(cands.iter(), "array", "", &already, &fixb_uri());
        let for_item = item_named(&items, "for").expect("`for` must be offered on an array receiver");
        assert_eq!(parse_completion_resolve_module(for_item.data.as_ref()).as_deref(), Some("std/iter"));
    }

    /// End-to-end over the REAL embedded stdlib: the memoised candidate set built from the actual
    /// stdlib sources offers `map`/`filter`/`for` on an array receiver (each owned by std/iter), proves
    /// the build path type-checks the modules, and confirms the prefix filter narrows the list.
    #[test]
    fn stdlib_dot_real_candidates_offer_iter_combinators_on_array() {
        let already = HashSet::new();
        let items = stdlib_dot_completion_items(STDLIB_DOT_CANDIDATES.iter(), "array", "", &already, &fixb_uri());
        for name in ["map", "filter", "for"] {
            let it = item_named(&items, name)
                .unwrap_or_else(|| panic!("`{name}` should be offered on an array receiver from the real stdlib"));
            assert_eq!(
                parse_completion_resolve_module(it.data.as_ref()).as_deref(),
                Some("std/iter"),
                "`{name}` should be owned by std/iter"
            );
        }
        // `range` (number first param) is gated out even from the real set.
        assert!(item_named(&items, "range").is_none(), "`range` must be gated out on an array receiver");
        // Prefix filter: only `map` survives a "ma" prefix.
        let only_ma = stdlib_dot_completion_items(STDLIB_DOT_CANDIDATES.iter(), "array", "ma", &already, &fixb_uri());
        assert!(item_named(&only_ma, "map").is_some());
        assert!(item_named(&only_ma, "filter").is_none(), "prefix `ma` must exclude `filter`");
    }

    // ── FIX 2: only EXPORTED (non-`_`) stdlib symbols are offered ────────────────

    /// `ast_exported_names` reads the `export` flag from the PARSED AST. On a small mixed source it
    /// returns ONLY the `export`ed names — a plain `val`/`var` (not exported) and an `export val`
    /// helper named `_priv` are still reported faithfully (the export filter proper lives in the
    /// candidate builder, which separately drops `_`-prefixed names). Destructuring exports surface
    /// each bound name.
    #[test]
    fn ast_exported_names_reads_export_flag() {
        let src = "\
export val pub = 1
val priv_v = 2
export var counter = 0
var hidden = 9
export val { a, b } = pt
val _helper = 3
";
        let names = ast_exported_names(&parse(src));
        assert!(names.contains("pub"), "exported `val pub` missing: {names:?}");
        assert!(names.contains("counter"), "exported `var counter` missing: {names:?}");
        assert!(names.contains("a") && names.contains("b"), "destructured exports missing: {names:?}");
        // NOT exported → absent.
        assert!(!names.contains("priv_v"), "non-exported `val priv_v` leaked: {names:?}");
        assert!(!names.contains("hidden"), "non-exported `var hidden` leaked: {names:?}");
        assert!(!names.contains("_helper"), "non-exported `val _helper` leaked: {names:?}");
    }

    /// `stdlib_module_exports` honours the AST `export` flag: over the REAL std/array source it returns
    /// the genuinely-exported `lowerBound`/`sort`/`push` but NOT the private `val _bisect` helper
    /// (declared `val`, not `export val`) that `extract_exports(&typed)` would otherwise include.
    #[test]
    fn stdlib_module_exports_excludes_private_helpers() {
        let exports = stdlib_module_exports("std/array");
        let names: HashSet<String> = exports.into_iter().map(|(n, _)| n).collect();
        assert!(!names.is_empty(), "std/array must type-check and export something");
        for pub_name in ["lowerBound", "sort", "push", "length"] {
            assert!(names.contains(pub_name), "expected exported `{pub_name}`, got {names:?}");
        }
        assert!(!names.contains("_bisect"), "private `val _bisect` leaked into exports: {names:?}");
    }

    /// The real dot-candidate set (the source of the leak in the report) no longer offers `_bisect`
    /// (or any `_`-prefixed/unexported helper), while genuinely-exported functions remain. Tested on
    /// an array receiver where both an array-shaped export (`push`) and `_bisect` would qualify by
    /// first-param category, so only the export filter explains `_bisect`'s absence.
    #[test]
    fn stdlib_dot_candidates_exclude_underscore_helpers() {
        // `_bisect` must not appear ANYWHERE in the memoised candidate set.
        assert!(
            !STDLIB_DOT_CANDIDATES.iter().any(|c| c.name == "_bisect"),
            "`_bisect` leaked into the dot-candidate set"
        );
        // And no `_`-prefixed name at all (the defensive convention filter).
        assert!(
            !STDLIB_DOT_CANDIDATES.iter().any(|c| c.name.starts_with('_')),
            "an `_`-prefixed helper leaked into the dot-candidate set"
        );
        // Genuinely-exported array ops ARE still offered on an array receiver.
        let already = HashSet::new();
        let items = stdlib_dot_completion_items(STDLIB_DOT_CANDIDATES.iter(), "array", "", &already, &fixb_uri());
        assert!(item_named(&items, "push").is_some(), "exported `push` should still be offered");
        assert!(item_named(&items, "_bisect").is_none(), "private `_bisect` must not be offered");
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
        // `<`/`>` are comparison operators in source, NOT bracket depth: `x > b` is one arg, then
        // the cursor after the comma is in the SECOND argument.
        assert_eq!(top_level_commas("a > b, c"), 1);
        assert_eq!(top_level_commas("a < b, c"), 1);
        // `=>` (lambda arrow) must not be read as `>` opening/closing a bracket.
        assert_eq!(top_level_commas("x => x, y"), 1);
        // Commas inside a string literal are NOT argument separators (respecting `\"` escapes).
        assert_eq!(top_level_commas("\"a,b\", c"), 1);
        assert_eq!(top_level_commas("\"a,b,c\""), 0);
        assert_eq!(top_level_commas("\"a\\\",b\", c"), 1);
    }

    /// End to end: with a comparison in the first argument (`f(x > 0, y)`) and the cursor in the
    /// SECOND argument, the active parameter is index 1 — not skewed by the `>` operator.
    #[test]
    fn signature_help_active_param_unskewed_by_comparison() {
        let src = "val f = (a: Boolean, b: Int32) => b\nval r = f(1 > 0, 2)\n";
        let analysis = analyse(src, None);
        let call_open = src.rfind('(').unwrap();
        let cursor = call_open + src[call_open..].find('2').unwrap();
        let help = signature_help(src, &analysis, cursor, |_| None).expect("expected signature help");
        assert_eq!(help.signatures[0].active_parameter, Some(1), "comparison must not skew active param");
    }

    /// A lambda argument (`f(x => x, y)`) must not skew the active parameter via its `=>` arrow:
    /// cursor in the second arg is index 1.
    #[test]
    fn signature_help_active_param_unskewed_by_lambda() {
        let src = "val f = (g: (Int32) => Int32, b: Int32) => b\nval r = f(x => x, 9)\n";
        let analysis = analyse(src, None);
        let call_open = src.rfind('(').unwrap();
        let cursor = call_open + src[call_open..].find('9').unwrap();
        let help = signature_help(src, &analysis, cursor, |_| None).expect("expected signature help");
        assert_eq!(help.signatures[0].active_parameter, Some(1), "lambda arrow must not skew active param");
    }

    /// A comma inside a string-literal argument (`f("a,b", c)`) is not an argument separator: cursor
    /// in the second arg is index 1, not 2.
    #[test]
    fn signature_help_active_param_ignores_string_comma() {
        let src = "val f = (s: String, n: Int32) => n\nval r = f(\"a,b\", 7)\n";
        let analysis = analyse(src, None);
        let call_open = src.rfind('(').unwrap();
        let cursor = call_open + src[call_open..].find('7').unwrap();
        let help = signature_help(src, &analysis, cursor, |_| None).expect("expected signature help");
        assert_eq!(help.signatures[0].active_parameter, Some(1), "string comma must not advance active param");
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
        let help = signature_help(src, &analysis, cursor, |_| None).expect("expected signature help");
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
        assert!(signature_help(src, &analysis, 0, |_| None).is_none());
    }

    // ── Tier-2: code actions ────────────────────────────────────────────────────

    fn dummy_uri() -> Url {
        Url::parse("file:///tmp/lsp_test.lin").unwrap()
    }

    /// Apply a single `TextEdit` to `src`, returning the new document. Uses the same
    /// position→char→byte conversion discipline as production so the result matches what a
    /// client would compute.
    fn apply_text_edit(src: &str, edit: &TextEdit) -> String {
        let start = char_offset_to_byte(src, position_to_offset(src, edit.range.start));
        let end = char_offset_to_byte(src, position_to_offset(src, edit.range.end));
        let mut out = String::with_capacity(src.len() + edit.new_text.len());
        out.push_str(&src[..start]);
        out.push_str(&edit.new_text);
        out.push_str(&src[end..]);
        out
    }

    /// Build `CodeActionParams` requesting actions over the whole document, with the freshly
    /// analysed diagnostics supplied as context (what a real client would echo back).
    fn code_action_params(src: &str, diags: Vec<Diagnostic>) -> CodeActionParams {
        // `offset_to_position` takes a CHAR offset → end-of-document is the char count.
        let end = offset_to_position(src, src.chars().count());
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

    /// Cross-file goto-definition: B imports `foo` from A; a goto-def on the USE of `foo` in B must
    /// resolve to A's `foo` DECLARATION span (in A's source). This mirrors the handler's cross-file
    /// fallback (`resolve_symbol` → `decl_span` → owner file URI/source).
    #[test]
    fn cross_file_goto_definition_jumps_to_owner_decl() {
        let a = "export val foo = 1\n";
        let b = "import { foo } from \"a\"\nval x = foo + 1\n";
        let index = index_from(&[("/ws/a.lin", a), ("/ws/b.lin", b)]);
        let a_id = id_of("/ws/a.lin");
        let b_id = id_of("/ws/b.lin");

        // Cursor on the USE of `foo` in b.lin's body.
        let use_off = b.rfind("foo").unwrap();
        let (owner, name) = index
            .resolve_symbol(&b_id, use_off)
            .expect("imported use must resolve to its owner");
        assert_eq!(owner, a_id, "owner of `foo` is a.lin");
        assert_eq!(name, "foo");

        // The decl span points into A's source at A's `foo` declaration.
        let decl = decl_span(&index, &owner, &name).expect("owner declares foo");
        let owner_file = index.files.get(&owner).unwrap();
        assert_eq!(
            &owner_file.source[decl.start as usize..decl.end as usize],
            "foo",
            "decl span must cover `foo` in a.lin"
        );
        // And the target URI is A's file (not B's, and never a stdlib id).
        assert_eq!(module_id_to_uri(&owner), module_id_to_uri(&a_id));
        assert!(module_id_to_uri(&owner).is_some(), "owner has an on-disk URI");
    }

    /// Goto-def on an imported name owned by STDLIB yields no navigable target (stdlib has no
    /// on-disk URI) — the handler returns nothing, which is acceptable.
    #[test]
    fn cross_file_goto_definition_skips_stdlib_owner() {
        let b = "import { print } from \"std/io\"\nval x = print\n";
        let index = index_from(&[("/ws/b.lin", b)]);
        let b_id = id_of("/ws/b.lin");
        let use_off = b.rfind("print").unwrap();
        let (owner, _name) = index
            .resolve_symbol(&b_id, use_off)
            .expect("imported stdlib use resolves to std/io");
        assert!(owner.starts_with("std/"), "owner is a stdlib module: {}", owner);
        // No on-disk URI for stdlib → handler emits no Location.
        assert!(module_id_to_uri(&owner).is_none(), "stdlib owner must have no file URI");
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

    /// SOUND cross-file rename: an importing file that has BOTH `import { foo }` AND a shadowing
    /// local `val foo` (plus a comment and a string literal mentioning `foo`) must NOT have its
    /// shadow uses, comment, or string rewritten. Only the import-clause `foo` is renamed.
    #[test]
    fn cross_file_rename_does_not_rewrite_shadow_comment_or_string() {
        let a = "export val foo = 1\n";
        // b.lin imports `foo`, but a nested function locally re-binds `foo`. The body use of the
        // local `foo`, the `// foo` comment, and the `"foo"` string must all be left alone.
        let b = "import { foo } from \"a\"\n\
                 // foo is mentioned here\n\
                 val g = (foo: Int32) => foo + foo\n\
                 val s = \"foo bar\"\n";
        let index = index_from(&[("/ws/a.lin", a), ("/ws/b.lin", b)]);
        let a_id = id_of("/ws/a.lin");
        let b_id = id_of("/ws/b.lin");

        let edits = index.rename_edits(&a_id, "foo").expect("user export renameable");
        let a_edits: Vec<_> = edits.iter().filter(|(m, _)| m == &a_id).collect();
        let b_edits: Vec<_> = edits.iter().filter(|(m, _)| m == &b_id).collect();

        // a.lin: the single declaration site renames (a.lin doesn't shadow its own export).
        assert_eq!(a_edits.len(), 1, "owner decl renamed once: {:?}", edits);
        // b.lin: ONLY the import-clause token renames — the shadow `foo` param + its 2 body uses,
        // the comment, and the string are all excluded.
        assert_eq!(b_edits.len(), 1, "shadowed importer renames only the import token: {:?}", b_edits);
        let (_, span) = b_edits[0];
        assert_eq!(&b[span.start as usize..span.end as usize], "foo");
        // The renamed token must sit on the import line (line 0), never in the body/comment/string.
        assert!((span.start as usize) < b.find('\n').unwrap(), "edit must be in the import clause");
    }

    /// SOUND cross-file rename, non-shadow case: when the importer does NOT re-bind the name, body
    /// uses ARE renamed — but matches inside a comment or string literal are STILL excluded (token
    /// scan, not text scan). This is the regression guard for the comment/string over-match.
    #[test]
    fn cross_file_rename_excludes_comment_and_string_when_unshadowed() {
        let a = "export val foo = 1\n";
        // No local `foo` binding here — body uses rename, but the comment and string don't.
        let b = "import { foo } from \"a\"\n\
                 // call foo twice\n\
                 val s = \"foo literal\"\n\
                 val x = foo + foo\n";
        let index = index_from(&[("/ws/a.lin", a), ("/ws/b.lin", b)]);
        let b_id = id_of("/ws/b.lin");
        let a_id = id_of("/ws/a.lin");

        let edits = index.rename_edits(&a_id, "foo").expect("renameable");
        let b_edits: Vec<_> = edits.iter().filter(|(m, _)| m == &b_id).collect();
        // Import clause `foo` + the two real body uses = 3. The comment's `foo` and the string's
        // `foo` are NOT edited.
        assert_eq!(b_edits.len(), 3, "import token + 2 body uses, no comment/string: {:?}", b_edits);
        for (_, span) in &b_edits {
            let text = &b[span.start as usize..span.end as usize];
            assert_eq!(text, "foo");
            // No edit may fall inside the comment line or the string-literal line.
            let comment_line_start = b.find("// call").unwrap();
            let comment_line_end = b[comment_line_start..].find('\n').unwrap() + comment_line_start;
            assert!(
                !((span.start as usize) >= comment_line_start && (span.start as usize) < comment_line_end),
                "comment occurrence must not be renamed"
            );
            let str_start = b.find("\"foo literal\"").unwrap();
            assert!(
                (span.start as usize) < str_start || (span.start as usize) >= str_start + "\"foo literal\"".len(),
                "string-literal occurrence must not be renamed"
            );
        }
    }

    /// `file_rebinds_name` detects shadowing introductions (val/var/param/destructuring) but not a
    /// top-level import of the same name (that's the symbol we track, not a shadow).
    #[test]
    fn file_rebinds_name_detects_shadows_only() {
        assert!(file_rebinds_name("val foo = 1\n", "foo"), "top-level val shadows");
        assert!(file_rebinds_name("val g = (foo: Int32) => foo\n", "foo"), "param shadows");
        assert!(file_rebinds_name("val { foo } = bar\n", "foo"), "destructure shadows");
        assert!(!file_rebinds_name("import { foo } from \"a\"\nval x = foo\n", "foo"), "import is not a shadow");
        assert!(!file_rebinds_name("val x = foo + foo\n", "foo"), "mere uses are not a shadow");
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
        // A merge inserts `, printErr` (not a whole new import line); the inserted TEXT is
        // unchanged by the spacing fix.
        assert_eq!(edit.new_text, ", printErr");
        // Inserted on line 0 (the existing import line).
        assert_eq!(edit.range.start.line, 0);
        // The insertion lands right after the last binding (`print`), NOT at the `}` — so the
        // char immediately before the insertion point is `t` (end of `print`), preserving the
        // formatter's trailing space before `}`.
        let byte_at = char_offset_to_byte(src, position_to_offset(src, edit.range.start));
        assert_eq!(src.as_bytes()[byte_at - 1], b't');
        // Applying the edit yields a well-spaced merged line.
        let merged = apply_text_edit(src, &edit);
        assert_eq!(
            merged.lines().next().unwrap(),
            "import { print, printErr } from \"std/io\""
        );
    }

    /// The exact user-reported shape: MULTIPLE existing bindings with a trailing space before `}`.
    /// The merge must produce `{ a, b, c }` — no `b , c` and no `c}`.
    #[test]
    fn auto_import_edit_merges_multi_binding_preserves_spacing() {
        let src = "import { a, b } from \"std/iter\"\nval x = c(1)\n";
        let edit = auto_import_edit(src, "c", "std/iter").expect("expected a merge edit");
        assert_eq!(edit.new_text, ", c");
        let merged = apply_text_edit(src, &edit);
        assert_eq!(
            merged.lines().next().unwrap(),
            "import { a, b, c } from \"std/iter\""
        );
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

    /// Completion suppression: the cursor inside a normal string literal or after `//` on a comment
    /// line is NOT a code position, so completion must be suppressed there — but normal code
    /// positions (and the `${ … }` interpolation hole) stay enabled.
    #[test]
    fn in_string_or_comment_detects_strings_and_comments() {
        // Inside a normal string literal.
        let src = "val s = \"hello wor";
        assert!(in_string_or_comment(src, src.len()), "cursor inside a string");
        // String closed before the cursor → code position again.
        let src = "val s = \"hi\" + x";
        assert!(!in_string_or_comment(src, src.len()), "after a closed string is code");
        // `\"` escape does not close the string.
        let src = "val s = \"a\\\" b ";
        assert!(in_string_or_comment(src, src.len()), "escaped quote keeps us in the string");
        // After `//` on a comment line.
        let src = "val x = 1 // note he";
        assert!(in_string_or_comment(src, src.len()), "after // is a comment");
        // Plain code position.
        let src = "val foo = ba";
        assert!(!in_string_or_comment(src, src.len()), "bare code is not string/comment");
        // Inside a `${ … }` interpolation hole IS code position (completion stays on).
        let src = "val s = \"x ${ ba";
        assert!(!in_string_or_comment(src, src.len()), "inside ${{}} hole is code");
        // After a closed interpolation, back inside the string body.
        let src = "val s = \"x ${y} mor";
        assert!(in_string_or_comment(src, src.len()), "after a closed ${{}} we're back in the string");
        // A `//` INSIDE a string is not a comment.
        let src = "val s = \"http://ex";
        assert!(in_string_or_comment(src, src.len()), "// inside a string is string, not comment");
    }

    // SCOPE-AWARENESS (FIX 4b) is intentionally NOT implemented: the completion handler still offers
    // every binding in `span_type_map` regardless of lexical scope. Building a correct scope model
    // off the flat span map risks DROPPING valid in-scope bindings (worse than over-offering), so per
    // the task brief we leave the binding list as-is and document the limitation here rather than
    // half-implement it. The clear correctness win — suppressing completion inside strings/comments
    // (FIX 4a) — is implemented and tested above (`in_string_or_comment_detects_strings_and_comments`).

    // ── signature help with parameter names (feature 13) ──────────────────────────

    /// Same-file callee bound to a function literal: signature help renders `name: Type`
    /// for each parameter (names pulled from the binding AST, no checker change).
    #[test]
    fn signature_help_includes_param_names() {
        let src = "val add = (a: Int32, b: Int32) => a + b\nval r = add(1, 2)\n";
        let analysis = analyse(src, None);
        let call_open = src.rfind('(').unwrap();
        let cursor = call_open + 1;
        let help = signature_help(src, &analysis, cursor, |_| None).expect("expected signature help");
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

    /// An astral character (an emoji: 1 `char` / 2 UTF-16 code units / 4 bytes) pins BOTH axes of the
    /// conversion contract simultaneously: the INPUT to `offset_to_position` is a CHAR offset (the
    /// space all `lin_common::Span` offsets live in — 1 per emoji), while the OUTPUT column is in
    /// UTF-16 units (2 per emoji). `position_to_offset` is the inverse and returns a CHAR offset.
    /// FAILS ON OLD CODE: the old `offset_to_position` compared a byte index against the offset, so
    /// the char-offset input (10 for `a`) stopped after the 4-byte emoji and reported the wrong
    /// column; the old `position_to_offset` returned the byte offset of `a` (13), not its char
    /// offset (10).
    #[test]
    fn position_encoding_is_utf16_for_astral_char() {
        // `val x = "😀ab"` — the emoji is U+1F600 (1 char, 4 bytes, 2 UTF-16 units).
        let src = "val x = \"😀ab\"\n";
        assert_eq!('😀'.len_utf16(), 2);

        // CHAR offsets: `val x = "` is 9 ASCII chars, so the emoji is at char 9 and `a` at char 10.
        let emoji_char = 9usize;
        let a_char = 10usize;

        // Column at the emoji's start (char 9 → col 9, all preceding chars are 1 UTF-16 unit).
        let pos_emoji = offset_to_position(src, emoji_char);
        assert_eq!(pos_emoji, Position { line: 0, character: 9 });

        // `a` follows the emoji: its UTF-16 column must be 9 + 2 = 11 (NOT 10), since the emoji is
        // 2 UTF-16 units. Its CHAR offset is only 10.
        let pos_a = offset_to_position(src, a_char);
        assert_eq!(pos_a, Position { line: 0, character: 11 }, "emoji must count as 2 UTF-16 units");

        // Round-trip in CHAR-offset space: col 11 → char 10 (`a`), col 9 → char 9 (emoji).
        assert_eq!(position_to_offset(src, pos_a), a_char);
        assert_eq!(position_to_offset(src, pos_emoji), emoji_char);

        // A position landing inside the surrogate pair (col 10, only sendable by a malformed client)
        // must NOT panic and must round forward to the next char boundary — `a` at char 10.
        let mid = position_to_offset(src, Position { line: 0, character: 10 });
        assert_eq!(mid, a_char, "mid-surrogate column rounds forward to the next char boundary");
    }

    /// A BMP multibyte char (`é`: 1 char / 1 UTF-16 unit / 2 bytes) BEFORE an identifier pins the
    /// INPUT-space fix on its own (no astral/UTF-16 confound). FAILS ON OLD CODE: with `é` (2 bytes)
    /// before `x`, the identifier's CHAR offset (e.g. 11) fed to the old byte-comparing
    /// `offset_to_position` overshot by 1 byte and reported column 12; `position_to_offset` returned
    /// the byte offset (12), not the char offset (11). Both now agree on char offset 11 / col 11.
    #[test]
    fn position_offset_round_trips_with_leading_bmp_multibyte() {
        // `val s = "é"; xé` — the second line is `xé` (a real `xé` identifier doesn't matter; we
        // only assert the conversion). The identifier `y` sits after a leading `é`.
        let src = "val é = 1\nval y = é\n";
        // The `é` on line 0 is at char offset 4 (`val ` = 4 chars). Its column is 4 (1 UTF-16 unit).
        let e_char = 4usize;
        assert_eq!(offset_to_position(src, e_char), Position { line: 0, character: 4 });
        assert_eq!(position_to_offset(src, Position { line: 0, character: 4 }), e_char);

        // The `é` use on line 1 follows `val y = ` (8 chars on that line). Its CHAR offset within the
        // whole source: line 0 is "val é = 1\n" = 10 chars, then 8 chars on line 1 → char 18.
        let line0_chars = "val é = 1\n".chars().count();
        let use_char = line0_chars + "val y = ".chars().count();
        // It's the 9th char (col 8) of line 1, all preceding chars on the line are 1 UTF-16 unit.
        let pos = offset_to_position(src, use_char);
        assert_eq!(pos, Position { line: 1, character: 8 });
        assert_eq!(position_to_offset(src, pos), use_char, "char offset must round-trip through a leading multibyte char");
    }

    /// End-to-end through `span_to_range`: a span over an identifier on a line that begins with a
    /// multibyte char must produce the correct UTF-16 Range. FAILS ON OLD CODE: the old byte-based
    /// `offset_to_position` would shift the start/end columns by the extra bytes of the leading `é`.
    #[test]
    fn span_to_range_correct_after_leading_multibyte() {
        // Line 0: `é = 1` — `é` is 1 char (2 bytes). The identifier `é` is the whole word at char 0.
        // Build a span over the identifier `ab` that starts after a leading `é `.
        let src = "// é\nval ab = 1\n";
        // The identifier `ab` is on line 1. CHAR offsets: line 0 "// é\n" = 5 chars; "val " = 4 →
        // `ab` starts at char 9 and ends at char 11.
        let line0 = "// é\n".chars().count();
        let ab_start = line0 + "val ".chars().count();
        let ab_end = ab_start + 2;
        // Sanity: that char range really is `ab` (slice by byte to confirm).
        let bs = char_offset_to_byte(src, ab_start);
        let be = char_offset_to_byte(src, ab_end);
        assert_eq!(&src[bs..be], "ab");

        let span = lin_common::Span::new(0, ab_start as u32, ab_end as u32);
        let range = span_to_range(src, span);
        // `ab` is on line 1 at columns 4..6 (the leading `é` on line 0 doesn't affect line 1).
        assert_eq!(range.start, Position { line: 1, character: 4 });
        assert_eq!(range.end, Position { line: 1, character: 6 });
    }

    /// Span-driven byte-offset slicing must never panic on multibyte input or stale/out-of-range
    /// offsets — they now go through `.get(..)` rather than raw `source[a..b]` indexing. We feed
    /// content with an emoji (4-byte char) through the slice sites and assert they return gracefully.
    #[test]
    fn span_slicing_is_panic_safe_on_multibyte_and_stale_offsets() {
        let src = "import { foo } from \"😀\"\nval r = foo(😀, 2)\n";

        // `find_name_in_import` with an out-of-range start must return None, not panic.
        assert_eq!(find_name_in_import(src, src.len() + 100, "foo"), None);
        // A start that lands mid-emoji (not a char boundary) must not panic.
        let emoji = src.find('😀').unwrap();
        let _ = find_name_in_import(src, emoji + 1, "foo"); // any result is fine; must not panic.

        // `matching_paren` over a `.get` slice from a stale open index must not panic.
        assert_eq!("(a, b)".get(0..).and_then(matching_paren), Some(5));

        // Full signature_help pass over multibyte source at a cursor inside the emoji span: must
        // produce some result (Some or None) without panicking.
        let analysis = analyse(src, None);
        let cursor = emoji + 1; // mid-surrogate / mid-utf8 byte offset
        let _ = signature_help(src, &analysis, cursor, |_| None);

        // Completion at a multibyte offset must not panic either. `position_to_offset` now returns a
        // CHAR offset, so it's bounded by the char count (not the byte length).
        let _ = analyse(src, None);
        let off = position_to_offset(src, Position { line: 1, character: 8 });
        assert!(off <= src.chars().count());
    }

    /// The poison-tolerant lock idiom (`unwrap_or_else(|e| e.into_inner())`) must recover access
    /// after a thread panics while holding the guard, instead of cascading the poison to every later
    /// acquisition (which a plain `.unwrap()` would do, bricking the server for the session).
    #[test]
    fn poisoned_lock_idiom_recovers_inner() {
        use std::sync::{Arc, RwLock};
        let lock = Arc::new(RwLock::new(7u32));
        // Poison the lock by panicking while holding the write guard on another thread.
        let l2 = Arc::clone(&lock);
        let _ = std::thread::spawn(move || {
            let mut g = l2.write().unwrap_or_else(|e| e.into_inner());
            *g = 42;
            panic!("poison the lock");
        })
        .join();
        assert!(lock.is_poisoned(), "lock should be poisoned after the panicking thread");
        // The idiom still yields the (last-written) inner value — no cascade.
        let v = *lock.read().unwrap_or_else(|e| e.into_inner());
        assert_eq!(v, 42, "poison-tolerant read recovers the inner value");
    }

    // ── doc comments (extract / parse / render) ─────────────────────────────────

    /// `extract_doc` collects the CONTIGUOUS leading own-line block above a decl, ignoring a comment
    /// separated by a blank line and trailing (same-line) comments.
    #[test]
    fn extract_doc_takes_only_the_contiguous_leading_block() {
        let src = concat!(
            "// detached header line\n",
            "\n",
            "// first doc line\n",
            "// @param x  the input\n",
            "val f = (x: Int32) => x   // trailing not part of block\n",
        );
        let module = parse(src);
        let span = local_decl_name_span(&module, "f").expect("f decl");
        let doc = extract_doc(src, span).expect("doc block");
        // The blank-separated header is NOT included; only the two contiguous lines are.
        assert_eq!(doc.description, vec!["first doc line".to_string()]);
        assert_eq!(doc.params, vec![("x".to_string(), "the input".to_string())]);
        // Trailing same-line comment (own_line == false) is never collected.
        assert!(doc.returns.is_none());
        assert!(doc.examples.is_empty());
    }

    /// A `── … ──` section banner directly above a decl is NOT that decl's doc — it terminates the
    /// leading block (so a banner-only run yields no doc).
    #[test]
    fn extract_doc_stops_at_section_banner() {
        let src = concat!(
            "// ── Arithmetic ──────────────\n",
            "export val add = (a: Int32, b: Int32) => a + b\n",
        );
        let module = parse(src);
        let span = local_decl_name_span(&module, "add").expect("add decl");
        assert!(extract_doc(src, span).is_none(), "a banner is not the decl's doc");
    }

    /// Parsing splits prose / @param / @returns / @example into the structured `DocComment`, and a
    /// doc with only prose carries an empty params/returns/examples.
    #[test]
    fn parse_doc_block_separates_tags_and_handles_prose_only() {
        let lines: Vec<String> = vec![
            "Build the ascending integer sequence.".to_string(),
            "@param start  the first value (inclusive).".to_string(),
            "@param end    the upper bound (exclusive).".to_string(),
            "@returns an `Int32[]` of the range.".to_string(),
            "@example range(0, 5)".to_string(),
            "@example range(1, 6).map(i => i * i)".to_string(),
        ];
        let doc = parse_doc_block(&lines);
        assert_eq!(doc.description, vec!["Build the ascending integer sequence.".to_string()]);
        assert_eq!(doc.params.len(), 2);
        assert_eq!(doc.params[0], ("start".to_string(), "the first value (inclusive).".to_string()));
        assert_eq!(doc.params[1], ("end".to_string(), "the upper bound (exclusive).".to_string()));
        assert_eq!(doc.returns.as_deref(), Some("an `Int32[]` of the range."));
        assert_eq!(doc.examples.len(), 2);
        assert_eq!(doc.examples[1], "range(1, 6).map(i => i * i)");

        // Prose-only block: no tags.
        let prose = parse_doc_block(&["Just a description.".to_string()]);
        assert_eq!(prose.description, vec!["Just a description.".to_string()]);
        assert!(prose.params.is_empty());
        assert!(prose.returns.is_none());
        assert!(prose.examples.is_empty());
    }

    /// A compact `@param name desc @returns x` line splits into a param entry AND a returns entry,
    /// and a mid-line `@returns` on a prose line splits prose from the tag (mirrors gen-stdlib).
    #[test]
    fn parse_doc_block_splits_compact_returns() {
        let doc = parse_doc_block(&["@param a  first @returns the sum".to_string()]);
        assert_eq!(doc.params, vec![("a".to_string(), "first".to_string())]);
        assert_eq!(doc.returns.as_deref(), Some("the sum"));

        let doc2 = parse_doc_block(&["Sum of a and b. @returns a + b.".to_string()]);
        assert_eq!(doc2.description, vec!["Sum of a and b.".to_string()]);
        assert_eq!(doc2.returns.as_deref(), Some("a + b."));
    }

    /// `render_doc_markdown` produces the expected shape: prose paragraph, a **Parameters** bullet
    /// list, a **Returns** line, and ```lin example fences.
    #[test]
    fn render_doc_markdown_shape() {
        let doc = DocComment {
            description: vec!["Run f over every item.".to_string()],
            params: vec![
                ("iterable".to_string(), "any Array, Iterator, or Stream.".to_string()),
                ("f".to_string(), "callback `(item, index?) => …`.".to_string()),
            ],
            returns: Some("`null`.".to_string()),
            examples: vec!["[1, 2, 3].for(x => print(x))".to_string()],
        };
        let md = render_doc_markdown(&doc);
        assert!(md.contains("Run f over every item."), "prose: {md}");
        assert!(md.contains("**Parameters**"), "params header: {md}");
        assert!(md.contains("- `iterable` — any Array, Iterator, or Stream."), "param list: {md}");
        assert!(md.contains("- `f` — callback `(item, index?) => …`."), "param list: {md}");
        assert!(md.contains("**Returns** `null`."), "returns: {md}");
        assert!(md.contains("**Example**\n```lin\n[1, 2, 3].for(x => print(x))\n```"), "example fence: {md}");
        // An empty doc renders to nothing.
        assert_eq!(render_doc_markdown(&DocComment::default()), "");
    }

    /// Cross-file/stdlib-style: module A exports a documented `foo`; resolving foo's doc from an
    /// importing file B (via the index, mirroring hover/completion) returns A's doc text.
    #[test]
    fn resolve_doc_via_index_finds_imported_symbol_doc() {
        let a = concat!(
            "// Double the input.\n",
            "// @param n  the number to double.\n",
            "// @returns twice n.\n",
            "export val foo = (n: Int32) => n * 2\n",
        );
        let b = "import { foo } from \"a\"\nval x = foo(21)\n";
        let index = index_from(&[("/ws/a.lin", a), ("/ws/b.lin", b)]);
        let b_id = id_of("/ws/b.lin");

        // Resolve foo's doc as seen from the importing file B.
        let doc = resolve_doc_via_index(&index, &b_id, "foo").expect("imported doc");
        assert_eq!(doc.description, vec!["Double the input.".to_string()]);
        assert_eq!(doc.params, vec![("n".to_string(), "the number to double.".to_string())]);
        assert_eq!(doc.returns.as_deref(), Some("twice n."));

        // And from A itself (its own export).
        let a_id = id_of("/ws/a.lin");
        let doc_self = resolve_doc_via_index(&index, &a_id, "foo").expect("own-export doc");
        assert_eq!(doc_self.returns.as_deref(), Some("twice n."));
    }

    /// Regression: lexer/parser spans are CHAR offsets, not byte offsets. The stdlib `std/iter`
    /// source has multibyte chars (box-drawing banners, ellipses) BEFORE the `range` declaration, so
    /// a byte/char mix-up would mis-locate the leading block and drop the prose. Extract `range`'s
    /// doc from the real stdlib source and assert the prose + tags are all captured.
    #[test]
    fn extract_doc_char_offset_correct_on_multibyte_stdlib_source() {
        let src = stdlib_source("std/iter").expect("std/iter source");
        let module = parse(src);
        let span = local_decl_name_span(&module, "range").expect("range decl");
        let doc = extract_doc(src, span).expect("range doc");
        // Prose line (the multibyte-bearing description) must be present, not dropped.
        assert!(
            doc.description_text().contains("ascending integer sequence"),
            "prose dropped (byte/char span mismatch?): {:?}",
            doc.description
        );
        assert!(doc.params.iter().any(|(n, _)| n == "start"));
        assert!(doc.params.iter().any(|(n, _)| n == "end"));
        assert!(doc.returns.is_some());
        assert_eq!(doc.examples.len(), 2, "both @example lines captured");
    }

    /// Signature-help param-doc matching: each `@param` description lands on the right parameter
    /// (matched by name), and the function description annotates the whole signature.
    #[test]
    fn signature_help_attaches_param_docs_by_name() {
        let src = concat!(
            "// Add two numbers.\n",
            "// @param a  the first addend.\n",
            "// @param b  the second addend.\n",
            "// @returns the sum.\n",
            "val add = (a: Int32, b: Int32) => a + b\n",
            "val r = add(1, 2)\n",
        );
        let analysis = analyse(src, None);
        let call_open = src.rfind('(').unwrap();
        let cursor = call_open + 1; // inside the args, on the first parameter.
        // Resolve the doc from the same source (local decl).
        let module = analysis.module.clone();
        let resolve = |name: &str| -> Option<DocComment> {
            local_decl_name_span(&module, name).and_then(|s| extract_doc(src, s))
        };
        let help = signature_help(src, &analysis, cursor, resolve).expect("signature help");
        let sig = &help.signatures[0];
        let params = sig.parameters.as_ref().expect("params");
        assert_eq!(params.len(), 2);

        let doc_text = |p: &ParameterInformation| match &p.documentation {
            Some(Documentation::MarkupContent(m)) => m.value.clone(),
            Some(Documentation::String(s)) => s.clone(),
            None => String::new(),
        };
        assert_eq!(doc_text(&params[0]), "the first addend.");
        assert_eq!(doc_text(&params[1]), "the second addend.");
        // The signature itself carries the description + returns.
        let sig_doc = match &sig.documentation {
            Some(Documentation::MarkupContent(m)) => m.value.clone(),
            _ => String::new(),
        };
        assert!(sig_doc.contains("Add two numbers."), "sig doc: {sig_doc}");
        assert!(sig_doc.contains("**Returns** the sum."), "sig doc: {sig_doc}");
    }

    /// The completion-resolve `data` key round-trips losslessly through serialization.
    #[test]
    fn completion_resolve_data_round_trips() {
        let uri = Url::parse("file:///ws/b.lin").unwrap();
        let data = completion_resolve_data(&uri, "foo");
        let (back_uri, back_name) = parse_completion_resolve_data(data.as_ref()).expect("round-trip");
        assert_eq!(back_uri, uri);
        assert_eq!(back_name, "foo");
        // Absent/malformed payloads yield None (the item resolves to itself).
        assert!(parse_completion_resolve_data(None).is_none());
        assert!(parse_completion_resolve_data(Some(&serde_json::json!({ "uri": "file:///x" }))).is_none());
    }

    /// CJK characters are in the BMP (1 UTF-16 unit, 3 bytes, 1 `char`): they advance the column by
    /// 1. This pins that BMP-multibyte text is unaffected by the UTF-16 fix (only astral codepoints
    /// differ from a naive char count). Offsets here are CHAR offsets (the canonical span space).
    /// FAILS ON OLD CODE: the old byte-based `offset_to_position` shifted columns by the extra bytes
    /// of each 3-byte CJK char.
    #[test]
    fn position_encoding_bmp_multibyte_is_one_unit() {
        // `日本` — each is 3 bytes, 1 UTF-16 unit, 1 char.
        let src = "val s = \"日本\"\n";
        // CHAR offsets: `val s = "` is 9 ASCII chars → `日` at char 9, `本` at char 10.
        let first = 9usize;
        assert_eq!(offset_to_position(src, first), Position { line: 0, character: 9 });
        let second = 10usize;
        // Second CJK char is at col 10 (one UTF-16 unit past the first).
        assert_eq!(offset_to_position(src, second), Position { line: 0, character: 10 });
        assert_eq!(position_to_offset(src, Position { line: 0, character: 10 }), second);
    }
}

