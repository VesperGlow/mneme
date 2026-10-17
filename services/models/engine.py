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
        cache = prepare_cache()
        # All remote I/O and source retries finish before Transformers runs.
        # This also prevents custom-code imports from bypassing failover.
        common = {"local_files_only": True, "cache_dir": str(cache)}
        LOG.info("loading CPU embedding model=%s dtype=float32", EMBED_MODEL)
        self.embed_tokenizer = AutoTokenizer.from_pretrained(
            EMBED_MODEL, revision=EMBED_REVISION, padding_side="left", **common
        )
        self.embed_model = AutoModel.from_pretrained(
            EMBED_MODEL, revision=EMBED_REVISION, torch_dtype=torch.float32,
            attn_implementation="sdpa", low_cpu_mem_usage=True, use_safetensors=True, **common
        ).cpu().eval()
        LOG.info("CPU embedding ready; loading reranker model=%s dtype=float32", RERANK_MODEL)
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
            config=config, trust_remote_code=True, torch_dtype=torch.float32,
            low_cpu_mem_usage=True, use_safetensors=True, **common
        ).cpu().eval()
        self.cache = OrderedDict()
        LOG.info("CPU embedding and reranker ready")

    @torch.inference_mode()
    def embed(self, texts, query=False):
        result = []
        # Two small documents at a time keep peak CPU attention memory bounded.
        missing = [(i, text) for i, text in enumerate(texts) if (query, text) not in self.cache]
        for start in range(0, len(missing), 2):
            items = missing[start:start + 2]
            inputs = [f"Instruct: {INSTRUCTION}\nQuery: {text}" if query else text for _, text in items]
            batch = self.embed_tokenizer(inputs, padding=True, truncation=True, max_length=512, return_tensors="pt")
            hidden = self.embed_model(**batch).last_hidden_state
            # Left padding guarantees that the final position is the last real token.
            vectors = F.normalize(hidden[:, -1, :], p=2, dim=1).float().tolist()
            for (_, text), vector in zip(items, vectors):
                self.cache[(query, text)] = vector
        for text in texts:
            key = (query, text)
            self.cache.move_to_end(key)
            result.append(self.cache[key])
        while len(self.cache) > 256:
            self.cache.popitem(last=False)
        return result

    @torch.inference_mode()
    def rerank(self, query, documents):
        scores = []
        for start in range(0, len(documents), 4):
            pairs = [[query, document] for document in documents[start:start + 4]]
            batch = self.rerank_tokenizer(pairs, padding=True, truncation=True, max_length=512, return_tensors="pt")
            logits = self.rerank_model(**batch, return_dict=True).logits.view(-1).float()
            scores.extend(torch.sigmoid(logits).tolist())
        return scores
