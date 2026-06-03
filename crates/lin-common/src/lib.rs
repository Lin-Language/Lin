pub mod tags;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub file_id: u32,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(file_id: u32, start: u32, end: u32) -> Self {
        Self { file_id, start, end }
    }

    pub fn dummy() -> Self {
        Self { file_id: 0, start: 0, end: 0 }
    }

    pub fn line_col(&self, source: &str) -> (usize, usize) {
        let offset = self.start as usize;
        let mut line = 1;
        let mut col = 1;
        for (i, ch) in source.char_indices() {
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
}

/// An explicit numeric type suffix on a literal (e.g. `42i8`, `3.14f32`, `5u64`).
/// Carried from the lexer through the surface AST so the type checker can pin the
/// literal's type, overriding context/default inference (spec §2.6). `None` ⇒ no suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumSuffix {
    I8, I16, I32, I64,
    U8, U16, U32, U64,
    F32, F64,
}

impl NumSuffix {
    /// Parse the suffix letters after the digits (e.g. "i64", "u8", "f32").
    /// Returns `None` for an unrecognised suffix.
    pub fn parse(s: &str) -> Option<NumSuffix> {
        match s {
            "i8" => Some(NumSuffix::I8),
            "i16" => Some(NumSuffix::I16),
            "i32" => Some(NumSuffix::I32),
            "i64" => Some(NumSuffix::I64),
            "u8" => Some(NumSuffix::U8),
            "u16" => Some(NumSuffix::U16),
            "u32" => Some(NumSuffix::U32),
            "u64" => Some(NumSuffix::U64),
            "f32" => Some(NumSuffix::F32),
            "f64" => Some(NumSuffix::F64),
            _ => None,
        }
    }

    /// True for `f32`/`f64`.
    pub fn is_float(self) -> bool {
        matches!(self, NumSuffix::F32 | NumSuffix::F64)
    }
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
    pub severity: Severity,
    /// Secondary spans with contextual messages (e.g. "type defined here").
    pub notes: Vec<(Span, String)>,
    /// Optional suggestion text shown below the error.
    pub help: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Diagnostic {
    pub fn error(span: Span, message: impl Into<String>) -> Self {
        Self { span, message: message.into(), severity: Severity::Error, notes: Vec::new(), help: None }
    }

    pub fn warning(span: Span, message: impl Into<String>) -> Self {
        Self { span, message: message.into(), severity: Severity::Warning, notes: Vec::new(), help: None }
    }

    pub fn with_note(mut self, span: Span, message: impl Into<String>) -> Self {
        self.notes.push((span, message.into()));
        self
    }

    pub fn with_help(mut self, message: impl Into<String>) -> Self {
        self.help = Some(message.into());
        self
    }

    /// Render this diagnostic to stderr using ariadne with source context.
    /// `file_name` is the display name shown in the report header.
    /// `source` is the full source text for span resolution.
    pub fn render(&self, file_name: &str, source: &str) {
        use ariadne::{Color, ColorGenerator, Label, Report, ReportKind, Source};

        let kind = match self.severity {
            Severity::Error => ReportKind::Error,
            Severity::Warning => ReportKind::Warning,
        };

        let mut colors = ColorGenerator::new();
        let primary_color = match self.severity {
            Severity::Error => Color::Red,
            Severity::Warning => Color::Yellow,
        };

        let start = self.span.start as usize;
        let end = (self.span.end as usize).max(start + 1);

        let mut report = Report::build(kind, (file_name, start..end))
            .with_message(&self.message)
            .with_label(
                Label::new((file_name, start..end))
                    .with_message(&self.message)
                    .with_color(primary_color),
            );

        for (note_span, note_msg) in &self.notes {
            let ns = note_span.start as usize;
            let ne = (note_span.end as usize).max(ns + 1);
            report = report.with_label(
                Label::new((file_name, ns..ne))
                    .with_message(note_msg)
                    .with_color(colors.next()),
            );
        }

        if let Some(ref help) = self.help {
            report = report.with_help(help);
        }

        report
            .finish()
            .eprint((file_name, Source::from(source)))
            .unwrap_or_else(|_| {
                // Fallback: plain text if ariadne can't render.
                let (line, col) = self.span.line_col(source);
                eprintln!("{}:{}:{}: {:?}: {}", file_name, line, col, self.severity, self.message);
            });
    }
}

// -------------------------------------------------------------------------
// Edit distance & suggestions
// -------------------------------------------------------------------------

/// Wagner-Fischer edit distance between two strings.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 0..=m { dp[i][0] = i; }
    for j in 0..=n { dp[0][j] = j; }
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if a[i - 1] == b[j - 1] {
                dp[i - 1][j - 1]
            } else {
                1 + dp[i - 1][j].min(dp[i][j - 1]).min(dp[i - 1][j - 1])
            };
        }
    }
    dp[m][n]
}

/// Return the closest match from `candidates` to `name` if within `max_dist`, else `None`.
pub fn closest_match<'a>(name: &str, candidates: impl Iterator<Item = &'a str>, max_dist: usize) -> Option<&'a str> {
    candidates
        .map(|c| (c, edit_distance(name, c)))
        .filter(|(_, d)| *d <= max_dist)
        .min_by_key(|(_, d)| *d)
        .map(|(c, _)| c)
}

use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct Interner {
    map: HashMap<String, u32>,
    strings: Vec<String>,
}

impl Interner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = self.strings.len() as u32;
        self.strings.push(s.to_string());
        self.map.insert(s.to_string(), id);
        id
    }

    pub fn resolve(&self, id: u32) -> &str {
        &self.strings[id as usize]
    }
}
