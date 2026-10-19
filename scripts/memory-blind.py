#!/usr/bin/env python3
"""Author a sealed test set via the owned budget relay, before examining outcomes.

No application source, retrieval output, or tuning examples enter this request.
Never overwrite gold: after a run it is a regression, not a fresh blind test.
"""
import argparse
import hashlib
import json
from pathlib import Path
import urllib.request

PROMPT = r'''为中文个人长期记忆系统独立编写一次盲测。只输出 JSON，不要 Markdown。
不读取任何被测实现；自己选择与图书、展馆、茶、咖啡、住址无关的新主题。
一个 profile，12条按时间先后的用户原话 turns，覆盖：稳定事实、明确否定、人物关系链、独立属性、数字更正、语义改写的明确重复确认、假设不应保存、不保存要求。共8个最终原子事实，每条原话最多明确告知一个原子事实；更新前的旧值可有自己的 label，但不能在 final 中。每条本轮重申仍应包含对应 facts。
10个最终 queries：2个跨主题复合问题、1个需两条人物关系才能回答的问题、3个语义改写、1个更新值、1个未知属性、1个假设内容不能当事实、1个纯寒暄。复合问题 relevant 必须包含所有需要的原子事实；多跳包含所有桥接事实；不相关问题 relevant=[]。
schema：{"version":1,"notes":"independently authored sealed evaluation","profiles":[{"id":"blind-independent","split":"blind","turns":[{"day":0,"category":"fact","say":"用户第一人称原话","facts":[{"label":"stable_ascii_id","all":[["必要中文词","可接受改写"],["另一必要内容"]]}]}],"final":[{"label":"...","all":[["..."]]}],"queries":[{"say":"...","relevant":["stable_ascii_id"],"answer_all":[["必须出现的答案词","可接受改写"]],"answer_forbidden":[]}]}]}。
all 和 answer_all 是 AND of OR，不要把常用词或“用户”单独作为事实判断的唯一要求。标签含义一致，不能把独立属性挤成一条事实。否定必须在 all 中包括否定词；输出事实不得依赖助手回复。day递增且在0..60内。未知问题只要求合理说明不知道，不要求特定措辞。'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--profile-id', default='blind-independent')
    parser.add_argument('--split', default='blind')
    parser.add_argument('--quality-focus', action='store_true')
    args = parser.parse_args()
    if args.output.exists():
        parser.error('Refusing to replace frozen blind gold')
    with urllib.request.urlopen('http://127.0.0.1:18091/health', timeout=5) as response:
        health = json.load(response)
    assert health['provider'] == 'official-deepseek-test-relay'
    body = {'model':'mneme-eval-local','temperature':0,'max_tokens':6500,'stream':False,
            'response_format':{'type':'json_object'},'messages':[{'role':'user','content':PROMPT+'\n被测运行固定 allow_sensitive=false：私人的健康/过敏和身份敏感资料不应保存，其 facts 与后续 relevant 应为空；公开职业不是健康状态。事实判断只要求当前值，不应强制保留旧值、纠正措辞或源句的语序。未知信息可坦诚说明缺失，不要求特定否定用词。'+f'\n本次 profile.id 必须为 {args.profile_id}，split 必须为 {args.split}。所有数组元素类型严格遵守 schema。'}]}
    if args.quality_focus:
        body['messages'][0]['content'] += '\n本轮重点：包含同一主题中两个可同时成立、不能互相覆盖的独立肯定/否定约束，以及真正互斥的新旧值更正；查询至少两个自然比喻/惯用语改写。另设主体混淆、同主题但未知属性、无关寒暄等负例。只用你自己选择的新领域，不参考任何既有题库。确保事实、最终状态、查询答案和桥接证据之间一致。'
    request = urllib.request.Request('http://127.0.0.1:18091/v1/chat/completions',
                                     json.dumps(body).encode(), {'Content-Type':'application/json'})
    with urllib.request.urlopen(request, timeout=120) as response:
        result = json.load(response)
    assert result['choices'][0]['finish_reason'] == 'stop'
    gold = json.loads(result['choices'][0]['message']['content'])
    profiles = gold['profiles']
    assert len(profiles) == 1 and profiles[0]['split'] == args.split and profiles[0]['id'] == args.profile_id
    # Validate structure without showing test content to the implementation agent.
    profile = profiles[0]
    assert len(profile['turns']) >= 10 and len(profile['queries']) >= 8
    labels = {f['label'] for f in profile['final']}
    assert all(set(q['relevant']) <= labels for q in profile['queries'])
    for q in profile['queries']:
        assert isinstance(q['say'],str) and isinstance(q['answer_all'],list) and isinstance(q['answer_forbidden'],list)
        assert all(isinstance(group,list) and all(isinstance(word,str) for word in group) for group in q['answer_all'])
    for t in profile['turns']:
        assert isinstance(t['say'], str) and isinstance(t['facts'], list)
    for fact in profile['final'] + [f for t in profile['turns'] for f in t['facts']]:
        assert isinstance(fact['label'],str) and isinstance(fact['all'],list)
        assert all(isinstance(group,list) and all(isinstance(word,str) for word in group) for group in fact['all'])
    raw = (json.dumps(gold, ensure_ascii=False, indent=2)+'\n').encode()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_bytes(raw)
    manifest = {'sha256':hashlib.sha256(raw).hexdigest(), 'author':'deepseek-flash separate authoring request',
                'usage':result.get('usage'),'prompt_sha256':hashlib.sha256(PROMPT.encode()).hexdigest(),
                'request_prompt_sha256':hashlib.sha256(body['messages'][0]['content'].encode()).hexdigest(),
                'turns':len(profile['turns']), 'queries':len(profile['queries']),
                'rule':'No tuning on this corpus; report first run, including failures.'}
    args.output.with_suffix('.manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    print(json.dumps(manifest,ensure_ascii=False))


if __name__ == '__main__':
    main()
