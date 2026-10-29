//! Provider-independent embeddings and re-rankers, chosen with one string:
//!
//! - `hf:<model>`: a local ONNX model, by its Hugging Face id. A fastembed preset is used when the id
//!   names one (`hf:intfloat/multilingual-e5-small`, `hf:Qdrant/bge-small-en-v1.5-onnx-Q`). Any other
//!   repository with an ONNX export loads directly, pinned with options:
//!   `hf:Xenova/multilingual-e5-small?revision=<sha>&dtype=q8&pooling=mean` (`dtype` fp32 or q8,
//!   `pooling` mean or cls; e5 models get their `query: `/`passage: ` prefixes).
//! - `http:<base url>?model=<m>&dims=<n>`: any OpenAI-compatible `/v1/embeddings` endpoint (TEI, vLLM, custom).
//! - `nvidia:<model>?dims=<n>`: NVIDIA's API catalog (key in `NVIDIA_API_KEY`).
//! - `cloudflare:<model>?dims=<n>`: Workers AI (`CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_API_TOKEN`).
//! - `none`: no embedder (BM25 only) or no re-ranker.
//!
//! Re-rankers: `hf:BAAI/bge-reranker-base` (or another fastembed re-ranker id) or `none`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fastembed::{
    EmbeddingModel, InitOptionsUserDefined, Pooling, QuantizationMode, RerankInitOptions,
    RerankInitOptionsUserDefined, RerankerModel, TextEmbedding, TextInitOptions, TextRerank,
    TokenizerFiles, UserDefinedEmbeddingModel, UserDefinedRerankingModel,
};
use serde_json::{Value, json};
use sha2::Digest;

use crate::types::{EmbeddingSpace, Error, Result};

/// ONNX Runtime threads per model (`HYPERAG_MODEL_THREADS` or `HYPERAG_THREADS`). Using every core for
/// one short query costs more in thread wake-ups and memory than it gains.
fn threads() -> usize {
    // HYPERAG_THREADS is the name the TypeScript reference reads; either works.
    // Default: half the cores, at most 8. The re-ranker scored 24 pairs in 239 ms at 4 threads and
    // 192 ms at 8 on an 18-core Mac; more threads than that only add wake-ups.
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    ["HYPERAG_MODEL_THREADS", "HYPERAG_THREADS"]
        .iter()
        .find_map(|k| std::env::var(k).ok()?.parse().ok())
        .unwrap_or((cores / 2).clamp(1, 8))
}

pub fn cache_root() -> PathBuf {
    cache_dir()
}

/// Records that the licence of a model was shown, with the time, in the cache's licences.json.
pub fn record_licence(repo: &str, licence: &str) -> std::io::Result<()> {
    let path = cache_dir().join("licences.json");
    let mut seen: serde_json::Map<String, Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default();
    seen.insert(
        repo.to_owned(),
        json!({ "licence": licence, "shown_at": chrono::Utc::now().to_rfc3339() }),
    );
    std::fs::create_dir_all(cache_dir())?;
    std::fs::write(
        path,
        serde_json::to_string_pretty(&seen).unwrap_or_default(),
    )
}

fn cache_dir() -> PathBuf {
    std::env::var("HYPERAG_MODEL_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".cache/hyperag/models")
        })
}

/// The snapshot hash fastembed downloaded, so the space records the exact model files.
fn snapshot_of(model_code: &str) -> Option<String> {
    let dir = cache_dir()
        .join(format!("models--{}", model_code.replace('/', "--")))
        .join("snapshots");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .next()
}

/// A Hugging Face repository's ONNX export at a pinned revision, downloaded once into the cache.
pub struct HfFiles {
    onnx: Vec<u8>,
    /// The graph on disk; its external weights sit next to it, so ONNX Runtime can open it directly.
    onnx_path: PathBuf,
    /// Weights stored next to the graph (`<file>_data`), when the export has them.
    external: Option<(String, Vec<u8>)>,
    tokenizer: TokenizerFiles,
    config: Value,
    revision: String,
    /// The licence the model card declares, for `hyperag models pull` to show.
    pub licence: String,
    /// Bytes downloaded or read from the cache for the model weights.
    pub weight_bytes: u64,
    /// Weight files the hub publishes no SHA-256 for: downloaded, but not checked.
    pub unchecked: Vec<String>,
}

/// Files of one revision with the SHA-256 Hugging Face publishes for its large (LFS) files.
async fn hf_tree(
    http: &reqwest::Client,
    repo: &str,
    revision: &str,
) -> Result<Vec<(String, Option<String>)>> {
    let mut url =
        format!("https://huggingface.co/api/models/{repo}/tree/{revision}?recursive=true");
    let mut out = vec![];
    // The tree API pages large repositories through a `Link: <...>; rel="next"` header.
    loop {
        let r = http
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(fail)?;
        let next = r
            .headers()
            .get("link")
            .and_then(|l| l.to_str().ok())
            .and_then(|l| {
                l.split(',')
                    .find(|p| p.contains("rel=\"next\""))?
                    .split(';')
                    .next()?
                    .trim()
                    .strip_prefix('<')?
                    .strip_suffix('>')
                    .map(str::to_owned)
            });
        let rows: Value = r.json().await.map_err(fail)?;
        out.extend(rows.as_array().into_iter().flatten().filter_map(|f| {
            Some((
                f["path"].as_str()?.to_owned(),
                f["lfs"]["oid"].as_str().map(str::to_owned),
            ))
        }));
        match next {
            Some(n) => url = n,
            None => return Ok(out),
        }
    }
}

pub async fn hf_files(repo: &str, revision: &str, dtype: &str) -> Result<HfFiles> {
    // Both become path segments of the cache and of the download URL.
    let part = |p: &str| {
        !p.is_empty()
            && p != "."
            && p != ".."
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    let valid_repo = repo
        .split_once('/')
        .is_some_and(|(o, n)| part(o) && part(n));
    if !valid_repo || !part(revision) || revision.len() > 64 {
        return Err(Error::InvalidInput(
            "hf repo is <owner>/<name> and revision a branch, tag or commit".into(),
        ));
    }
    let onnx = match dtype {
        "fp32" => "onnx/model.onnx",
        "q8" => "onnx/model_quantized.onnx",
        "q4" => "onnx/model_q4.onnx",
        "fp16" => "onnx/model_fp16.onnx",
        _ => return Err(Error::InvalidInput("dtype is fp32, fp16, q8 or q4".into())),
    };
    let dir = cache_dir()
        .join("hf")
        .join(repo.replace('/', "--"))
        .join(revision);
    let http = reqwest::Client::new();
    // The tree is read once per download; a fully cached revision works offline.
    let marker = dir.join(".complete");
    let tree = if marker.exists() {
        None
    } else {
        Some(hf_tree(&http, repo, revision).await?)
    };
    let sha_of = |file: &str| {
        tree.as_ref().and_then(|t| {
            t.iter()
                .find(|(p, _)| p == file)
                .and_then(|(_, sha)| sha.clone())
        })
    };
    let exists = |file: &str| {
        tree.as_ref()
            .map(|t| t.iter().any(|(p, _)| p == file))
            .unwrap_or_else(|| dir.join(file).exists())
    };
    // Large files are checked against the SHA-256 the hub publishes for this revision (small files
    // are fixed by the pinned commit itself). A checked file gets a `<file>.sha256` marker; a cached
    // file is checked again whenever the tree is at hand, and replaced if it does not match.
    let unchecked = std::sync::Mutex::new(Vec::<String>::new());
    let io = |e: std::io::Error| Error::Dependency(e.to_string());
    let get = async |file: &str| -> Result<Vec<u8>> {
        let path = dir.join(file);
        let marker = dir.join(format!("{file}.sha256"));
        let want = sha_of(file);
        if let Ok(bytes) = tokio::fs::read(&path).await {
            let large = file.ends_with("_data") || file.ends_with(".onnx");
            let ok = match (&want, tokio::fs::read_to_string(&marker).await.ok()) {
                (Some(w), _) => hex::encode(sha2::Sha256::digest(&bytes)) == *w,
                (None, Some(m)) => hex::encode(sha2::Sha256::digest(&bytes)) == m.trim(),
                (None, None) => {
                    // Offline, a large file without a marker is used but reported as not checked.
                    if large {
                        unchecked.lock().expect("lock").push(file.to_owned());
                    }
                    tree.is_none() || !large
                }
            };
            if ok {
                if let Some(w) = &want {
                    let _ = tokio::fs::write(&marker, w).await;
                }
                return Ok(bytes);
            }
            let _ = tokio::fs::remove_file(&path).await;
        }
        let url = format!("https://huggingface.co/{repo}/resolve/{revision}/{file}");
        let bytes = http
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(fail)?
            .bytes()
            .await
            .map_err(fail)?;
        let got = hex::encode(sha2::Sha256::digest(&bytes));
        match &want {
            Some(w) if *w != got => {
                return Err(Error::Dependency(format!(
                    "{repo}/{file}: SHA-256 {got} does not match the published {w}"
                )));
            }
            Some(_) => {}
            None if file.ends_with(".onnx") || file.ends_with("_data") => {
                unchecked.lock().expect("lock").push(file.to_owned())
            }
            None => {}
        }
        // Written beside the target and renamed, so a killed download never leaves a partial file.
        tokio::fs::create_dir_all(path.parent().expect("has a parent"))
            .await
            .map_err(io)?;
        let part = dir.join(format!("{file}.part"));
        tokio::fs::write(&part, &bytes).await.map_err(io)?;
        tokio::fs::rename(&part, &path).await.map_err(io)?;
        if want.is_some() {
            tokio::fs::write(&marker, &got).await.map_err(io)?;
        }
        Ok(bytes.to_vec())
    };
    let config_bytes = get("config.json").await?;
    let optional = async |file: &str| {
        if exists(file) {
            get(file).await
        } else {
            Ok(b"{}".to_vec())
        }
    };
    let tokenizer = TokenizerFiles {
        tokenizer_file: get("tokenizer.json").await?,
        config_file: config_bytes.clone(),
        special_tokens_map_file: optional("special_tokens_map.json").await?,
        tokenizer_config_file: optional("tokenizer_config.json").await?,
    };
    let config = serde_json::from_slice(&config_bytes)
        .map_err(|e| Error::Dependency(format!("bad config.json: {e}")))?;
    let graph = get(onnx).await?;
    let data_file = format!("{onnx}_data");
    let external = if exists(&data_file) {
        let name = data_file.rsplit('/').next().expect("has a name").to_owned();
        Some((name, get(&data_file).await?))
    } else {
        None
    };
    let weight_bytes = (graph.len() + external.as_ref().map(|e| e.1.len()).unwrap_or(0)) as u64;
    // The licence comes from the model card; cached next to the files for offline use.
    let licence_file = dir.join(".licence");
    let licence = match tokio::fs::read_to_string(&licence_file).await {
        Ok(l) => l,
        Err(_) => {
            let card_of = async |id: &str| -> Value {
                match http
                    .get(format!("https://huggingface.co/api/models/{id}"))
                    .send()
                    .await
                {
                    Ok(r) => r.json().await.unwrap_or(Value::Null),
                    Err(_) => Value::Null,
                }
            };
            let card = card_of(&format!("{repo}/revision/{revision}")).await;
            // A conversion often declares no licence of its own; it inherits its base model's.
            let l = match card["cardData"]["license"].as_str() {
                Some(l) => l.to_owned(),
                None => match card["cardData"]["base_model"]
                    .as_array()
                    .and_then(|b| b.first()?.as_str().map(str::to_owned))
                    .or_else(|| card["cardData"]["base_model"].as_str().map(str::to_owned))
                {
                    Some(base) => card_of(&base).await["cardData"]["license"]
                        .as_str()
                        .map(|l| format!("{l} (from base model {base})"))
                        .unwrap_or_else(|| "unknown".into()),
                    None => "unknown".into(),
                },
            };
            let _ = tokio::fs::write(&licence_file, &l).await;
            l
        }
    };
    let _ = tokio::fs::write(&marker, b"").await;
    let mut unchecked = unchecked.into_inner().expect("lock");
    unchecked.sort();
    unchecked.dedup();
    Ok(HfFiles {
        onnx_path: dir.join(onnx),
        onnx: graph,
        external,
        tokenizer,
        config,
        revision: revision.to_owned(),
        licence,
        weight_bytes,
        unchecked,
    })
}

/// A model whose ONNX graph returns its own pooled embedding (`output=`) and may declare extra
/// inputs for other modalities (EmbeddingGemma 2: image, video and audio features), which get
/// empty `[0, d]` tensors for text.
struct Raw {
    dims: usize,
    session: ort::session::Session,
    tokenizer: tokenizers::Tokenizer,
    output: String,
    empty: Vec<(String, usize)>,
    type_ids: bool,
}

impl Raw {
    fn load(files: &HfFiles, output: &str, dims: usize) -> Result<Raw> {
        let session = ort::session::Session::builder()
            .and_then(|b| Ok(b.with_intra_threads(threads())?))
            .and_then(|mut b| Ok(b.commit_from_file(&files.onnx_path)?))
            .map_err(|e| Error::Dependency(format!("model load failed: {e}")))?;
        let mut tokenizer = tokenizers::Tokenizer::from_bytes(&files.tokenizer.tokenizer_file)
            .map_err(|e| Error::Dependency(format!("bad tokenizer.json: {e}")))?;
        if tokenizer.get_padding().is_none() {
            let pad_token = ["<pad>", "[PAD]"]
                .into_iter()
                .find(|t| tokenizer.token_to_id(t).is_some())
                .unwrap_or("<pad>")
                .to_owned();
            let pad_id = tokenizer.token_to_id(&pad_token).unwrap_or(0);
            tokenizer.with_padding(Some(tokenizers::PaddingParams {
                pad_id,
                pad_token,
                ..Default::default()
            }));
        }
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: 512,
                ..Default::default()
            }))
            .map_err(|e| Error::Dependency(format!("tokenizer: {e}")))?;
        let mut empty = vec![];
        let mut type_ids = false;
        for input in session.inputs() {
            match input.name() {
                "input_ids" | "attention_mask" => {}
                "token_type_ids" => type_ids = true,
                name => {
                    let ort::value::ValueType::Tensor { shape, .. } = input.dtype() else {
                        return Err(Error::Dependency(format!("unsupported model input {name}")));
                    };
                    let d = shape.last().copied().filter(|d| *d > 0).ok_or_else(|| {
                        Error::Dependency(format!("model input {name} has no fixed last dimension"))
                    })?;
                    empty.push((name.to_owned(), d as usize));
                }
            }
        }
        if !session.outputs().iter().any(|o| o.name() == output) {
            return Err(Error::Dependency(format!("model has no output {output}")));
        }
        Ok(Raw {
            dims,
            session,
            tokenizer,
            output: output.to_owned(),
            empty,
            type_ids,
        })
    }

    fn embed(&mut self, texts: &[String]) -> std::result::Result<Vec<Vec<f32>>, String> {
        let enc = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| e.to_string())?;
        let (b, s) = (
            enc.len(),
            enc.first().map(|e| e.get_ids().len()).unwrap_or(0),
        );
        let ids: Vec<i64> = enc
            .iter()
            .flat_map(|e| e.get_ids().iter().map(|&x| x as i64))
            .collect();
        let mask: Vec<i64> = enc
            .iter()
            .flat_map(|e| e.get_attention_mask().iter().map(|&x| x as i64))
            .collect();
        let tensor =
            |v: Vec<i64>| ort::value::Tensor::from_array(([b, s], v)).map(|t| t.into_dyn());
        let mut inputs: Vec<(
            std::borrow::Cow<'static, str>,
            ort::session::SessionInputValue<'static>,
        )> = vec![
            (
                "input_ids".into(),
                tensor(ids).map_err(|e| e.to_string())?.into(),
            ),
            (
                "attention_mask".into(),
                tensor(mask).map_err(|e| e.to_string())?.into(),
            ),
        ];
        if self.type_ids {
            inputs.push((
                "token_type_ids".into(),
                tensor(vec![0; b * s]).map_err(|e| e.to_string())?.into(),
            ));
        }
        for (name, d) in &self.empty {
            let t = ort::value::Tensor::<f32>::from_array(([0usize, *d], Vec::<f32>::new()))
                .map_err(|e| e.to_string())?;
            inputs.push((name.clone().into(), t.into_dyn().into()));
        }
        let out = self.session.run(inputs).map_err(|e| e.to_string())?;
        let (shape, data) = out[self.output.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        let dim = *shape.last().ok_or("empty output shape")? as usize;
        if dim != self.dims {
            return Err(format!(
                "model output has {dim} dimensions, the spec says dims={}",
                self.dims
            ));
        }
        Ok(data
            .chunks(dim)
            .map(|v| {
                let n = v
                    .iter()
                    .map(|x| x * x)
                    .sum::<f32>()
                    .sqrt()
                    .max(f32::MIN_POSITIVE);
                v.iter().map(|x| x / n).collect()
            })
            .collect())
    }
}

enum Backend {
    Raw {
        model: Arc<Mutex<Raw>>,
        query_prefix: &'static str,
        document_prefix: &'static str,
    },
    Local {
        model: Arc<Mutex<TextEmbedding>>,
        query_prefix: &'static str,
        document_prefix: &'static str,
    },
    /// `truncate`: keep the first `dimensions` values and re-normalise (Matryoshka models).
    OpenAi {
        url: String,
        model: String,
        key: Option<String>,
        query_body: Value,
        document_body: Value,
        truncate: bool,
    },
    Cloudflare {
        url: String,
        token: String,
    },
}

pub struct Embedder {
    backend: Backend,
    space: EmbeddingSpace,
    pub spec: String,
    http: reqwest::Client,
}

type CustomRerank = dyn Fn(&str, &[String]) -> Result<Vec<f32>> + Send + Sync;

enum RerankBackend {
    Local(Arc<Mutex<TextRerank>>),
    Custom(Arc<CustomRerank>),
}

pub struct Reranker {
    model: RerankBackend,
    pub name: String,
    pub spec: String,
}

fn bad(spec: &str, why: &str) -> Error {
    Error::InvalidInput(format!("model spec {spec:?}: {why}"))
}

/// `rest` split into the part before `?` and its query parameters.
fn params(rest: &str) -> (&str, Vec<(String, String)>) {
    match rest.split_once('?') {
        None => (rest, vec![]),
        Some((head, q)) => (
            head,
            q.split('&')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        ),
    }
}

fn param<'a>(ps: &'a [(String, String)], key: &str) -> Option<&'a str> {
    ps.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn dims(spec: &str, ps: &[(String, String)]) -> Result<usize> {
    param(ps, "dims")
        .and_then(|d| d.parse().ok())
        .ok_or_else(|| bad(spec, "needs ?dims=<n>"))
}

impl Embedder {
    /// Parses a spec and loads the model; `Ok(None)` for `none`. No remote call is made here.
    pub async fn from_spec(spec: &str) -> Result<Option<Embedder>> {
        let spec = spec.trim();
        if spec == "none" {
            return Ok(None);
        }
        let (scheme, rest) = spec
            .split_once(':')
            .ok_or_else(|| bad(spec, "expected <scheme>:<model>"))?;
        let http = reqwest::Client::new();
        let (backend, space) = match scheme {
            "hf" if preset_for(rest).is_none() => {
                let (repo, ps) = params(rest);
                let revision = param(&ps, "revision").unwrap_or("main");
                let dtype = param(&ps, "dtype").unwrap_or("fp32");
                // `pooling=none` with `output=<name>` reads a model's own pooled output (EmbeddingGemma 2:
                // `sentence_embedding`).
                let pooling = match param(&ps, "pooling").unwrap_or("mean") {
                    "mean" => Some(Pooling::Mean),
                    "cls" => Some(Pooling::Cls),
                    "none" => None,
                    _ => return Err(bad(spec, "pooling is mean, cls or none")),
                };
                let files = hf_files(repo, revision, dtype).await?;
                let dimensions = match param(&ps, "dims") {
                    Some(d) => d.parse().map_err(|_| bad(spec, "dims is a number"))?,
                    None => files.config["hidden_size"]
                        .as_u64()
                        .ok_or_else(|| bad(spec, "config.json has no hidden_size; pass ?dims="))?
                        as usize,
                };
                let (query_prefix, document_prefix) = prompts(repo);
                // fastembed refuses batches for "dynamic" quantized models; the reference build batches
                // the same q8 file 16 at a time, so it is loaded as a plain model here too.
                if pooling.is_none() {
                    let output = param(&ps, "output").ok_or_else(|| {
                        bad(
                            spec,
                            "pooling=none needs output=<name of the model's pooled output>",
                        )
                    })?;
                    let raw = Raw::load(&files, output, dimensions)?;
                    let space = EmbeddingSpace {
                        model: format!("{repo}#{dtype}"),
                        revision: files.revision.clone(),
                        dimensions,
                        normalized: true,
                        preprocessing: format!(
                            "model output {output}; prompts query={query_prefix:?} document={document_prefix:?}"
                        ),
                    };
                    return Ok(Some(Embedder {
                        backend: Backend::Raw {
                            model: Arc::new(Mutex::new(raw)),
                            query_prefix,
                            document_prefix,
                        },
                        space,
                        spec: spec.to_owned(),
                        http,
                    }));
                }
                let mut user = UserDefinedEmbeddingModel::new(files.onnx, files.tokenizer);
                user.pooling = pooling.clone();
                user.quantization = QuantizationMode::None;
                if let Some((name, buffer)) = files.external {
                    user = user.with_external_initializer(name, buffer);
                }
                if let Some(out) = param(&ps, "output") {
                    // The model lives for the whole process, so its output name may too.
                    user.output_key = Some(fastembed::OutputKey::ByName(Box::leak(
                        out.to_owned().into_boxed_str(),
                    )));
                }
                let model = TextEmbedding::try_new_from_user_defined(
                    user,
                    InitOptionsUserDefined::default().with_intra_threads(threads()),
                )
                .map_err(|e| Error::Dependency(format!("model load failed: {e}")))?;
                (
                    Backend::Local {
                        model: Arc::new(Mutex::new(model)),
                        query_prefix,
                        document_prefix,
                    },
                    EmbeddingSpace {
                        model: format!("{repo}#{dtype}"),
                        revision: files.revision,
                        dimensions,
                        normalized: true,
                        preprocessing: format!(
                            "{} pooling; prefixes query={query_prefix:?} document={document_prefix:?}",
                            pooling
                                .map(|p| format!("{p:?}").to_lowercase())
                                .unwrap_or_else(|| format!(
                                    "model output {}",
                                    param(&ps, "output").unwrap_or("default")
                                ))
                        ),
                    },
                )
            }
            "hf" => {
                let info = preset_for(rest).expect("checked above");
                let (query_prefix, document_prefix) = prompts(&info.model_code);
                let model = TextEmbedding::try_new(
                    TextInitOptions::new(info.model.clone())
                        .with_cache_dir(cache_dir())
                        .with_show_download_progress(false)
                        .with_intra_threads(threads()),
                )
                .map_err(|e| Error::Dependency(format!("model load failed: {e}")))?;
                let revision = snapshot_of(&info.model_code).unwrap_or_else(|| "unknown".into());
                (
                    Backend::Local {
                        model: Arc::new(Mutex::new(model)),
                        query_prefix,
                        document_prefix,
                    },
                    EmbeddingSpace {
                        model: format!("{}#fastembed-7.0.1", info.model_code),
                        revision,
                        dimensions: info.dim,
                        normalized: true,
                        preprocessing: format!(
                            "fastembed default pooling; prefixes query={query_prefix:?} document={document_prefix:?}"
                        ),
                    },
                )
            }
            "http" | "nvidia" => {
                let (head, ps) = params(rest);
                let (url, model, key, qb, db) = if scheme == "http" {
                    let model = param(&ps, "model")
                        .ok_or_else(|| bad(spec, "needs ?model=<name>"))?
                        .to_owned();
                    (
                        head.to_owned(),
                        model,
                        std::env::var("HYPERAG_EMBEDDING_API_KEY").ok(),
                        json!({}),
                        json!({}),
                    )
                } else {
                    (
                        "https://integrate.api.nvidia.com".to_owned(),
                        head.to_owned(),
                        std::env::var("NVIDIA_API_KEY").ok(),
                        json!({ "input_type": "query" }),
                        json!({ "input_type": "passage" }),
                    )
                };
                let space = EmbeddingSpace {
                    model: format!("{url}|{model}"),
                    revision: param(&ps, "revision")
                        .unwrap_or("provider-managed")
                        .to_owned(),
                    dimensions: dims(spec, &ps)?,
                    normalized: param(&ps, "normalized") == Some("true"),
                    preprocessing: format!("query={qb} document={db}"),
                };
                let truncate = param(&ps, "truncate") == Some("true");
                (
                    Backend::OpenAi {
                        url: format!("{}/v1/embeddings", url.trim_end_matches('/')),
                        model,
                        key,
                        query_body: qb,
                        document_body: db,
                        truncate,
                    },
                    space,
                )
            }
            "cloudflare" => {
                let (model, ps) = params(rest);
                let account = std::env::var("CLOUDFLARE_ACCOUNT_ID")
                    .map_err(|_| bad(spec, "CLOUDFLARE_ACCOUNT_ID is not set"))?;
                let token = std::env::var("CLOUDFLARE_API_TOKEN")
                    .map_err(|_| bad(spec, "CLOUDFLARE_API_TOKEN is not set"))?;
                let space = EmbeddingSpace {
                    model: format!("cloudflare|{model}"),
                    revision: "provider-managed".into(),
                    dimensions: dims(spec, &ps)?,
                    normalized: false,
                    preprocessing: "none".into(),
                };
                (
                    Backend::Cloudflare {
                        url: format!(
                            "https://api.cloudflare.com/client/v4/accounts/{account}/ai/run/{model}"
                        ),
                        token,
                    },
                    space,
                )
            }
            _ => return Err(bad(spec, "scheme is hf, http, nvidia, cloudflare or none")),
        };
        Ok(Some(Embedder {
            backend,
            space,
            spec: spec.to_owned(),
            http,
        }))
    }

    pub fn describe(&self) -> &EmbeddingSpace {
        &self.space
    }

    pub async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts, false).await
    }

    pub async fn embed_query(&self, question: &str) -> Result<Vec<f32>> {
        Ok(self.embed(&[question.to_owned()], true).await?.remove(0))
    }

    async fn embed(&self, texts: &[String], query: bool) -> Result<Vec<Vec<f32>>> {
        let vectors = match &self.backend {
            Backend::Raw {
                model,
                query_prefix,
                document_prefix,
            } => {
                let prefix = if query {
                    *query_prefix
                } else {
                    *document_prefix
                };
                let inputs: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
                let model = model.clone();
                tokio::task::spawn_blocking(move || {
                    let mut m = model.lock().expect("embedder lock");
                    let mut out = Vec::with_capacity(inputs.len());
                    for chunk in inputs.chunks(8) {
                        out.extend(m.embed(chunk)?);
                    }
                    Ok::<_, String>(out)
                })
                .await
                .map_err(|e| Error::Dependency(e.to_string()))?
                .map_err(|e| Error::Dependency(format!("embedder_failed: {e}")))?
            }
            Backend::Local {
                model,
                query_prefix,
                document_prefix,
            } => {
                let prefix = if query {
                    *query_prefix
                } else {
                    *document_prefix
                };
                let inputs: Vec<String> = texts.iter().map(|t| format!("{prefix}{t}")).collect();
                let model = model.clone();
                tokio::task::spawn_blocking(move || {
                    model.lock().expect("embedder lock").embed(inputs, Some(4))
                })
                .await
                .map_err(|e| Error::Dependency(e.to_string()))?
                .map_err(|e| Error::Dependency(format!("embedder_failed: {e}")))?
            }
            Backend::OpenAi {
                url,
                model,
                key,
                query_body,
                document_body,
                truncate,
            } => {
                let mut body = json!({ "model": model, "input": texts });
                for (k, v) in (if query { query_body } else { document_body })
                    .as_object()
                    .into_iter()
                    .flatten()
                {
                    body[k] = v.clone();
                }
                let mut req = self.http.post(url).json(&body);
                if let Some(key) = key {
                    req = req.bearer_auth(key);
                }
                let reply: Value = req
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .map_err(fail)?
                    .json()
                    .await
                    .map_err(fail)?;
                let mut rows: Vec<(u64, Vec<f32>)> = reply["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| (r["index"].as_u64().unwrap_or(0), floats(&r["embedding"])))
                    .collect();
                rows.sort_by_key(|r| r.0);
                let keep = self.space.dimensions;
                rows.into_iter()
                    .map(|(_, v)| {
                        if !*truncate || v.len() <= keep {
                            return v;
                        }
                        let head = &v[..keep];
                        let n = head
                            .iter()
                            .map(|x| x * x)
                            .sum::<f32>()
                            .sqrt()
                            .max(f32::MIN_POSITIVE);
                        head.iter().map(|x| x / n).collect()
                    })
                    .collect()
            }
            Backend::Cloudflare { url, token } => {
                let reply: Value = self
                    .http
                    .post(url)
                    .bearer_auth(token)
                    .json(&json!({ "text": texts }))
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .map_err(fail)?
                    .json()
                    .await
                    .map_err(fail)?;
                reply["result"]["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(floats)
                    .collect()
            }
        };
        // One vector per input, each of the declared dimension, or the call failed.
        if vectors.len() != texts.len()
            || vectors
                .iter()
                .any(|v| v.len() != self.space.dimensions || !v.iter().all(|x| x.is_finite()))
        {
            return Err(Error::Dependency(format!(
                "embedder_failed: expected {} vectors of {} dimensions",
                texts.len(),
                self.space.dimensions
            )));
        }
        Ok(vectors)
    }
}

/// The query and document prompts a model was trained with, by its family.
fn prompts(model: &str) -> (&'static str, &'static str) {
    let m = model.to_lowercase();
    if m.contains("embeddinggemma") {
        ("task: search result | query: ", "title: none | text: ")
    } else if m.contains("e5") {
        ("query: ", "passage: ")
    } else {
        ("", "")
    }
}

/// True when a spec loads straight from a Hugging Face repository (not a fastembed preset), so
/// Hyperag downloads and checks its files itself.
pub fn is_direct_hf(spec: &str) -> bool {
    spec.strip_prefix("hf:")
        .is_some_and(|rest| preset_for(rest).is_none())
}

/// A fastembed preset for `<repo>` or `<repo>?dtype=fp32|q8|q4` when one exists; any other option
/// (revision, pooling) means the repository is loaded directly instead.
fn preset_for(rest: &str) -> Option<fastembed::ModelInfo<EmbeddingModel>> {
    let (repo, ps) = params(rest);
    if ps.iter().any(|(k, _)| k != "dtype") {
        return None;
    }
    let file = match param(&ps, "dtype") {
        None => return preset(repo),
        Some("fp32") => "onnx/model.onnx",
        Some("q8") => "onnx/model_quantized.onnx",
        Some("q4") => "onnx/model_q4.onnx",
        Some(_) => return None,
    };
    TextEmbedding::list_supported_models()
        .into_iter()
        .find(|m| m.model_code.eq_ignore_ascii_case(repo) && m.model_file == file)
}

fn preset(name: &str) -> Option<fastembed::ModelInfo<EmbeddingModel>> {
    TextEmbedding::list_supported_models()
        .into_iter()
        .find(|m| m.model_code.eq_ignore_ascii_case(name) || alias(name) == Some(m.model.clone()))
}

/// Short names a person would type for the common models.
fn alias(name: &str) -> Option<EmbeddingModel> {
    match name.to_ascii_lowercase().as_str() {
        "baai/bge-small-en-v1.5" => Some(EmbeddingModel::BGESmallENV15),
        "intfloat/multilingual-e5-small" => Some(EmbeddingModel::MultilingualE5Small),
        _ => None,
    }
}

fn fail(e: reqwest::Error) -> Error {
    Error::Dependency(format!("embedder_failed: {e}"))
}

fn floats(v: &Value) -> Vec<f32> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_f64)
        .map(|x| x as f32)
        .collect()
}

/// Only the re-ranker uses this override; embedding sessions retain their existing provider.
fn reranker_execution_providers(
    requested: Option<&str>,
) -> Result<Vec<ort::ep::ExecutionProviderDispatch>> {
    match requested.unwrap_or("cpu") {
        "cpu" => Ok(vec![]),
        "coreml" => {
            #[cfg(all(feature = "coreml", target_os = "macos"))]
            {
                Ok(vec![ort::ep::CoreML::default().build().error_on_failure()])
            }
            #[cfg(not(all(feature = "coreml", target_os = "macos")))]
            {
                Err(Error::InvalidInput(
                    "CoreML re-ranking requires macOS and a --features coreml build".into(),
                ))
            }
        }
        _ => Err(Error::InvalidInput(
            "HYPERAG_RERANK_PROVIDER must be cpu or coreml".into(),
        )),
    }
}

impl Reranker {
    pub async fn from_spec(spec: &str) -> Result<Option<Reranker>> {
        let spec = spec.trim();
        if spec == "none" {
            return Ok(None);
        }
        let rest = spec
            .strip_prefix("hf:")
            .ok_or_else(|| bad(spec, "a re-ranker is hf:<model> or none"))?;
        let providers =
            reranker_execution_providers(std::env::var("HYPERAG_RERANK_PROVIDER").ok().as_deref())?;
        if rest.contains('?') {
            let (repo, ps) = params(rest);
            let revision = param(&ps, "revision").unwrap_or("main");
            let dtype = param(&ps, "dtype").unwrap_or("fp32");
            let files = hf_files(repo, revision, dtype).await?;
            if files.external.is_some() {
                return Err(bad(
                    spec,
                    "this re-ranker export keeps its weights in a separate file; pick a dtype with a single file (q8)",
                ));
            }
            let model = TextRerank::try_new_from_user_defined(
                UserDefinedRerankingModel::new(files.onnx, files.tokenizer),
                RerankInitOptionsUserDefined::default()
                    .with_intra_threads(threads())
                    .with_execution_providers(providers),
            )
            .map_err(|e| Error::Dependency(format!("re-ranker load failed: {e}")))?;
            return Ok(Some(Reranker {
                model: RerankBackend::Local(Arc::new(Mutex::new(model))),
                name: format!("{repo}@{}#{dtype}", &revision[..revision.len().min(7)]),
                spec: spec.to_owned(),
            }));
        }
        let info = TextRerank::list_supported_models()
            .into_iter()
            .find(|m| m.model_code.eq_ignore_ascii_case(rest))
            .ok_or_else(|| bad(spec, "not a local fastembed re-ranker"))?;
        let model: RerankerModel = info.model.clone();
        let loaded = TextRerank::try_new(
            RerankInitOptions::new(model)
                .with_cache_dir(cache_dir())
                .with_show_download_progress(false)
                .with_intra_threads(threads())
                .with_execution_providers(providers),
        )
        .map_err(|e| Error::Dependency(format!("re-ranker load failed: {e}")))?;
        let revision = snapshot_of(&info.model_code).unwrap_or_else(|| "unknown".into());
        Ok(Some(Reranker {
            model: RerankBackend::Local(Arc::new(Mutex::new(loaded))),
            name: format!(
                "{}@{}#fastembed-7.0.1",
                info.model_code,
                &revision[..revision.len().min(7)]
            ),
            spec: spec.to_owned(),
        }))
    }

    pub fn custom(
        name: impl Into<String>,
        spec: impl Into<String>,
        scorer: impl Fn(&str, &[String]) -> Result<Vec<f32>> + Send + Sync + 'static,
    ) -> Reranker {
        Reranker {
            model: RerankBackend::Custom(Arc::new(scorer)),
            name: name.into(),
            spec: spec.into(),
        }
    }

    /// One score per document, in the documents' order; higher is more relevant.
    pub async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f32>> {
        if documents.is_empty() {
            return Ok(vec![]);
        }
        let model = match &self.model {
            RerankBackend::Custom(scorer) => return scorer(query, documents),
            RerankBackend::Local(model) => model.clone(),
        };
        let (q, docs) = (query.to_owned(), documents.to_vec());
        let results = tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = docs.iter().map(String::as_str).collect();
            model
                .lock()
                .expect("reranker lock")
                .rerank(q.as_str(), refs, false, Some(8))
        })
        .await
        .map_err(|e| Error::Dependency(e.to_string()))?
        .map_err(|e| Error::Dependency(format!("reranker_failed: {e}")))?;
        let mut scores = vec![f32::NEG_INFINITY; documents.len()];
        for r in results {
            if let Some(s) = scores.get_mut(r.index) {
                *s = r.score;
            }
        }
        Ok(scores)
    }
}

/// The local models a spec can name, for `hyperag models list --available`.
pub fn available() -> Vec<(String, usize)> {
    TextEmbedding::list_supported_models()
        .into_iter()
        .map(|m| (format!("hf:{}", m.model_code), m.dim))
        .collect()
}

pub fn available_rerankers() -> Vec<String> {
    TextRerank::list_supported_models()
        .into_iter()
        .map(|m| format!("hf:{}", m.model_code))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_parsing() {
        assert_eq!(params("Xenova/x?revision=abc&dtype=q8").0, "Xenova/x");
        assert_eq!(param(&params("m?dims=768").1, "dims"), Some("768"));
        assert!(matches!(
            dims("http:x", &params("u?model=m").1),
            Err(Error::InvalidInput(_))
        ));
        assert!(preset("intfloat/multilingual-e5-small").is_some());
        assert!(preset("BAAI/bge-small-en-v1.5").is_some());
        assert!(preset("someone/unknown-model").is_none());
        let q8 = preset_for("onnx-community/embeddinggemma-300m-ONNX?dtype=q8").unwrap();
        assert_eq!(
            (q8.model_file.as_str(), q8.dim),
            ("onnx/model_quantized.onnx", 768)
        );
        assert!(preset_for("Xenova/multilingual-e5-small?revision=abc&dtype=q8").is_none());
        assert_eq!(
            prompts("onnx-community/embeddinggemma-300m-ONNX").0,
            "task: search result | query: "
        );
        assert_eq!(prompts("intfloat/multilingual-e5-small").1, "passage: ");
    }

    #[tokio::test]
    async fn none_and_bad_specs() {
        assert!(Embedder::from_spec("none").await.unwrap().is_none());
        assert!(Reranker::from_spec("none").await.unwrap().is_none());
        assert!(matches!(
            Embedder::from_spec("ftp:x").await,
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            Embedder::from_spec("http:http://h?model=m").await,
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            Reranker::from_spec("bge").await,
            Err(Error::InvalidInput(_))
        ));
    }
}

#[cfg(test)]
mod reranker_parity {
    use super::*;

    /// Scores HYPERAG_PAIRS (a JSON list of [query, document]) with HYPERAG_SPEC and prints the
    /// scores, the time per pair and the time of one batch of 24. Run by hand: it downloads models.
    #[tokio::test]
    #[ignore]
    async fn score_pairs() {
        let pairs: Vec<(String, String)> = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("HYPERAG_PAIRS").unwrap()).unwrap(),
        )
        .unwrap();
        let r = Reranker::from_spec(&std::env::var("HYPERAG_SPEC").unwrap())
            .await
            .unwrap()
            .unwrap();
        let mut scores = vec![];
        let t = std::time::Instant::now();
        // Pairs of one query are not grouped in the file, so each pair is scored alone.
        for (q, d) in &pairs {
            scores.push(r.rerank(q, std::slice::from_ref(d)).await.unwrap()[0]);
        }
        let per_pair = t.elapsed().as_secs_f64() * 1000.0 / pairs.len() as f64;
        let t2 = std::time::Instant::now();
        let (q0, _) = &pairs[0];
        let docs: Vec<String> = pairs.iter().take(24).map(|(_, d)| d.clone()).collect();
        r.rerank(q0, &docs).await.unwrap();
        println!("SCORES {}", serde_json::to_string(&scores).unwrap());
        println!(
            "TIMING single {per_pair:.1} ms/pair, batch of 24 {:.1} ms",
            t2.elapsed().as_secs_f64() * 1000.0
        );
    }
}

#[cfg(test)]
mod rerank_provider_tests {
    use super::*;

    #[test]
    fn cpu_default_and_explicit_cpu_keep_existing_provider_list() {
        assert!(reranker_execution_providers(None).unwrap().is_empty());
        assert!(
            reranker_execution_providers(Some("cpu"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn unknown_provider_is_rejected_before_loading_any_model() {
        assert!(matches!(
            reranker_execution_providers(Some("coremll")),
            Err(Error::InvalidInput(_))
        ));
    }

    #[cfg(not(all(feature = "coreml", target_os = "macos")))]
    #[test]
    fn coreml_request_without_apple_build_support_is_explicit_error() {
        assert!(matches!(
            reranker_execution_providers(Some("coreml")),
            Err(Error::InvalidInput(_))
        ));
    }

    #[cfg(all(feature = "coreml", target_os = "macos"))]
    #[test]
    fn coreml_request_selects_only_coreml_and_registration_must_succeed() {
        let providers = reranker_execution_providers(Some("coreml")).unwrap();
        assert_eq!(providers.len(), 1);
        assert!(providers[0].downcast_ref::<ort::ep::CoreML>().is_some());
        assert!(format!("{:?}", providers[0]).contains("error_on_failure: true"));
    }
    #[cfg(all(feature = "coreml", target_os = "macos"))]
    #[test]
    #[ignore = "native Apple CoreML registration probe; run explicitly under the local resource gate"]
    fn native_coreml_provider_is_available_and_registers() {
        use ort::ep::ExecutionProvider;
        assert!(ort::ep::CoreML::default().is_available().unwrap());
        let _builder = ort::session::Session::builder()
            .unwrap()
            .with_execution_providers(reranker_execution_providers(Some("coreml")).unwrap())
            .unwrap();
    }
}
