//! Embedding：默认进程内 ONNX 推理（onnx feature），或远程 OpenAI-compatible 接口。
//! 模型下载到 `$HF_HOME/local/<owner--repo>/`，已有缓存直接复用。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};

use crate::config::{Config, EmbeddingStyle};

pub fn normalize_and_resize(mut vector: Vec<f32>, dimensions: usize) -> Result<Vec<f32>> {
    if vector.len() < dimensions {
        bail!(
            "Embedding 返回 {} 维，但 EMBEDDING_DIMENSIONS={dimensions}",
            vector.len()
        );
    }
    vector.truncate(dimensions);
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if !norm.is_finite() || norm == 0.0 {
        bail!("Embedding 返回了无效的零向量或非有限数值");
    }
    for value in &mut vector {
        *value /= norm;
    }
    Ok(vector)
}

/// 把 uint8 非对称线性量化的输出还原为 float32。
/// 量化把 [out_min, out_max] 线性映射到 [0, 255]；余弦相似度对平移不封闭，
/// 必须先还原原值再归一化，不能直接拿 uint8 点积。
pub fn dequantize(values: &[u8], out_min: f32, out_max: f32) -> Vec<f32> {
    let scale = (out_max - out_min) / 255.0;
    values.iter().map(|v| *v as f32 * scale + out_min).collect()
}

pub(crate) fn hf_home() -> PathBuf {
    if let Ok(home) = std::env::var("HF_HOME") {
        return PathBuf::from(home);
    }
    let base = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join(".cache").join("huggingface")
}

/// 随模型一起下载的元数据文件（tokenizer / config，均取仓库根）。onnx 单独按变体选取。
pub(crate) fn wanted_meta(name: &str) -> bool {
    matches!(
        name,
        "tokenizer.json"
            | "tokenizer_config.json"
            | "special_tokens_map.json"
            | "added_tokens.json"
            | "config.json"
    )
}

/// 从候选 onnx 文件名里按偏好选一个：先匹配 `prefer` 子串（如 "quantized"），
/// 再按量化/体积从小到大的经验顺序，最后回退到 model.onnx 或第一个。
/// onnx-community 这类仓库把多份量化变体放进 onnx/ 子目录，必须挑一个而不是全下。
pub(crate) fn pick_onnx_name<'a>(names: &'a [String], prefer: &str) -> Option<&'a String> {
    if !prefer.is_empty() {
        let pl = prefer.to_lowercase();
        if let Some(n) = names.iter().find(|n| n.to_lowercase().contains(pl.as_str())) {
            return Some(n);
        }
    }
    for key in ["uint8", "quantized", "q4f16", "q4", "int8", "fp16"] {
        if let Some(n) = names.iter().find(|n| n.to_lowercase().contains(key)) {
            return Some(n);
        }
    }
    names
        .iter()
        .find(|n| n.to_lowercase().ends_with("model.onnx"))
        .or_else(|| names.first())
}

/// 递归收集目录下所有 .onnx 文件（onnx 可能在 onnx/ 子目录）。
pub(crate) fn collect_onnx(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(collect_onnx(&path));
            } else if path.extension().map_or(false, |ext| ext.eq_ignore_ascii_case("onnx")) {
                out.push(path);
            }
        }
    }
    out
}

/// 从磁盘上已下载的多个 onnx 里按偏好选一个（复用 [`pick_onnx_name`] 的文件名偏好）。
pub(crate) fn pick_onnx_path(paths: Vec<PathBuf>, prefer: &str) -> Option<PathBuf> {
    if paths.len() <= 1 {
        return paths.into_iter().next();
    }
    let names: Vec<String> = paths
        .iter()
        .map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
        .collect();
    let chosen = pick_onnx_name(&names, prefer)?.clone();
    paths.into_iter().find(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .as_deref()
            == Some(chosen.as_str())
    })
}

/// 下载模型仓库需要的文件（tokenizer/config + 按 `prefer_onnx` 选中的**单个** onnx 变体
/// 及其外部权重），已存在的跳过，返回目录路径。embedding 与 reranker 共用；变体可能在
/// onnx/ 子目录，本地保留相同的相对路径。
pub(crate) async fn ensure_model_files(
    model: &str,
    hf_token: &str,
    prefer_onnx: &str,
) -> Result<PathBuf> {
    let target = hf_home().join("local").join(model.replace('/', "--"));
    tokio::fs::create_dir_all(&target).await?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1800))
        .build()?;
    let auth = |req: reqwest::RequestBuilder| {
        if hf_token.is_empty() {
            req
        } else {
            req.bearer_auth(hf_token)
        }
    };

    // 已有 tokenizer.json + 任意 onnx（含子目录）就不碰网络（离线也能重启）。
    let have_tokenizer = tokio::fs::try_exists(target.join("tokenizer.json"))
        .await
        .unwrap_or(false);
    if have_tokenizer && !collect_onnx(&target).is_empty() {
        return Ok(target);
    }

    tracing::info!("正在下载模型 {model} ...");
    let listing: serde_json::Value = auth(client.get(format!(
        "https://huggingface.co/api/models/{model}"
    )))
    .send()
    .await?
    .error_for_status()
    .with_context(|| format!("查询模型仓库 {model} 失败"))?
    .json()
    .await?;
    let siblings: Vec<String> = listing["siblings"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["rfilename"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    // 元数据只取仓库根（避免各变体子目录里重复的 config）。
    let mut to_get: Vec<String> = siblings
        .iter()
        .filter(|n| wanted_meta(n) && !n.contains('/'))
        .cloned()
        .collect();
    // onnx 变体（可能在 onnx/ 子目录）按偏好选一个，连同其外部权重数据一起下。
    let onnx_names: Vec<String> = siblings
        .iter()
        .filter(|n| n.to_lowercase().ends_with(".onnx"))
        .cloned()
        .collect();
    let chosen = pick_onnx_name(&onnx_names, prefer_onnx)
        .ok_or_else(|| anyhow!("模型仓库 {model} 里没有 .onnx 文件"))?
        .clone();
    let stem = chosen.trim_end_matches(".onnx");
    to_get.push(chosen.clone());
    for name in &siblings {
        if *name != chosen
            && name.starts_with(stem)
            && (name.ends_with(".onnx_data") || name.ends_with(".onnx.data"))
        {
            to_get.push(name.clone());
        }
    }

    use tokio::io::AsyncWriteExt;
    for name in to_get {
        let path = target.join(&name);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            continue;
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tracing::info!("下载 {name} ...");
        // 流式落盘：模型文件可达数百 MB，整块读进内存再写盘会造成首次启动的瞬时内存峰值。
        let mut response = auth(client.get(format!(
            "https://huggingface.co/{model}/resolve/main/{name}"
        )))
        .send()
        .await?
        .error_for_status()
        .with_context(|| format!("下载 {name} 失败"))?;
        let tmp = path.with_file_name(format!(
            "{}.part",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        let mut file = tokio::fs::File::create(&tmp).await?;
        while let Some(chunk) = response.chunk().await? {
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        drop(file);
        tokio::fs::rename(&tmp, &path).await?;
    }
    Ok(target)
}

#[cfg(feature = "onnx")]
mod local {
    use super::*;
    use tokenizers::Tokenizer;

    /// ort 的错误类型带非 Send/Sync 的状态泛型，不能直接 `?` 进 anyhow，统一转文本。
    fn ort_err(e: impl std::fmt::Display) -> anyhow::Error {
        anyhow!("ONNX 推理错误：{e}")
    }

    pub struct LocalModel {
        // InMemorySession 借用 mmap 的模型字节（'static：mmap 有意泄漏、与进程同寿）。
        session: std::sync::Mutex<ort::session::InMemorySession<'static>>,
        tokenizer: Tokenizer,
        eos_id: Option<u32>,
        output_min: f32,
        output_max: f32,
        context_size: usize,
    }

    impl LocalModel {
        pub async fn load(cfg: &Config) -> Result<Arc<Self>> {
            let dir = ensure_model_files(&cfg.embedding_model, &cfg.hf_token, "").await?;
            let cfg = cfg.clone();
            tokio::task::spawn_blocking(move || Self::load_sync(&cfg, &dir)).await?
        }

        fn load_sync(cfg: &Config, dir: &std::path::Path) -> Result<Arc<Self>> {
            // 递归找已下载的 onnx（可能在 onnx/ 子目录）并选一个。
            let model_path = pick_onnx_path(collect_onnx(dir), "")
                .ok_or_else(|| anyhow!("{} 里没有 .onnx 文件", dir.display()))?;

            let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
                .map_err(|e| anyhow!("加载 tokenizer 失败：{e}"))?;
            tokenizer
                .with_truncation(Some(tokenizers::TruncationParams {
                    max_length: cfg.embedding_context_size,
                    ..Default::default()
                }))
                .map_err(|e| anyhow!("配置截断失败：{e}"))?;
            let eos_id = ["<|endoftext|>", "</s>", "<|im_end|>"]
                .iter()
                .find_map(|token| tokenizer.token_to_id(token));

            // mmap 模型文件并让 ORT 直接引用映射内存：权重成为文件页缓存
            // （内存紧张时可回收，不算硬占用），避免"protobuf 缓冲 + 权重副本"
            // 双份常驻——2GB 小内存机器的启动 OOM 就是这个瞬时峰值造成的。
            // 泄漏 mmap 是有意的：模型与进程同生命周期。
            let file = std::fs::File::open(&model_path)
                .with_context(|| format!("打开模型文件失败：{}", model_path.display()))?;
            let mmap: &'static memmap2::Mmap =
                Box::leak(Box::new(unsafe { memmap2::Mmap::map(&file)? }));
            let session = ort::session::Session::builder()
                .map_err(ort_err)?
                .with_intra_threads(cfg.embedding_threads)
                .map_err(ort_err)?
                .with_memory_pattern(false)
                .map_err(ort_err)?
                // 不把权重预打包成优化布局的副本，省几百 MB 峰值，推理略慢可接受。
                .with_config_entry("session.disable_prepacking", "1")
                .map_err(ort_err)?
                .commit_from_memory_directly(&mmap[..])
                .map_err(ort_err)?;
            tracing::info!("本地 embedding 模型加载完成（mmap）：{}", model_path.display());
            Ok(Arc::new(Self {
                session: std::sync::Mutex::new(session),
                tokenizer,
                eos_id,
                output_min: cfg.embedding_output_min,
                output_max: cfg.embedding_output_max,
                context_size: cfg.embedding_context_size,
            }))
        }

        pub fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
            let encoding = self
                .tokenizer
                .encode(text, true)
                .map_err(|e| anyhow!("tokenize 失败：{e}"))?;
            let mut ids: Vec<i64> = encoding.get_ids().iter().map(|id| *id as i64).collect();
            // Qwen3-Embedding 约定输入以 EOS 结尾（最后 token 池化取的就是它）。
            if let Some(eos) = self.eos_id {
                if ids.last() != Some(&(eos as i64)) {
                    ids.truncate(self.context_size - 1);
                    ids.push(eos as i64);
                }
            }
            let len = ids.len();
            let attention: Vec<i64> = vec![1; len];
            let positions: Vec<i64> = (0..len as i64).collect();

            let mut session = self.session.lock().map_err(|_| anyhow!("推理会话锁中毒"))?;
            let mut feeds: Vec<(String, ort::value::DynValue)> = Vec::new();
            let input_names: Vec<String> =
                session.inputs().iter().map(|i| i.name().to_string()).collect();
            for name in &input_names {
                let tensor = match name.as_str() {
                    "input_ids" => ort::value::Tensor::from_array(([1usize, len], ids.clone())),
                    "attention_mask" => {
                        ort::value::Tensor::from_array(([1usize, len], attention.clone()))
                    }
                    "position_ids" => {
                        ort::value::Tensor::from_array(([1usize, len], positions.clone()))
                    }
                    other => bail!("ONNX 模型需要未知输入 {other}"),
                };
                feeds.push((name.clone(), tensor.map_err(ort_err)?.into_dyn()));
            }
            // 每次 run 后收缩 CPU arena：arena 默认只涨不还，进程 RSS 会永久停在
            // 历史最长输入的激活峰值上。推理本就串行（infer_lock），收缩没有并发复用损失。
            let mut run_options = ort::session::RunOptions::new().map_err(ort_err)?;
            run_options
                .add_config_entry("memory.enable_memory_arena_shrinkage", "cpu:0")
                .map_err(ort_err)?;
            let outputs = session.run_with_options(feeds, &run_options).map_err(ort_err)?;
            let output = &outputs[0];

            // 输出可能是 [batch, dim]（图内已池化）或 [batch, seq, dim]（需取最后 token）；
            // dtype 可能是 uint8（量化输出）或 float32。只转换需要的那一段，
            // 免得先把整个 [1, seq, dim] 拷成 Vec 再切片。
            let last_token = |dims: &[i64]| -> Result<std::ops::Range<usize>> {
                match dims {
                    [_, dim] => Ok(0..*dim as usize),
                    [_, seq, dim] => {
                        let (seq, dim) = (*seq as usize, *dim as usize);
                        Ok((seq - 1) * dim..seq * dim)
                    }
                    other => bail!("无法理解的 embedding 输出形状：{other:?}"),
                }
            };
            let hidden: Vec<f32> = if let Ok((shape, data)) = output.try_extract_tensor::<u8>() {
                let range = last_token(&shape.to_vec())?;
                dequantize(&data[range], self.output_min, self.output_max)
            } else {
                let (shape, data) = output.try_extract_tensor::<f32>().map_err(ort_err)?;
                data[last_token(&shape.to_vec())?].to_vec()
            };
            Ok(hidden)
        }
    }
}

pub struct Embedder {
    cfg: Arc<Config>,
    http: reqwest::Client,
    #[cfg(feature = "onnx")]
    local: tokio::sync::OnceCell<Arc<local::LocalModel>>,
    /// 推理串行化：并发请求会把激活值内存翻倍，逐条排队。
    infer_lock: tokio::sync::Mutex<()>,
}

impl Embedder {
    pub fn new(cfg: Arc<Config>) -> Result<Self> {
        #[cfg(not(feature = "onnx"))]
        if cfg.embedding_api_style == EmbeddingStyle::Local {
            bail!("此二进制编译时未启用 onnx feature，只支持 EMBEDDING_API_STYLE=openai");
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs_f64(
                    cfg.embedding_timeout_seconds,
                ))
                .build()?,
            cfg,
            #[cfg(feature = "onnx")]
            local: tokio::sync::OnceCell::new(),
            infer_lock: tokio::sync::Mutex::new(()),
        })
    }

    #[cfg(feature = "onnx")]
    async fn local_model(&self) -> Result<Arc<local::LocalModel>> {
        self.local
            .get_or_try_init(|| local::LocalModel::load(&self.cfg))
            .await
            .cloned()
    }

    /// 启动时预热：下载/加载模型并跑一次推理，避免首条消息长时间等待。
    pub async fn warmup(&self) -> Result<()> {
        if self.cfg.embedding_api_style != EmbeddingStyle::Local {
            return Ok(());
        }
        #[cfg(feature = "onnx")]
        {
            let model = self.local_model().await?;
            let _guard = self.infer_lock.lock().await;
            tokio::task::spawn_blocking(move || model.embed_one("warmup")).await??;
        }
        Ok(())
    }

    pub fn ready(&self) -> bool {
        match self.cfg.embedding_api_style {
            #[cfg(feature = "onnx")]
            EmbeddingStyle::Local => self.local.initialized(),
            #[cfg(not(feature = "onnx"))]
            EmbeddingStyle::Local => false,
            EmbeddingStyle::OpenAi => true,
        }
    }

    pub async fn embed(&self, texts: &[String], is_query: bool) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut prepared: Vec<String> = Vec::with_capacity(texts.len());
        for text in texts {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                bail!("不能向量化空文本");
            }
            let instruction = self.cfg.embedding_query_instruction.trim();
            if is_query && !instruction.is_empty() {
                prepared.push(format!("Instruct: {instruction}\nQuery: {trimmed}"));
            } else {
                prepared.push(trimmed.to_string());
            }
        }

        let raw: Vec<Vec<f32>> = match self.cfg.embedding_api_style {
            EmbeddingStyle::Local => {
                #[cfg(feature = "onnx")]
                {
                    let model = self.local_model().await?;
                    let _guard = self.infer_lock.lock().await;
                    let mut vectors = Vec::with_capacity(prepared.len());
                    for text in prepared {
                        let model = model.clone();
                        vectors.push(
                            tokio::task::spawn_blocking(move || model.embed_one(&text)).await??,
                        );
                    }
                    vectors
                }
                #[cfg(not(feature = "onnx"))]
                bail!("此二进制编译时未启用 onnx feature")
            }
            EmbeddingStyle::OpenAi => self.embed_openai(&prepared).await?,
        };
        raw.into_iter()
            .map(|vector| normalize_and_resize(vector, self.cfg.embedding_dimensions))
            .collect()
    }

    async fn embed_openai(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let base = &self.cfg.embedding_base_url;
        if base.is_empty() {
            bail!("EMBEDDING_API_STYLE=openai 时必须设置 EMBEDDING_BASE_URL");
        }
        let url = if base.ends_with("/embeddings") {
            base.clone()
        } else {
            format!("{base}/embeddings")
        };
        let mut payload = serde_json::json!({
            "model": self.cfg.embedding_model,
            "input": texts,
            "dimensions": self.cfg.embedding_dimensions,
        });
        let mut request = self.http.post(&url).json(&payload);
        if !self.cfg.embedding_api_key.is_empty() {
            request = request.bearer_auth(&self.cfg.embedding_api_key);
        }
        let mut response = request.send().await?;
        if response.status() == reqwest::StatusCode::BAD_REQUEST {
            let text = response.text().await.unwrap_or_default();
            if text.to_lowercase().contains("dimension") {
                payload.as_object_mut().unwrap().remove("dimensions");
                let mut retry = self.http.post(&url).json(&payload);
                if !self.cfg.embedding_api_key.is_empty() {
                    retry = retry.bearer_auth(&self.cfg.embedding_api_key);
                }
                response = retry.send().await?;
            } else {
                bail!("OpenAI-compatible Embedding 请求失败：{}", &text[..text.len().min(1000)]);
            }
        }
        let body: serde_json::Value = response.error_for_status()?.json().await?;
        let mut data: Vec<(i64, Vec<f32>)> = body["data"]
            .as_array()
            .context("Embedding 接口返回格式不正确")?
            .iter()
            .map(|item| {
                let index = item["index"].as_i64().unwrap_or(0);
                let vector: Vec<f32> = item["embedding"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|v| v.as_f64())
                            .map(|v| v as f32)
                            .collect()
                    })
                    .unwrap_or_default();
                (index, vector)
            })
            .collect();
        data.sort_by_key(|(index, _)| *index);
        let vectors: Vec<Vec<f32>> = data.into_iter().map(|(_, v)| v).collect();
        if vectors.len() != texts.len() || vectors.iter().any(|v| v.is_empty()) {
            bail!("Embedding 接口返回的向量数量或格式不正确");
        }
        Ok(vectors)
    }
}

/// 真实模型冒烟：下载 uint8 量化的 Qwen3-Embedding 并验证语义检索方向正确。
/// 模型约 640MB，默认跳过；CI 显式设 RUN_EMBEDDING_SMOKE=1 运行（模型目录有缓存）。
#[cfg(all(test, feature = "onnx"))]
mod smoke_tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn real_model_semantic_sanity() {
        if std::env::var("RUN_EMBEDDING_SMOKE").is_err() {
            eprintln!("RUN_EMBEDDING_SMOKE 未设置，跳过真模型冒烟");
            return;
        }
        let mut cfg = Config::from_env().unwrap();
        cfg.embedding_context_size = 512;
        let embedder = Embedder::new(Arc::new(cfg.clone())).unwrap();
        let docs = embedder
            .embed(
                &[
                    "用户养了一只叫年糕的猫".to_string(),
                    "用户家里有只小猫咪".to_string(),
                    "用户今天买了新键盘".to_string(),
                ],
                false,
            )
            .await
            .unwrap();
        let dot = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        for vector in &docs {
            assert_eq!(vector.len(), cfg.embedding_dimensions);
            let norm: f32 = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-3);
        }
        // 语义方向：两句猫的相似度必须高于猫 vs 键盘
        assert!(dot(&docs[0], &docs[1]) > dot(&docs[0], &docs[2]) + 0.05);
        // 查询走 instruction 前缀路径
        let query = embedder
            .embed(&["我的宠物叫什么名字".to_string()], true)
            .await
            .unwrap();
        assert!(dot(&query[0], &docs[0]) > dot(&query[0], &docs[2]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dequantize_recovers_calibration_range() {
        let restored = dequantize(&[0, 255], -0.3009, 0.3952);
        assert!((restored[0] - (-0.3009)).abs() < 1e-6);
        assert!((restored[1] - 0.3952).abs() < 1e-6);
    }

    #[test]
    fn matryoshka_resize_renormalizes() {
        let vector = normalize_and_resize(vec![3.0, 4.0, 99.0], 2).unwrap();
        assert!((vector[0] - 0.6).abs() < 1e-6);
        assert!((vector[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn zero_vector_rejected() {
        assert!(normalize_and_resize(vec![0.0, 0.0], 2).is_err());
    }
}
