"""The evaluator must grade supported facts independently of storage granularity."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('report', Path(__file__).resolve().parents[1]/'scripts/memory-e2e-report.py')
report = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(report)


class ScoringTests(unittest.TestCase):
    def evaluate(self, texts, expected, dead=0):
        facts = [{'label':'sister_job','all':[['林悦'],['苏州'],['建筑','设计']]}]
        turn = {'index':0,'gold':{'category':'fixture','facts':facts if expected else []},'writes':[[str(i),'slot'+str(i),text] for i,text in enumerate(texts)],
                'jobs':{'dead':dead},'llm_calls':[],'extraction_and_projection_ms':1}
        query = {'gold':{'relevant':['sister_job'] if expected else [],'say':'standard question'},'selected_memories':[{'content':t} for t in texts],
                 'answer_ok':True,'answer':'standard answer','degraded':False,'trace':{'embedding_ms':1,'rerank_ms':1},'recall_ms':1,'context_ms':1,'generation_ms':1}
        profile = {'id':'dev-lin','turns':[turn],'queries':[query],'final_memories':[{'content':t} for t in texts],'final_expected':7}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for filename, value in [('results.json',{'profiles':[profile]}),('resources.json',[]),('metadata.json',{})]:
                (root/filename).write_text(json.dumps(value))
            return report.summarize(root)

    def test_city_and_profession_can_be_stored_separately_without_becoming_false_memories(self):
        result = self.evaluate(['用户的妹妹林悦在苏州工作。','用户的妹妹林悦从事建筑设计。'], True)
        self.assertEqual(result['writes']['precision'],1)
        self.assertEqual(result['writes']['recall'],1)
        self.assertEqual(result['retrieval']['precision'],1)
        self.assertEqual(result['retrieval']['recall'],1)
        self.assertEqual(result['retrieval']['exact'],1)
        self.assertEqual(result['final']['duplicates'],0)

    def test_empty_failed_extraction_does_not_count_as_a_successful_skip(self):
        result = self.evaluate([],False,dead=1)
        self.assertEqual(result['writes']['exact'],1)
        self.assertEqual(result['writes']['processed_exact'],0)

    def test_unrelated_context_is_false_recall_even_if_answer_predicate_passes(self):
        result = self.evaluate(['用户养猫。'],False)
        self.assertEqual(result['retrieval']['fp'],1)
        self.assertEqual(result['retrieval']['exact'],0)


if __name__ == '__main__':
    unittest.main()
