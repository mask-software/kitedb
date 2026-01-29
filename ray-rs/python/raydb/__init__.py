"""
RayDB - High-performance embedded graph database with vector search

A Python interface to the RayDB graph database, providing:
- ACID transactions
- Node and edge CRUD operations
- Property storage
- Vector embeddings with IVF/IVF-PQ indexes
- Graph traversal and pathfinding (BFS, Dijkstra, A*)

Fluent API (Recommended):
    >>> from raydb import ray, define_node, define_edge, prop, optional
    >>> 
    >>> # Define schema
    >>> user = define_node("user",
    ...     key=lambda id: f"user:{id}",
    ...     props={
    ...         "name": prop.string("name"),
    ...         "email": prop.string("email"),
    ...         "age": optional(prop.int("age")),
    ...     }
    ... )
    >>> 
    >>> knows = define_edge("knows", {"since": prop.int("since")})
    >>> 
    >>> # Open database and use fluent API
    >>> with ray("./my-graph", nodes=[user], edges=[knows]) as db:
    ...     alice = db.insert(user).values(
    ...         key="alice", name="Alice", email="alice@example.com", age=30
    ...     ).returning()
    ...     
    ...     bob = db.insert(user).values(
    ...         key="bob", name="Bob", email="bob@example.com", age=25
    ...     ).returning()
    ...     
    ...     db.link(alice, knows, bob, since=2020)
    ...     
    ...     friends = db.from_(alice).out(knows).nodes().to_list()
    ...     print([f.key for f in friends])  # ['user:bob']

Low-level API (for advanced use):
    >>> from raydb import Database, PropValue
    >>> 
    >>> with Database("my_graph.raydb") as db:
    ...     db.begin()
    ...     alice = db.create_node("user:alice")
    ...     name_key = db.get_or_create_propkey("name")
    ...     db.set_node_prop(alice, name_key, PropValue.string("Alice"))
    ...     db.commit()
"""

from raydb._raydb import (
    # Core classes
    Database,
    OpenOptions,
    DbStats,
    CheckResult,
    CacheStats,
    PropValue,
    Edge,
    FullEdge,
    NodeProp,
    
    # Traversal result classes
    TraversalResult as LowLevelTraversalResult,
    PathResult as LowLevelPathResult,
    PathEdge,
    
    # Vector search classes
    IvfIndex,
    IvfPqIndex,
    IvfConfig,
    PqConfig,
    SearchOptions,
    SearchResult,
    IvfStats,
    
    # Functions
    open_database,
    version,
    brute_force_search,
)

# Fluent API imports
from raydb.schema import (
    prop,
    PropDef,
    PropBuilder,
    optional,
    NodeDef,
    define_node,
    EdgeDef,
    define_edge,
    PropsSchema,
)

from raydb.builders import (
    NodeRef,
    InsertBuilder,
    UpdateBuilder,
    DeleteBuilder,
)

from raydb.traversal import (
    EdgeResult,
    EdgeTraversalResult,
    RawEdge,
    TraverseOptions,
    TraversalBuilder,
    TraversalResult,
    PathFindingBuilder,
    PathResult,
)

from raydb.fluent import (
    EdgeData,
    Ray,
    ray,
)

from raydb.vector_index import (
    VectorIndex,
    VectorIndexOptions,
    SimilarOptions,
    VectorSearchHit,
    create_vector_index,
)

__version__ = version()

__all__ = [
    # ==========================================================================
    # Fluent API (Recommended)
    # ==========================================================================
    
    # Entry point
    "ray",
    "Ray",
    "EdgeData",
    "VectorIndex",
    "VectorIndexOptions",
    "SimilarOptions",
    "VectorSearchHit",
    "create_vector_index",
    
    # Schema builders
    "define_node",
    "define_edge",
    "prop",
    "optional",
    "PropDef",
    "PropBuilder",
    "NodeDef",
    "EdgeDef",
    "PropsSchema",
    
    # Node and edge references
    "NodeRef",
    
    # Builders
    "InsertBuilder",
    "UpdateBuilder",
    "DeleteBuilder",
    
    # Traversal
    "TraversalBuilder",
    "TraversalResult",
    "EdgeTraversalResult",
    "EdgeResult",
    "RawEdge",
    "TraverseOptions",
    "PathFindingBuilder",
    "PathResult",
    
    # ==========================================================================
    # Low-level API
    # ==========================================================================
    
    # Core
    "Database",
    "OpenOptions",
    "DbStats",
    "CheckResult",
    "CacheStats",
    "PropValue",
    "Edge",
    "FullEdge",
    "NodeProp",
    
    # Traversal (low-level)
    "LowLevelTraversalResult",
    "LowLevelPathResult",
    "PathEdge",
    
    # Vector
    "IvfIndex",
    "IvfPqIndex",
    "IvfConfig",
    "PqConfig",
    "SearchOptions",
    "SearchResult",
    "IvfStats",
    
    # Functions
    "open_database",
    "version",
    "brute_force_search",
    
    # Version
    "__version__",
]
