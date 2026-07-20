//! Rerank：本地 ONNX 交叉编码器（默认 Qwen3-Reranker 家族）做二段精排。
//! 一段余弦召回候选后，把 (query, 候选文本) 成对喂给它联合打分重排。
//!
//! 设计原则：**永不因重排而中断检索**。未启用、模型下载/加载失败、或推理出错，
//! 一律返回 None，由调用方保持一段余弦的原始顺序。所以即使重排模型还没配好，
//! 系统也能以「append-only + 余弦」正常工作。

use std::sync::Arc;

use crate::config::Config;

pub struct Reranker {
    cfg: Arc<Config>,
    // Some(model) = 加载成功；None = 已尝试但失败（不再重试，恒回退）。只初始化一次。
    #[cfg(feature = "onnx")]
    model: tokio::sync::OnceCell<Option<Arc<local::LocalReranker>>>,
    /// 推理串行化：并发会把激活内存翻倍，逐条排队（与 embedding 同策略）。
    #[cfg(feature = "onnx")]
    infer_lock: tokio::sync::Mutex<()>,
}

impl Reranker {
    pub fn new(cfg: Arc<Config>) -> Self {
        Self {
            cfg,
            #[cfg(feature = "onnx")]
            model: tokio::sync::OnceCell::new(),
            #[cfg(feature = "onnx")]
            infer_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.cfg.rerank_enabled && cfg!(feature = "onnx")
    }

    #[cfg(feature = "onnx")]
    async fn model(&self) -> Option<Arc<local::LocalReranker>> {
        self.model
            .get_or_init(|| async {
                match local::LocalReranker::load(&self.cfg).await {
                    Ok(model) => Some(model),
                    Err(error) => {
                        tracing::warn!(
                            "重排模型加载失败，本次运行将回退到纯余弦顺序：{error:#}"
                        );
                        None
                    }
                }
            })
            .await
            .clone()
    }

    /// 给每个候选打一个「与 query 的相关性」分数（越大越相关），与输入等长。
    /// 返回 None 表示无法重排（未启用/加载失败/推理失败），调用方保持原顺序。
    pub async fn scores(&self, query: &str, docs: &[String]) -> Option<Vec<f32>> {
        if !self.cfg.rerank_enabled || docs.is_empty() {
            return None;
        }
        self.scores_impl(query, docs).await
    }

    #[cfg(feature = "onnx")]
    async fn scores_impl(&self, query: &str, docs: &[String]) -> Option<Vec<f32>> {
        let model = self.model().await?;
        let _guard = self.infer_lock.lock().await;
        let instruction = self.cfg.rerank_instruction.clone();
        let query = query.to_string();
        let docs = docs.to_vec();
        match tokio::task::spawn_blocking(move || model.score_batch(&instruction, &query, &docs))
            .await
        {
            Ok(Ok(scores)) => Some(scores),
            Ok(Err(error)) => {
                tracing::warn!("重排推理失败，回退到余弦顺序：{error:#}");
                None
            }
            Err(_) => None,
        }
    }

    #[cfg(not(feature = "onnx"))]
    async fn scores_impl(&self, _query: &str, _docs: &[String]) -> Option<Vec<f32>> {
        None
    }

    /// 启动预热：触发下载/加载并跑一次打分，避免首条消息卡顿。失败只告警、不致命。
    pub async fn warmup(&self) {
        if !self.cfg.rerank_enabled {
            return;
        }
        let _ = self
            .scores("warmup", &["warmup document".to_string()])
            .await;
    }
}

#[cfg(feature = "onnx")]
mod local {
    use std::sync::Arc;

    use anyhow::{anyhow, bail, Context, Result};
    use tokenizers::Tokenizer;

    use crate::config::Config;
    use crate::embedding::{collect_onnx, pick_onnx_path};

    fn ort_err(e: impl std::fmt::Display) -> anyhow::Error {
        anyhow!("ONNX 重排推理错误：{e}")
    }

    pub struct LocalReranker {
        session: std::sync::Mutex<ort::session::InMemorySession<'static>>,
        tokenizer: Tokenizer,
        yes_id: Option<usize>,
        no_id: Option<usize>,
        context_size: usize,
        // KV cache 维度（Qwen3-Reranker 是因果 LM，ONNX 会要 past_key_values 输入）。
        kv_heads: usize,
        head_dim: usize,
    }

    impl LocalReranker {
        pub async fn load(cfg: &Config) -> Result<Arc<Self>> {
            let dir = crate::embedding::ensure_model_files(
                &cfg.rerank_model,
                &cfg.hf_token,
                &cfg.rerank_onnx_file,
            )
            .await?;
            let cfg = cfg.clone();
            tokio::task::spawn_blocking(move || Self::load_sync(&cfg, &dir)).await?
        }

        fn load_sync(cfg: &Config, dir: &std::path::Path) -> Result<Arc<Self>> {
            // 递归找已下载的 onnx（onnx-community 放在 onnx/ 子目录）并按偏好选一个。
            let model_path = pick_onnx_path(collect_onnx(dir), &cfg.rerank_onnx_file)
                .ok_or_else(|| anyhow!("{} 里没有 .onnx 文件", dir.display()))?;

            let mut tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
                .map_err(|e| anyhow!("加载 tokenizer 失败：{e}"))?;
            tokenizer
                .with_truncation(Some(tokenizers::TruncationParams {
                    max_length: cfg.rerank_context_size,
                    ..Default::default()
                }))
                .map_err(|e| anyhow!("配置截断失败：{e}"))?;
            // 因果 LM 式重排（Qwen3-Reranker）读末位对 "yes"/"no" 的 logit；
            // 序列分类式导出没有这两个 token，交给输出形状分支处理即可。
            let yes_id = tokenizer.token_to_id("yes").map(|v| v as usize);
            let no_id = tokenizer.token_to_id("no").map(|v| v as usize);
            let (kv_heads, head_dim) = read_kv_dims(dir);

            let file = std::fs::File::open(&model_path)
                .with_context(|| format!("打开重排模型文件失败：{}", model_path.display()))?;
            // mmap + 直接引用映射内存：与 embedding 同策略，省掉权重副本的启动内存峰值。
            let mmap: &'static memmap2::Mmap =
                Box::leak(Box::new(unsafe { memmap2::Mmap::map(&file)? }));
            let session = ort::session::Session::builder()
                .map_err(ort_err)?
                .with_intra_threads(cfg.rerank_threads)
                .map_err(ort_err)?
                .with_memory_pattern(false)
                .map_err(ort_err)?
                .with_config_entry("session.disable_prepacking", "1")
                .map_err(ort_err)?
                .commit_from_memory_directly(&mmap[..])
                .map_err(ort_err)?;
            tracing::info!(
                "本地重排模型加载完成（mmap，kv_heads={kv_heads} head_dim={head_dim}）：{}",
                model_path.display()
            );
            Ok(Arc::new(Self {
                session: std::sync::Mutex::new(session),
                tokenizer,
                yes_id,
                no_id,
                context_size: cfg.rerank_context_size,
                kv_heads,
                head_dim,
            }))
        }

        pub fn score_batch(
            &self,
            instruction: &str,
            query: &str,
            docs: &[String],
        ) -> Result<Vec<f32>> {
            let mut scores = Vec::with_capacity(docs.len());
            for doc in docs {
                scores.push(self.score_one(instruction, query, doc)?);
            }
            Ok(scores)
        }

        fn score_one(&self, instruction: &str, query: &str, doc: &str) -> Result<f32> {
            let prompt = build_prompt(instruction, query, doc);
            let encoding = self
                .tokenizer
                .encode(prompt, false)
                .map_err(|e| anyhow!("tokenize 失败：{e}"))?;
            let mut ids: Vec<i64> = encoding.get_ids().iter().map(|id| *id as i64).collect();
            if ids.len() > self.context_size {
                ids.truncate(self.context_size);
            }
            let len = ids.len();
            if len == 0 {
                bail!("重排输入被 tokenize 成空序列");
            }
            let attention: Vec<i64> = vec![1; len];
            let positions: Vec<i64> = (0..len as i64).collect();

            let mut session = self.session.lock().map_err(|_| anyhow!("推理会话锁中毒"))?;
            let input_names: Vec<String> =
                session.inputs().iter().map(|i| i.name().to_string()).collect();
            let output_names: Vec<String> =
                session.outputs().iter().map(|o| o.name().to_string()).collect();
            let mut feeds: Vec<(String, ort::value::DynValue)> = Vec::new();
            for name in &input_names {
                let n = name.as_str();
                let tensor: ort::value::DynValue = if n == "input_ids" {
                    ort::value::Tensor::from_array(([1usize, len], ids.clone()))
                        .map_err(ort_err)?
                        .into_dyn()
                } else if n == "attention_mask" {
                    ort::value::Tensor::from_array(([1usize, len], attention.clone()))
                        .map_err(ort_err)?
                        .into_dyn()
                } else if n == "position_ids" {
                    ort::value::Tensor::from_array(([1usize, len], positions.clone()))
                        .map_err(ort_err)?
                        .into_dyn()
                } else if n.starts_with("past_key_values") || n.starts_with("past_key")
                    || n.starts_with("past_value")
                {
                    // 因果 LM 的 ONNX 要 past_key_values 输入；单次（唯一）前向用空 KV
                    //（序列长度 0）= 对整段 prompt 直接 prefill，这正是 transformers.js 的做法。
                    if self.kv_heads == 0 || self.head_dim == 0 {
                        bail!("重排模型需要 KV cache 输入，但 config.json 缺 num_key_value_heads/head_dim");
                    }
                    ort::value::Tensor::from_array((
                        [1usize, self.kv_heads, 0usize, self.head_dim],
                        Vec::<f32>::new(),
                    ))
                    .map_err(ort_err)?
                    .into_dyn()
                } else {
                    // 遇到真正不认识的输入直接报错，由上层回退余弦——宁可退化不喂错张量。
                    bail!("重排 ONNX 需要未支持的输入 {n}");
                };
                feeds.push((name.clone(), tensor));
            }
            let mut run_options = ort::session::RunOptions::new().map_err(ort_err)?;
            run_options
                .add_config_entry("memory.enable_memory_arena_shrinkage", "cpu:0")
                .map_err(ort_err)?;
            let outputs = session.run_with_options(feeds, &run_options).map_err(ort_err)?;
            // 取名为 logits 的输出（因果 LM 还会输出一堆 present.* KV，忽略）。
            let idx = output_names.iter().position(|n| n == "logits").unwrap_or(0);
            let output = &outputs[idx];
            let (shape, data) = output.try_extract_tensor::<f32>().map_err(ort_err)?;
            self.interpret(&shape.to_vec(), data)
        }

        /// 把输出张量折成一个相关性分数（越大越相关）。自动适配三种导出：
        /// - 因果 LM logits `[1, (seq,) vocab]`：取末位 token 对 yes/no 的 logit 做二分 softmax；
        /// - 序列分类 `[1, 1]`：单 logit 过 sigmoid；
        /// - 序列分类 `[1, 2]`：两类 logit softmax，取正类。
        /// 三者都是单调映射，只用于排序，绝对值无所谓。
        fn interpret(&self, dims: &[i64], data: &[f32]) -> Result<f32> {
            let last = *dims.last().ok_or_else(|| anyhow!("重排输出无维度"))? as usize;
            if last == 0 || data.is_empty() {
                bail!("重排输出为空");
            }
            if last == 1 {
                let logit = data[data.len() - 1];
                return Ok(sigmoid(logit));
            }
            if last == 2 && dims.len() == 2 {
                let a = data[data.len() - 2];
                let b = data[data.len() - 1];
                return Ok(softmax2(b, a));
            }
            // 其余按 vocab 处理：末位 token 的 logit 行就是 data 的最后 `last` 个元素
            //（无论形状是 [1, vocab] 还是 [1, seq, vocab]）。
            let (yes, no) = (
                self.yes_id.context("重排模型缺少 yes token，无法用因果 LM 方式打分")?,
                self.no_id.context("重排模型缺少 no token，无法用因果 LM 方式打分")?,
            );
            if yes >= last || no >= last {
                bail!("yes/no token id 超出 logits 维度");
            }
            let row = &data[data.len() - last..];
            Ok(softmax2(row[yes], row[no]))
        }
    }

    /// 从 config.json 读 KV cache 维度：优先显式 head_dim，否则 hidden_size/num_attention_heads。
    /// 读不到返回 (0,0)，score_one 会在需要 KV 输入时据此报错并让上层回退余弦。
    fn read_kv_dims(dir: &std::path::Path) -> (usize, usize) {
        let Ok(text) = std::fs::read_to_string(dir.join("config.json")) else {
            return (0, 0);
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            return (0, 0);
        };
        let kv = v["num_key_value_heads"]
            .as_u64()
            .or_else(|| v["num_attention_heads"].as_u64())
            .unwrap_or(0) as usize;
        let head_dim = v["head_dim"].as_u64().map(|x| x as usize).unwrap_or_else(|| {
            let hidden = v["hidden_size"].as_u64().unwrap_or(0) as usize;
            let heads = v["num_attention_heads"].as_u64().unwrap_or(0) as usize;
            if heads > 0 {
                hidden / heads
            } else {
                0
            }
        });
        (kv, head_dim)
    }

    fn sigmoid(x: f32) -> f32 {
        1.0 / (1.0 + (-x).exp())
    }

    /// 目标类相对另一类的 softmax 概率：exp(a)/(exp(a)+exp(b))，数值稳定版。
    fn softmax2(a: f32, b: f32) -> f32 {
        let m = a.max(b);
        let (ea, eb) = ((a - m).exp(), (b - m).exp());
        ea / (ea + eb)
    }

    /// Qwen3-Reranker 官方判定格式：系统固定指令 + 用户段（Instruct/Query/Document）+
    /// 助手前缀（含空 think 块），模型在末位预测 yes/no。
    fn build_prompt(instruction: &str, query: &str, doc: &str) -> String {
        format!(
            "<|im_start|>system\nJudge whether the Document meets the requirements based on the Query and the Instruct provided. Note that the answer can only be \"yes\" or \"no\".<|im_end|>\n<|im_start|>user\n<Instruct>: {instruction}\n<Query>: {query}\n<Document>: {doc}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
        )
    }
}
