"""
Traversal Builder for RayDB

Provides a fluent API for graph traversals, similar to the TypeScript API.

Example:
    >>> # Find friends of alice
    >>> friends = db.from_(alice).out(knows).nodes().to_list()
    >>> 
    >>> # Find friends who are under 35
    >>> young_friends = (
    ...     db.from_(alice)
    ...     .out(knows)
    ...     .where_node(lambda n: n.age is not None and n.age < 35)
    ...     .nodes()
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
# Traversal Result
# ============================================================================

class TraversalResult(Generic[N]):
    """
    Result of a traversal that can be iterated or collected.
    
    This is a lazy iterator - it doesn't execute until you call
    to_list(), first(), or iterate over it.
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
        include_edges: bool = False,
    ):
        self._db = db
        self._start_nodes = start_nodes
        self._steps = steps
        self._node_filter = node_filter
        self._resolve_etype_id = resolve_etype_id
        self._resolve_prop_key_id = resolve_prop_key_id
        self._get_node_def = get_node_def
        self._include_edges = include_edges
    
    def _load_node_props(self, node_id: int, node_def: NodeDef) -> Dict[str, Any]:
        """Load all properties for a node."""
        props: Dict[str, Any] = {}
        for prop_name, prop_def in node_def.props.items():
            prop_key_id = self._resolve_prop_key_id(node_def, prop_name)
            prop_value = self._db.get_node_prop(node_id, prop_key_id)
            if prop_value is not None:
                props[prop_name] = from_prop_value(prop_value)
        return props
    
    def _create_node_ref(self, node_id: int) -> Optional[NodeRef[Any]]:
        """Create a NodeRef from a node ID."""
        node_def = self._get_node_def(node_id)
        if node_def is None:
            return None
        
        key = self._db.get_node_key(node_id)
        if key is None:
            key = f"node:{node_id}"
        
        props = self._load_node_props(node_id, node_def)
        return NodeRef(id=node_id, key=key, node_def=node_def, props=props)
    
    def _execute(self) -> Generator[NodeRef[Any], None, None]:
        """Execute the traversal and yield results."""
        current_nodes = list(self._start_nodes)
        
        for step in self._steps:
            next_nodes: List[NodeRef[Any]] = []
            visited: set[int] = set()
            
            for node in current_nodes:
                # Get the edge type ID if specified
                etype_id = None
                if step.edge_def is not None:
                    etype_id = self._resolve_etype_id(step.edge_def)
                
                # Execute the traversal step
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
                        neighbor_ref = self._create_node_ref(neighbor_id)
                        if neighbor_ref is not None:
                            next_nodes.append(neighbor_ref)
            
            current_nodes = next_nodes
        
        # Apply filter and yield results
        for node in current_nodes:
            if self._node_filter is None or self._node_filter(node):
                yield node
    
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
        
        Returns:
            Number of matching nodes
        """
        return sum(1 for _ in self._execute())


# ============================================================================
# Traversal Builder
# ============================================================================

class TraversalBuilder(Generic[N]):
    """
    Builder for graph traversals.
    
    Example:
        >>> # Find all friends
        >>> friends = db.from_(alice).out(knows).nodes().to_list()
        >>> 
        >>> # Find friends of friends
        >>> fof = db.from_(alice).out(knows).out(knows).nodes().to_list()
        >>> 
        >>> # Find young friends
        >>> young = (
        ...     db.from_(alice)
        ...     .out(knows)
        ...     .where_node(lambda n: n.age < 35)
        ...     .nodes()
        ...     .to_list()
        ... )
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
    
    def where_node(self, predicate: Callable[[NodeRef[Any]], bool]) -> TraversalBuilder[N]:
        """
        Filter nodes by a predicate.
        
        The predicate receives a NodeRef with all properties loaded.
        
        Args:
            predicate: Function that returns True for nodes to include
        
        Returns:
            Self for chaining
        
        Example:
            >>> # Filter by property value
            >>> .where_node(lambda n: n.age < 35)
            >>> 
            >>> # Filter by property existence
            >>> .where_node(lambda n: n.email is not None)
        """
        self._node_filter = predicate
        return self
    
    def nodes(self) -> TraversalResult[N]:
        """
        Return node results.
        
        Returns:
            TraversalResult that can be iterated or collected
        """
        return TraversalResult(
            db=self._db,
            start_nodes=self._start_nodes,
            steps=self._steps,
            node_filter=self._node_filter,
            resolve_etype_id=self._resolve_etype_id,
            resolve_prop_key_id=self._resolve_prop_key_id,
            get_node_def=self._get_node_def,
            include_edges=False,
        )
    
    def to_list(self) -> List[NodeRef[N]]:
        """
        Shortcut for .nodes().to_list()
        
        Returns:
            List of NodeRef objects
        """
        return self.nodes().to_list()
    
    def first(self) -> Optional[NodeRef[N]]:
        """
        Shortcut for .nodes().first()
        
        Returns:
            First NodeRef or None
        """
        return self.nodes().first()
    
    def count(self) -> int:
        """
        Shortcut for .nodes().count()
        
        Returns:
            Number of matching nodes
        """
        return self.nodes().count()


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
    
    def _create_node_ref(self, node_id: int) -> Optional[NodeRef[Any]]:
        """Create a NodeRef from a node ID."""
        node_def = self._get_node_def(node_id)
        if node_def is None:
            return None
        
        key = self._db.get_node_key(node_id)
        if key is None:
            key = f"node:{node_id}"
        
        # Load properties
        props: Dict[str, Any] = {}
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
    "OutStep",
    "InStep",
    "BothStep",
    "TraversalStep",
]
