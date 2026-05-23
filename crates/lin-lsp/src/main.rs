use std::collections::HashMap;
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

        // Work out the partial word the cursor is inside.
        let prefix = word_before(&source, offset);

        let mut items: Vec<CompletionItem> = Vec::new();

        // 1. Bindings visible at the cursor (from span_type_map def_spans).
        for (_, ty_str, def_span) in &analysis.span_type_map {
            if let Some(ds) = def_span {
                // Extract the identifier text from the source at the def_span.
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

        // 4. Stdlib exports — useful when typing import paths.
        let stdlib_exports = stdlib_completion_items();
        for item in stdlib_exports {
            if item.label.starts_with(prefix) {
                items.push(item);
            }
        }

        Ok(Some(CompletionResponse::Array(items)))
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

    // Pre-resolve imports so hover and type errors are accurate for imported names.
    let mut imported: HashMap<String, TypedModule> = HashMap::new();
    let effective_base = base_dir
        .map(|p| p.to_path_buf())
        .or_else(|| WORKSPACE_ROOT.read().unwrap().clone())
        .unwrap_or_else(|| PathBuf::from("."));

    pre_resolve_imports(&module, &effective_base, &mut imported);

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

    Analysis {
        diagnostics: diags,
        span_type_map: checker.span_type_map,
    }
}

// ── import resolution (mirrors lin-compile logic) ────────────────────────────

fn stdlib_source(path: &str) -> Option<&'static str> {
    match path {
        "std/io"     => Some(include_str!("../../../stdlib/io.lin")),
        "std/string" => Some(include_str!("../../../stdlib/string.lin")),
        "std/number" => Some(include_str!("../../../stdlib/number.lin")),
        "std/array"  => Some(include_str!("../../../stdlib/array.lin")),
        "std/iter"   => Some(include_str!("../../../stdlib/iter.lin")),
        "std/result" => Some(include_str!("../../../stdlib/result.lin")),
        _ => None,
    }
}

fn pre_resolve_imports(
    ast_module: &lin_parse::ast::Module,
    base_dir: &Path,
    cache: &mut HashMap<String, TypedModule>,
) {
    for stmt in &ast_module.statements {
        if let Stmt::Import { path, .. } = stmt {
            if cache.contains_key(path.as_str()) {
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

            // Recurse depth-first so transitive deps are ready.
            pre_resolve_imports(&ast_mod, &child_base, cache);

            let mut import_type_map: HashMap<(String, String), Type> = HashMap::new();
            for (dep_path, dep_module) in cache.iter() {
                for (name, ty) in extract_exports(dep_module) {
                    import_type_map.insert((dep_path.clone(), name), ty);
                }
            }

            let mut checker = Checker::new();
            checker.import_types = import_type_map;
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

// ── completion helpers ────────────────────────────────────────────────────────

fn stdlib_completion_items() -> Vec<CompletionItem> {
    let entries: &[(&str, &str, &str)] = &[
        // (label, detail, module)
        ("trim",        "(String) => String",  "std/string"),
        ("toUpper",     "(String) => String",  "std/string"),
        ("toLower",     "(String) => String",  "std/string"),
        ("substring",   "(String, Int32, Int32) => String", "std/string"),
        ("indexOf",     "(String, String) => Int32",        "std/string"),
        ("contains",    "(String, String) => Boolean",      "std/string"),
        ("startsWith",  "(String, String) => Boolean",      "std/string"),
        ("endsWith",    "(String, String) => Boolean",      "std/string"),
        ("split",       "(String, String) => String[]",     "std/string"),
        ("join",        "(String[], String) => String",     "std/string"),
        ("replace",     "(String, String, String) => String", "std/string"),
        ("charAt",      "(String, Int32) => String",        "std/string"),
        ("repeat",      "(String, Int32) => String",        "std/string"),
        ("parseInt32",  "(String) => Int32",   "std/number"),
        ("parseFloat64","(String) => Float64", "std/number"),
        ("toInt32",     "(Float64) => Int32",  "std/number"),
        ("toFloat64",   "(Int32) => Float64",  "std/number"),
        ("isInt32",     "(String) => Boolean", "std/number"),
        ("map",    "(Iterable<T>, (T) => U) => Iterator<U>",         "std/array"),
        ("filter", "(Iterable<T>, (T) => Boolean) => Iterator<T>",   "std/array"),
        ("reduce", "(Iterable<T>, U, (U, T) => U) => U",             "std/array"),
        ("find",   "(Iterable<T>, (T) => Boolean) => T | Null",      "std/array"),
        ("some",   "(Iterable<T>, (T) => Boolean) => Boolean",       "std/array"),
        ("every",  "(Iterable<T>, (T) => Boolean) => Boolean",       "std/array"),
        ("flatMap","(Iterable<T>, (T) => Iterable<U>) => Iterator<U>","std/array"),
        ("reverse","(T[]) => T[]",                                   "std/array"),
        ("range",  "(Int32, Int32) => Iterator<Int32>", "std/iter"),
        ("iterOf", "(T[]) => Iterator<T>",              "std/iter"),
        ("print",  "(T) => Null",                       "std/io"),
    ];

    entries
        .iter()
        .map(|(label, detail, module)| CompletionItem {
            label: label.to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some(detail.to_string()),
            documentation: Some(Documentation::String(
                format!("from {}", module),
            )),
            ..Default::default()
        })
        .collect()
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
