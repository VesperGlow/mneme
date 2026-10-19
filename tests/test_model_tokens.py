"""Offline tokenizer accounting; run in the existing models image, no model inference."""
import importlib.util
import os
from pathlib import Path
import sys
import types
import unittest


@unittest.skipUnless(importlib.util.find_spec('transformers') and importlib.util.find_spec('torch'),
                     'requires model-service dependencies and pinned local tokenizers')
class TokenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        sys.path.insert(0, str(Path(os.environ.get('MNEME_MODEL_APP', 'services/models/app.py')).parent))
        import torch
        from transformers import AutoTokenizer
        from artifacts import EMBED_MODEL, EMBED_REVISION, RERANK_MODEL, RERANK_REVISION, prepare_cache
        from engine import Engine, INSTRUCTION
        from collections import OrderedDict
        cls.torch, cls.instruction = torch, INSTRUCTION
        cls.engine = Engine.__new__(Engine)
        common = dict(local_files_only=True, cache_dir=str(prepare_cache()))
        cls.engine.embed_tokenizer = AutoTokenizer.from_pretrained(EMBED_MODEL, revision=EMBED_REVISION, padding_side='left', **common)
        cls.engine.rerank_tokenizer = AutoTokenizer.from_pretrained(RERANK_MODEL, revision=RERANK_REVISION, **common)
        cls.engine.cache = OrderedDict()
        def embed(**batch):
            shape = batch['input_ids'].shape
            return types.SimpleNamespace(last_hidden_state=torch.ones((*shape, 4)))
        def rerank(**batch):
            return types.SimpleNamespace(logits=torch.zeros(len(batch['input_ids'])))
        cls.engine.embed_model, cls.engine.rerank_model = embed, rerank

    def test_embedding_counts_instruction_truncation_without_padding_and_reports_cache(self):
        texts = ['中文短句', 'a long English sentence. ' * 400]
        inputs = [f'Instruct: {self.instruction}\nQuery: {t}' for t in texts]
        expected = sum(len(self.engine.embed_tokenizer.encode(t, truncation=True, max_length=512)) for t in inputs)
        self.engine.cache.clear()
        first = self.engine.embed(texts, True, with_usage=True)
        self.assertEqual(first['usage']['prompt_tokens'], expected)
        self.assertEqual(first['usage']['prompt_tokens_details']['cached_tokens'], 0)
        self.assertLess(expected, 1024)
        self.assertGreater(expected, 512)
        second = self.engine.embed(texts, True, with_usage=True)
        self.assertEqual(second['usage']['prompt_tokens_details']['cached_tokens'], expected)
        self.assertEqual(first['embeddings'], second['embeddings'])
        self.assertEqual(second['usage']['completion_tokens'], 0)

    def test_reranker_counts_query_for_each_pair_and_excludes_padding(self):
        query, docs = '用于测试的查询', ['很短', 'another document ' * 500]
        expected = sum(len(self.engine.rerank_tokenizer(query, d, truncation=True, max_length=512)['input_ids']) for d in docs)
        result = self.engine.rerank(query, docs, with_usage=True)
        self.assertEqual(result['usage']['prompt_tokens'], expected)
        self.assertEqual(result['usage']['total_tokens'], expected)
        self.assertEqual(result['usage']['source'], 'model_tokenizer')
        self.assertLess(expected, 1024)

    def test_embedding_duplicate_within_first_batch_is_reused_not_uncached_twice(self):
        self.engine.cache.clear()
        result = self.engine.embed(['duplicate', 'duplicate'], with_usage=True)
        self.assertEqual(result['usage']['prompt_tokens_details']['cached_tokens'] * 2,
                         result['usage']['prompt_tokens'])

    def test_bucketing_preserves_document_order_and_counts_unpadded_input(self):
        torch = self.torch
        original = self.engine.rerank_model
        shapes = []
        def forward(**batch):
            shapes.append(tuple(batch['input_ids'].shape))
            logits = (batch['input_ids'] * batch['attention_mask']).sum(dim=1).float() / 100000
            return types.SimpleNamespace(logits=logits)
        self.engine.rerank_model = forward
        docs = ['long ' * 200, 'tiny', 'medium ' * 20, 'a', 'larger ' * 60, 'b']
        try:
            result = self.engine.rerank('query', docs, with_usage=True)
            expected=[]
            for doc in docs:
                row=self.engine.rerank_tokenizer('query', doc, truncation=True, max_length=512)
                expected.append(torch.sigmoid(torch.tensor(sum(row['input_ids']) / 100000)).item())
            self.assertEqual(result['scores'], expected)
            self.assertEqual(result['usage']['prompt_tokens'],sum(len(self.engine.rerank_tokenizer('query',d,truncation=True,max_length=512)['input_ids']) for d in docs))
            self.assertEqual(len(shapes),2)
        finally:
            self.engine.rerank_model = original


if __name__ == '__main__':
    unittest.main()
