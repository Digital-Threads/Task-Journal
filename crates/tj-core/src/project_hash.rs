use anyhow::Context;
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn from_path(p: impl AsRef<Path>) -> anyhow::Result<String> {
    let canonical = dunce::canonicalize(p.as_ref())
        .with_context(|| format!("canonicalize {:?}", p.as_ref()))?;
    let bytes = canonical.as_os_str().as_encoded_bytes();
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    debug_assert_eq!(hex.len(), 16);
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn same_path_yields_same_hash() {
        let d = TempDir::new().unwrap();
        let a = from_path(d.path()).unwrap();
        let b = from_path(d.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 16, "16 hex chars expected, got: {a}");
    }

    #[test]
    fn different_paths_yield_different_hashes() {
        let d1 = TempDir::new().unwrap();
        let d2 = TempDir::new().unwrap();
        let a = from_path(d1.path()).unwrap();
        let b = from_path(d2.path()).unwrap();
        assert_ne!(a, b);
    }
}
