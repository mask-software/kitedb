# RayDB Python Bindings

High-performance embedded graph database with vector search support.

## Installation

### From Source (Development)

```bash
# Install maturin (Python build tool for Rust extensions)
pip install maturin

# Build and install in development mode
cd ray-rs
maturin develop --features python

# Or build a wheel
maturin build --features python --release
pip install target/wheels/raydb-*.whl
```

### From PyPI (Coming Soon)

```bash
pip install raydb
```

## Quick Start

```python
from raydb import Database, PropValue

# Open or create a database
with Database("my_graph.raydb") as db:
    # Start a transaction
    db.begin()
    
    # Create nodes
    alice = db.create_node("user:alice")
    bob = db.create_node("user:bob")
    
    # Set properties
    name_key = db.get_or_create_propkey("name")
    db.set_node_prop(alice, name_key, PropValue.string("Alice"))
    db.set_node_prop(bob, name_key, PropValue.string("Bob"))
    
    # Create an edge
    knows = db.get_or_create_etype("knows")
    db.add_edge(alice, knows, bob)
    
    # Commit changes
    db.commit()
    
    # Query the graph
    print(f"Total nodes: {db.count_nodes()}")
    print(f"Total edges: {db.count_edges()}")
```

## Features

### Graph Database
- ACID transactions with commit/rollback
- Node and edge CRUD operations
- Property storage (strings, integers, floats, booleans)
- Node labels
- Single-file storage format

### Graph Traversal
- Multi-hop traversal
- Variable-depth traversal
- Direction control (out, in, both)
- Edge type filtering

### Pathfinding
- BFS (unweighted shortest path)
- Dijkstra's algorithm (weighted shortest path)
- Yen's k-shortest paths algorithm
- Reachability queries

### Vector Search
- IVF (Inverted File) index for approximate nearest neighbor search
- IVF-PQ (Product Quantization) for memory-efficient search
- Multiple distance metrics (cosine, euclidean, dot product)
- Multi-query search with aggregation

## API Reference

### Database Operations

```python
# Open database
db = Database("path/to/db.raydb")
db = Database("path/to/db.raydb", OpenOptions(read_only=True))

# Transaction management
db.begin()           # Start transaction
db.begin(read_only=True)  # Read-only transaction
db.commit()          # Commit changes
db.rollback()        # Discard changes

# Node operations
node_id = db.create_node()           # Create anonymous node
node_id = db.create_node("key:123")  # Create node with key
db.delete_node(node_id)
exists = db.node_exists(node_id)
node_id = db.get_node_by_key("key:123")
key = db.get_node_key(node_id)
nodes = db.list_nodes()
count = db.count_nodes()

# Edge operations
db.add_edge(src, etype, dst)
db.delete_edge(src, etype, dst)
exists = db.edge_exists(src, etype, dst)
out_edges = db.get_out_edges(node_id)
in_edges = db.get_in_edges(node_id)
edges = db.list_edges()
count = db.count_edges()

# Property operations
db.set_node_prop(node_id, key_id, PropValue.string("value"))
prop = db.get_node_prop(node_id, key_id)
db.delete_node_prop(node_id, key_id)
props = db.get_node_props(node_id)

# Schema operations
label_id = db.get_or_create_label("Person")
etype_id = db.get_or_create_etype("knows")
key_id = db.get_or_create_propkey("name")
```

### Graph Traversal

```python
from raydb import GraphAccessor, TraversalStep, TraverseOptions

# Create accessor and add edges
graph = GraphAccessor()
graph.add_edge(1, 1, 2, 1.0)  # src, etype, dst, weight
graph.add_edges([(1, 1, 2, 1.0), (2, 1, 3, 1.0)])

# Multi-hop traversal
results = graph.traverse(
    [1],  # Start nodes
    [TraversalStep("out", 1), TraversalStep("out")],
    limit=100
)

# Variable-depth traversal
results = graph.traverse_depth(
    [1],
    edge_type=1,
    options=TraverseOptions(max_depth=3, direction="out")
)
```

### Pathfinding

```python
from raydb import PathConfig

# Shortest path
path = graph.shortest_path(source=1, target=5)
if path.found:
    print(f"Path: {path.path}")
    print(f"Weight: {path.total_weight}")

# BFS (unweighted)
path = graph.bfs(PathConfig(source=1, target=5))

# Dijkstra (weighted)
path = graph.dijkstra(PathConfig(source=1, target=5))

# K-shortest paths
paths = graph.k_shortest(PathConfig(source=1, target=5), k=3)

# Reachability
nodes = graph.reachable_nodes(source=1, max_depth=3)
can_reach = graph.has_path(source=1, target=5)
```

### Vector Search

```python
from raydb import IvfIndex, IvfConfig, SearchOptions

# Create index
index = IvfIndex(dimensions=128, config=IvfConfig(n_clusters=100))

# Train on sample data
training_data = [...]  # Flat list of floats
index.add_training_vectors(training_data, num_vectors=1000)
index.train()

# Insert vectors
index.insert(vector_id=1, vector=[0.1, 0.2, ...])

# Search
results = index.search(
    manifest_json='{"vectors": {...}}',
    query=[0.1, 0.2, ...],
    k=10,
    options=SearchOptions(n_probe=20)
)

for result in results:
    print(f"Node {result.node_id}: distance={result.distance:.4f}")
```

## License

MIT License - see the main project LICENSE file for details.
