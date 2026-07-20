//! Environment/user-context memory: durable facts about THIS user's specific setup (terminal
//! app, project locations, tool preferences) that Holo3/Kimi has no other way to know, embedded
//! and semantically retrieved so the most relevant handful get prepended to a prompt right
//! before [`crate::holo_bridge::a2a_client::A2aClient::send_and_stream`] sends it.
//!
//! ## The concrete failure this exists to fix
//!
//! Live-witnessed: asked to "go to Claude Code" to modify this very project, the agent opened
//! a NEW terminal window instead of finding the existing Claude Code session already running
//! in Ghostty (this user's actual terminal app, not Terminal.app -- a fact the model has zero
//! way to know from a screenshot alone if the target window isn't currently visible/focused).
//! There was no mechanism anywhere in this daemon to inject a fact like "the user's terminal is
//! Ghostty" into a turn; `A2aClient::send_and_stream` sends the raw prompt text and nothing
//! else (see its own doc). This module is that mechanism.
//!
//! ## Architecture: pattern-ported from `AnEntrypoint/gm`'s `rs-plugkit`, not vendored
//!
//! Two prior research passes (this project's own session history) confirmed neither of
//! `rs-plugkit`'s real embedding/memory implementation nor `agentplug-libsql` (the real,
//! standalone libsql-wrapper repo at `AnEntrypoint/agentplug-libsql`) is vendorable into a
//! native Rust binary:
//! - `rs-plugkit/crates/plugkit-core/src/embed.rs` is `#![cfg(target_arch = "wasm32")]` in
//!   its entirety, embedding BGE-small-en-v1.5 weights via `include_bytes!` for
//!   `wasm32-wasip1`, with candle pinned to 0.8 specifically because 0.11 fails to compile for
//!   that target.
//! - `agentplug-libsql`'s `src/lib.rs` line 1 is likewise `#![cfg(target_arch = "wasm32")]`;
//!   its only real dependency (`libsql-ffi`) is scoped under
//!   `[target.'cfg(target_arch = "wasm32")'.dependencies]`, and the crate defines raw
//!   `#[no_mangle] extern "C"` host-ABI functions (`plugkit_alloc`/`plugin_call`) matching a
//!   wasm-guest-calls-host convention, not a normal linkable Rust API. It also defines NO
//!   vector schema of its own -- confirmed zero hits for `f32_blob|vector_top_k|embed|bge`
//!   anywhere in that repo; it is a stateless raw-SQL passthrough over `libsql-ffi`, and any
//!   vector/embedding scheme is entirely the CALLER's responsibility.
//!
//! So this module reimplements the same ARCHITECTURE natively instead: `candle-core` +
//! `candle-transformers`' BERT implementation running the real BGE-small-en-v1.5 weights
//! (fetched from HuggingFace on first use, cached locally -- see [`EMBEDDING_CACHE_SUBDIR`]),
//! and the real, native `libsql` crate (from crates.io, NOT `agentplug-libsql`) with the same
//! `F32_BLOB(384)` + `vector_top_k`/`vector_distance_cos` schema shape `rs-plugkit`'s
//! `rssearch_vectors.rs` uses.
//!
//! ## On-disk shape (mirrors `.gm/memories/*.md` + `.gm/gm.db`)
//!
//! Two stores, same relationship gm's own memory system has between them: the markdown files
//! under `~/.holoiroh/context/*.md` are the DURABLE, human-readable/editable corpus (frontmatter
//! `key`/`ns`/`created`/`updated` fields, exactly `rs-plugkit`'s real on-disk format -- verified
//! directly against a real checked-out `.gm/memories/mem-00135b4a71185432-52.md`), and the
//! libsql database at `~/.holoiroh/context.db` is a DERIVED vector index over that corpus,
//! rebuildable from the markdown files at any time (see [`EnvContextStore::reindex`]). `ns`
//! (namespace) is always `"default"` here too -- the gm research confirmed rs-plugkit's own
//! store has no category/tag schema either; every real memory file checked had `ns: default`,
//! disambiguated purely by embedding-similarity at recall time, not by a type field. Facts are
//! written as present-tense invariants ("the user's terminal app is Ghostty"), matching the
//! ingestion-time framing constraint the gm research found (`memorize-fire`'s classifier
//! rejects history-framed text; only present-tense rules about what must be true now).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig, DTYPE};
use tokenizers::Tokenizer;

/// BGE-small-en-v1.5: the exact model `rs-plugkit`'s real embedding pipeline uses (confirmed
/// directly in its `embed.rs`), 384-dim output. Kept identical here so a future genuine port
/// of `rs-plugkit`'s own vector rows (if that ever becomes possible/desirable) would be
/// dimension-compatible.
const MODEL_REPO: &str = "BAAI/bge-small-en-v1.5";
const EMBEDDING_DIM: usize = 384;

/// BGE's own documented query-prefix convention (verified in `rs-plugkit`'s `embed.rs`:
/// `"Represent this sentence for searching relevant passages: "` for QUERY embeddings only --
/// stored documents are embedded WITHOUT this prefix). Asymmetric embedding is how BGE models
/// are actually trained; using the wrong side's convention measurably hurts retrieval quality.
const QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";

/// Where model weights are cached after first download (see [`EnvContextStore::new`]).
const EMBEDDING_CACHE_SUBDIR: &str = "models/bge-small-en-v1.5";

/// A single durable environment/user-context fact.
///
/// `#[allow(dead_code)]` on `key`/`ns`/`updated_at_ms`: real fields populated by every
/// `retrieve()` call (needed to round-trip `reindex()`'s markdown-file identity), but the only
/// FIELD [`format_context_block`] (today's sole consumer) reads is `text` -- the others are
/// live API surface for a future admin/debug surface (list facts, show freshness), not unused
/// code to delete.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ContextFact {
    /// Kebab-slug identifier, e.g. `"terminal-app-ghostty"`. Mirrors `rs-plugkit`'s PRD/memory
    /// `key` field convention (mandatory, slug-shaped).
    pub key: String,
    /// Always `"default"` today -- see this module's doc on why no category schema exists.
    pub ns: String,
    /// The fact itself, present-tense, e.g. "The user's terminal application is Ghostty, not
    /// Apple's Terminal.app or iTerm2. When asked to open/use/go to a terminal or an existing
    /// CLI session (e.g. Claude Code), check for an already-running Ghostty window first
    /// instead of opening a new terminal application."
    pub text: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// The environment-context memory store: an embedding pipeline plus a libsql-backed vector
/// index over a markdown corpus. One instance is held for the daemon's process lifetime (see
/// `main.rs`), analogous to how `HoloBridge` is held.
pub struct EnvContextStore {
    corpus_dir: PathBuf,
    db_path: PathBuf,
    model_cache_dir: PathBuf,
    // Model state is genuinely expensive to construct (loads + runs a real BERT forward pass
    // graph) and is NOT `Send`-cheap to clone, so it's held behind a `Mutex` rather than
    // reloaded per call -- matching the "load once, reuse" discipline `rs-plugkit`'s own
    // `embed.rs` uses internally (though there it's the wasm host's job; here it's ours).
    embedder: Mutex<Option<Embedder>>,
}

struct Embedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl EnvContextStore {
    /// Open (creating if needed) the store at the daemon's standard location:
    /// `~/.holoiroh/context/*.md` for the durable corpus, `~/.holoiroh/context.db` for the
    /// derived vector index. Does NOT load the embedding model yet -- that happens lazily on
    /// first [`Self::retrieve`]/[`Self::remember`] call (see [`Self::ensure_embedder`]), so
    /// daemon startup never blocks on a ~130MB first-run download.
    pub fn open() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME not set; cannot locate ~/.holoiroh/context")?;
        let base = home.join(".holoiroh");
        Self::open_at(base.join("context"), base.join("context.db"), base.join(EMBEDDING_CACHE_SUBDIR))
    }

    /// Like [`Self::open`], but with explicit paths -- the real entry point `open()` calls
    /// with the daemon's standard `~/.holoiroh/*` locations. Exists so
    /// `examples/env_context_probe.rs` can point the CORPUS/DB at an isolated, disposable
    /// location while still sharing the real `$HOME`-cached model weights (avoiding a
    /// redundant ~130MB re-download every probe run) -- probing must never touch or depend on
    /// the real daemon's `~/.holoiroh/context/` corpus.
    pub fn open_at(corpus_dir: PathBuf, db_path: PathBuf, model_cache_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&corpus_dir)
            .with_context(|| format!("creating {}", corpus_dir.display()))?;
        Ok(Self {
            corpus_dir,
            db_path,
            model_cache_dir,
            embedder: Mutex::new(None),
        })
    }

    /// Lazily load the embedding model, downloading+caching BGE-small-en-v1.5's weights on
    /// first use if not already cached. Network access here is a one-time cost per machine
    /// (subsequent calls, even across daemon restarts, hit the local cache).
    ///
    /// Fetches files directly via `reqwest` (already a dependency elsewhere in this crate)
    /// rather than through `hf_hub`'s own `ApiRepo::get`/`download` -- BOTH of `hf_hub` 0.3.2's
    /// API surfaces (sync/`ureq` and tokio/`reqwest`) have the same real, live bug in their
    /// redirect-following `metadata()`: they pass the response's `Location` header straight to
    /// a fresh request as if it were always an absolute URL. Confirmed directly against the
    /// real endpoint (`curl -I https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main/
    /// tokenizer.json`): HuggingFace's current CDN returns a genuinely RELATIVE `Location:
    /// /api/resolve-cache/...` header, which is valid HTTP but breaks both of `hf_hub`'s
    /// hand-rolled redirect-followers. `reqwest`'s own built-in redirect policy resolves
    /// relative `Location` headers against the request's own URL correctly (per the HTTP
    /// spec), so calling `.get(url).send()` directly and letting reqwest's normal redirect
    /// handling run sidesteps the bug entirely instead of working around it in this code.
    async fn ensure_embedder(&self) -> Result<()> {
        {
            let guard = self.embedder.lock().expect("embedder lock poisoned");
            if guard.is_some() {
                return Ok(());
            }
        }
        tracing::info!("env_context: loading BGE-small-en-v1.5 embedding model (first use may download ~130MB)");
        std::fs::create_dir_all(&self.model_cache_dir)
            .with_context(|| format!("creating {}", self.model_cache_dir.display()))?;

        let tokenizer_path = self.fetch_model_file("tokenizer.json").await?;
        let config_path = self.fetch_model_file("config.json").await?;
        let weights_path = self.fetch_model_file("model.safetensors").await?;

        let config_json = std::fs::read_to_string(&config_path).context("reading config.json")?;
        let config: BertConfig = serde_json::from_str(&config_json).context("parsing BERT config.json")?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("loading tokenizer.json: {e}"))?;

        let device = Device::Cpu;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], DTYPE, &device)
                .context("loading model.safetensors")?
        };
        let model = BertModel::load(vb, &config).context("constructing BertModel")?;

        *self.embedder.lock().expect("embedder lock poisoned") = Some(Embedder { model, tokenizer, device });
        tracing::info!("env_context: embedding model ready");
        Ok(())
    }

    /// Fetch one file from `MODEL_REPO`'s `main` branch, using the local cache if already
    /// present -- see [`Self::ensure_embedder`]'s doc for why this bypasses `hf_hub`'s own
    /// (broken) fetch methods and drives `reqwest` directly instead.
    async fn fetch_model_file(&self, filename: &str) -> Result<PathBuf> {
        let cached = self.model_cache_dir.join(filename);
        if cached.exists() {
            return Ok(cached);
        }
        let url = format!("https://huggingface.co/{MODEL_REPO}/resolve/main/{filename}");
        tracing::info!(filename, "env_context: downloading model file");
        let response = reqwest::get(&url)
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url} returned an error status"))?;
        let bytes = response.bytes().await.with_context(|| format!("reading body of {url}"))?;
        // Write to a temp file then rename, so a killed/crashed download never leaves a
        // partial file that a later run mistakes for a complete, valid cache entry.
        let tmp_path = cached.with_extension("tmp-download");
        std::fs::write(&tmp_path, &bytes).with_context(|| format!("writing {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &cached)
            .with_context(|| format!("renaming {} -> {}", tmp_path.display(), cached.display()))?;
        Ok(cached)
    }

    /// Embed `text` (as a stored document, no query prefix) or a query (with BGE's query
    /// prefix) -- see [`QUERY_PREFIX`]'s doc for why the two are asymmetric.
    async fn embed(&self, text: &str, is_query: bool) -> Result<Vec<f32>> {
        self.ensure_embedder().await?;
        let guard = self.embedder.lock().expect("embedder lock poisoned");
        let embedder = guard.as_ref().expect("just ensured embedder is Some");

        let input = if is_query {
            format!("{QUERY_PREFIX}{text}")
        } else {
            text.to_string()
        };

        let encoding = embedder
            .tokenizer
            .encode(input, true)
            .map_err(|e| anyhow::anyhow!("tokenizing: {e}"))?;
        let token_ids = Tensor::new(encoding.get_ids(), &embedder.device)
            .context("building token id tensor")?
            .unsqueeze(0)
            .context("unsqueeze token ids")?;
        let token_type_ids = token_ids.zeros_like().context("building token_type_ids")?;
        let attention_mask = Tensor::new(encoding.get_attention_mask(), &embedder.device)
            .context("building attention mask tensor")?
            .unsqueeze(0)
            .context("unsqueeze attention mask")?;

        let output = embedder
            .model
            .forward(&token_ids, &token_type_ids, Some(&attention_mask))
            .context("BERT forward pass")?;

        // Mean-pool over the sequence dimension (BGE's documented pooling strategy), then
        // L2-normalize -- required for `vector_distance_cos` to behave as a true cosine
        // distance rather than an unnormalized dot-product-derived value.
        let (_batch, seq_len, _hidden) = output.dims3().context("output dims3")?;
        let pooled = (output.sum(1).context("sum over sequence dim")? / (seq_len as f64))
            .context("mean-pool divide")?;
        let norm = pooled.sqr().context("square")?.sum_keepdim(1).context("sum for norm")?.sqrt().context("sqrt for norm")?;
        let normalized = pooled.broadcast_div(&norm).context("normalize")?;

        let vec: Vec<f32> = normalized
            .squeeze(0)
            .context("squeeze batch dim")?
            .to_vec1()
            .context("tensor to Vec<f32>")?;
        if vec.len() != EMBEDDING_DIM {
            bail!("unexpected embedding dimension {} (expected {EMBEDDING_DIM})", vec.len());
        }
        Ok(vec)
    }

    /// Open (creating the schema if needed) the derived libsql vector index. A fresh
    /// connection per call, matching `agentplug-libsql`'s own "open, do one thing, close"
    /// discipline (its `db.rs` doc: safe under concurrent access from multiple processes) --
    /// this daemon has no long-lived writer contention to optimize away, so the simplicity is
    /// worth the tiny per-call open cost.
    async fn open_db(&self) -> Result<libsql::Connection> {
        let db = libsql::Builder::new_local(&self.db_path)
            .build()
            .await
            .with_context(|| format!("opening libsql db at {}", self.db_path.display()))?;
        let conn = db.connect().context("connecting to libsql db")?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS context_facts (
                key TEXT PRIMARY KEY,
                ns TEXT NOT NULL,
                text TEXT NOT NULL,
                embedding F32_BLOB(384) NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            )",
            (),
        )
        .await
        .context("creating context_facts table")?;
        // libsql's vector index needs to be created separately from the table (matching
        // rs-plugkit's rssearch_vectors.rs schema shape).
        conn.execute(
            "CREATE INDEX IF NOT EXISTS context_facts_vec_idx ON context_facts (libsql_vector_idx(embedding))",
            (),
        )
        .await
        .context("creating vector index")?;
        Ok(conn)
    }

    /// Store a new fact (or update an existing one by `key`): write the durable markdown file
    /// (the primary corpus -- see this module's doc), embed the text, and upsert into the
    /// derived vector index. Present-tense phrasing is the caller's responsibility (matching
    /// the ingestion-time constraint the gm research found); this function does not enforce it
    /// mechanically, only documents the convention.
    pub async fn remember(&self, key: &str, text: &str) -> Result<()> {
        let now = now_ms();
        let existing_created = self.read_markdown(key).ok().flatten().map(|f| f.created_at_ms);
        let created_at_ms = existing_created.unwrap_or(now);

        self.write_markdown(key, text, created_at_ms, now)?;

        let embedding = self.embed(text, false).await?;
        let conn = self.open_db().await?;
        let embedding_json = serde_json::to_string(&embedding).context("serializing embedding")?;
        conn.execute(
            "INSERT INTO context_facts (key, ns, text, embedding, created_at_ms, updated_at_ms)
             VALUES (?1, 'default', ?2, vector32(?3), ?4, ?5)
             ON CONFLICT(key) DO UPDATE SET
                text = excluded.text,
                embedding = excluded.embedding,
                updated_at_ms = excluded.updated_at_ms",
            libsql::params![key, text, embedding_json, created_at_ms as i64, now as i64],
        )
        .await
        .context("upserting context fact into vector index")?;
        tracing::info!(key, "env_context: remembered fact");
        Ok(())
    }

    /// Semantic top-k retrieval: embed `query_text` (with BGE's query prefix) and return the
    /// `limit` most similar stored facts by cosine distance, using libsql's real
    /// `vector_top_k`/`vector_distance_cos` (the same functions `rs-plugkit`'s
    /// `rssearch_vectors.rs` uses -- verified real SQL, not a hand-rolled scan).
    pub async fn retrieve(&self, query_text: &str, limit: usize) -> Result<Vec<ContextFact>> {
        let query_embedding = self.embed(query_text, true).await?;
        let query_json = serde_json::to_string(&query_embedding).context("serializing query embedding")?;
        let conn = self.open_db().await?;

        // vector_top_k requires the ANN index name (not the table) as its first argument,
        // joined back to the source table on rowid -- this is libsql's documented usage shape
        // for its vector search extension.
        let mut rows = conn
            .query(
                "SELECT f.key, f.ns, f.text, f.created_at_ms, f.updated_at_ms
                 FROM vector_top_k('context_facts_vec_idx', vector32(?1), ?2) AS v
                 JOIN context_facts AS f ON f.rowid = v.id",
                libsql::params![query_json, limit as i64],
            )
            .await
            .context("vector_top_k query")?;

        let mut facts = Vec::new();
        while let Some(row) = rows.next().await.context("reading vector_top_k row")? {
            facts.push(ContextFact {
                key: row.get::<String>(0).context("row.key")?,
                ns: row.get::<String>(1).context("row.ns")?,
                text: row.get::<String>(2).context("row.text")?,
                created_at_ms: row.get::<i64>(3).context("row.created_at_ms")? as u64,
                updated_at_ms: row.get::<i64>(4).context("row.updated_at_ms")? as u64,
            });
        }
        Ok(facts)
    }

    /// Rebuild the entire vector index from the markdown corpus on disk -- the corpus is the
    /// source of truth (mirrors `rs-plugkit`'s own memory-md-primary/vector-derived
    /// relationship), so this recovers a deleted/corrupted `context.db` without losing any
    /// facts, and lets a human hand-edit/add `.md` files directly and have them picked up.
    #[allow(dead_code)] // real recovery API; no caller yet in the bin target
    pub async fn reindex(&self) -> Result<usize> {
        let mut count = 0;
        for entry in std::fs::read_dir(&self.corpus_dir)
            .with_context(|| format!("reading {}", self.corpus_dir.display()))?
        {
            let entry = entry.context("reading corpus dir entry")?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Some(key) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Some(fact) = self.parse_markdown_file(&path)? {
                self.remember(key, &fact.text).await?;
                count += 1;
            }
        }
        tracing::info!(count, "env_context: reindexed corpus into vector store");
        Ok(count)
    }

    // MARK: - Markdown corpus I/O (mirrors `.gm/memories/*.md`'s real on-disk shape)

    fn markdown_path(&self, key: &str) -> PathBuf {
        self.corpus_dir.join(format!("{key}.md"))
    }

    fn write_markdown(&self, key: &str, text: &str, created_at_ms: u64, updated_at_ms: u64) -> Result<()> {
        let contents = format!(
            "---\nkey: {key}\nns: default\ncreated: {created_at_ms}\nupdated: {updated_at_ms}\n---\n\n{text}\n"
        );
        let path = self.markdown_path(key);
        std::fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))
    }

    fn read_markdown(&self, key: &str) -> Result<Option<ContextFact>> {
        let path = self.markdown_path(key);
        if !path.exists() {
            return Ok(None);
        }
        self.parse_markdown_file(&path)
    }

    /// Parse gm's real frontmatter shape: `---\nkey: ...\nns: ...\ncreated: ...\nupdated:
    /// ...\n---\n\n<body>`. Deliberately hand-rolled (not a YAML frontmatter crate) since the
    /// format is this small and fixed-shape -- matching the same "don't add a dependency for
    /// five fields" judgment call this daemon already makes elsewhere.
    fn parse_markdown_file(&self, path: &Path) -> Result<Option<ContextFact>> {
        let contents = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let Some(rest) = contents.strip_prefix("---\n") else {
            return Ok(None);
        };
        let Some(end) = rest.find("\n---\n") else {
            return Ok(None);
        };
        let frontmatter = &rest[..end];
        let body = rest[end + 5..].trim_start_matches('\n').trim_end().to_string();

        let mut key = None;
        let mut ns = None;
        let mut created_at_ms = None;
        let mut updated_at_ms = None;
        for line in frontmatter.lines() {
            let Some((field, value)) = line.split_once(':') else { continue };
            let value = value.trim();
            match field.trim() {
                "key" => key = Some(value.to_string()),
                "ns" => ns = Some(value.to_string()),
                "created" => created_at_ms = value.parse::<u64>().ok(),
                "updated" => updated_at_ms = value.parse::<u64>().ok(),
                _ => {}
            }
        }
        let (Some(key), Some(ns), Some(created_at_ms), Some(updated_at_ms)) =
            (key, ns, created_at_ms, updated_at_ms)
        else {
            return Ok(None);
        };
        Ok(Some(ContextFact {
            key,
            ns,
            text: body,
            created_at_ms,
            updated_at_ms,
        }))
    }
}

/// Build the text actually prepended to a prompt, from a list of retrieved facts. `None` if
/// `facts` is empty (callers should skip prepending anything in that case, not send an empty
/// header). Kept as a free function (not a method) since it's pure text formatting with no
/// need for `&self`.
pub fn format_context_block(facts: &[ContextFact]) -> Option<String> {
    if facts.is_empty() {
        return None;
    }
    let mut block = String::from(
        "Known facts about this user's environment and setup (use these -- do not \
         re-derive or contradict them):\n",
    );
    for fact in facts {
        block.push_str("- ");
        block.push_str(&fact.text);
        block.push('\n');
    }
    Some(block)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
