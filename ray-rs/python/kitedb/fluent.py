"""
Kite Database - Fluent API

High-level, type-safe API for KiteDB matching the TypeScript fluent style.

Example:
    >>> from kitedb import kite, node, edge, prop, optional
    >>> 
    >>> # Define schema
    >>> user = node("user",
    ...     key=lambda id: f"user:{id}",
    ...     props={
    ...         "name": prop.string("name"),
    ...         "email": prop.string("email"),
    ...         "age": optional(prop.int("age")),
    ...     }
    ... )
    >>> 
    >>> knows = edge("knows", {
    ...     "since": prop.int("since"),
    ... })
    >>> 
    >>> # Open database
    >>> db = kite("./my-graph", nodes=[user], edges=[knows])
    >>> 
    >>> # Insert nodes with fluent API
    >>> alice = db.insert(user).values(
    ...     key="alice",
    ...     name="Alice",
    ...     email="alice@example.com",
    ...     age=30
    ... ).returning()
    >>> 
    >>> bob = db.insert(user).values(
    ...     key="bob",
    ...     name="Bob",
    ...     email="bob@example.com",
    ...     age=25
    ... ).returning()
    >>> 
    >>> # Create edges
    >>> db.link(alice, knows, bob, since=2020)
    >>> 
    >>> # Traverse graph
    >>> friends = db.from_(alice).out(knows).nodes().to_list()
    >>> 
    >>> # Cleanup
    >>> db.close()
"""

from __future__ import annotations

from contextlib import contextmanager
from dataclasses import dataclass
from typing import (
    Any,
    Callable,
    Dict,
    Generator,
    Generic,
    Iterator,
    List,
    Optional,
    TypeVar,
    Union,
    overload,
)

from kitedb._kitedb import CheckResult, Database, OpenOptions

from .builders import (
    DeleteBuilder,
    InsertBuilder,
    UpsertBuilder,
    UpsertByIdBuilder,
    NodeRef,
    UpdateBuilder,
    UpdateByRefBuilder,
    UpdateEdgeBuilder,
    UpsertEdgeBuilder,
    create_link,
    delete_link,
    from_prop_value,
    prop_names_by_key_id,
)
from .schema import EdgeDef, NodeDef, PropsSchema
from .traversal import PathFindingBuilder, TraversalBuilder, WeightSpec


N = TypeVar("N", bound=NodeDef)
E = TypeVar("E", bound=EdgeDef)


@dataclass
class EdgeData:
    """Edge data with source/destination refs and properties."""
    src: NodeRef[Any]
    dst: NodeRef[Any]
    edge: Any
    props: Dict[str, Any]


class Kite:
    """
    Kite Database - High-level fluent API.
    
    Provides a type-safe, chainable interface for graph database operations
    similar to the TypeScript API.
    
    Example:
        >>> db = kite("./my-graph", nodes=[user, company], edges=[knows, worksAt])
        >>> 
        >>> # Insert
        >>> alice = db.insert(user).values(key="alice", name="Alice").returning()
        >>> 
        >>> # Update
        >>> db.update(user).set(email="new@example.com").where(key="user:alice").execute()
        >>> 
        >>> # Or update by reference
        >>> db.update(alice).set(age=31).execute()
        >>> 
        >>> # Link
        >>> db.link(alice, knows, bob, since=2020)
        >>> 
        >>> # Traverse
        >>> friends = db.from_(alice).out(knows).nodes().to_list()
        >>> 
        >>> # Close
        >>> db.close()
    """
    
    def __init__(
        self,
        path: str,
        *,
        nodes: List[NodeDef[Any]],
        edges: List[EdgeDef],
        options: Optional[OpenOptions] = None,
        close_checkpoint_if_wal_usage_at_least: Optional[float] = 0.2,
    ):
        """
        Open or create a Kite database.
        
        Args:
            path: Path to the database file
            nodes: List of node definitions
            edges: List of edge definitions
            options: Optional database options
            close_checkpoint_if_wal_usage_at_least:
                On close, checkpoint if the log the snapshot does not cover is at
                least this fraction of the checkpoint trigger. Set None to disable.
        """
        self._db = Database(path, options)
        self._close_checkpoint_if_wal_usage_at_least = (
            None
            if close_checkpoint_if_wal_usage_at_least is None
            else max(0.0, min(1.0, float(close_checkpoint_if_wal_usage_at_least)))
        )
        self._nodes: Dict[str, NodeDef[Any]] = {n.name: n for n in nodes}
        self._edges: Dict[str, EdgeDef] = {e.name: e for e in edges}
        # Schema ids of this database. Definitions can be shared by several
        # Kite instances on different databases, so the ids live here and the
        # definitions are never mutated.
        self._etype_ids: Dict[EdgeDef, int] = {}
        self._label_ids: Dict[NodeDef[Any], int] = {}
        self._node_prop_key_ids: Dict[str, Dict[str, int]] = {}
        self._edge_prop_key_ids: Dict[str, Dict[str, int]] = {}

        # Build key prefix -> NodeDef cache for fast lookups
        self._key_prefix_to_node_def: Dict[str, NodeDef[Any]] = {}
        for node_def in nodes:
            try:
                test_key = node_def.key_fn("__test__")
                prefix = test_key.replace("__test__", "")
                self._key_prefix_to_node_def[prefix] = node_def
            except Exception:
                pass

        try:
            self._init_schema(nodes, edges)
        except BaseException:
            # Don't leak the handle (and its file lock) when init fails.
            try:
                self._db.close()
            except Exception:
                pass
            raise

    def _init_schema(
        self,
        nodes: List[NodeDef[Any]],
        edges: List[EdgeDef],
    ) -> None:
        """Resolve labels, edge types and property keys for this database.

        Existing entries are looked up without a transaction, so a read-only
        database or a replica opens fine when its schema is complete. A write
        transaction opens only to create missing entries.
        """
        db = self._db
        labels = {node.name: db.get_label_id(node.name) for node in nodes}
        etypes = {edge.name: db.get_etype_id(edge.name) for edge in edges}
        propkeys: Dict[str, Optional[int]] = {}
        for def_ in [*nodes, *edges]:
            for prop_def in def_.props.values():
                if prop_def.name not in propkeys:
                    propkeys[prop_def.name] = db.get_propkey_id(prop_def.name)

        schema_missing = None in etypes.values() or None in propkeys.values()
        # Labels only tag nodes this Kite creates. A database written before
        # Kite defined labels may lack them; if it can't take writes anyway,
        # open it without labels instead of failing.
        writable = not db.read_only and db.replica_replication_status() is None
        labels_missing = None in labels.values() and (writable or schema_missing)

        if schema_missing or labels_missing:
            db.begin()
            try:
                for name, label_id in labels.items():
                    if label_id is None:
                        labels[name] = db.get_or_create_label(name)
                for name, etype_id in etypes.items():
                    if etype_id is None:
                        etypes[name] = db.get_or_create_etype(name)
                for name, prop_key_id in propkeys.items():
                    if prop_key_id is None:
                        propkeys[name] = db.get_or_create_propkey(name)
                db.commit()
            except BaseException:
                if db.has_transaction():
                    db.rollback()
                raise

        for node in nodes:
            label_id = labels[node.name]
            if label_id is not None:
                self._label_ids[node] = label_id
            self._node_prop_key_ids[node.name] = {
                prop_name: propkeys[prop_def.name]  # type: ignore[misc]
                for prop_name, prop_def in node.props.items()
            }
        for edge in edges:
            self._etype_ids[edge] = etypes[edge.name]  # type: ignore[assignment]
            self._edge_prop_key_ids[edge.name] = {
                prop_name: propkeys[prop_def.name]  # type: ignore[misc]
                for prop_name, prop_def in edge.props.items()
            }

    # ==========================================================================
    # Schema Resolution Helpers
    # ==========================================================================
    
    def _resolve_etype_id(self, edge_def: EdgeDef) -> int:
        """Resolve edge type ID from definition."""
        etype_id = self._etype_ids.get(edge_def)
        if etype_id is None:
            raise ValueError(f"Unknown edge type: {edge_def.name}")
        return etype_id
    
    def _resolve_prop_key_id(
        self,
        def_: Union[NodeDef[Any], EdgeDef],
        prop_name: str,
    ) -> int:
        """Resolve property key ID from definition."""
        if isinstance(def_, NodeDef):
            ids = self._node_prop_key_ids.get(def_.name)
        else:
            ids = self._edge_prop_key_ids.get(def_.name)
        prop_key_id = ids.get(prop_name) if ids is not None else None
        if prop_key_id is None:
            raise ValueError(f"Unknown property: {prop_name} on {def_.name}")
        return prop_key_id

    def _resolve_label_id(self, node_def: NodeDef[Any]) -> Optional[int]:
        """Label ID for a node type, or None if the database has none for it."""
        return self._label_ids.get(node_def)
    
    def _get_node_def(self, node_id: int) -> Optional[NodeDef[Any]]:
        """Get node definition from node ID by matching key prefix."""
        key = self._db.get_node_key(node_id)
        if key:
            for prefix, node_def in self._key_prefix_to_node_def.items():
                if key.startswith(prefix):
                    return node_def
        
        # Fall back to first node def
        if self._nodes:
            return next(iter(self._nodes.values()))
        return None
    
    def _load_node_props(self, node_id: int, node_def: NodeDef[Any]) -> Dict[str, Any]:
        """Load all properties for a node using single FFI call."""
        props: Dict[str, Any] = {}
        # Use get_node_props() for single FFI call instead of per-property calls
        all_props = self._db.get_node_props(node_id)
        if all_props is None:
            return props
        
        key_id_to_name = prop_names_by_key_id(node_def, self._resolve_prop_key_id)
        
        for node_prop in all_props:
            prop_name = key_id_to_name.get(node_prop.key_id)
            if prop_name is not None:
                props[prop_name] = from_prop_value(node_prop.value)
        
        return props
    
    # ==========================================================================
    # Node Operations
    # ==========================================================================
    
    def insert(self, node: NodeDef[Any]) -> InsertBuilder[NodeDef[Any]]:
        """
        Insert a new node.
        
        Args:
            node: Node definition
        
        Returns:
            InsertBuilder for chaining
        
        Example:
            >>> alice = db.insert(user).values(
            ...     key="alice",
            ...     name="Alice",
            ...     email="alice@example.com"
            ... ).returning()
        """
        return InsertBuilder(
            db=self._db,
            node_def=node,
            resolve_prop_key_id=self._resolve_prop_key_id,
            label_id=self._resolve_label_id(node),
        )

    def upsert(self, node: NodeDef[Any]) -> UpsertBuilder[NodeDef[Any]]:
        """
        Upsert a node (create if missing, otherwise update).
        
        Args:
            node: Node definition
        
        Returns:
            UpsertBuilder for chaining
        
        Example:
            >>> alice = db.upsert(user).values(
            ...     key="alice",
            ...     name="Alice",
            ...     email="alice@example.com"
            ... ).returning()
        """
        return UpsertBuilder(
            db=self._db,
            node_def=node,
            resolve_prop_key_id=self._resolve_prop_key_id,
            label_id=self._resolve_label_id(node),
        )

    def upsert_by_id(self, node: NodeDef[Any], node_id: int) -> UpsertByIdBuilder[NodeDef[Any]]:
        """
        Upsert a node by ID (create if missing, otherwise update).

        Args:
            node: Node definition
            node_id: Internal node ID

        Returns:
            UpsertByIdBuilder for chaining

        Example:
            >>> db.upsert_by_id(user, 42).set(name="Alice").execute()
        """
        return UpsertByIdBuilder(
            db=self._db,
            node_def=node,
            node_id=node_id,
            resolve_prop_key_id=self._resolve_prop_key_id,
            label_id=self._resolve_label_id(node),
        )
    
    @overload
    def update(self, node_or_ref: NodeDef[Any]) -> UpdateBuilder[NodeDef[Any]]: ...
    
    @overload
    def update(self, node_or_ref: NodeRef[Any]) -> UpdateByRefBuilder: ...
    
    def update(
        self,
        node_or_ref: Union[NodeDef[Any], NodeRef[Any]],
    ) -> Union[UpdateBuilder[Any], UpdateByRefBuilder]:
        """
        Update a node by definition or reference.
        
        Args:
            node_or_ref: Node definition or node reference
        
        Returns:
            UpdateBuilder or UpdateByRefBuilder for chaining
        
        Example:
            >>> # By definition with where clause
            >>> db.update(user).set(email="new@example.com").where(key="user:alice").execute()
            >>> 
            >>> # By reference
            >>> db.update(alice).set(age=31).execute()
        """
        if isinstance(node_or_ref, NodeRef):
            return UpdateByRefBuilder(
                db=self._db,
                node_ref=node_or_ref,
                resolve_prop_key_id=self._resolve_prop_key_id,
            )
        return UpdateBuilder(
            db=self._db,
            node_def=node_or_ref,
            resolve_prop_key_id=self._resolve_prop_key_id,
        )
    
    @overload
    def delete(self, node_or_ref: NodeDef[Any]) -> DeleteBuilder[NodeDef[Any]]: ...
    
    @overload
    def delete(self, node_or_ref: NodeRef[Any]) -> bool: ...
    
    def delete(
        self,
        node_or_ref: Union[NodeDef[Any], NodeRef[Any]],
    ) -> Union[DeleteBuilder[Any], bool]:
        """
        Delete a node by definition or reference.
        
        Args:
            node_or_ref: Node definition or node reference
        
        Returns:
            DeleteBuilder for chaining, or bool if deleting by reference
        
        Example:
            >>> # By definition with where clause
            >>> db.delete(user).where(key="user:bob").execute()
            >>> 
            >>> # By reference (immediate execution)
            >>> db.delete(bob)
        """
        if isinstance(node_or_ref, NodeRef):
            return DeleteBuilder(self._db, node_or_ref.node_def).where(
                id=node_or_ref.id
            ).execute()
        return DeleteBuilder(self._db, node_or_ref)
    
    def get(
        self,
        node: NodeDef[Any],
        key: Any,
    ) -> Optional[NodeRef[Any]]:
        """
        Get a node by key.
        
        Args:
            node: Node definition
            key: Application key (will be transformed by key function)
        
        Returns:
            NodeRef with loaded properties, or None if not found
        
        Example:
            >>> alice = db.get(user, "alice")
            >>> if alice:
            ...     print(alice.name, alice.email)
        """
        full_key = node.key_fn(key)
        node_id = self._db.get_node_by_key(full_key)
        
        if node_id is None:
            return None
        
        props = self._load_node_props(node_id, node)
        return NodeRef(id=node_id, key=full_key, node_def=node, props=props)
    
    def get_ref(
        self,
        node: NodeDef[Any],
        key: Any,
    ) -> Optional[NodeRef[Any]]:
        """
        Get a lightweight node reference by key (without loading properties).
        
        This is faster than get() when you only need the reference for
        traversals or edge operations.
        
        Args:
            node: Node definition
            key: Application key
        
        Returns:
            NodeRef without properties, or None if not found
        """
        full_key = node.key_fn(key)
        node_id = self._db.get_node_by_key(full_key)
        
        if node_id is None:
            return None
        
        return NodeRef(id=node_id, key=full_key, node_def=node, props={})
    
    def exists(self, node_ref: NodeRef[Any]) -> bool:
        """Check if a node exists."""
        return self._db.node_exists(node_ref.id)
    
    # ==========================================================================
    # Edge Operations
    # ==========================================================================
    
    def link(
        self,
        src: NodeRef[Any],
        edge: EdgeDef,
        dst: NodeRef[Any],
        props: Optional[Dict[str, Any]] = None,
        **kwargs: Any,
    ) -> None:
        """
        Create an edge between two nodes.
        
        Args:
            src: Source node reference
            edge: Edge definition
            dst: Destination node reference
            props: Optional edge properties as dict
            **kwargs: Optional edge properties as keyword arguments
        
        Example:
            >>> db.link(alice, knows, bob, since=2020)
            >>> # or
            >>> db.link(alice, knows, bob, {"since": 2020})
        """
        all_props = {**(props or {}), **kwargs}
        create_link(
            db=self._db,
            src=src,
            edge_def=edge,
            dst=dst,
            props=all_props if all_props else None,
            resolve_etype_id=self._resolve_etype_id,
            resolve_prop_key_id=self._resolve_prop_key_id,
        )
    
    def unlink(
        self,
        src: NodeRef[Any],
        edge: EdgeDef,
        dst: NodeRef[Any],
    ) -> None:
        """
        Remove an edge between two nodes.
        
        Args:
            src: Source node reference
            edge: Edge definition
            dst: Destination node reference
        
        Example:
            >>> db.unlink(alice, knows, bob)
        """
        delete_link(
            db=self._db,
            src=src,
            edge_def=edge,
            dst=dst,
            resolve_etype_id=self._resolve_etype_id,
        )
    
    def has_edge(
        self,
        src: NodeRef[Any],
        edge: EdgeDef,
        dst: NodeRef[Any],
    ) -> bool:
        """
        Check if an edge exists between two nodes.
        
        Args:
            src: Source node reference
            edge: Edge definition
            dst: Destination node reference
        
        Returns:
            True if the edge exists
        """
        etype_id = self._resolve_etype_id(edge)
        return self._db.edge_exists(src.id, etype_id, dst.id)
    
    def update_edge(
        self,
        src: NodeRef[Any],
        edge: EdgeDef,
        dst: NodeRef[Any],
    ) -> UpdateEdgeBuilder[EdgeDef]:
        """
        Update edge properties.
        
        Args:
            src: Source node reference
            edge: Edge definition
            dst: Destination node reference
        
        Returns:
            UpdateEdgeBuilder for chaining
        
        Example:
            >>> db.update_edge(alice, knows, bob).set(weight=0.9).execute()
        """
        return UpdateEdgeBuilder(
            db=self._db,
            src=src,
            edge_def=edge,
            dst=dst,
            resolve_etype_id=self._resolve_etype_id,
            resolve_prop_key_id=self._resolve_prop_key_id,
        )

    def upsert_edge(
        self,
        src: NodeRef[Any],
        edge: EdgeDef,
        dst: NodeRef[Any],
    ) -> UpsertEdgeBuilder[EdgeDef]:
        """
        Upsert edge properties (create edge if missing).

        Args:
            src: Source node reference
            edge: Edge definition
            dst: Destination node reference

        Returns:
            UpsertEdgeBuilder for chaining

        Example:
            >>> db.upsert_edge(alice, knows, bob).set(weight=0.9).execute()
        """
        return UpsertEdgeBuilder(
            db=self._db,
            src=src,
            edge_def=edge,
            dst=dst,
            resolve_etype_id=self._resolve_etype_id,
            resolve_prop_key_id=self._resolve_prop_key_id,
        )
    
    # ==========================================================================
    # Traversal
    # ==========================================================================
    
    def from_(self, node: NodeRef[Any]) -> TraversalBuilder[Any]:
        """
        Start a traversal from a node.
        
        Note: Named `from_` because `from` is a Python reserved word.
        
        Args:
            node: Starting node reference
        
        Returns:
            TraversalBuilder for chaining
        
        Example:
            >>> friends = db.from_(alice).out(knows).nodes().to_list()
            >>> 
            >>> young_friends = (
            ...     db.from_(alice)
            ...     .out(knows)
            ...     .where_node(lambda n: n.age < 35)
            ...     .nodes()
            ...     .to_list()
            ... )
        """
        return TraversalBuilder(
            db=self._db,
            start_nodes=[node],
            resolve_etype_id=self._resolve_etype_id,
            resolve_prop_key_id=self._resolve_prop_key_id,
            get_node_def=self._get_node_def,
        )
    
    def shortest_path(
        self,
        source: NodeRef[Any],
        weight: Optional[WeightSpec] = None,
    ) -> PathFindingBuilder[Any]:
        """
        Start a pathfinding query from a node.
        
        Args:
            source: Starting node reference
        
        Returns:
            PathFindingBuilder for chaining
        
        Example:
            >>> path = db.shortest_path(alice).to(bob).find()
            >>> if path:
            ...     for node in path.nodes:
            ...         print(node.key)
        """
        builder = PathFindingBuilder(
            db=self._db,
            source=source,
            resolve_etype_id=self._resolve_etype_id,
            resolve_prop_key_id=self._resolve_prop_key_id,
            get_node_def=self._get_node_def,
        )
        if weight is not None:
            builder.weight(weight)
        return builder
    
    # ==========================================================================
    # Listing and Counting
    # ==========================================================================
    
    def all(self, node_def: NodeDef[Any]) -> Iterator[NodeRef[Any]]:
        """
        Iterate all nodes of a specific type.
        
        Args:
            node_def: Node definition to filter by
        
        Yields:
            NodeRef objects with properties
        
        Example:
            >>> for user in db.all(user):
            ...     print(user.name)
        """
        # Get key prefix for filtering using Rust prefix-based listing
        try:
            test_key = node_def.key_fn("__test__")
            key_prefix = test_key.replace("__test__", "")
        except Exception:
            key_prefix = ""
        
        # Use Rust prefix-based filtering
        for node_id in self._db.list_nodes_with_prefix(key_prefix):
            key = self._db.get_node_key(node_id)
            if key:
                props = self._load_node_props(node_id, node_def)
                yield NodeRef(id=node_id, key=key, node_def=node_def, props=props)
    
    def count(self, node_def: Optional[NodeDef[Any]] = None) -> int:
        """
        Count nodes, optionally filtered by type.
        
        Args:
            node_def: Optional node definition to filter by
        
        Returns:
            Number of matching nodes
        """
        if node_def is None:
            return self._db.count_nodes()
        
        # Filter by type using Rust prefix-based count
        try:
            test_key = node_def.key_fn("__test__")
            key_prefix = test_key.replace("__test__", "")
        except Exception:
            return 0
        
        return self._db.count_nodes_with_prefix(key_prefix)
    
    def count_edges(self, edge_def: Optional[EdgeDef] = None) -> int:
        """
        Count edges, optionally filtered by type.
        
        Args:
            edge_def: Optional edge definition to filter by
        
        Returns:
            Number of matching edges
        """
        if edge_def is None:
            return self._db.count_edges()
        
        etype_id = self._resolve_etype_id(edge_def)
        return self._db.count_edges_by_type(etype_id)

    def all_edges(self, edge_def: Optional[EdgeDef] = None) -> Iterator[EdgeData]:
        """
        Iterate all edges, optionally filtered by type.

        Yields:
            EdgeData objects with src/dst refs and edge properties
        """
        etype_id: Optional[int] = None
        if edge_def is not None:
            etype_id = self._resolve_etype_id(edge_def)

        edges = self._db.list_edges(etype_id)
        for edge in edges:
            src_def = self._get_node_def(edge.src)
            dst_def = self._get_node_def(edge.dst)

            src_key = self._db.get_node_key(edge.src) or f"node:{edge.src}"
            dst_key = self._db.get_node_key(edge.dst) or f"node:{edge.dst}"

            if src_def is None or dst_def is None:
                continue

            src_ref = NodeRef(id=edge.src, key=src_key, node_def=src_def, props={})
            dst_ref = NodeRef(id=edge.dst, key=dst_key, node_def=dst_def, props={})

            props: Dict[str, Any] = {}
            if edge_def is not None and edge_def.props:
                for prop_name in edge_def.props.keys():
                    prop_key_id = self._resolve_prop_key_id(edge_def, prop_name)
                    prop_value = self._db.get_edge_prop(edge.src, edge.etype, edge.dst, prop_key_id)
                    if prop_value is not None:
                        props[prop_name] = from_prop_value(prop_value)

            yield EdgeData(src=src_ref, dst=dst_ref, edge=edge, props=props)
    
    # ==========================================================================
    # Database Operations
    # ==========================================================================
    
    def stats(self) -> Any:
        """Get database statistics."""
        return self._db.stats()

    def check(self) -> CheckResult:
        """Check database integrity, plus this Kite's schema bindings."""
        result = self._db.check()
        warnings = list(result.warnings)
        for edge_name, edge_def in self._edges.items():
            if self._etype_ids.get(edge_def) is None:
                warnings.append(f"Edge type '{edge_name}' has no assigned etype_id")
        # CheckResult's lists are copies, so build a new result.
        return CheckResult(result.valid, list(result.errors), warnings)
    
    def optimize(self) -> None:
        """Optimize the database."""
        self._db.optimize()
    
    def close(self) -> None:
        """Close the database."""
        if self._close_checkpoint_if_wal_usage_at_least is None:
            self._db.close()
            return
        self._db.close_with_checkpoint_if_wal_over(
            self._close_checkpoint_if_wal_usage_at_least
        )
    
    @property
    def raw(self) -> Database:
        """Get the raw database handle (escape hatch)."""
        return self._db
    
    # ==========================================================================
    # Transaction Batching
    # ==========================================================================

    def batch(self, operations: List[Any]) -> List[Any]:
        """
        Execute multiple operations in a single transaction.

        Each item can be a callable or an executor with .execute()/.returning().
        """
        self._db.begin()
        try:
            results: List[Any] = []
            for op in operations:
                if callable(op):
                    results.append(op())
                elif hasattr(op, "returning"):
                    results.append(op.returning())
                elif hasattr(op, "execute"):
                    results.append(op.execute())
                else:
                    raise ValueError("Unsupported batch operation")
            self._db.commit()
            return results
        except BaseException:
            self._rollback_if_open()
            raise

    def bulk(self, operations: List[Any]) -> List[Any]:
        """
        Execute multiple operations in a bulk-load transaction (max throughput).

        The bulk load runs alone among writers: it waits for open write
        transactions, and they wait for it. Readers never wait for it.
        """
        if self._db.has_transaction():
            raise ValueError("bulk() cannot run inside an active transaction")

        begin_bulk = getattr(self._db, "begin_bulk", None)
        if begin_bulk is None:
            return self.batch(operations)

        begin_bulk()

        try:
            results: List[Any] = []
            for op in operations:
                if callable(op):
                    results.append(op())
                elif hasattr(op, "returning"):
                    results.append(op.returning())
                elif hasattr(op, "execute"):
                    results.append(op.execute())
                else:
                    raise ValueError("Unsupported batch operation")
            self._db.commit()
            return results
        except BaseException:
            self._rollback_if_open()
            raise
    
    @contextmanager
    def transaction(self) -> Generator[Kite, None, None]:
        """
        Context manager for batching multiple operations in a single transaction.
        
        This is more efficient than letting each operation auto-commit.
        
        Example:
            >>> with db.transaction():
            ...     alice = db.insert(user).values(key="alice", name="Alice").returning()
            ...     bob = db.insert(user).values(key="bob", name="Bob").returning()
            ...     db.link(alice, knows, bob, since=2024)
            ...     # All operations commit together on exit
        
        Note:
            If an exception occurs, the transaction is rolled back. The commit
            on exit may be paced while a background checkpoint runs (up to
            100 ms; see ``Database.commit``).
        """
        self._db.begin()
        try:
            yield self
            self._db.commit()
        except BaseException:
            self._rollback_if_open()
            raise
    
    def in_transaction(self) -> bool:
        """Check if currently in a transaction."""
        return self._db.has_transaction()

    def _rollback_if_open(self) -> None:
        """Roll back this thread's transaction after a failure, if it is
        still open. A failed commit has already ended it (a ConflictError, for
        one), and rolling back then would raise TransactionError in place of
        the error the caller needs."""
        if self._db.has_transaction():
            self._db.rollback()
    
    # ==========================================================================
    # Context Manager
    # ==========================================================================
    
    def __enter__(self) -> Kite:
        return self
    
    def __exit__(self, exc_type: Any, exc_val: Any, exc_tb: Any) -> bool:
        self.close()
        return False


# ============================================================================
# Entry Point
# ============================================================================

def kite(
    path: str,
    *,
    nodes: List[NodeDef[Any]],
    edges: List[EdgeDef],
    options: Optional[OpenOptions] = None,
    close_checkpoint_if_wal_usage_at_least: Optional[float] = 0.2,
) -> Kite:
    """
    Open or create a KiteDB database.
    
    This is the main entry point for the fluent API.
    
    Args:
        path: Path to the database file
        nodes: List of node definitions
        edges: List of edge definitions
        options: Optional database options
        close_checkpoint_if_wal_usage_at_least:
            On close, checkpoint if the log the snapshot does not cover is at
            least this fraction of the checkpoint trigger. Set None to disable.
    
    Returns:
        Kite database instance
    
    Example:
        >>> from kitedb import kite, node, edge, prop, optional
        >>> 
        >>> user = node("user",
        ...     key=lambda id: f"user:{id}",
        ...     props={
        ...         "name": prop.string("name"),
        ...         "email": prop.string("email"),
        ...         "age": optional(prop.int("age")),
        ...     }
        ... )
        >>> 
        >>> knows = edge("knows", {
        ...     "since": prop.int("since"),
        ... })
        >>> 
        >>> db = kite("./my-graph", nodes=[user], edges=[knows])
        >>> 
        >>> # Use as context manager
        >>> with kite("./my-graph", nodes=[user], edges=[knows]) as db:
        ...     alice = db.insert(user).values(key="alice", name="Alice").returning()
    """
    return Kite(
        path,
        nodes=nodes,
        edges=edges,
        options=options,
        close_checkpoint_if_wal_usage_at_least=close_checkpoint_if_wal_usage_at_least,
    )


__all__ = [
    "Kite",
    "kite",
    "EdgeData",
]
