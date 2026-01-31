"""
Ray Database - Fluent API

High-level, type-safe API for RayDB matching the TypeScript fluent style.

Example:
    >>> from raydb import ray, node, edge, prop, optional
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
    >>> db = ray("./my-graph", nodes=[user], edges=[knows])
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

from raydb._raydb import Database, OpenOptions

from .builders import (
    DeleteBuilder,
    InsertBuilder,
    NodeRef,
    UpdateBuilder,
    UpdateByRefBuilder,
    UpdateEdgeBuilder,
    create_link,
    delete_link,
    from_prop_value,
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


class Ray:
    """
    Ray Database - High-level fluent API.
    
    Provides a type-safe, chainable interface for graph database operations
    similar to the TypeScript API.
    
    Example:
        >>> db = ray("./my-graph", nodes=[user, company], edges=[knows, worksAt])
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
    ):
        """
        Open or create a Ray database.
        
        Args:
            path: Path to the database file
            nodes: List of node definitions
            edges: List of edge definitions
            options: Optional database options
        """
        self._db = Database(path, options)
        self._nodes: Dict[str, NodeDef[Any]] = {n.name: n for n in nodes}
        self._edges: Dict[str, EdgeDef] = {e.name: e for e in edges}
        self._etype_ids: Dict[EdgeDef, int] = {}
        self._prop_key_ids: Dict[str, int] = {}
        
        # Build key prefix -> NodeDef cache for fast lookups
        self._key_prefix_to_node_def: Dict[str, NodeDef[Any]] = {}
        for node_def in nodes:
            try:
                test_key = node_def.key_fn("__test__")
                prefix = test_key.replace("__test__", "")
                self._key_prefix_to_node_def[prefix] = node_def
            except Exception:
                pass
        
        # Initialize schema
        self._init_schema(nodes, edges)
    
    def _init_schema(
        self,
        nodes: List[NodeDef[Any]],
        edges: List[EdgeDef],
    ) -> None:
        """Initialize edge types and property keys."""
        self._db.begin()
        try:
            # Define edge types
            for edge in edges:
                etype_id = self._db.get_or_create_etype(edge.name)
                self._etype_ids[edge] = etype_id
                edge._etype_id = etype_id
            
            # Define property keys for nodes
            for node in nodes:
                node._prop_key_ids = {}
                for prop_name, prop_def in node.props.items():
                    key = f"{node.name}:{prop_def.name}"
                    if key not in self._prop_key_ids:
                        prop_key_id = self._db.get_or_create_propkey(prop_def.name)
                        self._prop_key_ids[key] = prop_key_id
                    node._prop_key_ids[prop_name] = self._prop_key_ids[key]
            
            # Define property keys for edges
            for edge in edges:
                edge._prop_key_ids = {}
                for prop_name, prop_def in edge.props.items():
                    key = f"{edge.name}:{prop_def.name}"
                    if key not in self._prop_key_ids:
                        prop_key_id = self._db.get_or_create_propkey(prop_def.name)
                        self._prop_key_ids[key] = prop_key_id
                    edge._prop_key_ids[prop_name] = self._prop_key_ids[key]
            
            self._db.commit()
        except Exception:
            self._db.rollback()
            raise
    
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
        prop_key_id = def_._prop_key_ids.get(prop_name)
        if prop_key_id is None:
            raise ValueError(f"Unknown property: {prop_name} on {def_.name}")
        return prop_key_id
    
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
        
        # Build reverse mapping: prop_key_id -> prop_name
        # This is cached on node_def._prop_key_ids
        key_id_to_name = {v: k for k, v in node_def._prop_key_ids.items()}
        
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

    def check(self) -> Any:
        """Check database integrity."""
        result = self._db.check()
        for edge_name, edge_def in self._edges.items():
            if getattr(edge_def, "_etype_id", None) is None:
                result.warnings.append(
                    f"Edge type '{edge_name}' has no assigned etype_id"
                )
        return result
    
    def optimize(self) -> None:
        """Optimize the database."""
        self._db.optimize()
    
    def close(self) -> None:
        """Close the database."""
        self._db.close()
    
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
        except Exception:
            self._db.rollback()
            raise
    
    @contextmanager
    def transaction(self) -> Generator[Ray, None, None]:
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
            If an exception occurs, the transaction is rolled back.
        """
        self._db.begin()
        try:
            yield self
            self._db.commit()
        except Exception:
            self._db.rollback()
            raise
    
    def in_transaction(self) -> bool:
        """Check if currently in a transaction."""
        return self._db.has_transaction()
    
    # ==========================================================================
    # Context Manager
    # ==========================================================================
    
    def __enter__(self) -> Ray:
        return self
    
    def __exit__(self, exc_type: Any, exc_val: Any, exc_tb: Any) -> bool:
        self.close()
        return False


# ============================================================================
# Entry Point
# ============================================================================

def ray(
    path: str,
    *,
    nodes: List[NodeDef[Any]],
    edges: List[EdgeDef],
    options: Optional[OpenOptions] = None,
) -> Ray:
    """
    Open or create a Ray database.
    
    This is the main entry point for the fluent API.
    
    Args:
        path: Path to the database file
        nodes: List of node definitions
        edges: List of edge definitions
        options: Optional database options
    
    Returns:
        Ray database instance
    
    Example:
        >>> from raydb import ray, node, edge, prop, optional
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
        >>> db = ray("./my-graph", nodes=[user], edges=[knows])
        >>> 
        >>> # Use as context manager
        >>> with ray("./my-graph", nodes=[user], edges=[knows]) as db:
        ...     alice = db.insert(user).values(key="alice", name="Alice").returning()
    """
    return Ray(path, nodes=nodes, edges=edges, options=options)


__all__ = [
    "Ray",
    "ray",
    "EdgeData",
]
