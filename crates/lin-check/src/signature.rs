use std::collections::HashMap;
use crate::types::Type;
use crate::typed_ir::{TypedModule, TypedStmt};

/// The public interface of a compiled Lin module — just the exported name→type map.
/// Dependents only need this, not the full TypedModule, to type-check imports.
/// If the signature is unchanged, dependents do not need to re-check even if the
/// implementation changed (analogous to Haskell .hi files or rustc crate metadata).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModuleSignature {
    /// Exported (or top-level visible) name → type pairs.
    pub exports: HashMap<String, Type>,
    /// Exported `type` decls: name → (type params, resolved body). Lets dependents resolve an
    /// imported type name used in a type annotation. Empty for modules with no exported types.
    #[serde(default)]
    pub type_exports: HashMap<String, (Vec<String>, Type)>,
}

impl ModuleSignature {
    /// Extract the signature from a fully type-checked module.
    pub fn from_module(module: &TypedModule) -> Self {
        let mut exports = HashMap::new();
        for stmt in &module.statements {
            if let TypedStmt::Val { name: Some(n), ty, .. } = stmt {
                exports.insert(n.clone(), ty.clone());
            }
        }
        Self { exports, type_exports: module.exported_types.clone() }
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
