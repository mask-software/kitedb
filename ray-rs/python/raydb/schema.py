"""
Schema Definition API for RayDB

Provides type-safe schema builders for defining graph nodes and edges.

Example:
    >>> from raydb import node, edge, prop, optional
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
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import (
    Any,
    Callable,
    Dict,
    Generic,
    Literal,
    Optional,
    TypeVar,
    Union,
    overload,
)

# ============================================================================
# Property Type System
# ============================================================================

PropType = Literal["string", "int", "float", "bool", "vector"]

# Type variable for property values
T = TypeVar("T")
KeyArg = TypeVar("KeyArg", str, int)


@dataclass(frozen=True)
class PropDef(Generic[T]):
    """
    A property definition with type information.
    
    This holds metadata about a property (name, type, whether it's optional).
    The generic parameter T represents the Python type this property maps to:
    - "string" -> str
    - "int" -> int  
    - "float" -> float
    - "bool" -> bool
    - "vector" -> list[float]
    """
    name: str
    type: PropType
    optional: bool = False
    
    def make_optional(self) -> PropDef[T]:
        """Convert this property to an optional property."""
        return PropDef(name=self.name, type=self.type, optional=True)


class PropBuilder:
    """
    Property type builders.
    
    Use these to define typed properties on nodes and edges.
    
    Example:
        >>> name = prop.string("name")
        >>> age = prop.int("age")
        >>> score = optional(prop.float("score"))
        >>> active = prop.bool("active")
    """
    
    @staticmethod
    def string(name: str) -> PropDef[str]:
        """
        String property.
        Stored as UTF-8 strings.
        """
        return PropDef(name=name, type="string")
    
    @staticmethod
    def int(name: str) -> PropDef[int]:
        """
        Integer property.
        Stored as 64-bit signed integers.
        """
        return PropDef(name=name, type="int")
    
    @staticmethod
    def float(name: str) -> PropDef[float]:
        """
        Float property.
        Stored as 64-bit IEEE 754 floats.
        """
        return PropDef(name=name, type="float")
    
    @staticmethod
    def bool(name: str) -> PropDef[bool]:
        """
        Boolean property.
        """
        return PropDef(name=name, type="bool")
    
    @staticmethod
    def vector(name: str) -> PropDef[list[float]]:
        """
        Vector property for embeddings.
        Stored as float32 arrays.
        """
        return PropDef(name=name, type="vector")


# Global prop builder instance
prop = PropBuilder()


def optional(p: PropDef[T]) -> PropDef[T]:
    """
    Helper to make a property optional.
    
    Example:
        >>> age = optional(prop.int("age"))
    """
    return p.make_optional()


# ============================================================================
# Node Definition
# ============================================================================

# Type for property schemas
PropsSchema = Dict[str, PropDef[Any]]


@dataclass
class NodeDef(Generic[KeyArg]):
    """
    A defined node type with metadata.
    
    Created by `node()` and used throughout the API.
    
    Attributes:
        name: The node type name (must be unique per schema)
        key_fn: Function to transform application IDs to full node keys
        props: Property definitions for this node type
    """
    name: str
    key_fn: Callable[[KeyArg], str]
    props: PropsSchema
    # Internal: resolved prop key IDs (set during db initialization)
    _prop_key_ids: Dict[str, int] = field(default_factory=dict, repr=False)
    
    def __hash__(self) -> int:
        return hash(self.name)
    
    def __eq__(self, other: object) -> bool:
        if isinstance(other, NodeDef):
            return self.name == other.name
        return False


def node(
    name: str,
    *,
    key: Callable[[KeyArg], str],
    props: PropsSchema,
) -> NodeDef[KeyArg]:
    """
    Define a node type with properties.
    
    Creates a node definition that can be used for all node operations
    (insert, update, delete, query). Provides full type inference for
    insert values and return types.
    
    Args:
        name: The node type name (must be unique)
        key: Function to generate full keys from application IDs
        props: Property definitions using `prop.*` builders
    
    Returns:
        A NodeDef that can be used with the database API
    
    Example:
        >>> user = node("user",
        ...     key=lambda id: f"user:{id}",
        ...     props={
        ...         "name": prop.string("name"),
        ...         "email": prop.string("email"),
        ...         "age": optional(prop.int("age")),
        ...     }
        ... )
    """
    return NodeDef(name=name, key_fn=key, props=props)


# Backwards compatibility alias
define_node = node


# ============================================================================
# Edge Definition
# ============================================================================

@dataclass
class EdgeDef:
    """
    A defined edge type with metadata.
    
    Created by `edge()` and used throughout the API.
    
    Attributes:
        name: The edge type name (must be unique per schema)
        props: Property definitions for this edge type
    """
    name: str
    props: PropsSchema = field(default_factory=dict)
    # Internal: resolved edge type ID (set during db initialization)
    _etype_id: Optional[int] = field(default=None, repr=False)
    # Internal: resolved prop key IDs (set during db initialization)
    _prop_key_ids: Dict[str, int] = field(default_factory=dict, repr=False)
    
    def __hash__(self) -> int:
        return hash(self.name)
    
    def __eq__(self, other: object) -> bool:
        if isinstance(other, EdgeDef):
            return self.name == other.name
        return False


@overload
def edge(name: str) -> EdgeDef: ...

@overload
def edge(name: str, props: PropsSchema) -> EdgeDef: ...

def edge(name: str, props: Optional[PropsSchema] = None) -> EdgeDef:
    """
    Define an edge type with optional properties.
    
    Creates an edge definition that can be used for all edge operations
    (link, unlink, query). Edges are directional and can have properties.
    
    Args:
        name: The edge type name (must be unique)
        props: Optional property definitions using `prop.*` builders
    
    Returns:
        An EdgeDef that can be used with the database API
    
    Example:
        >>> # Edge with properties
        >>> knows = edge("knows", {
        ...     "since": prop.int("since"),
        ...     "weight": optional(prop.float("weight")),
        ... })
        >>> 
        >>> # Edge without properties
        >>> follows = edge("follows")
    """
    return EdgeDef(name=name, props=props or {})


# Backwards compatibility alias
define_edge = edge


# ============================================================================
# Type Inference Helpers (for documentation and type checkers)
# ============================================================================

# These TypedDict-style types help type checkers understand the shape
# of data passed to/from the API. In practice, we use runtime dicts
# but these provide IDE completion and type checking.

from typing import TypedDict


class NodeRefBase(TypedDict, total=False):
    """Base type for node references."""
    pass


class NodeRef(TypedDict):
    """
    A node reference with its ID and key.
    
    All returned nodes have these system fields plus their property fields.
    """
    id: int  # Internal node ID
    key: str  # Full node key (e.g., "user:alice")


# For dynamic property types, we can't use TypedDict directly
# but we provide runtime type checking and IDE support through
# the builder pattern.


__all__ = [
    # Property builders
    "prop",
    "PropDef",
    "PropBuilder",
    "optional",
    # Node definitions  
    "NodeDef",
    "node",
    "define_node",  # backwards compat
    # Edge definitions
    "EdgeDef", 
    "edge",
    "define_edge",  # backwards compat
    # Type helpers
    "PropsSchema",
    "NodeRef",
]
