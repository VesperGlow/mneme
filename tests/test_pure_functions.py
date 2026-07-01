import math
from datetime import timedelta

import pytest

from src.agent import contains_sensitive_secret, extract_json_object, format_gap, format_time_context
from src.embedding import EmbeddingError, normalize_and_resize
from src.memory_store import clean_relation


def test_normalize_and_resize_matryoshka_vector():
    vector = normalize_and_resize([3, 4, 99], 2)
    assert vector == pytest.approx([0.6, 0.8])
    assert math.sqrt(sum(v * v for v in vector)) == pytest.approx(1.0)


def test_vector_dimension_guard():
    with pytest.raises(EmbeddingError):
        normalize_and_resize([1, 2], 3)


def test_extract_json_from_fenced_response():
    assert extract_json_object('```json\n{"should_remember": false}\n```')[
        "should_remember"
    ] is False


def test_relation_is_sanitized():
    assert clean_relation("Works With / 合作") == "works_with___合作"


def test_sensitive_credentials_are_detected():
    assert contains_sensitive_secret("API key: sk-abcdefghijklmnopqrstuvwxyz")
    assert not contains_sensitive_secret("用户使用 1Password 管理自己的密码")


def test_format_gap_omits_short_gaps():
    assert format_gap(timedelta(minutes=5)) == ""


def test_format_gap_hours_and_minutes():
    assert format_gap(timedelta(hours=3, minutes=20)) == "3 小时 20 分钟"


def test_format_gap_days():
    assert format_gap(timedelta(days=2, hours=5)) == "2 天 5 小时"


def test_format_time_context_includes_beijing_time_and_weekday():
    context = format_time_context(None)
    assert "当前北京时间" in context
    assert "星期" in context


def test_format_time_context_mentions_gap_when_significant():
    from datetime import timedelta as _td

    from src.agent import _now_beijing

    last = (_now_beijing() - _td(hours=5)).isoformat()
    context = format_time_context(last)
    assert "距离上一条消息已过" in context
