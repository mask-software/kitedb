"""Tests for RayDB Python bindings."""

import os
import tempfile
import pytest

from raydb import (
    Database,
    OpenOptions,
    PropValue,
)


class TestDatabase:
    """Test database operations."""

    def test_create_and_close(self):
        """Test database creation and closing."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            db = Database(path)
            assert db.is_open
            db.close()
            assert not db.is_open

    def test_context_manager(self):
        """Test database as context manager."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                assert db.is_open
            assert not db.is_open

    def test_create_node(self):
        """Test node creation."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                node_id = db.create_node()
                assert node_id >= 0
                assert db.node_exists(node_id)
                db.commit()

    def test_create_node_with_key(self):
        """Test node creation with key."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                node_id = db.create_node("user:alice")
                assert node_id >= 0
                assert db.get_node_by_key("user:alice") == node_id
                assert db.get_node_key(node_id) == "user:alice"
                db.commit()

    def test_node_properties(self):
        """Test node properties."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                node_id = db.create_node()
                
                # Set properties
                name_key = db.get_or_create_propkey("name")
                age_key = db.get_or_create_propkey("age")
                
                db.set_node_prop(node_id, name_key, PropValue.string("Alice"))
                db.set_node_prop(node_id, age_key, PropValue.int(30))
                
                # Get properties
                name_prop = db.get_node_prop(node_id, name_key)
                assert name_prop is not None
                assert name_prop.string_value == "Alice"
                
                age_prop = db.get_node_prop(node_id, age_key)
                assert age_prop is not None
                assert age_prop.int_value == 30
                
                db.commit()

    def test_edges(self):
        """Test edge operations."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                
                alice = db.create_node("user:alice")
                bob = db.create_node("user:bob")
                
                knows = db.get_or_create_etype("knows")
                db.add_edge(alice, knows, bob)
                
                assert db.edge_exists(alice, knows, bob)
                assert not db.edge_exists(bob, knows, alice)
                
                out_edges = db.get_out_edges(alice)
                assert len(out_edges) == 1
                assert out_edges[0].etype == knows
                assert out_edges[0].node_id == bob
                
                in_edges = db.get_in_edges(bob)
                assert len(in_edges) == 1
                assert in_edges[0].etype == knows
                assert in_edges[0].node_id == alice
                
                db.commit()

    def test_transaction_rollback(self):
        """Test transaction rollback."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                node_id = db.create_node("temp")
                assert db.node_exists(node_id)
                db.rollback()
                
                # Node should not exist after rollback
                assert not db.node_exists(node_id)

    def test_statistics(self):
        """Test database statistics."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                db.create_node()
                db.create_node()
                db.commit()
                
                stats = db.stats()
                assert stats.delta_nodes_created == 2

    def test_check(self):
        """Test database integrity check."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                alice = db.create_node("user:alice")
                bob = db.create_node("user:bob")
                knows = db.get_or_create_etype("knows")
                db.add_edge(alice, knows, bob)
                db.commit()

                result = db.check()
                assert result.valid
                assert result.errors == []

    def test_labels(self):
        """Test node labels."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                node_id = db.create_node()
                
                person_label = db.define_label("Person")
                db.add_node_label(node_id, person_label)
                
                assert db.node_has_label(node_id, person_label)
                
                labels = db.get_node_labels(node_id)
                assert person_label in labels
                
                db.commit()


class TestTraversal:
    """Test graph traversal operations."""

    def test_traverse_out(self):
        """Test outgoing traversal."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                
                # Create graph: alice -> bob -> carol
                alice = db.create_node("alice")
                bob = db.create_node("bob")
                carol = db.create_node("carol")
                
                knows = db.get_or_create_etype("knows")
                db.add_edge(alice, knows, bob)
                db.add_edge(bob, knows, carol)
                
                db.commit()
                
                # Traverse from alice
                neighbors = db.traverse_out(alice, knows)
                assert bob in neighbors
                assert carol not in neighbors

    def test_traverse_in(self):
        """Test incoming traversal."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                
                alice = db.create_node("alice")
                bob = db.create_node("bob")
                
                knows = db.get_or_create_etype("knows")
                db.add_edge(alice, knows, bob)
                
                db.commit()
                
                # Traverse incoming to bob
                sources = db.traverse_in(bob, knows)
                assert alice in sources

    def test_variable_depth_traverse(self):
        """Test variable depth traversal."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                
                # Create chain: 1 -> 2 -> 3 -> 4
                n1 = db.create_node("1")
                n2 = db.create_node("2")
                n3 = db.create_node("3")
                n4 = db.create_node("4")
                
                knows = db.get_or_create_etype("knows")
                db.add_edge(n1, knows, n2)
                db.add_edge(n2, knows, n3)
                db.add_edge(n3, knows, n4)
                
                db.commit()
                
                # Traverse up to depth 3
                results = db.traverse(n1, max_depth=3, etype=knows)
                node_ids = [r.node_id for r in results]
                
                assert n2 in node_ids
                assert n3 in node_ids
                assert n4 in node_ids


class TestPathfinding:
    """Test pathfinding operations."""

    def test_bfs_path(self):
        """Test BFS shortest path."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                
                # Create: a -> b -> c
                a = db.create_node("a")
                b = db.create_node("b")
                c = db.create_node("c")
                
                knows = db.get_or_create_etype("knows")
                db.add_edge(a, knows, b)
                db.add_edge(b, knows, c)
                
                db.commit()
                
                # Find path a -> c
                result = db.find_path_bfs(a, c, etype=knows)
                
                assert result.found
                assert result.path == [a, b, c]

    def test_dijkstra_path(self):
        """Test Dijkstra shortest path."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                
                a = db.create_node("a")
                b = db.create_node("b")
                c = db.create_node("c")
                
                knows = db.get_or_create_etype("knows")
                db.add_edge(a, knows, b)
                db.add_edge(b, knows, c)
                
                db.commit()
                
                result = db.find_path_dijkstra(a, c, etype=knows)
                
                assert result.found
                assert result.path == [a, b, c]
                assert result.total_weight == 2.0  # Each edge has weight 1

    def test_no_path(self):
        """Test when no path exists."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                
                a = db.create_node("a")
                b = db.create_node("b")  # Not connected
                
                db.commit()
                
                result = db.find_path_bfs(a, b)
                assert not result.found

    def test_has_path(self):
        """Test has_path check."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                
                a = db.create_node("a")
                b = db.create_node("b")
                c = db.create_node("c")  # Disconnected
                
                knows = db.get_or_create_etype("knows")
                db.add_edge(a, knows, b)
                
                db.commit()
                
                assert db.has_path(a, b, etype=knows)
                assert not db.has_path(a, c)

    def test_reachable_nodes(self):
        """Test reachable_nodes."""
        with tempfile.TemporaryDirectory() as tmpdir:
            path = os.path.join(tmpdir, "test.raydb")
            with Database(path) as db:
                db.begin()
                
                a = db.create_node("a")
                b = db.create_node("b")
                c = db.create_node("c")
                d = db.create_node("d")  # Disconnected
                
                knows = db.get_or_create_etype("knows")
                db.add_edge(a, knows, b)
                db.add_edge(b, knows, c)
                
                db.commit()
                
                reachable = db.reachable_nodes(a, max_depth=2, etype=knows)
                
                assert b in reachable
                assert c in reachable
                assert d not in reachable


class TestPropValue:
    """Test property value types."""

    def test_null_value(self):
        """Test null property value."""
        v = PropValue.null()
        assert v.prop_type == "null"

    def test_bool_value(self):
        """Test boolean property value."""
        v = PropValue.bool(True)
        assert v.prop_type == "bool"
        assert v.bool_value == True

    def test_int_value(self):
        """Test integer property value."""
        v = PropValue.int(42)
        assert v.prop_type == "int"
        assert v.int_value == 42

    def test_float_value(self):
        """Test float property value."""
        v = PropValue.float(3.14)
        assert v.prop_type == "float"
        assert v.float_value is not None
        assert abs(v.float_value - 3.14) < 0.001

    def test_string_value(self):
        """Test string property value."""
        v = PropValue.string("hello")
        assert v.prop_type == "string"
        assert v.string_value == "hello"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
