"""
Traversal Builder for RayDB

Provides a fluent API for graph traversals with lazy property loading.

By default, traversals are fast and don't load properties. Use `.load_props()`
or `.with_props()` to opt-in to property loading when needed.

Example:
    >>> # Fast traversal - no properties loaded (just IDs and keys)
    >>> friend_ids = db.from_(alice).out(knows).to_list()
    >>> 
    >>> # Opt-in to load properties when you need them
    >>> friends_with_props = db.from_(alice).out(knows).with_props().to_list()
    >>> 
    >>> # Load specific properties only
    >>> friends = db.from_(alice).out(knows).load_props("name", "age").to_list()
    >>> 
    >>> # Filter requires properties - auto-loads them
    >>> young_friends = (
    ...     db.from_(alice)
    ...     .out(knows)
    ...     .where_node(lambda n: n.age is not None and n.age < 35)
    ...     .to_list()
    ... )
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import (
    TYPE_CHECKING,
    Any,
    Callable,
    Dict,
    Generator,
    Generic,
    Iterator,
    List,
    Literal,
    Optional,
    Set,
    Tuple,
    TypeVar,
    Union,
)

from .builders import NodeRef, from_prop_value
from .schema import EdgeDef, NodeDef

if TYPE_CHECKING:
    from raydb._raydb import Database


N = TypeVar("N", bound=NodeDef)


# ============================================================================
# Traversal Step Types
# ============================================================================

@dataclass
class OutStep:
    """Traverse outgoing edges."""
    type: Literal["out"] = "out"
    edge_def: Optional[EdgeDef] = None


@dataclass
class InStep:
    """Traverse incoming edges."""
    type: Literal["in"] = "in"
    edge_def: Optional[EdgeDef] = None


@dataclass
class BothStep:
    """Traverse both directions."""
    type: Literal["both"] = "both"
    edge_def: Optional[EdgeDef] = None


TraversalStep = Union[OutStep, InStep, BothStep]


# ============================================================================
# Property Loading Strategy
# ============================================================================

@dataclass
class PropLoadStrategy:
    """Strategy for loading properties."""
    load_all: bool = False
    prop_names: Optional[Set[str]] = None
    
    @staticmethod
    def none() -> PropLoadStrategy:
        """Don't load any properties."""
        return PropLoadStrategy(load_all=False, prop_names=None)
    
    @staticmethod
    def all() -> PropLoadStrategy:
        """Load all properties."""
        return PropLoadStrategy(load_all=True, prop_names=None)
    
    @staticmethod
    def only(*names: str) -> PropLoadStrategy:
        """Load only specified properties."""
        return PropLoadStrategy(load_all=False, prop_names=set(names))
    
    def should_load(self, prop_name: str) -> bool:
        """Check if a property should be loaded."""
        if self.load_all:
            return True
        if self.prop_names is not None:
            return prop_name in self.prop_names
        return False
    
    def needs_any_props(self) -> bool:
        """Check if any properties need to be loaded."""
        return self.load_all or (self.prop_names is not None and len(self.prop_names) > 0)


# ============================================================================
# Traversal Result
# ============================================================================

class TraversalResult(Generic[N]):
    """
    Result of a traversal that can be iterated or collected.
    
    This is a lazy iterator - it doesn't execute until you call
    to_list(), first(), or iterate over it.
    
    By default, no properties are loaded for performance. Use:
    - `.with_props()` to load all properties
    - `.load_props("name", "age")` to load specific properties
    """
    
    def __init__(
        self,
        db: Database,
        start_nodes: List[NodeRef[Any]],
        steps: List[TraversalStep],
        node_filter: Optional[Callable[[NodeRef[Any]], bool]],
        resolve_etype_id: Callable[[EdgeDef], int],
        resolve_prop_key_id: Callable[[NodeDef, str], int],
        get_node_def: Callable[[int], Optional[NodeDef]],
        prop_strategy: PropLoadStrategy,
    ):
        self._db = db
        self._start_nodes = start_nodes
        self._steps = steps
        self._node_filter = node_filter
        self._resolve_etype_id = resolve_etype_id
        self._resolve_prop_key_id = resolve_prop_key_id
        self._get_node_def = get_node_def
        self._prop_strategy = prop_strategy
    
    def _load_node_props(self, node_id: int, node_def: NodeDef) -> Dict[str, Any]:
        """Load properties for a node based on strategy using single FFI call."""
        props: Dict[str, Any] = {}
        
        if not self._prop_strategy.needs_any_props():
            return props
        
        # Use get_node_props() for single FFI call instead of per-property calls
        all_props = self._db.get_node_props(node_id)
        if all_props is None:
            return props
        
        # Build reverse mapping: prop_key_id -> prop_name
        key_id_to_name = {v: k for k, v in node_def._prop_key_ids.items()}
        
        for node_prop in all_props:
            prop_name = key_id_to_name.get(node_prop.key_id)
            if prop_name is not None and self._prop_strategy.should_load(prop_name):
                props[prop_name] = from_prop_value(node_prop.value)
        
        return props
    
    def _create_node_ref(self, node_id: int, load_props: bool = False) -> Optional[NodeRef[Any]]:
        """Create a NodeRef from a node ID."""
        node_def = self._get_node_def(node_id)
        if node_def is None:
            return None
        
        key = self._db.get_node_key(node_id)
        if key is None:
            key = f"node:{node_id}"
        
        if load_props:
            props = self._load_node_props(node_id, node_def)
        else:
            props = {}
        
        return NodeRef(id=node_id, key=key, node_def=node_def, props=props)
    
    def _create_node_ref_fast(self, node_id: int, node_def: NodeDef) -> NodeRef[Any]:
        """Create a minimal NodeRef without loading key or properties."""
        return NodeRef(id=node_id, key="", node_def=node_def, props={})
    
    def _build_steps_for_rust(self) -> List[Tuple[str, Optional[int]]]:
        """Build step tuples for Rust traverse_multi call."""
        rust_steps = []
        for step in self._steps:
            etype_id = None
            if step.edge_def is not None:
                etype_id = step.edge_def._etype_id
                if etype_id is None:
                    etype_id = self._resolve_etype_id(step.edge_def)
            rust_steps.append((step.type, etype_id))
        return rust_steps
    
    def _execute_fast(self) -> Generator[int, None, None]:
        """Execute traversal and yield only node IDs (fastest path)."""
        if not self._steps:
            for node in self._start_nodes:
                yield node.id
            return
        
        # For single step, use direct call (lower overhead)
        # For multi-step, use Rust batch traversal
        if len(self._steps) == 1:
            step = self._steps[0]
            etype_id = None
            if step.edge_def is not None:
                etype_id = step.edge_def._etype_id
                if etype_id is None:
                    etype_id = self._resolve_etype_id(step.edge_def)
            
            visited: Set[int] = set()
            for node in self._start_nodes:
                if step.type == "out":
                    neighbor_ids = self._db.traverse_out(node.id, etype_id)
                elif step.type == "in":
                    neighbor_ids = self._db.traverse_in(node.id, etype_id)
                else:  # both
                    out_ids = self._db.traverse_out(node.id, etype_id)
                    in_ids = self._db.traverse_in(node.id, etype_id)
                    neighbor_ids = list(set(out_ids) | set(in_ids))
                
                for neighbor_id in neighbor_ids:
                    if neighbor_id not in visited:
                        visited.add(neighbor_id)
                        yield neighbor_id
        else:
            # Multi-step: use Rust batch traversal
            start_ids = [node.id for node in self._start_nodes]
            rust_steps = self._build_steps_for_rust()
            results = self._db.traverse_multi(start_ids, rust_steps)
            for node_id, _ in results:
                yield node_id
    
    def _execute_fast_with_keys(self) -> Generator[Tuple[int, str], None, None]:
        """Execute traversal and yield (node_id, key) pairs."""
        # No steps - just yield start nodes
        if not self._steps:
            for node in self._start_nodes:
                yield (node.id, node.key)
            return
        
        # For single step, use direct batch call (lower overhead)
        if len(self._steps) == 1:
            step = self._steps[0]
            etype_id = None
            if step.edge_def is not None:
                etype_id = step.edge_def._etype_id
                if etype_id is None:
                    etype_id = self._resolve_etype_id(step.edge_def)
            
            visited: Set[int] = set()
            for node in self._start_nodes:
                if step.type == "out":
                    pairs = self._db.traverse_out_with_keys(node.id, etype_id)
                elif step.type == "in":
                    pairs = self._db.traverse_in_with_keys(node.id, etype_id)
                else:  # both
                    out_pairs = self._db.traverse_out_with_keys(node.id, etype_id)
                    in_pairs = self._db.traverse_in_with_keys(node.id, etype_id)
                    seen = set()
                    pairs = []
                    for nid, key in out_pairs + in_pairs:
                        if nid not in seen:
                            seen.add(nid)
                            pairs.append((nid, key))
                
                for neighbor_id, key in pairs:
                    if neighbor_id not in visited:
                        visited.add(neighbor_id)
                        yield (neighbor_id, key or f"node:{neighbor_id}")
        else:
            # Multi-step: use Rust batch traversal
            start_ids = [node.id for node in self._start_nodes]
            rust_steps = self._build_steps_for_rust()
            results = self._db.traverse_multi(start_ids, rust_steps)
            for node_id, key in results:
                yield (node_id, key or f"node:{node_id}")
    
    def _execute_fast_count(self) -> int:
        """Execute traversal and return just the count."""
        if not self._steps:
            return len(self._start_nodes)
        
        # For single step, count directly
        if len(self._steps) == 1:
            step = self._steps[0]
            etype_id = None
            if step.edge_def is not None:
                etype_id = step.edge_def._etype_id
                if etype_id is None:
                    etype_id = self._resolve_etype_id(step.edge_def)
            
            visited: Set[int] = set()
            for node in self._start_nodes:
                if step.type == "out":
                    neighbor_ids = self._db.traverse_out(node.id, etype_id)
                elif step.type == "in":
                    neighbor_ids = self._db.traverse_in(node.id, etype_id)
                else:  # both
                    out_ids = self._db.traverse_out(node.id, etype_id)
                    in_ids = self._db.traverse_in(node.id, etype_id)
                    neighbor_ids = list(set(out_ids) | set(in_ids))
                
                for neighbor_id in neighbor_ids:
                    visited.add(neighbor_id)
            
            return len(visited)
        else:
            # Multi-step: use Rust batch traversal count
            start_ids = [node.id for node in self._start_nodes]
            rust_steps = self._build_steps_for_rust()
            return self._db.traverse_multi_count(start_ids, rust_steps)
    
    def _execute(self) -> Generator[NodeRef[Any], None, None]:
        """Execute the traversal and yield results."""
        # Fast path: no filter and no properties needed
        needs_filter = self._node_filter is not None
        needs_props = self._prop_strategy.needs_any_props()
        
        if not needs_filter and not needs_props:
            # Ultra-fast path: use batch operation to get IDs + keys in one call
            for node_id, key in self._execute_fast_with_keys():
                node_def = self._get_node_def(node_id)
                if node_def is not None:
                    yield NodeRef(id=node_id, key=key, node_def=node_def, props={})
            return
        
        # Standard path: may need to load properties
        for node_id in self._execute_fast():
            node_ref = self._create_node_ref(node_id, load_props=needs_props or needs_filter)
            if node_ref is not None:
                if self._node_filter is None or self._node_filter(node_ref):
                    yield node_ref
    
    def __iter__(self) -> Iterator[NodeRef[Any]]:
        """Iterate over the traversal results."""
        return iter(self._execute())
    
    def to_list(self) -> List[NodeRef[N]]:
        """
        Execute the traversal and collect results into a list.
        
        Returns:
            List of NodeRef objects
        """
        return list(self._execute())  # type: ignore
    
    def first(self) -> Optional[NodeRef[N]]:
        """
        Execute the traversal and return the first result.
        
        Returns:
            First NodeRef or None if no results
        """
        for node in self._execute():
            return node  # type: ignore
        return None
    
    def count(self) -> int:
        """
        Execute the traversal and count results.
        
        This is optimized to not load properties when counting.
        
        Returns:
            Number of matching nodes
        """
        if self._node_filter is None:
            # Fast count - optimized path that avoids generator overhead
            return self._execute_fast_count()
        else:
            # Need to check filter - must load props
            return sum(1 for _ in self._execute())
    
    def ids(self) -> List[int]:
        """
        Get just the node IDs (fastest possible).
        
        Returns:
            List of node IDs
        """
        return list(self._execute_fast())
    
    def keys(self) -> List[str]:
        """
        Get just the node keys.
        
        Returns:
            List of node keys
        """
        result = []
        for node_id in self._execute_fast():
            key = self._db.get_node_key(node_id)
            if key:
                result.append(key)
        return result


# ============================================================================
# Traversal Builder
# ============================================================================

class TraversalBuilder(Generic[N]):
    """
    Builder for graph traversals.
    
    By default, traversals are fast and don't load properties.
    Use `.with_props()` or `.load_props()` to opt-in to loading.
    
    Example:
        >>> # Fast: no properties loaded
        >>> friend_ids = db.from_(alice).out(knows).ids()
        >>> friend_keys = db.from_(alice).out(knows).keys()
        >>> friend_refs = db.from_(alice).out(knows).to_list()
        >>> 
        >>> # Load all properties
        >>> friends = db.from_(alice).out(knows).with_props().to_list()
        >>> 
        >>> # Load specific properties only
        >>> friends = db.from_(alice).out(knows).load_props("name").to_list()
        >>> 
        >>> # Filter automatically loads properties
        >>> young = db.from_(alice).out(knows).where_node(lambda n: n.age < 35).to_list()
    """
    
    def __init__(
        self,
        db: Database,
        start_nodes: List[NodeRef[Any]],
        resolve_etype_id: Callable[[EdgeDef], int],
        resolve_prop_key_id: Callable[[NodeDef, str], int],
        get_node_def: Callable[[int], Optional[NodeDef]],
    ):
        self._db = db
        self._start_nodes = start_nodes
        self._resolve_etype_id = resolve_etype_id
        self._resolve_prop_key_id = resolve_prop_key_id
        self._get_node_def = get_node_def
        self._steps: List[TraversalStep] = []
        self._node_filter: Optional[Callable[[NodeRef[Any]], bool]] = None
        self._prop_strategy: PropLoadStrategy = PropLoadStrategy.none()
    
    def out(self, edge: Optional[EdgeDef] = None) -> TraversalBuilder[N]:
        """
        Traverse outgoing edges.
        
        Args:
            edge: Optional edge definition to filter by type
        
        Returns:
            Self for chaining
        """
        self._steps.append(OutStep(edge_def=edge))
        return self
    
    def in_(self, edge: Optional[EdgeDef] = None) -> TraversalBuilder[N]:
        """
        Traverse incoming edges.
        
        Note: Named `in_` because `in` is a Python reserved word.
        
        Args:
            edge: Optional edge definition to filter by type
        
        Returns:
            Self for chaining
        """
        self._steps.append(InStep(edge_def=edge))
        return self
    
    def both(self, edge: Optional[EdgeDef] = None) -> TraversalBuilder[N]:
        """
        Traverse both incoming and outgoing edges.
        
        Args:
            edge: Optional edge definition to filter by type
        
        Returns:
            Self for chaining
        """
        self._steps.append(BothStep(edge_def=edge))
        return self
    
    def with_props(self) -> TraversalBuilder[N]:
        """
        Load all properties for traversed nodes.
        
        This is slower but gives you access to all node properties.
        
        Returns:
            Self for chaining
        
        Example:
            >>> friends = db.from_(alice).out(knows).with_props().to_list()
            >>> for f in friends:
            ...     print(f.name, f.email)
        """
        self._prop_strategy = PropLoadStrategy.all()
        return self
    
    def load_props(self, *prop_names: str) -> TraversalBuilder[N]:
        """
        Load only specific properties for traversed nodes.
        
        This is faster than with_props() when you only need a few properties.
        
        Args:
            *prop_names: Names of properties to load
        
        Returns:
            Self for chaining
        
        Example:
            >>> friends = db.from_(alice).out(knows).load_props("name").to_list()
            >>> for f in friends:
            ...     print(f.name)  # Available
            ...     print(f.email)  # Will be None
        """
        self._prop_strategy = PropLoadStrategy.only(*prop_names)
        return self
    
    def where_node(self, predicate: Callable[[NodeRef[Any]], bool]) -> TraversalBuilder[N]:
        """
        Filter nodes by a predicate.
        
        Note: Using a filter will automatically load all properties
        since the predicate may access any property.
        
        Args:
            predicate: Function that returns True for nodes to include
        
        Returns:
            Self for chaining
        
        Example:
            >>> young_friends = (
            ...     db.from_(alice)
            ...     .out(knows)
            ...     .where_node(lambda n: n.age is not None and n.age < 35)
            ...     .to_list()
            ... )
        """
        self._node_filter = predicate
        # Filter needs properties to work, so enable loading all
        self._prop_strategy = PropLoadStrategy.all()
        return self
    
    def _build_result(self) -> TraversalResult[N]:
        """Build the traversal result."""
        return TraversalResult(
            db=self._db,
            start_nodes=self._start_nodes,
            steps=self._steps,
            node_filter=self._node_filter,
            resolve_etype_id=self._resolve_etype_id,
            resolve_prop_key_id=self._resolve_prop_key_id,
            get_node_def=self._get_node_def,
            prop_strategy=self._prop_strategy,
        )
    
    def nodes(self) -> TraversalResult[N]:
        """
        Return node results.
        
        Returns:
            TraversalResult that can be iterated or collected
        """
        return self._build_result()
    
    def to_list(self) -> List[NodeRef[N]]:
        """
        Shortcut for .nodes().to_list()
        
        Returns:
            List of NodeRef objects
        """
        return self._build_result().to_list()
    
    def first(self) -> Optional[NodeRef[N]]:
        """
        Shortcut for .nodes().first()
        
        Returns:
            First NodeRef or None
        """
        return self._build_result().first()
    
    def count(self) -> int:
        """
        Shortcut for .nodes().count()
        
        This is optimized to not load properties when counting
        (unless a filter is set).
        
        Returns:
            Number of matching nodes
        """
        return self._build_result().count()
    
    def ids(self) -> List[int]:
        """
        Get just the node IDs (fastest possible).
        
        Returns:
            List of node IDs
        """
        return self._build_result().ids()
    
    def keys(self) -> List[str]:
        """
        Get just the node keys.
        
        Returns:
            List of node keys
        """
        return self._build_result().keys()


# ============================================================================
# Pathfinding Builder (simplified version)
# ============================================================================

@dataclass
class PathResult(Generic[N]):
    """
    Result of a pathfinding query.
    
    Attributes:
        nodes: List of node references in the path
        found: Whether a path was found
        total_weight: Total path weight (for weighted paths)
    """
    nodes: List[NodeRef[N]]
    found: bool
    total_weight: float = 0.0
    
    def __bool__(self) -> bool:
        return self.found
    
    def __len__(self) -> int:
        return len(self.nodes)


class PathFindingBuilder(Generic[N]):
    """
    Builder for pathfinding queries.
    
    Example:
        >>> path = db.shortest_path(alice).to(bob).find()
        >>> if path:
        ...     for node in path.nodes:
        ...         print(node.key)
    """
    
    def __init__(
        self,
        db: Database,
        source: NodeRef[N],
        resolve_etype_id: Callable[[EdgeDef], int],
        resolve_prop_key_id: Callable[[NodeDef, str], int],
        get_node_def: Callable[[int], Optional[NodeDef]],
    ):
        self._db = db
        self._source = source
        self._resolve_etype_id = resolve_etype_id
        self._resolve_prop_key_id = resolve_prop_key_id
        self._get_node_def = get_node_def
        self._target: Optional[NodeRef[Any]] = None
        self._edge_type: Optional[EdgeDef] = None
        self._max_depth: Optional[int] = None
        self._direction: str = "out"
        self._load_props: bool = False
    
    def to(self, target: NodeRef[Any]) -> PathFindingBuilder[N]:
        """Set the target node."""
        self._target = target
        return self
    
    def via(self, edge: EdgeDef) -> PathFindingBuilder[N]:
        """Filter by edge type."""
        self._edge_type = edge
        return self
    
    def max_depth(self, depth: int) -> PathFindingBuilder[N]:
        """Set maximum path length."""
        self._max_depth = depth
        return self
    
    def direction(self, dir: Literal["out", "in", "both"]) -> PathFindingBuilder[N]:
        """Set traversal direction."""
        self._direction = dir
        return self
    
    def with_props(self) -> PathFindingBuilder[N]:
        """Load properties for nodes in the path."""
        self._load_props = True
        return self
    
    def _create_node_ref(self, node_id: int) -> Optional[NodeRef[Any]]:
        """Create a NodeRef from a node ID."""
        node_def = self._get_node_def(node_id)
        if node_def is None:
            return None
        
        key = self._db.get_node_key(node_id)
        if key is None:
            key = f"node:{node_id}"
        
        props: Dict[str, Any] = {}
        if self._load_props:
            for prop_name, prop_def in node_def.props.items():
                prop_key_id = self._resolve_prop_key_id(node_def, prop_name)
                prop_value = self._db.get_node_prop(node_id, prop_key_id)
                if prop_value is not None:
                    props[prop_name] = from_prop_value(prop_value)
        
        return NodeRef(id=node_id, key=key, node_def=node_def, props=props)
    
    def find(self) -> PathResult[N]:
        """
        Find the shortest path using BFS.
        
        Returns:
            PathResult containing the path if found
        """
        if self._target is None:
            raise ValueError("Target node required. Use .to(target) first.")
        
        etype_id = None
        if self._edge_type is not None:
            etype_id = self._resolve_etype_id(self._edge_type)
        
        result = self._db.find_path_bfs(
            source=self._source.id,
            target=self._target.id,
            etype=etype_id,
            max_depth=self._max_depth,
            direction=self._direction,
        )
        
        if not result.found:
            return PathResult(nodes=[], found=False)
        
        # Convert path node IDs to NodeRefs
        nodes: List[NodeRef[N]] = []
        for node_id in result.path:
            node_ref = self._create_node_ref(node_id)
            if node_ref is not None:
                nodes.append(node_ref)  # type: ignore
        
        return PathResult(
            nodes=nodes,
            found=True,
            total_weight=result.total_weight,
        )
    
    def find_weighted(self) -> PathResult[N]:
        """
        Find the shortest weighted path using Dijkstra.
        
        Returns:
            PathResult containing the path if found
        """
        if self._target is None:
            raise ValueError("Target node required. Use .to(target) first.")
        
        etype_id = None
        if self._edge_type is not None:
            etype_id = self._resolve_etype_id(self._edge_type)
        
        result = self._db.find_path_dijkstra(
            source=self._source.id,
            target=self._target.id,
            etype=etype_id,
            max_depth=self._max_depth,
            direction=self._direction,
        )
        
        if not result.found:
            return PathResult(nodes=[], found=False)
        
        # Convert path node IDs to NodeRefs
        nodes: List[NodeRef[N]] = []
        for node_id in result.path:
            node_ref = self._create_node_ref(node_id)
            if node_ref is not None:
                nodes.append(node_ref)  # type: ignore
        
        return PathResult(
            nodes=nodes,
            found=True,
            total_weight=result.total_weight,
        )
    
    def exists(self) -> bool:
        """
        Check if a path exists between source and target.
        
        Returns:
            True if a path exists
        """
        if self._target is None:
            raise ValueError("Target node required. Use .to(target) first.")
        
        etype_id = None
        if self._edge_type is not None:
            etype_id = self._resolve_etype_id(self._edge_type)
        
        return self._db.has_path(
            source=self._source.id,
            target=self._target.id,
            etype=etype_id,
            max_depth=self._max_depth,
        )


__all__ = [
    "TraversalBuilder",
    "TraversalResult",
    "PathFindingBuilder",
    "PathResult",
    "PropLoadStrategy",
    "OutStep",
    "InStep",
    "BothStep",
    "TraversalStep",
]
