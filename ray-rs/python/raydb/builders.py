"""
Query Builders for RayDB

Fluent builders for insert, update, delete, and edge operations.
These provide a type-safe, chainable API for database operations.

Example:
    >>> # Insert with returning
    >>> alice = db.insert(user).values(
    ...     key="alice",
    ...     name="Alice",
    ...     email="alice@example.com"
    ... ).returning()
    >>> 
    >>> # Update by key
    >>> db.update(user).set(email="new@example.com").where(key="user:alice").execute()
    >>> 
    >>> # Create edge with properties
    >>> db.link(alice, knows, bob, since=2020)
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import (
    TYPE_CHECKING,
    Any,
    Callable,
    Dict,
    Generic,
    List,
    Optional,
    Protocol,
    TypeVar,
    Union,
)

from .schema import EdgeDef, NodeDef, PropDef

if TYPE_CHECKING:
    from raydb._raydb import Database, PropValue


# ============================================================================
# Node Reference
# ============================================================================

class NodeRef(Generic[TypeVar("N", bound=NodeDef)]):
    """
    A reference to a node in the database.
    
    Contains the node's internal ID, key, and properties.
    Can be used for updates, edge operations, and traversals.
    
    Attributes:
        id: Internal node ID
        key: Full node key (e.g., "user:alice")
        node_def: The node definition this reference belongs to
        props: Dictionary of property values
    """
    __slots__ = ('id', 'key', 'node_def', 'props')
    
    def __init__(
        self,
        id: int,
        key: str,
        node_def: NodeDef[Any],
        props: Optional[Dict[str, Any]] = None,
    ):
        self.id = id
        self.key = key
        self.node_def = node_def
        self.props = props if props is not None else {}
    
    def __getattr__(self, name: str) -> Any:
        """Allow attribute-style access to properties."""
        # __slots__ classes don't have __dict__, so we check props directly
        props = object.__getattribute__(self, 'props')
        if name in props:
            return props[name]
        # Check if it's a valid property in the schema
        node_def = object.__getattribute__(self, 'node_def')
        if name in node_def.props:
            return None  # Property exists but wasn't set
        raise AttributeError(f"'{type(self).__name__}' object has no attribute '{name}'")
    
    def __repr__(self) -> str:
        props_str = ", ".join(f"{k}={v!r}" for k, v in self.props.items())
        return f"NodeRef(id={self.id}, key={self.key!r}, {props_str})"
    
    def __eq__(self, other: object) -> bool:
        if isinstance(other, NodeRef):
            return self.id == other.id
        return False
    
    def __hash__(self) -> int:
        return hash(self.id)


N = TypeVar("N", bound=NodeDef)
E = TypeVar("E", bound=EdgeDef)


# ============================================================================
# PropValue Conversion
# ============================================================================

def to_prop_value(prop_def: PropDef[Any], value: Any, PropValue: type) -> PropValue:
    """Convert a Python value to a PropValue based on the property definition."""
    if value is None:
        return PropValue.null()
    
    if prop_def.type == "string":
        return PropValue.string(str(value))
    elif prop_def.type == "int":
        return PropValue.int(int(value))
    elif prop_def.type == "float":
        return PropValue.float(float(value))
    elif prop_def.type == "bool":
        return PropValue.bool(bool(value))
    elif prop_def.type == "vector":
        # Vectors are handled separately
        raise ValueError("Vector properties should be set using set_node_vector")
    else:
        raise ValueError(f"Unknown property type: {prop_def.type}")


def from_prop_value(pv: PropValue) -> Any:
    """Convert a PropValue to a Python value."""
    return pv.value()


# ============================================================================
# Insert Builder
# ============================================================================

class InsertExecutor(Generic[N]):
    """
    Executor for insert operations.
    
    Can either return the created node(s) or execute without returning.
    """
    
    def __init__(
        self,
        db: Database,
        node_def: N,
        data: Union[Dict[str, Any], List[Dict[str, Any]]],
        resolve_prop_key_id: Callable[[NodeDef, str], int],
        use_batch: bool = False,
    ):
        self._db = db
        self._node_def = node_def
        self._data = data if isinstance(data, list) else [data]
        self._is_single = not isinstance(data, list)
        self._resolve_prop_key_id = resolve_prop_key_id
        self._use_batch = use_batch
    
    def returning(self) -> Union[NodeRef[N], List[NodeRef[N]]]:
        """Execute insert and return the created node(s)."""
        from raydb._raydb import PropValue
        
        # For batch inserts with many items, use Rust batch API
        if self._use_batch and len(self._data) > 1:
            return self._returning_batch()
        
        results: List[NodeRef[N]] = []
        
        # Check if we're already in a transaction
        in_tx = self._db.has_transaction()
        if not in_tx:
            self._db.begin()
        try:
            for item in self._data:
                key_arg = item.pop("key", None)
                if key_arg is None:
                    raise ValueError("Insert requires a 'key' field")
                
                full_key = self._node_def.key_fn(key_arg)
                
                # Create the node
                node_id = self._db.create_node(full_key)
                
                # Set properties
                for prop_name, value in item.items():
                    if value is None:
                        continue
                    prop_def = self._node_def.props.get(prop_name)
                    if prop_def is None:
                        continue
                    
                    prop_key_id = self._resolve_prop_key_id(self._node_def, prop_name)
                    prop_value = to_prop_value(prop_def, value, PropValue)
                    self._db.set_node_prop(node_id, prop_key_id, prop_value)
                
                results.append(NodeRef(
                    id=node_id,
                    key=full_key,
                    node_def=self._node_def,
                    props=item,
                ))
            
            if not in_tx:
                self._db.commit()
        except Exception:
            if not in_tx:
                self._db.rollback()
            raise
        
        return results[0] if self._is_single else results
    
    def _returning_batch(self) -> Union[NodeRef[N], List[NodeRef[N]]]:
        """Execute batch insert using Rust batch API."""
        from raydb._raydb import PropValue
        
        # Prepare batch data: list of (key, [(prop_key_id, PropValue)])
        batch_nodes = []
        items_for_results = []
        
        for item in self._data:
            item_copy = dict(item)  # Copy to preserve for results
            key_arg = item_copy.pop("key", None)
            if key_arg is None:
                raise ValueError("Insert requires a 'key' field")
            
            full_key = self._node_def.key_fn(key_arg)
            
            # Build props list
            props_list = []
            for prop_name, value in item_copy.items():
                if value is None:
                    continue
                prop_def = self._node_def.props.get(prop_name)
                if prop_def is None:
                    continue
                
                prop_key_id = self._resolve_prop_key_id(self._node_def, prop_name)
                prop_value = to_prop_value(prop_def, value, PropValue)
                props_list.append((prop_key_id, prop_value))
            
            batch_nodes.append((full_key, props_list))
            items_for_results.append((full_key, item_copy))
        
        # Execute batch insert in Rust
        node_ids = self._db.batch_create_nodes(batch_nodes)
        
        # Build results
        results = []
        for node_id, (full_key, item) in zip(node_ids, items_for_results):
            results.append(NodeRef(
                id=node_id,
                key=full_key,
                node_def=self._node_def,
                props=item,
            ))
        
        return results[0] if self._is_single else results
    
    def execute(self) -> None:
        """Execute insert without returning."""
        self.returning()  # Just discard the result


class InsertBuilder(Generic[N]):
    """
    Builder for insert operations.
    
    Example:
        >>> alice = db.insert(user).values(
        ...     key="alice",
        ...     name="Alice",
        ...     email="alice@example.com"
        ... ).returning()
    """
    
    def __init__(
        self,
        db: Database,
        node_def: N,
        resolve_prop_key_id: Callable[[NodeDef, str], int],
    ):
        self._db = db
        self._node_def = node_def
        self._resolve_prop_key_id = resolve_prop_key_id
    
    def values(
        self,
        data: Optional[Union[Dict[str, Any], List[Dict[str, Any]]]] = None,
        **kwargs: Any,
    ) -> InsertExecutor[N]:
        """
        Set the values to insert.
        
        Can be called with a dict, a list of dicts, or with keyword arguments.
        
        Args:
            data: Dictionary of property values (including 'key'),
                or a list of dictionaries
            **kwargs: Alternative way to pass property values
        
        Returns:
            InsertExecutor for executing the insert
        
        Example:
            >>> # Using dict
            >>> db.insert(user).values({"key": "alice", "name": "Alice"})
            >>> 
            >>> # Using kwargs
            >>> db.insert(user).values(key="alice", name="Alice")
            >>> 
            >>> # Using list
            >>> db.insert(user).values([
            ...     {"key": "alice", "name": "Alice"},
            ...     {"key": "bob", "name": "Bob"},
            ... ])
        """
        # Avoid unnecessary dict copy
        if data is None:
            data = kwargs
        elif isinstance(data, list):
            if kwargs:
                raise ValueError("Cannot combine list data with keyword arguments")
            return InsertExecutor(
                db=self._db,
                node_def=self._node_def,
                data=data,
                resolve_prop_key_id=self._resolve_prop_key_id,
                use_batch=True,
            )
        elif kwargs:
            data = {**data, **kwargs}
        # else: use data as-is

        return InsertExecutor(
            db=self._db,
            node_def=self._node_def,
            data=data,
            resolve_prop_key_id=self._resolve_prop_key_id,
        )
    
    def values_many(self, data: List[Dict[str, Any]], *, batch: bool = True) -> InsertExecutor[N]:
        """
        Insert multiple nodes at once.
        
        Args:
            data: List of dictionaries with property values
            batch: Use Rust batch API for better performance (default True)
        
        Returns:
            InsertExecutor for executing the batch insert
        
        Example:
            >>> users = db.insert(user).values_many([
            ...     {"key": "alice", "name": "Alice"},
            ...     {"key": "bob", "name": "Bob"},
            ...     {"key": "carol", "name": "Carol"},
            ... ]).returning()
        """
        return InsertExecutor(
            db=self._db,
            node_def=self._node_def,
            data=data,
            resolve_prop_key_id=self._resolve_prop_key_id,
            use_batch=batch,
        )


# ============================================================================
# Update Builder
# ============================================================================

class UpdateExecutor(Generic[N]):
    """Executor for update operations."""
    
    def __init__(
        self,
        db: Database,
        node_def: N,
        data: Dict[str, Any],
        resolve_prop_key_id: Callable[[NodeDef, str], int],
    ):
        self._db = db
        self._node_def = node_def
        self._data = data
        self._resolve_prop_key_id = resolve_prop_key_id
        self._where_id: Optional[int] = None
        self._where_key: Optional[str] = None
    
    def where(
        self,
        *,
        id: Optional[int] = None,
        key: Optional[str] = None,
    ) -> UpdateExecutor[N]:
        """
        Set the condition for which node to update.
        
        Args:
            id: Update node by internal ID
            key: Update node by full key (e.g., "user:alice")
        
        Returns:
            Self for chaining
        """
        self._where_id = id
        self._where_key = key
        return self
    
    def execute(self) -> None:
        """Execute the update."""
        from raydb._raydb import PropValue
        
        if self._where_id is None and self._where_key is None:
            raise ValueError("Update requires a where condition (id or key)")
        
        # Resolve node ID
        node_id: Optional[int] = self._where_id
        if node_id is None and self._where_key:
            node_id = self._db.get_node_by_key(self._where_key)
        
        if node_id is None:
            raise ValueError(f"Node not found: {self._where_key}")
        
        resolved_node_id: int = node_id  # Now guaranteed non-None
        
        # Check if we're already in a transaction
        in_tx = self._db.has_transaction()
        if not in_tx:
            self._db.begin()
        try:
            for prop_name, value in self._data.items():
                prop_def = self._node_def.props.get(prop_name)
                if prop_def is None:
                    continue
                
                prop_key_id = self._resolve_prop_key_id(self._node_def, prop_name)
                
                if value is None:
                    self._db.delete_node_prop(resolved_node_id, prop_key_id)
                else:
                    prop_value = to_prop_value(prop_def, value, PropValue)
                    self._db.set_node_prop(resolved_node_id, prop_key_id, prop_value)
            
            if not in_tx:
                self._db.commit()
        except Exception:
            if not in_tx:
                self._db.rollback()
            raise


class UpdateByRefExecutor:
    """Executor for updating a node by reference."""
    
    def __init__(
        self,
        db: Database,
        node_ref: NodeRef[Any],
        data: Dict[str, Any],
        resolve_prop_key_id: Callable[[NodeDef, str], int],
    ):
        self._db = db
        self._node_ref = node_ref
        self._data = data
        self._resolve_prop_key_id = resolve_prop_key_id
    
    def execute(self) -> None:
        """Execute the update."""
        from raydb._raydb import PropValue
        
        # Check if we're already in a transaction
        in_tx = self._db.has_transaction()
        if not in_tx:
            self._db.begin()
        try:
            for prop_name, value in self._data.items():
                prop_def = self._node_ref.node_def.props.get(prop_name)
                if prop_def is None:
                    continue
                
                prop_key_id = self._resolve_prop_key_id(self._node_ref.node_def, prop_name)
                
                if value is None:
                    self._db.delete_node_prop(self._node_ref.id, prop_key_id)
                else:
                    prop_value = to_prop_value(prop_def, value, PropValue)
                    self._db.set_node_prop(self._node_ref.id, prop_key_id, prop_value)
            
            if not in_tx:
                self._db.commit()
        except Exception:
            if not in_tx:
                self._db.rollback()
            raise


class UpdateBuilder(Generic[N]):
    """
    Builder for update operations by node definition.
    
    Example:
        >>> db.update(user).set(email="new@example.com").where(key="user:alice").execute()
    """
    
    def __init__(
        self,
        db: Database,
        node_def: N,
        resolve_prop_key_id: Callable[[NodeDef, str], int],
    ):
        self._db = db
        self._node_def = node_def
        self._resolve_prop_key_id = resolve_prop_key_id
    
    def set(self, data: Optional[Dict[str, Any]] = None, **kwargs: Any) -> UpdateExecutor[N]:
        """
        Set the properties to update.
        
        Args:
            data: Dictionary of property values
            **kwargs: Alternative way to pass property values
        
        Returns:
            UpdateExecutor for setting where condition and executing
        """
        # Avoid unnecessary dict copy
        if data is None:
            data = kwargs
        elif kwargs:
            data = {**data, **kwargs}
        # else: use data as-is
        
        return UpdateExecutor(
            db=self._db,
            node_def=self._node_def,
            data=data,
            resolve_prop_key_id=self._resolve_prop_key_id,
        )


class UpdateByRefBuilder:
    """
    Builder for update operations by node reference.
    
    Example:
        >>> db.update(alice).set(age=31).execute()
    """
    
    def __init__(
        self,
        db: Database,
        node_ref: NodeRef[Any],
        resolve_prop_key_id: Callable[[NodeDef, str], int],
    ):
        self._db = db
        self._node_ref = node_ref
        self._resolve_prop_key_id = resolve_prop_key_id
    
    def set(self, data: Optional[Dict[str, Any]] = None, **kwargs: Any) -> UpdateByRefExecutor:
        """
        Set the properties to update.
        
        Args:
            data: Dictionary of property values
            **kwargs: Alternative way to pass property values
        
        Returns:
            UpdateByRefExecutor for executing
        """
        # Avoid unnecessary dict copy
        if data is None:
            data = kwargs
        elif kwargs:
            data = {**data, **kwargs}
        # else: use data as-is
        
        return UpdateByRefExecutor(
            db=self._db,
            node_ref=self._node_ref,
            data=data,
            resolve_prop_key_id=self._resolve_prop_key_id,
        )


# ============================================================================
# Delete Builder
# ============================================================================

class DeleteExecutor:
    """Executor for delete operations."""
    
    def __init__(self, db: Database):
        self._db = db
        self._where_id: Optional[int] = None
        self._where_key: Optional[str] = None
    
    def where(
        self,
        *,
        id: Optional[int] = None,
        key: Optional[str] = None,
    ) -> DeleteExecutor:
        """
        Set the condition for which node to delete.
        
        Args:
            id: Delete node by internal ID
            key: Delete node by full key (e.g., "user:alice")
        
        Returns:
            Self for chaining
        """
        self._where_id = id
        self._where_key = key
        return self
    
    def execute(self) -> bool:
        """
        Execute the delete.
        
        Returns:
            True if a node was deleted, False otherwise
        """
        if self._where_id is None and self._where_key is None:
            raise ValueError("Delete requires a where condition (id or key)")
        
        # Resolve node ID
        node_id: Optional[int] = self._where_id
        if node_id is None and self._where_key:
            node_id = self._db.get_node_by_key(self._where_key)
        
        if node_id is None:
            return False
        
        resolved_node_id: int = node_id  # Now guaranteed non-None
        
        # Check if we're already in a transaction
        in_tx = self._db.has_transaction()
        if not in_tx:
            self._db.begin()
        try:
            self._db.delete_node(resolved_node_id)
            if not in_tx:
                self._db.commit()
            return True
        except Exception:
            if not in_tx:
                self._db.rollback()
            raise


class DeleteBuilder(Generic[N]):
    """
    Builder for delete operations.
    
    Example:
        >>> db.delete(user).where(key="user:bob").execute()
    """
    
    def __init__(self, db: Database, node_def: N):
        self._db = db
        self._node_def = node_def
    
    def where(
        self,
        *,
        id: Optional[int] = None,
        key: Optional[str] = None,
    ) -> DeleteExecutor:
        """
        Set the condition for which node to delete.
        
        Args:
            id: Delete node by internal ID
            key: Delete node by full key (e.g., "user:alice")
        
        Returns:
            DeleteExecutor for executing
        """
        executor = DeleteExecutor(self._db)
        return executor.where(id=id, key=key)


# ============================================================================
# Link Builder (Edge Creation)
# ============================================================================

def create_link(
    db: Database,
    src: NodeRef[Any],
    edge_def: EdgeDef,
    dst: NodeRef[Any],
    props: Optional[Dict[str, Any]],
    resolve_etype_id: Callable[[EdgeDef], int],
    resolve_prop_key_id: Callable[[EdgeDef, str], int],
) -> None:
    """
    Create an edge between two nodes.
    
    Args:
        db: Database instance
        src: Source node reference
        edge_def: Edge definition
        dst: Destination node reference
        props: Optional edge properties
        resolve_etype_id: Function to resolve edge type ID
        resolve_prop_key_id: Function to resolve property key ID
    """
    from raydb._raydb import PropValue
    
    etype_id = resolve_etype_id(edge_def)
    
    # Check if we're already in a transaction (e.g., from db.transaction() context)
    in_tx = db.has_transaction()
    if not in_tx:
        db.begin()
    try:
        db.add_edge(src.id, etype_id, dst.id)
        
        # Set edge properties if provided
        if props:
            for prop_name, value in props.items():
                if value is None:
                    continue
                prop_def = edge_def.props.get(prop_name)
                if prop_def is None:
                    continue
                
                prop_key_id = resolve_prop_key_id(edge_def, prop_name)
                prop_value = to_prop_value(prop_def, value, PropValue)
                db.set_edge_prop(src.id, etype_id, dst.id, prop_key_id, prop_value)
        
        if not in_tx:
            db.commit()
    except Exception:
        if not in_tx:
            db.rollback()
        raise


def delete_link(
    db: Database,
    src: NodeRef[Any],
    edge_def: EdgeDef,
    dst: NodeRef[Any],
    resolve_etype_id: Callable[[EdgeDef], int],
) -> None:
    """
    Delete an edge between two nodes.
    
    Args:
        db: Database instance
        src: Source node reference
        edge_def: Edge definition
        dst: Destination node reference
        resolve_etype_id: Function to resolve edge type ID
    """
    etype_id = resolve_etype_id(edge_def)
    
    # Check if we're already in a transaction
    in_tx = db.has_transaction()
    if not in_tx:
        db.begin()
    try:
        db.delete_edge(src.id, etype_id, dst.id)
        if not in_tx:
            db.commit()
    except Exception:
        if not in_tx:
            db.rollback()
        raise


# ============================================================================
# Update Edge Builder
# ============================================================================

class UpdateEdgeExecutor:
    """Executor for edge property updates."""
    
    def __init__(
        self,
        db: Database,
        src: NodeRef[Any],
        edge_def: EdgeDef,
        dst: NodeRef[Any],
        data: Dict[str, Any],
        resolve_etype_id: Callable[[EdgeDef], int],
        resolve_prop_key_id: Callable[[EdgeDef, str], int],
    ):
        self._db = db
        self._src = src
        self._edge_def = edge_def
        self._dst = dst
        self._data = data
        self._resolve_etype_id = resolve_etype_id
        self._resolve_prop_key_id = resolve_prop_key_id
    
    def execute(self) -> None:
        """Execute the edge property update."""
        from raydb._raydb import PropValue
        
        etype_id = self._resolve_etype_id(self._edge_def)
        
        # Check if we're already in a transaction
        in_tx = self._db.has_transaction()
        if not in_tx:
            self._db.begin()
        try:
            for prop_name, value in self._data.items():
                prop_def = self._edge_def.props.get(prop_name)
                if prop_def is None:
                    continue
                
                prop_key_id = self._resolve_prop_key_id(self._edge_def, prop_name)
                
                if value is None:
                    self._db.delete_edge_prop(
                        self._src.id, etype_id, self._dst.id, prop_key_id
                    )
                else:
                    prop_value = to_prop_value(prop_def, value, PropValue)
                    self._db.set_edge_prop(
                        self._src.id, etype_id, self._dst.id, prop_key_id, prop_value
                    )
            
            if not in_tx:
                self._db.commit()
        except Exception:
            if not in_tx:
                self._db.rollback()
            raise


class UpdateEdgeBuilder(Generic[E]):
    """
    Builder for edge property updates.
    
    Example:
        >>> db.update_edge(alice, knows, bob).set(weight=0.9).execute()
    """
    
    def __init__(
        self,
        db: Database,
        src: NodeRef[Any],
        edge_def: E,
        dst: NodeRef[Any],
        resolve_etype_id: Callable[[EdgeDef], int],
        resolve_prop_key_id: Callable[[EdgeDef, str], int],
    ):
        self._db = db
        self._src = src
        self._edge_def = edge_def
        self._dst = dst
        self._resolve_etype_id = resolve_etype_id
        self._resolve_prop_key_id = resolve_prop_key_id
    
    def set(self, data: Optional[Dict[str, Any]] = None, **kwargs: Any) -> UpdateEdgeExecutor:
        """
        Set the edge properties to update.
        
        Args:
            data: Dictionary of property values
            **kwargs: Alternative way to pass property values
        
        Returns:
            UpdateEdgeExecutor for executing
        """
        if data is None:
            data = kwargs
        else:
            data = {**data, **kwargs}
        
        return UpdateEdgeExecutor(
            db=self._db,
            src=self._src,
            edge_def=self._edge_def,
            dst=self._dst,
            data=data,
            resolve_etype_id=self._resolve_etype_id,
            resolve_prop_key_id=self._resolve_prop_key_id,
        )


__all__ = [
    "NodeRef",
    "InsertBuilder",
    "InsertExecutor",
    "UpdateBuilder",
    "UpdateExecutor",
    "UpdateByRefBuilder",
    "UpdateByRefExecutor",
    "DeleteBuilder",
    "DeleteExecutor",
    "UpdateEdgeBuilder",
    "UpdateEdgeExecutor",
    "create_link",
    "delete_link",
    "to_prop_value",
    "from_prop_value",
]
