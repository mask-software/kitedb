"""
Traversal Builder for KiteDB

Provides a fluent API for graph traversals with lazy property loading.

By default, traversals load all properties. Use `.select([...])` or
`.load_props(...)` to load a subset for performance.

Example:
    >>> # Default traversal - properties loaded
    >>> friends = db.from_(alice).out(knows).to_list()
    >>> 
    >>> # Load specific properties only
    >>> friends = db.from_(alice).out(knows).select(["name", "age"]).to_list()
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

from .builders import NodeRef, from_prop_value, prop_names_by_key_id
from .schema import EdgeDef, NodeDef

if TYPE_CHECKING:
    from kitedb._kitedb import Database


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


_DIRECTIONS = ("out", "in", "both")


def _check_direction(direction: str) -> str:
    """Return `direction`, or raise ValueError if it isn't out, in or both."""
    if direction not in _DIRECTIONS:
        raise ValueError(
            f"unknown direction {direction!r}; expected one of: out, in, both"
        )
    return direction


@dataclass
class TraverseOptions:
    """Options for variable-depth traversal."""
    max_depth: int
    min_depth: int = 1
    direction: Literal["out", "in", "both"] = "out"
    unique: bool = True
    where_edge: Optional[Callable[["EdgeResult"], bool]] = None
    where_node: Optional[Callable[[NodeRef[Any]], bool]] = None

    def __post_init__(self) -> None:
        _check_direction(self.direction)


@dataclass
class TraverseStep:
    """Variable-depth traversal step."""
    type: Literal["traverse"] = "traverse"
    edge_def: Optional[EdgeDef] = None
    options: TraverseOptions = field(default_factory=lambda: TraverseOptions(max_depth=1))


TraversalStep = Union[OutStep, InStep, BothStep, TraverseStep]


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
# Edge Results
# ============================================================================


@dataclass
class EdgeResult:
    """Edge result with optional properties."""
    src: int
    etype: int
    dst: int
    props: Dict[str, Any] = field(default_factory=dict)

    def __getattr__(self, name: str) -> Any:
        props = object.__getattribute__(self, "props")
        if name in props:
            return props[name]
        raise AttributeError(f"'{type(self).__name__}' object has no attribute '{name}'")

    def __getitem__(self, key: str) -> Any:
        if key == "src":
            return self.src
        if key == "dst":
            return self.dst
        if key == "etype":
            return self.etype
        props = object.__getattribute__(self, "props")
        if key in props:
            return props[key]
        raise KeyError(key)

    def to_dict(self) -> Dict[str, Any]:
        data = {"src": self.src, "dst": self.dst, "etype": self.etype}
        data.update(self.props)
        return data


@dataclass
class RawEdge:
    """Raw edge data without property loading."""
    src: int
    etype: int
    dst: int


# ============================================================================
# Traversal Result
# ============================================================================

class TraversalResult(Generic[N]):
    """
    Result of a traversal that can be iterated or collected.
    
    This is a lazy iterator - it doesn't execute until you call
    to_list(), first(), or iterate over it.
    
    By default, all properties are loaded. Use `.select([...])` or
    `.load_props("name", "age")` to load specific properties.
    """
    
    def __init__(
        self,
        db: Database,
        start_nodes: List[NodeRef[Any]],
        steps: List[TraversalStep],
        node_filter: Optional[Callable[[NodeRef[Any]], bool]],
        edge_filter: Optional[Callable[[EdgeResult], bool]],
        limit: Optional[int],
        resolve_etype_id: Callable[[EdgeDef], int],
        resolve_prop_key_id: Callable[[NodeDef, str], int],
        get_node_def: Callable[[int], Optional[NodeDef]],
        prop_strategy: PropLoadStrategy,
    ):
        self._db = db
        self._start_nodes = start_nodes
        self._steps = steps
        self._node_filter = node_filter
        self._edge_filter = edge_filter
        self._limit = limit
        self._resolve_etype_id = resolve_etype_id
        self._resolve_prop_key_id = resolve_prop_key_id
        self._get_node_def = get_node_def
        self._prop_strategy = prop_strategy
        self._prop_names_cache: Dict[str, Dict[int, str]] = {}

    def _prop_names(self, node_def: NodeDef) -> Dict[int, str]:
        """Property key ID -> property name for a node type (cached)."""
        names = self._prop_names_cache.get(node_def.name)
        if names is None:
            names = prop_names_by_key_id(node_def, self._resolve_prop_key_id)
            self._prop_names_cache[node_def.name] = names
        return names

    def _step_etype_id(self, step: TraversalStep) -> Optional[int]:
        """Edge type ID a step filters on, or None for any type."""
        if step.edge_def is None:
            return None
        return self._resolve_etype_id(step.edge_def)
    
    def _load_node_props(
        self,
        node_id: int,
        node_def: NodeDef,
        prop_strategy: Optional[PropLoadStrategy] = None,
    ) -> Dict[str, Any]:
        """Load properties for a node based on strategy using single FFI call."""
        props: Dict[str, Any] = {}

        strategy = prop_strategy or self._prop_strategy

        if not strategy.needs_any_props():
            return props
        
        # Use get_node_props() for single FFI call instead of per-property calls
        all_props = self._db.get_node_props(node_id)
        if all_props is None:
            return props
        
        key_id_to_name = self._prop_names(node_def)
        
        for node_prop in all_props:
            prop_name = key_id_to_name.get(node_prop.key_id)
            if prop_name is not None and strategy.should_load(prop_name):
                props[prop_name] = from_prop_value(node_prop.value)
        
        return props

    def _load_edge_props(self, edge_def: Optional[EdgeDef], src: int, etype: int, dst: int) -> Dict[str, Any]:
        """Load edge properties using edge definition mapping."""
        if edge_def is None or not edge_def.props:
            return {}

        props: Dict[str, Any] = {}
        for prop_name in edge_def.props.keys():
            try:
                prop_key_id = self._resolve_prop_key_id(edge_def, prop_name)
            except Exception:
                continue
            prop_value = self._db.get_edge_prop(src, etype, dst, prop_key_id)
            if prop_value is not None:
                props[prop_name] = from_prop_value(prop_value)
        return props

    def _build_edge_result(
        self,
        edge_def: Optional[EdgeDef],
        src: int,
        etype: int,
        dst: int,
    ) -> EdgeResult:
        props = self._load_edge_props(edge_def, src, etype, dst)
        return EdgeResult(src=src, etype=etype, dst=dst, props=props)
    
    def _create_node_ref(
        self,
        node_id: int,
        load_props: bool = False,
        prop_strategy: Optional[PropLoadStrategy] = None,
    ) -> Optional[NodeRef[Any]]:
        """Create a NodeRef from a node ID."""
        node_def = self._get_node_def(node_id)
        if node_def is None:
            return None
        
        key = self._db.get_node_key(node_id)
        if key is None:
            key = f"node:{node_id}"
        
        if load_props:
            props = self._load_node_props(node_id, node_def, prop_strategy=prop_strategy)
        else:
            props = {}
        
        return NodeRef(id=node_id, key=key, node_def=node_def, props=props)
    
    def _create_node_ref_fast(self, node_id: int, node_def: NodeDef) -> NodeRef[Any]:
        """Create a minimal NodeRef without loading key or properties."""
        return NodeRef(id=node_id, key="", node_def=node_def, props={})

    def _has_traverse_step(self) -> bool:
        return any(isinstance(step, TraverseStep) for step in self._steps)

    def _needs_full_execution(self) -> bool:
        return (
            self._edge_filter is not None
            or self._node_filter is not None
            or self._prop_strategy.needs_any_props()
            or self._limit is not None
            or self._has_traverse_step()
        )

    def _needs_full_execution_for_scalar(self) -> bool:
        return (
            self._edge_filter is not None
            or self._node_filter is not None
            or self._limit is not None
            or self._has_traverse_step()
        )

    def _execute_single_hop(
        self,
        node: NodeRef[Any],
        step: Union[OutStep, InStep, BothStep],
    ) -> Generator[Tuple[NodeRef[Any], EdgeResult], None, None]:
        """Execute a single-hop step and yield (node, edge) pairs."""
        directions: List[str]
        if step.type == "both":
            directions = ["out", "in"]
        else:
            directions = [step.type]

        etype_id: Optional[int] = None
        if step.edge_def is not None:
            etype_id = self._resolve_etype_id(step.edge_def)

        for direction in directions:
            if direction == "out":
                edges = self._db.get_out_edges(node.id)
                for edge in edges:
                    if etype_id is not None and edge.etype != etype_id:
                        continue
                    neighbor_id = edge.node_id
                    neighbor_ref = self._create_node_ref(neighbor_id, load_props=self._prop_strategy.needs_any_props())
                    if neighbor_ref is None:
                        continue
                    edge_result = self._build_edge_result(step.edge_def, node.id, edge.etype, neighbor_id)
                    yield neighbor_ref, edge_result
            else:
                edges = self._db.get_in_edges(node.id)
                for edge in edges:
                    if etype_id is not None and edge.etype != etype_id:
                        continue
                    neighbor_id = edge.node_id
                    neighbor_ref = self._create_node_ref(neighbor_id, load_props=self._prop_strategy.needs_any_props())
                    if neighbor_ref is None:
                        continue
                    edge_result = self._build_edge_result(step.edge_def, neighbor_id, edge.etype, node.id)
                    yield neighbor_ref, edge_result

    def _iter_traverse_edges(
        self,
        node_id: int,
        direction: Literal["out", "in", "both"],
        edge_def: Optional[EdgeDef],
        etype_id: Optional[int],
    ) -> Generator[Tuple[int, EdgeResult], None, None]:
        """Yield (neighbor_id, edge_result) for a traversal step."""
        directions: List[str]
        if direction == "both":
            directions = ["out", "in"]
        else:
            directions = [direction]

        for dir_ in directions:
            if dir_ == "out":
                edges = self._db.get_out_edges(node_id)
                for edge in edges:
                    if etype_id is not None and edge.etype != etype_id:
                        continue
                    neighbor_id = edge.node_id
                    edge_result = self._build_edge_result(edge_def, node_id, edge.etype, neighbor_id)
                    yield neighbor_id, edge_result
            else:
                edges = self._db.get_in_edges(node_id)
                for edge in edges:
                    if etype_id is not None and edge.etype != etype_id:
                        continue
                    neighbor_id = edge.node_id
                    edge_result = self._build_edge_result(edge_def, neighbor_id, edge.etype, node_id)
                    yield neighbor_id, edge_result

    def _execute_traverse_filtered(
        self,
        node: NodeRef[Any],
        step: TraverseStep,
        etype_id: Optional[int],
    ) -> Generator[Tuple[NodeRef[Any], EdgeResult], None, None]:
        """Execute variable-depth traversal with filters applied during traversal."""
        from collections import deque

        options = step.options
        node_filter = options.where_node
        edge_filter = options.where_edge

        prop_strategy = self._prop_strategy
        if node_filter is not None:
            prop_strategy = PropLoadStrategy.all()

        load_props = prop_strategy.needs_any_props()

        visited: Set[int] = set()
        if options.unique:
            visited.add(node.id)

        queue = deque([(node, 0)])

        while queue:
            current, depth = queue.popleft()
            if depth >= options.max_depth:
                continue

            for neighbor_id, edge_result in self._iter_traverse_edges(
                current.id,
                options.direction,
                step.edge_def,
                etype_id,
            ):
                if options.unique and neighbor_id in visited:
                    continue

                neighbor_ref = self._create_node_ref(
                    neighbor_id,
                    load_props=load_props,
                    prop_strategy=prop_strategy,
                )
                if neighbor_ref is None:
                    continue

                if edge_filter is not None and not edge_filter(edge_result):
                    continue
                if node_filter is not None and not node_filter(neighbor_ref):
                    continue

                if options.unique:
                    visited.add(neighbor_id)

                next_depth = depth + 1
                if next_depth >= options.min_depth:
                    yield neighbor_ref, edge_result

                if next_depth < options.max_depth:
                    queue.append((neighbor_ref, next_depth))

    def _execute_traverse(
        self,
        node: NodeRef[Any],
        step: TraverseStep,
    ) -> Generator[Tuple[NodeRef[Any], EdgeResult], None, None]:
        """Execute variable-depth traversal from a node."""
        options = step.options
        etype_id: Optional[int] = None
        if step.edge_def is not None:
            etype_id = self._resolve_etype_id(step.edge_def)

        if options.where_node is not None or options.where_edge is not None:
            yield from self._execute_traverse_filtered(node, step, etype_id)
            return

        results = self._db.traverse(
            node.id,
            options.max_depth,
            etype_id,
            options.min_depth,
            options.direction,
            options.unique,
        )

        for result in results:
            node_ref = self._create_node_ref(
                result.node_id,
                load_props=self._prop_strategy.needs_any_props(),
            )
            if node_ref is None:
                continue

            if result.edge_src is None or result.edge_dst is None or result.edge_type is None:
                continue

            edge_result = self._build_edge_result(
                step.edge_def,
                int(result.edge_src),
                int(result.edge_type),
                int(result.edge_dst),
            )
            yield node_ref, edge_result

    @staticmethod
    def _step_dedupes(step: TraversalStep) -> bool:
        """Whether a step yields each node at most once, like the native fast paths."""
        return not isinstance(step, TraverseStep) or step.options.unique

    @staticmethod
    def _unique_by_node(
        results: List[Tuple[NodeRef[Any], Optional[EdgeResult]]],
    ) -> List[Tuple[NodeRef[Any], Optional[EdgeResult]]]:
        seen: Set[int] = set()
        unique: List[Tuple[NodeRef[Any], Optional[EdgeResult]]] = []
        for node, edge in results:
            if node.id not in seen:
                seen.add(node.id)
                unique.append((node, edge))
        return unique

    def _iter_results(
        self, unique_nodes: bool = True
    ) -> Generator[Tuple[NodeRef[Any], Optional[EdgeResult]], None, None]:
        """Full execution path that yields (node, edge) results.

        Every intermediate frontier is deduplicated by node id. With
        `unique_nodes`, the final results are too (after filtering, so a node
        reachable through any edge that passes `where_edge` is kept); the edge
        view passes False to keep every distinct edge into the last frontier.
        """
        current_results: List[Tuple[NodeRef[Any], Optional[EdgeResult]]] = [
            (node, None) for node in self._start_nodes
        ]

        for index, step in enumerate(self._steps):
            next_results: List[Tuple[NodeRef[Any], Optional[EdgeResult]]] = []
            for node, _ in current_results:
                if isinstance(step, TraverseStep):
                    next_results.extend(self._execute_traverse(node, step))
                else:
                    next_results.extend(self._execute_single_hop(node, step))
            is_last = index == len(self._steps) - 1
            if not is_last and self._step_dedupes(step):
                next_results = self._unique_by_node(next_results)
            current_results = next_results

        dedupe_final = unique_nodes and bool(self._steps) and self._step_dedupes(self._steps[-1])
        emitted: Set[int] = set()
        count = 0
        for node, edge in current_results:
            if edge is not None and self._edge_filter is not None:
                if not self._edge_filter(edge):
                    continue
            if self._node_filter is not None and not self._node_filter(node):
                continue
            if dedupe_final:
                if node.id in emitted:
                    continue
                emitted.add(node.id)
            if self._limit is not None and count >= self._limit:
                break
            yield node, edge
            count += 1

    def _execute_edges(self) -> Generator[EdgeResult, None, None]:
        """Execute traversal and yield edge results."""
        for _, edge in self._iter_results(unique_nodes=False):
            if edge is not None:
                yield edge
    
    def _build_steps_for_rust(self) -> List[Tuple[str, Optional[int]]]:
        """Build step tuples for Rust traverse_multi call."""
        return [(step.type, self._step_etype_id(step)) for step in self._steps]
    
    def _execute_fast(self) -> Generator[int, None, None]:
        """Execute traversal and yield only node IDs (fastest path)."""
        if self._has_traverse_step():
            for node, _ in self._iter_results():
                yield node.id
            return
        if not self._steps:
            for node in self._start_nodes:
                yield node.id
            return
        
        # For single step, use direct call (lower overhead)
        # For multi-step, use Rust batch traversal
        if len(self._steps) == 1:
            step = self._steps[0]
            etype_id = self._step_etype_id(step)
            
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
        if self._has_traverse_step():
            for node, _ in self._iter_results():
                yield (node.id, node.key)
            return
        # No steps - just yield start nodes
        if not self._steps:
            for node in self._start_nodes:
                yield (node.id, node.key)
            return
        
        # For single step, use direct batch call (lower overhead)
        if len(self._steps) == 1:
            step = self._steps[0]
            etype_id = self._step_etype_id(step)
            
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
        if self._has_traverse_step():
            return sum(1 for _ in self._iter_results())
        if not self._steps:
            return len(self._start_nodes)
        
        # For single step, count directly
        if len(self._steps) == 1:
            step = self._steps[0]
            etype_id = self._step_etype_id(step)
            
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
        if not self._needs_full_execution():
            # Ultra-fast path: use batch operation to get IDs + keys in one call
            for node_id, key in self._execute_fast_with_keys():
                node_def = self._get_node_def(node_id)
                if node_def is not None:
                    yield NodeRef(id=node_id, key=key, node_def=node_def, props={})
            return

        for node_ref, _ in self._iter_results():
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
        if not self._needs_full_execution():
            results: List[NodeRef[N]] = []
            for node_id, key in self._execute_fast_with_keys():
                node_def = self._get_node_def(node_id)
                if node_def is not None:
                    results.append(NodeRef(id=node_id, key=key, node_def=node_def, props={}))  # type: ignore
            return results

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
        if not self._needs_full_execution_for_scalar() and self._node_filter is None:
            return self._execute_fast_count()
        return sum(1 for _ in self._execute())
    
    def ids(self) -> List[int]:
        """
        Get just the node IDs (fastest possible).
        
        Returns:
            List of node IDs
        """
        if not self._needs_full_execution_for_scalar():
            return list(self._execute_fast())
        return [node.id for node, _ in self._iter_results()]
    
    def keys(self) -> List[str]:
        """
        Get just the node keys.
        
        Returns:
            List of node keys
        """
        if not self._needs_full_execution_for_scalar():
            result: List[str] = []
            for node_id in self._execute_fast():
                key = self._db.get_node_key(node_id)
                if key:
                    result.append(key)
            return result

        return [node.key for node, _ in self._iter_results() if node.key]


class EdgeTraversalResult:
    """Traversal result that yields edges."""

    def __init__(self, traversal: TraversalResult[Any]):
        self._traversal = traversal

    def __iter__(self) -> Iterator[EdgeResult]:
        return iter(self._traversal._execute_edges())

    def to_list(self) -> List[EdgeResult]:
        return list(self._traversal._execute_edges())

    def first(self) -> Optional[EdgeResult]:
        for edge in self._traversal._execute_edges():
            return edge
        return None

    def count(self) -> int:
        return sum(1 for _ in self._traversal._execute_edges())


# ============================================================================
# Traversal Builder
# ============================================================================

class TraversalBuilder(Generic[N]):
    """
    Builder for graph traversals.
    
    By default, traversals load all properties. Use `.select([...])` or
    `.load_props(...)` to load a subset.

    Example:
        >>> # Default traversal
        >>> friend_refs = db.from_(alice).out(knows).to_list()
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
        self._edge_filter: Optional[Callable[[EdgeResult], bool]] = None
        self._limit: Optional[int] = None
        self._prop_strategy: PropLoadStrategy = PropLoadStrategy.all()

    def _fork(self) -> "TraversalBuilder[N]":
        clone = TraversalBuilder(
            db=self._db,
            start_nodes=list(self._start_nodes),
            resolve_etype_id=self._resolve_etype_id,
            resolve_prop_key_id=self._resolve_prop_key_id,
            get_node_def=self._get_node_def,
        )
        clone._steps = list(self._steps)
        clone._node_filter = self._node_filter
        clone._edge_filter = self._edge_filter
        clone._limit = self._limit
        prop_names = (
            set(self._prop_strategy.prop_names)
            if self._prop_strategy.prop_names is not None
            else None
        )
        clone._prop_strategy = PropLoadStrategy(
            load_all=self._prop_strategy.load_all,
            prop_names=prop_names,
        )
        return clone
    
    def out(self, edge: Optional[EdgeDef] = None) -> TraversalBuilder[N]:
        """
        Traverse outgoing edges.
        
        Args:
            edge: Optional edge definition to filter by type
        
        Returns:
            Self for chaining
        """
        clone = self._fork()
        clone._steps.append(OutStep(edge_def=edge))
        return clone
    
    def in_(self, edge: Optional[EdgeDef] = None) -> TraversalBuilder[N]:
        """
        Traverse incoming edges.
        
        Note: Named `in_` because `in` is a Python reserved word.
        
        Args:
            edge: Optional edge definition to filter by type
        
        Returns:
            Self for chaining
        """
        clone = self._fork()
        clone._steps.append(InStep(edge_def=edge))
        return clone
    
    def both(self, edge: Optional[EdgeDef] = None) -> TraversalBuilder[N]:
        """
        Traverse both incoming and outgoing edges.
        
        Args:
            edge: Optional edge definition to filter by type
        
        Returns:
            Self for chaining
        """
        clone = self._fork()
        clone._steps.append(BothStep(edge_def=edge))
        return clone

    def traverse(self, edge: EdgeDef, options: TraverseOptions) -> TraversalBuilder[N]:
        """
        Variable-depth traversal.
        
        Args:
            edge: Edge definition to traverse
            options: TraverseOptions (max_depth required)
        """
        _check_direction(options.direction)
        clone = self._fork()
        clone._steps.append(TraverseStep(edge_def=edge, options=options))
        return clone
    
    def with_props(self) -> TraversalBuilder[N]:
        """
        Load all properties for traversed nodes.
        
        This is the default behavior; use load_props/select to limit properties.
        
        Returns:
            Self for chaining
        
        Example:
            >>> friends = db.from_(alice).out(knows).with_props().to_list()
            >>> for f in friends:
            ...     print(f.name, f.email)
        """
        clone = self._fork()
        clone._prop_strategy = PropLoadStrategy.all()
        return clone
    
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
        clone = self._fork()
        clone._prop_strategy = PropLoadStrategy.only(*prop_names)
        return clone

    def select(self, props: List[str]) -> TraversalBuilder[N]:
        """
        Select specific properties to load.

        This mirrors the TypeScript `select([...])` behavior.
        """
        clone = self._fork()
        clone._prop_strategy = PropLoadStrategy.only(*props)
        return clone

    def where_edge(self, predicate: Callable[[EdgeResult], bool]) -> TraversalBuilder[N]:
        """
        Filter results by edge properties.

        Args:
            predicate: Function that returns True for edges to include
        """
        clone = self._fork()
        clone._edge_filter = predicate
        return clone

    def take(self, limit: int) -> TraversalBuilder[N]:
        """Limit the number of results."""
        clone = self._fork()
        clone._limit = limit
        return clone
    
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
        clone = self._fork()
        clone._node_filter = predicate
        # Filter needs properties to work, so enable loading all
        clone._prop_strategy = PropLoadStrategy.all()
        return clone
    
    def _build_result(self) -> TraversalResult[N]:
        """Build the traversal result."""
        return TraversalResult(
            db=self._db,
            start_nodes=self._start_nodes,
            steps=self._steps,
            node_filter=self._node_filter,
            edge_filter=self._edge_filter,
            limit=self._limit,
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

    def edges(self) -> "EdgeTraversalResult":
        """Return edge results from the traversal."""
        return EdgeTraversalResult(self._build_result())

    def raw_edges(self) -> Generator[RawEdge, None, None]:
        """Return raw edge data without property loading."""
        if any(isinstance(step, TraverseStep) for step in self._steps):
            raise ValueError("raw_edges() does not support variable-depth traverse()")

        current_ids = [node.id for node in self._start_nodes]

        for step in self._steps:
            if isinstance(step, TraverseStep):
                raise ValueError("raw_edges() does not support variable-depth traverse()")

            directions: List[str]
            if step.type == "both":
                directions = ["out", "in"]
            else:
                directions = [step.type]

            etype_id: Optional[int] = None
            if step.edge_def is not None:
                etype_id = self._resolve_etype_id(step.edge_def)

            next_ids: List[int] = []

            for node_id in current_ids:
                for direction in directions:
                    if direction == "out":
                        edges = self._db.get_out_edges(node_id)
                        for edge in edges:
                            if etype_id is not None and edge.etype != etype_id:
                                continue
                            yield RawEdge(src=node_id, etype=edge.etype, dst=edge.node_id)
                            next_ids.append(edge.node_id)
                    else:
                        edges = self._db.get_in_edges(node_id)
                        for edge in edges:
                            if etype_id is not None and edge.etype != etype_id:
                                continue
                            yield RawEdge(src=edge.node_id, etype=edge.etype, dst=node_id)
                            next_ids.append(edge.node_id)

            current_ids = next_ids
    
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
        edges: List of edges in the path
        found: Whether a path was found
        total_weight: Total path weight (for weighted paths)
    """
    nodes: List[NodeRef[N]]
    found: bool
    total_weight: float = 0.0
    edges: List[EdgeResult] = field(default_factory=list)

    @property
    def path(self) -> List[NodeRef[N]]:
        return self.nodes

    @property
    def totalWeight(self) -> float:
        return self.total_weight
    
    def __bool__(self) -> bool:
        return self.found
    
    def __len__(self) -> int:
        return len(self.nodes)


WeightSpec = Union[str, Callable[[EdgeResult], float]]


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
        self._targets: Optional[List[NodeRef[Any]]] = None
        self._edge_type: Optional[EdgeDef] = None
        self._max_depth: Optional[int] = None
        self._direction: str = "out"
        self._load_props: bool = True
        self._weight_spec: Optional[WeightSpec] = None
    
    def to(self, target: NodeRef[Any]) -> PathFindingBuilder[N]:
        """Set the target node."""
        self._targets = [target]
        return self

    def to_any(self, targets: List[NodeRef[Any]]) -> PathFindingBuilder[N]:
        """Set multiple target nodes (find path to any)."""
        if not targets:
            raise ValueError("to_any requires at least one target")
        self._targets = targets
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
        """Set traversal direction ("out", "in" or "both")."""
        self._direction = _check_direction(dir)
        return self
    
    def with_props(self) -> PathFindingBuilder[N]:
        """Load properties for nodes in the path (default behavior)."""
        self._load_props = True
        return self

    def weight(self, spec: WeightSpec) -> PathFindingBuilder[N]:
        """Set weight specification (property name or function)."""
        self._weight_spec = spec
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

    def _get_targets(self) -> List[NodeRef[Any]]:
        if not self._targets:
            raise ValueError("Target node required. Use .to(target) or .to_any(targets) first.")
        if self._edge_type is None:
            raise ValueError("Must specify at least one edge type with via()")
        return self._targets

    def _max_depth_value(self) -> int:
        return self._max_depth if self._max_depth is not None else 100

    def _build_edge_result(self, edge_def: Optional[EdgeDef], src: int, etype: int, dst: int) -> EdgeResult:
        props: Dict[str, Any] = {}
        if edge_def is not None and edge_def.props:
            for prop_name in edge_def.props.keys():
                try:
                    prop_key_id = self._resolve_prop_key_id(edge_def, prop_name)
                except Exception:
                    continue
                prop_value = self._db.get_edge_prop(src, etype, dst, prop_key_id)
                if prop_value is not None:
                    props[prop_name] = from_prop_value(prop_value)
        return EdgeResult(src=src, etype=etype, dst=dst, props=props)

    def _coerce_weight(self, value: Any) -> float:
        try:
            weight = float(value)
        except Exception:
            return 1.0
        if not weight or weight <= 0:
            return 1.0
        return weight

    def _edge_weight(self, edge: EdgeResult) -> float:
        if self._weight_spec is None:
            return 1.0
        if isinstance(self._weight_spec, str):
            prop_name = self._weight_spec
            if prop_name in edge.props:
                return self._coerce_weight(edge.props[prop_name])
            return 1.0
        return self._coerce_weight(self._weight_spec(edge))

    def _iter_neighbors(self, node_id: int) -> Generator[Tuple[int, EdgeResult], None, None]:
        directions: List[str]
        if self._direction == "both":
            directions = ["out", "in"]
        else:
            directions = [self._direction]

        etype_id: Optional[int] = None
        if self._edge_type is not None:
            etype_id = self._resolve_etype_id(self._edge_type)

        for direction in directions:
            if direction == "out":
                edges = self._db.get_out_edges(node_id)
                for edge in edges:
                    if etype_id is not None and edge.etype != etype_id:
                        continue
                    neighbor_id = edge.node_id
                    edge_result = self._build_edge_result(self._edge_type, node_id, edge.etype, neighbor_id)
                    yield neighbor_id, edge_result
            else:
                edges = self._db.get_in_edges(node_id)
                for edge in edges:
                    if etype_id is not None and edge.etype != etype_id:
                        continue
                    neighbor_id = edge.node_id
                    edge_result = self._build_edge_result(self._edge_type, neighbor_id, edge.etype, node_id)
                    yield neighbor_id, edge_result

    def _reconstruct_path(
        self,
        parents: Dict[int, Tuple[Optional[int], Optional[EdgeResult]]],
        target_id: int,
    ) -> PathResult[N]:
        path_nodes: List[NodeRef[N]] = []
        path_edges: List[EdgeResult] = []

        current: Optional[int] = target_id
        while current is not None:
            parent, edge = parents.get(current, (None, None))
            node_ref = self._create_node_ref(current)
            if node_ref is not None:
                path_nodes.append(node_ref)  # type: ignore
            if edge is not None:
                path_edges.append(edge)
            current = parent

        path_nodes.reverse()
        path_edges.reverse()

        return PathResult(nodes=path_nodes, found=True, total_weight=0.0, edges=path_edges)
    
    def find(self) -> PathResult[N]:
        """
        Find the shortest path using BFS.
        
        Returns:
            PathResult containing the path if found
        """
        return self.bfs()
    
    def find_weighted(self) -> PathResult[N]:
        """
        Find the shortest weighted path using Dijkstra.
        
        Returns:
            PathResult containing the path if found
        """
        return self.dijkstra()
    
    def exists(self) -> bool:
        """
        Check if a path exists between source and target.
        
        Returns:
            True if a path exists
        """
        return self.bfs().found

    def bfs(self) -> PathResult[N]:
        """Execute BFS (unweighted shortest path)."""
        targets = self._get_targets()
        target_ids = {t.id for t in targets}
        max_depth = self._max_depth_value()

        if self._source.id in target_ids:
            node_ref = self._create_node_ref(self._source.id)
            if node_ref is None:
                return PathResult(nodes=[], found=False)
            return PathResult(nodes=[node_ref], found=True, total_weight=0.0)

        from collections import deque

        queue = deque([(self._source.id, 0)])
        visited = {self._source.id}
        parents: Dict[int, Tuple[Optional[int], Optional[EdgeResult]]] = {
            self._source.id: (None, None)
        }

        while queue:
            node_id, depth = queue.popleft()
            if depth >= max_depth:
                continue

            for neighbor_id, edge in self._iter_neighbors(node_id):
                if neighbor_id in visited:
                    continue
                visited.add(neighbor_id)
                parents[neighbor_id] = (node_id, edge)

                if neighbor_id in target_ids:
                    result = self._reconstruct_path(parents, neighbor_id)
                    result.total_weight = float(len(result.edges))
                    return result

                queue.append((neighbor_id, depth + 1))

        return PathResult(nodes=[], found=False)

    def dijkstra(self) -> PathResult[N]:
        """Execute Dijkstra's algorithm."""
        targets = self._get_targets()
        target_ids = {t.id for t in targets}
        max_depth = self._max_depth_value()

        if isinstance(self._weight_spec, str) and self._edge_type is None:
            raise ValueError("weight by property requires via(edge)")

        if self._source.id in target_ids:
            node_ref = self._create_node_ref(self._source.id)
            if node_ref is None:
                return PathResult(nodes=[], found=False)
            return PathResult(nodes=[node_ref], found=True, total_weight=0.0)

        import heapq

        dist: Dict[int, float] = {self._source.id: 0.0}
        depth_map: Dict[int, int] = {self._source.id: 0}
        parents: Dict[int, Tuple[Optional[int], Optional[EdgeResult]]] = {
            self._source.id: (None, None)
        }
        heap: List[Tuple[float, int, int]] = [(0.0, 0, self._source.id)]
        visited: Set[int] = set()

        while heap:
            cost, depth, node_id = heapq.heappop(heap)
            if node_id in visited:
                continue
            visited.add(node_id)

            if node_id in target_ids:
                result = self._reconstruct_path(parents, node_id)
                result.total_weight = cost
                return result

            if depth >= max_depth:
                continue

            for neighbor_id, edge in self._iter_neighbors(node_id):
                next_depth = depth + 1
                if next_depth > max_depth:
                    continue

                new_cost = cost + self._edge_weight(edge)
                if new_cost < dist.get(neighbor_id, float("inf")):
                    dist[neighbor_id] = new_cost
                    depth_map[neighbor_id] = next_depth
                    parents[neighbor_id] = (node_id, edge)
                    heapq.heappush(heap, (new_cost, next_depth, neighbor_id))

        return PathResult(nodes=[], found=False)

    def a_star(self, heuristic: Callable[[NodeRef[N], NodeRef[N]], float]) -> PathResult[N]:
        """Execute A* algorithm with a heuristic."""
        targets = self._get_targets()
        target_ids = {t.id for t in targets}
        target_ref = targets[0]
        max_depth = self._max_depth_value()

        if isinstance(self._weight_spec, str) and self._edge_type is None:
            raise ValueError("weight by property requires via(edge)")

        if self._source.id in target_ids:
            node_ref = self._create_node_ref(self._source.id)
            if node_ref is None:
                return PathResult(nodes=[], found=False)
            return PathResult(nodes=[node_ref], found=True, total_weight=0.0)

        import heapq

        def safe_heuristic(current: NodeRef[N]) -> float:
            try:
                return float(heuristic(current, target_ref))
            except Exception:
                return 0.0

        g_score: Dict[int, float] = {self._source.id: 0.0}
        parents: Dict[int, Tuple[Optional[int], Optional[EdgeResult]]] = {
            self._source.id: (None, None)
        }
        heap: List[Tuple[float, float, int, int]] = []

        source_ref = self._create_node_ref(self._source.id)
        if source_ref is None:
            return PathResult(nodes=[], found=False)
        heapq.heappush(heap, (safe_heuristic(source_ref), 0.0, self._source.id, 0))

        visited: Set[int] = set()

        while heap:
            f_score, g_score_val, node_id, depth = heapq.heappop(heap)
            if node_id in visited:
                continue
            visited.add(node_id)

            if node_id in target_ids:
                result = self._reconstruct_path(parents, node_id)
                result.total_weight = g_score_val
                return result

            if depth >= max_depth:
                continue

            for neighbor_id, edge in self._iter_neighbors(node_id):
                next_depth = depth + 1
                if next_depth > max_depth:
                    continue

                tentative_g = g_score_val + self._edge_weight(edge)
                if tentative_g < g_score.get(neighbor_id, float("inf")):
                    neighbor_ref = self._create_node_ref(neighbor_id)
                    if neighbor_ref is None:
                        continue
                    g_score[neighbor_id] = tentative_g
                    parents[neighbor_id] = (node_id, edge)
                    h = safe_heuristic(neighbor_ref)
                    heapq.heappush(heap, (tentative_g + h, tentative_g, neighbor_id, next_depth))

        return PathResult(nodes=[], found=False)

    def all_paths(self, max_paths: Optional[int] = None) -> Iterator[PathResult[N]]:
        """Yield shortest paths (currently returns at most one)."""
        result = self.dijkstra() if self._weight_spec is not None else self.bfs()
        if result.found:
            yield result


__all__ = [
    "TraversalBuilder",
    "TraversalResult",
    "EdgeTraversalResult",
    "PathFindingBuilder",
    "PathResult",
    "EdgeResult",
    "RawEdge",
    "TraverseOptions",
    "PropLoadStrategy",
    "OutStep",
    "InStep",
    "BothStep",
    "TraverseStep",
    "TraversalStep",
]
