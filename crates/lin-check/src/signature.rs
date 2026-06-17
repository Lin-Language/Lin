use std::collections::HashMap;
use crate::types::Type;
use crate::typed_ir::{TypedExpr, TypedModule, TypedStmt};

/// One exported member of a function overload set (ADR-074 cross-module). `ty` is the function
/// type used for call-site resolution; `symbol` is the exact mangled function name the exporting
/// module emitted (`TypedExpr::Function.name`), so a dependent can form the same `{module_key}_…`
/// `Named` call target without recomputing the mangling.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OverloadExport {
    pub ty: Type,
    pub symbol: String,
}

/// The public interface of a compiled Lin module — just the exported name→type map.
/// Dependents only need this, not the full TypedModule, to type-check imports.
/// If the signature is unchanged, dependents do not need to re-check even if the
/// implementation changed (analogous to Haskell .hi files or rustc crate metadata).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModuleSignature {
    /// Exported (or top-level visible) name → type pairs. For an overloaded name this holds the
    /// PRIMARY (first) overload; the full set lives in `overloads`.
    pub exports: HashMap<String, Type>,
    /// Exported `type` decls: name → (type params, resolved body). Lets dependents resolve an
    /// imported type name used in a type annotation. Empty for modules with no exported types.
    #[serde(default)]
    pub type_exports: HashMap<String, (Vec<String>, Type)>,
    /// Exported function overload sets (ADR-074), name → all members (primary first). Only present
    /// for names with ≥2 exported function definitions; absent (empty) for ordinary single exports.
    #[serde(default)]
    pub overloads: HashMap<String, Vec<OverloadExport>>,
}

impl ModuleSignature {
    /// Extract the signature from a fully type-checked module.
    pub fn from_module(module: &TypedModule) -> Self {
        let mut exports = HashMap::new();
        // Group exported function definitions by source name to recover overload sets. The mangled
        // emitted symbol lives in the Val's `TypedExpr::Function.name` (ADR-074); the source name is
        // `TypedStmt::Val.name`.
        let mut grouped: HashMap<String, Vec<OverloadExport>> = HashMap::new();
        for stmt in &module.statements {
            if let TypedStmt::Val { name: Some(n), ty, value, .. } = stmt {
                exports.insert(n.clone(), ty.clone());
                let symbol = match value {
                    TypedExpr::Function { name: Some(sym), .. } => sym.clone(),
                    _ => n.clone(),
                };
                grouped.entry(n.clone()).or_default().push(OverloadExport { ty: ty.clone(), symbol });
            }
        }
        // The HashMap insert above keeps only the LAST definition per name; for overloaded names the
        // primary must be the FIRST, so override `exports` from the grouped order.
        let overloads: HashMap<String, Vec<OverloadExport>> =
            grouped.into_iter().filter(|(_, v)| v.len() > 1).collect();
        for (name, members) in &overloads {
            exports.insert(name.clone(), members[0].ty.clone());
        }
        Self { exports, type_exports: module.exported_types.clone(), overloads }
    }

    /// Serialize to bytes (for caching).
    pub fn to_bytes(&self) -> Option<Vec<u8>> {
        bincode::serialize(self).ok()
    }

    /// Deserialize from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        bincode::deserialize(bytes).ok()
    }

    /// Stable content hash of the signature (SHA-256 of serialized form).
    /// Two signatures with the same public interface have the same hash,
    /// even if they came from different source files.
    pub fn content_hash(&self) -> String {
        use sha2::{Sha256, Digest};
        if let Some(bytes) = self.to_bytes() {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            format!("{:x}", hasher.finalize())
        } else {
            // Fallback: hash each export name+type display string.
            let mut entries: Vec<String> = self.exports.iter()
                .map(|(k, v)| format!("{}:{}", k, v))
                .collect();
            entries.sort();
            let combined = entries.join(";");
            let mut hasher = Sha256::new();
            hasher.update(combined.as_bytes());
            format!("{:x}", hasher.finalize())
        }
    }
}
