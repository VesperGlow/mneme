"""Deterministic, labelled acceptance corpus. No production/user data or credentials."""
import json
from pathlib import Path


def build():
    cases = []
    for i in range(10):
        game = ['终末地', '空洞骑士', '异度神剑', '星露谷物语', '博德之门3', '原神', '怪物猎人', '十三机兵防卫圈', '逆转裁判', '蔚蓝'][i]
        name = ['林悦', '陈砚', '周宁', '苏澄', '乔鹿', '闻溪', '陆星', '沈墨', '唐棠', '许遥'][i]
        city = ['杭州', '成都', '苏州', '武汉', '青岛', '厦门', '昆明', '南京', '天津', '长沙'][i]
        beverage = ['无糖燕麦拿铁', '冰美式', '乌龙茶', '淡焙咖啡', '无糖豆浆', '温红茶', '抹茶拿铁', '低糖可可', '茉莉花茶', '柠檬气泡水'][i]
        entries = [
            ('game_opinion', [f'{game}的剧情也太 ML 了，尤其是第{i+1}章。'], [[game, 'ML']], ['长期喜欢 ML', '长期喜欢ML']),
            ('daily_event', [f'今天下班后去{city}河边散步，看到了第{i+1}次晚霞，心情好多了。'], [[city, '晚霞']], []),
            ('explicit_preference', [f'我一直最喜欢喝{beverage}。', f'还是{beverage}最合我口味，这个习惯已经很多年了。'], [[beverage], [beverage]], []),
            ('ambiguous', [f'{game}好像还挺对我胃口，不过才玩了{i+1}小时，再看看。'], [[game, '好像']], []),
            ('changed_view', [f'我最近觉得{game}的战斗很无聊。', f'玩到第{i+2}章后我改观了，{game}的战斗现在挺有意思。'], [[game, '无聊'], [game, '意思']], []),
            ('attribution', [f'朋友{name}是兽医，我在{city}做 Rust 后端开发。'], [[name, '兽医'], [city, 'Rust']], ['用户是兽医', '用户在做兽医']),
            ('technical_entities', [f'项目 Mneme-{i} 里 tokio::sync::Notify 和 SQLite WAL 配合，Neo4j 5.26 的 HNSW 负责向量检索。'], [[f'Mneme-{i}', 'tokio::sync::Notify'], ['SQLite', 'WAL'], ['Neo4j', 'HNSW']], []),
            ('repetition', [f'今天又读了《{game}设定集》的第{i+1}页。'] * 2, [[game, str(i+1)]], []),
            ('low_information', ['嗯', '好的，谢谢'], [], []),
            ('cross_turn', [f'我妹妹叫{name}，现在住在{city}。', '刚才说的妹妹最近在学习拍候鸟，我帮她挑了镜头。'], [[name, city], ['妹妹', '候鸟']], ['用户在学习拍候鸟']),
            ('emotion', [f'准备第{i+1}场公开演讲之前有点紧张，排练了一遍后好多了。'], [['演讲', '紧张'], ['排练']], []),
            ('temporal', [f'明天下午三点我要在{city}参加第{i+1}次读书会，仅这次安排。'], [[city, '读书会', '三点']], []),
        ]
        for category, turns, clauses, forbidden in entries:
            cases.append({'id':f'{category}-{i:02}', 'category':category, 'turns':turns,
                          'expected_clauses':clauses, 'forbidden_assertions':forbidden,
                          'uncertain':category=='ambiguous', 'expect_empty':category=='low_information'})
    return {'version':1, 'description':'120 labelled Chinese dialogue groups; lexical clauses are conservative audit labels, not an LLM judge.', 'cases':cases}


if __name__ == '__main__':
    dest = Path(__file__).resolve().parents[2]/'tests/fixtures/preflight/dialogues.json'
    dest.write_text(json.dumps(build(), ensure_ascii=False, indent=2)+'\n')
    print(f'{dest}: 120 groups')
