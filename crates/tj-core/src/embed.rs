//! Embedding substrate for semantic memory (Pillar A).
//!
//! An [`Embedder`] turns text into a fixed-dimension vector so events can be
//! retrieved by *meaning*, not just keyword (FTS5). The real semantic backend is
//! a pure-Rust static model (model2vec, behind the `embed` feature); when it is
//! absent every caller falls back to FTS5, so the journal's zero-cost,
//! offline-by-default behaviour is preserved.
//!
//! This module is dependency-free on purpose: the trait, the cosine/recency
//! math, the SQLite blob codec, and a deterministic [`HashEmbedder`] all build
//! and test without pulling a model. The model2vec backend is added as an
//! isolated, feature-gated step on top.

/// A text embedder. Implementations return exactly one vector per input, all of
/// the same [`dim`](Embedder::dim), produced by the model named by
/// [`model_id`](Embedder::model_id).
pub trait Embedder: Send + Sync {
    /// Embed a batch of texts. `out[i]` corresponds to `texts[i]`.
    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>>;
    /// Stable identifier of the model (stored per vector so a model change can
    /// trigger a re-embed and we never compare vectors across models).
    fn model_id(&self) -> &str;
    /// Output dimensionality.
    fn dim(&self) -> usize;

    /// Convenience: embed a single text.
    fn embed_one(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut v = self.embed(&[text])?;
        Ok(v.pop().unwrap_or_default())
    }
}

/// Cosine similarity of two vectors. Returns `0.0` on a length mismatch or a
/// zero-norm input — callers *rank* with this, they don't assert on it, so it
/// must never panic.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Encode an `f32` vector as a little-endian byte blob for SQLite `BLOB`
/// storage. Round-trips with [`from_blob`].
pub fn to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Decode a little-endian byte blob back into an `f32` vector. Trailing bytes
/// that don't form a full `f32` are ignored (defensive; should never happen for
/// blobs produced by [`to_blob`]).
pub fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Whether an event's text is worth embedding. Skips empties and very short
/// boilerplate (e.g. the `[open]` marker) that carry no retrievable meaning.
pub fn is_embeddable(text: &str) -> bool {
    text.trim().chars().count() >= 12
}

/// A deterministic, dependency-free embedder using the feature-hashing trick:
/// each token is hashed into one of `dim` buckets and the resulting bag-of-words
/// vector is L2-normalised. It is **lexical**, not semantic — its job is to make
/// the trait, storage, ingest and ranking code testable without a model, and to
/// serve as a crude offline fallback. The real semantic quality comes from the
/// model2vec backend.
pub struct HashEmbedder {
    dim: usize,
}

impl HashEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(1) }
    }

    fn hash_token(tok: &str) -> u64 {
        // FNV-1a — small, deterministic, no deps.
        let mut h: u64 = 0xcbf29ce484222325;
        for b in tok.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new(64)
    }
}

impl Embedder for HashEmbedder {
    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            let mut v = vec![0.0f32; self.dim];
            for tok in t
                .split(|c: char| !c.is_alphanumeric())
                .filter(|s| !s.is_empty())
            {
                let lower = tok.to_lowercase();
                let bucket = (Self::hash_token(&lower) as usize) % self.dim;
                v[bucket] += 1.0;
            }
            // L2-normalise so cosine == dot product and lengths don't bias.
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in &mut v {
                    *x /= norm;
                }
            }
            out.push(v);
        }
        Ok(out)
    }

    fn model_id(&self) -> &str {
        "hash-v1"
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

/// Default model2vec repo — multilingual so RU/EN prose both embed well.
/// Overridable via `TJ_EMBED_MODEL`.
#[cfg(feature = "embed")]
pub const DEFAULT_EMBED_MODEL: &str = "minishlab/potion-multilingual-128M";

/// The embedder the journal uses unless overridden. With the `embed` feature
/// (on by default) it loads the model2vec static model for true semantic
/// recall; if that can't load — offline first run, download failure, or
/// `TJ_EMBED=hash` — it falls back to the dependency-free lexical
/// [`HashEmbedder`] so the journal never breaks.
pub fn default_embedder() -> Box<dyn Embedder> {
    // Test/escape hatch: force the deterministic lexical embedder.
    if std::env::var("TJ_EMBED").as_deref() == Ok("hash") {
        return Box::new(HashEmbedder::default());
    }
    #[cfg(feature = "embed")]
    {
        let repo =
            std::env::var("TJ_EMBED_MODEL").unwrap_or_else(|_| DEFAULT_EMBED_MODEL.to_string());
        match Model2VecEmbedder::load(&repo) {
            Ok(m) => return Box::new(m),
            Err(e) => {
                tracing::warn!("model2vec load failed ({e:#}); using hash embedder fallback");
            }
        }
    }
    Box::new(HashEmbedder::default())
}

/// True semantic embedder backed by a model2vec static model (pure-Rust, no
/// onnxruntime). The model is downloaded once via the HuggingFace hub and
/// cached locally; later loads read the cache. Behind the `embed` feature.
#[cfg(feature = "embed")]
pub struct Model2VecEmbedder {
    model: model2vec_rs::model::StaticModel,
    model_id: String,
    dim: usize,
}

#[cfg(feature = "embed")]
impl Model2VecEmbedder {
    /// Load `repo` (a HuggingFace model id or a local directory). Probes the
    /// model once to discover its output dimension.
    pub fn load(repo: &str) -> anyhow::Result<Self> {
        let model = model2vec_rs::model::StaticModel::from_pretrained(
            repo,
            None,       // no auth token
            Some(true), // L2-normalise outputs
            None,       // no subfolder
        )?;
        let dim = model.encode_single("probe").len();
        anyhow::ensure!(
            dim > 0,
            "model2vec model {repo} produced a zero-dim embedding"
        );
        Ok(Self {
            model,
            model_id: format!("model2vec:{repo}"),
            dim,
        })
    }
}

#[cfg(feature = "embed")]
impl Embedder for Model2VecEmbedder {
    fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let owned: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
        Ok(self.model.encode(&owned))
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
    }

    #[test]
    fn cosine_mismatch_or_zero_norm_is_zero() {
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn blob_round_trips() {
        let v = vec![0.5, -1.25, 3.0, 0.0];
        assert_eq!(from_blob(&to_blob(&v)), v);
    }

    #[test]
    fn is_embeddable_skips_short_boilerplate() {
        assert!(!is_embeddable(""));
        assert!(!is_embeddable("[open]"));
        assert!(is_embeddable("Fix the auth bug in middleware"));
    }

    #[test]
    fn hash_embedder_is_deterministic_and_normalised() {
        let e = HashEmbedder::new(32);
        let a = e.embed_one("payment gateway dedup").unwrap();
        let b = e.embed_one("payment gateway dedup").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn hash_embedder_overlap_ranks_above_disjoint() {
        let e = HashEmbedder::new(256);
        let q = e.embed_one("payment refund duplicate write").unwrap();
        let near = e.embed_one("duplicate refund write on payment").unwrap();
        let far = e.embed_one("frontend button color tweak").unwrap();
        assert!(
            cosine(&q, &near) > cosine(&q, &far),
            "lexical overlap must score higher: near={} far={}",
            cosine(&q, &near),
            cosine(&q, &far)
        );
    }
}
