"""b4 cache lane: the cache layer was removed.

Its open options, and the equally dead ``cache_snapshot``, are accepted and
ignored (out-of-range values included), and the ``cache_*`` methods,
``CacheStats`` and the cache metrics are deprecated no-op stubs kept for one
release so existing callers keep working.
"""

from __future__ import annotations

import pytest

from kitedb import CacheStats, Database, OpenOptions, PropValue, collect_metrics, health_check

CACHE_OPTIONS = dict(
    cache_snapshot=False,
    cache_enabled=True,
    cache_max_node_props=-1,
    cache_max_edge_props=0,
    cache_max_traversal_entries=1,
    cache_max_query_entries=2**62,
    cache_query_ttl_ms=-1,
)


def test_cache_open_options_are_accepted_and_ignored(tmp_path):
    path = str(tmp_path / "options.kitedb")
    db = Database(path, OpenOptions(**CACHE_OPTIONS))
    db.begin()
    node = db.create_node("a")
    name = db.get_or_create_propkey("name")
    db.set_node_prop(node, name, PropValue.string("first"))
    db.commit()
    assert db.get_node_prop_string(node, name) == "first"
    db.begin()
    db.set_node_prop(node, name, PropValue.string("second"))
    db.commit()
    assert db.get_node_prop_string(node, name) == "second"
    db.close()

    options = OpenOptions(**CACHE_OPTIONS)
    assert options.cache_snapshot is False and options.cache_enabled is True
    db = Database(path, options)
    assert db.get_node_by_key("a") == node
    assert db.get_node_prop_string(node, name) == "second"
    db.close()


def test_deprecated_cache_methods_are_noop_stubs(tmp_path):
    db = Database(str(tmp_path / "stubs.kitedb"), OpenOptions(cache_enabled=True))
    db.begin()
    node = db.create_node("a")
    db.commit()

    assert db.cache_is_enabled() is False
    assert db.cache_stats() is None
    db.cache_invalidate_node(node)
    db.cache_invalidate_edge(node, 1, node)
    db.cache_invalidate_key("a")
    db.cache_clear()
    db.cache_clear_query()
    db.cache_clear_key()
    db.cache_clear_property()
    db.cache_clear_traversal()
    db.cache_reset_stats()
    assert db.get_node_by_key("a") == node
    # The class stays importable for existing type checks.
    assert CacheStats.__name__ == "CacheStats"
    db.close()

    with pytest.raises(Exception, match="closed"):
        db.cache_is_enabled()


def test_metrics_report_the_removed_cache_as_disabled_and_empty(tmp_path):
    db = Database(str(tmp_path / "metrics.kitedb"), OpenOptions(cache_enabled=True))
    metrics = collect_metrics(db)
    assert metrics.cache.enabled is False
    for layer in (
        metrics.cache.property_cache,
        metrics.cache.traversal_cache,
        metrics.cache.query_cache,
    ):
        assert (layer.hits, layer.misses, layer.size, layer.max_size) == (0, 0, 0, 0)
        assert layer.hit_rate == 0.0 and layer.utilization_percent == 0.0
    memory = metrics.memory
    assert memory.cache_estimate_bytes == 0
    assert memory.total_estimate_bytes == memory.delta_estimate_bytes + memory.snapshot_bytes
    assert all(check.name != "cache_efficiency" for check in health_check(db).checks)
    db.close()
