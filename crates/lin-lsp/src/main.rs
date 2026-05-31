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
                document_formatting_provider: Some(OneOf::Left(true)),
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

        // 4. Stdlib exports — with auto-import edits and optional dot-filtering.
        // In dot context with no category, pass Some("any") so all stdlib items show
        // but keywords/types/bindings are still suppressed.
        let filter_cat = if in_dot_context {
            Some(receiver_category.as_deref().unwrap_or("any"))
        } else {
            None
        };
        for item in stdlib_completion_items(&source, prefix, filter_cat) {
            items.push(item);
        }

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

        let tokens = lin_lex::Lexer::new(&source, 0).tokenize();
        let mut parser = lin_parse::Parser::new(tokens);
        let module = parser.parse_module();

        if !parser.diagnostics.is_empty() {
            // Don't format files with parse errors.
            return Ok(None);
        }

        let formatted = lin_parse::Formatter::new().format_module(&module);
        let end_pos = offset_to_position(&source, source.len());

        Ok(Some(vec![TextEdit {
            range: Range {
                start: Position { line: 0, character: 0 },
                end: end_pos,
            },
            new_text: formatted,
        }]))
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

    // Warn on unused imports.
    diags.extend(unused_import_warnings(source, &module));

    Analysis {
        diagnostics: diags,
        span_type_map: checker.span_type_map,
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

fn stdlib_source(path: &str) -> Option<&'static str> {
    match path {
        "std/io"       => Some(include_str!("../../../stdlib/io.lin")),
        "std/string"   => Some(include_str!("../../../stdlib/string.lin")),
        "std/number"   => Some(include_str!("../../../stdlib/number.lin")),
        "std/array"    => Some(include_str!("../../../stdlib/array.lin")),
        "std/object"   => Some(include_str!("../../../stdlib/object.lin")),
        "std/fs"       => Some(include_str!("../../../stdlib/fs.lin")),
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

// ── auto-import helpers ───────────────────────────────────────────────────────

/// Returns true if `name` is already imported from `module` in `source`.
fn already_imported(source: &str, name: &str, module: &str) -> bool {
    let from_pattern = format!("from \"{}\"", module);
    for line in source.lines() {
        if !line.contains(&from_pattern) {
            continue;
        }
        if let (Some(open), Some(close)) = (line.find('{'), line.rfind('}')) {
            for n in line[open + 1..close].split(',') {
                if n.trim() == name {
                    return true;
                }
            }
        }
    }
    false
}

/// Builds `additionalTextEdits` to ensure `name` is imported from `module`.
/// Returns `None` if the import is already present (no edit needed).
fn make_import_edit(source: &str, name: &str, module: &str) -> Option<Vec<TextEdit>> {
    if already_imported(source, name, module) {
        return None;
    }

    let from_pattern = format!("from \"{}\"", module);

    // Module already imported but this specific name is missing — insert into existing braces.
    for (line_idx, line) in source.lines().enumerate() {
        if line.contains(&from_pattern) {
            if let Some(brace_pos) = line.rfind('}') {
                let insert_pos = Position {
                    line: line_idx as u32,
                    character: brace_pos as u32,
                };
                return Some(vec![TextEdit {
                    range: Range { start: insert_pos, end: insert_pos },
                    new_text: format!(", {}", name),
                }]);
            }
        }
    }

    // Module not imported at all — prepend a new import line at the top.
    Some(vec![TextEdit {
        range: Range {
            start: Position { line: 0, character: 0 },
            end: Position { line: 0, character: 0 },
        },
        new_text: format!("import {{ {} }} from \"{}\"\n", name, module),
    }])
}

// ── completion helpers ────────────────────────────────────────────────────────

/// Returns stdlib completion items filtered by `prefix` and optionally by `receiver_category`.
/// Each item carries `additionalTextEdits` to auto-insert its import if absent.
///
/// `receiver_category` is `Some("array"|"string"|"number"|"object"|"any")` when in dot context.
/// When `None` (not in dot context), all items are returned (filtered only by prefix).
fn stdlib_completion_items(
    source: &str,
    prefix: &str,
    receiver_category: Option<&str>,
) -> Vec<CompletionItem> {
    // (label, detail, module, category)
    // category: "array" | "string" | "number" | "object" | "any"
    let entries: &[(&str, &str, &str, &str)] = &[
        // std/string — category "string"
        ("trim",          "(String) => String",                       "std/string", "string"),
        ("trimStart",     "(String) => String",                       "std/string", "string"),
        ("trimEnd",       "(String) => String",                       "std/string", "string"),
        ("toUpper",       "(String) => String",                       "std/string", "string"),
        ("toLower",       "(String) => String",                       "std/string", "string"),
        ("substring",     "(String, Int32, Int32) => String",         "std/string", "string"),
        ("indexOf",       "(String, String) => Int32",                "std/string", "string"),
        ("lastIndexOf",   "(String, String) => Int32",                "std/string", "string"),
        ("isBlank",       "(String) => Boolean",                      "std/string", "string"),
        ("contains",      "(String, String) => Boolean",              "std/string", "string"),
        ("startsWith",    "(String, String) => Boolean",              "std/string", "string"),
        ("endsWith",      "(String, String) => Boolean",              "std/string", "string"),
        ("split",         "(String, String) => String[]",             "std/string", "string"),
        ("replace",       "(String, String, String) => String",       "std/string", "string"),
        ("replaceAll",    "(String, String, String) => String",       "std/string", "string"),
        ("charAt",        "(String, Int32) => String",                "std/string", "string"),
        ("at",            "(String, Int32) => String",                "std/string", "string"),
        ("repeat",        "(String, Int32) => String",                "std/string", "string"),
        ("padStart",      "(String, Int32, String) => String",        "std/string", "string"),
        ("padEnd",        "(String, Int32, String) => String",        "std/string", "string"),
        ("lines",         "(String) => String[]",                     "std/string", "string"),
        ("charCode",      "(String, Int32) => Int32",                 "std/string", "string"),
        ("toString",      "(Json) => String",                         "std/string", "any"),
        // std/number — category "number"
        ("parseInt32",    "(String) => Int32",                        "std/number", "number"),
        ("parseFloat64",  "(String) => Float64",                      "std/number", "number"),
        ("toInt32",       "(Float64) => Int32",                       "std/number", "number"),
        ("toFloat64",     "(Int32) => Float64",                       "std/number", "number"),
        ("isInt32",       "(String) => Boolean",                      "std/number", "number"),
        // std/array — category "array"
        ("map",           "(Json, (Json) => Json) => Json",           "std/array",  "array"),
        ("filter",        "(Json, (Json) => Boolean) => Json",        "std/array",  "array"),
        ("reduce",        "(Json, Json, (Json, Json) => Json) => Json","std/array", "array"),
        ("find",          "(Json, (Json) => Boolean) => Json",        "std/array",  "array"),
        ("some",          "(Json, (Json) => Boolean) => Boolean",     "std/array",  "array"),
        ("every",         "(Json, (Json) => Boolean) => Boolean",     "std/array",  "array"),
        ("flatMap",       "(Json, (Json) => Json) => Json",           "std/array",  "array"),
        ("reverse",       "(Json) => Json",                           "std/array",  "array"),
        ("indexOf",       "(Json, Json) => Int32",                    "std/array",  "array"),
        ("length",        "(Json) => Int32",                          "std/array",  "array"),
        ("push",          "(Json, Json) => Null",                     "std/array",  "array"),
        ("concat",        "(Json, Json) => Json",                     "std/array",  "array"),
        ("take",          "(Json, Int32) => Json",                    "std/array",  "array"),
        ("drop",          "(Json, Int32) => Json",                    "std/array",  "array"),
        ("takeWhile",     "(Json, (Json) => Boolean) => Json",        "std/array",  "array"),
        ("dropWhile",     "(Json, (Json) => Boolean) => Json",        "std/array",  "array"),
        ("flatten",       "(Json) => Json",                           "std/array",  "array"),
        ("chunk",         "(Json, Int32) => Json",                    "std/array",  "array"),
        ("partition",     "(Json, (Json) => Boolean) => Json",        "std/array",  "array"),
        ("zip",           "(Json, Json) => Json",                     "std/array",  "array"),
        ("unique",        "(Json) => Json",                           "std/array",  "array"),
        ("compact",       "(Json) => Json",                           "std/array",  "array"),
        ("at",            "(Json, Int32) => Json",                    "std/array",  "array"),
        ("set",           "(Json, Int32, Json) => Null",              "std/array",  "array"),
        ("join",          "(String[], String) => String",             "std/array",  "array"),
        ("range",         "(Int32, Int32) => Json",                   "std/array",  "array"),
        ("for",           "(Json, (Json) => Null) => Null",           "std/array",  "array"),
        // std/object — category "object"
        ("keys",          "(Json) => String[]",                       "std/object", "object"),
        ("values",        "(Json) => Json",                           "std/object", "object"),
        ("entries",       "(Json) => Json",                           "std/object", "object"),
        ("fromEntries",   "(Json) => Json",                           "std/object", "object"),
        ("merge",         "(Json, Json) => Json",                     "std/object", "object"),
        ("pick",          "(Json, String[]) => Json",                 "std/object", "object"),
        ("omit",          "(Json, String[]) => Json",                 "std/object", "object"),
        ("mapValues",     "(Json, (Json) => Json) => Json",           "std/object", "object"),
        ("isEmpty",       "(Json) => Boolean",                        "std/object", "object"),
        // std/io — category "any"
        ("print",         "(Json) => Null",                           "std/io",     "any"),
        ("readLine",      "() => String",                             "std/io",     "any"),
        ("readAll",       "() => String",                             "std/io",     "any"),
    ];

    entries
        .iter()
        .filter(|(label, _, _, category)| {
            // Prefix filter.
            if !label.starts_with(prefix) {
                return false;
            }
            // In dot context, filter by receiver type compatibility.
            if let Some(cat) = receiver_category {
                if cat == "any" {
                    // Unknown receiver type — show everything.
                    true
                } else {
                    // Known type — filter to matching category only.
                    // Items categorised "any" (like print) don't belong in dot context.
                    *category != "any" && *category == cat
                }
            } else {
                true
            }
        })
        .map(|(label, detail, module, _)| {
            let edits = make_import_edit(source, label, module)
                .map(|e| e);
            CompletionItem {
                label: label.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(detail.to_string()),
                documentation: Some(Documentation::String(format!("from {}", module))),
                additional_text_edits: edits,
                ..Default::default()
            }
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
