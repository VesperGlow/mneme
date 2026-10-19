"""Pinned CPU-only model runtime. No model weights are sent to an external API."""
import os
import logging
from collections import OrderedDict

import torch
import torch.nn.functional as F
from transformers import AutoConfig, AutoModel, AutoModelForSequenceClassification, AutoTokenizer

from artifacts import EMBED_MODEL, EMBED_REVISION, RERANK_MODEL, RERANK_REVISION, CODE_REVISION, prepare_cache

LOG = logging.getLogger("uvicorn.error")
INSTRUCTION = "Given a Chinese conversation query, retrieve relevant personal memories and facts about the user."


class Engine:
    def __init__(self):
        torch.set_num_threads(int(os.getenv("MNEME_CPU_THREADS", "3")))
        torch.set_num_interop_threads(1)
        self.dtype_name = os.getenv("MNEME_CPU_DTYPE", "float32")
        if self.dtype_name not in ("float32", "bfloat16"):
            raise ValueError("MNEME_CPU_DTYPE must be float32 or bfloat16")
        dtype = getattr(torch, self.dtype_name)
        # Reduced-precision reranker logits can flip a marginal relevance score.
        # Keep the smaller reranker in FP32 unless explicitly evaluated otherwise.
        self.rerank_dtype_name = os.getenv("MNEME_CPU_RERANK_DTYPE", "float32")
        if self.rerank_dtype_name not in ("float32", "bfloat16"):
            raise ValueError("MNEME_CPU_RERANK_DTYPE must be float32 or bfloat16")
        rerank_dtype = getattr(torch, self.rerank_dtype_name)
        cache = prepare_cache()
        # All remote I/O and source retries finish before Transformers runs.
        # This also prevents custom-code imports from bypassing failover.
        common = {"local_files_only": True, "cache_dir": str(cache)}
        LOG.info("loading CPU embedding model=%s dtype=%s", EMBED_MODEL, self.dtype_name)
        self.embed_tokenizer = AutoTokenizer.from_pretrained(
            EMBED_MODEL, revision=EMBED_REVISION, padding_side="left", **common
        )
        self.embed_model = AutoModel.from_pretrained(
            EMBED_MODEL, revision=EMBED_REVISION, torch_dtype=dtype,
            attn_implementation="sdpa", low_cpu_mem_usage=True, use_safetensors=True, **common
        ).cpu().eval()
        LOG.info("CPU embedding ready; loading reranker model=%s dtype=%s", RERANK_MODEL, self.rerank_dtype_name)
        self.rerank_tokenizer = AutoTokenizer.from_pretrained(
            RERANK_MODEL, revision=RERANK_REVISION, **common
        )
        # This model uses upstream custom Python. Both weights and code are pinned.
        config = AutoConfig.from_pretrained(
            RERANK_MODEL, revision=RERANK_REVISION, code_revision=CODE_REVISION,
            trust_remote_code=True, **common
        )
        config.use_memory_efficient_attention = False
        config.unpad_inputs = False
        self.rerank_model = AutoModelForSequenceClassification.from_pretrained(
            RERANK_MODEL, revision=RERANK_REVISION, code_revision=CODE_REVISION,
            config=config, trust_remote_code=True, torch_dtype=rerank_dtype,
            low_cpu_mem_usage=True, use_safetensors=True, **common
        ).cpu().eval()
        self.cache = OrderedDict()
        LOG.info("CPU embedding and reranker ready")

    @torch.inference_mode()
    def embed(self, texts, query=False, with_usage=False):
        result = []
        counts = {}
        prior = set(self.cache)
        # Two small documents at a time keep peak CPU attention memory bounded.
        missing = [text for text in dict.fromkeys(texts) if (query, text) not in self.cache]
        for start in range(0, len(missing), 2):
            items = missing[start:start + 2]
            inputs = [f"Instruct: {INSTRUCTION}\nQuery: {text}" if query else text for text in items]
            batch = self.embed_tokenizer(inputs, padding=True, truncation=True, max_length=512, return_tensors="pt")
            counts.update(zip(items, batch["attention_mask"].sum(dim=1).tolist()))
            hidden = self.embed_model(**batch).last_hidden_state
            # Left padding guarantees that the final position is the last real token.
            vectors = F.normalize(hidden[:, -1, :].float(), p=2, dim=1).tolist()
            for text, vector in zip(items, vectors):
                self.cache[(query, text)] = (vector, counts[text])
        for text in texts:
            key = (query, text)
            self.cache.move_to_end(key)
            result.append(self.cache[key][0])
        tokens, cached = 0, 0
        for text in texts:
            key = (query, text)
            count = self.cache[key][1]
            tokens += count
            if key in prior:
                cached += count
            prior.add(key)
        while len(self.cache) > 256:
            self.cache.popitem(last=False)
        if with_usage:
            return {"embeddings": result, "model": EMBED_MODEL, "usage": {
                "prompt_tokens": tokens, "completion_tokens": 0, "total_tokens": tokens,
                "prompt_tokens_details": {"cached_tokens": cached},
                "tokenizer": EMBED_MODEL, "source": "model_tokenizer", "max_length": 512}}
        return result

    @torch.inference_mode()
    def rerank(self, query, documents, with_usage=False):
        scores = [0.0] * len(documents)
        pairs = [[query, document] for document in documents]
        encoded = self.rerank_tokenizer(pairs, padding=False, truncation=True, max_length=512)
        tokens = sum(sum(mask) for mask in encoded['attention_mask'])
        # Bucket by actual truncated token length. Padding a short pair to a
        # long path's shape wastes quadratic attention work on the CPU.
        order = sorted(range(len(documents)), key=lambda i: len(encoded['input_ids'][i]))
        for start in range(0, len(order), 4):
            indices = order[start:start + 4]
            batch = self.rerank_tokenizer.pad([{k: v[i] for k, v in encoded.items()} for i in indices],
                                               padding=True, return_tensors="pt")
            logits = self.rerank_model(**batch, return_dict=True).logits.view(-1).float()
            for index, score in zip(indices, torch.sigmoid(logits).tolist()):
                scores[index] = score
        if with_usage:
            return {"scores": scores, "model": RERANK_MODEL, "usage": {
                "prompt_tokens": tokens, "completion_tokens": 0, "total_tokens": tokens,
                "tokenizer": RERANK_MODEL, "source": "model_tokenizer", "max_length": 512}}
        return scores
