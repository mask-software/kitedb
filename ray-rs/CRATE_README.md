# KiteDB

KiteDB is a high-performance embedded graph database with built-in vector search.
This crate provides the Rust core and the high-level Kite API.

## Features

- ACID transactions with WAL-based durability
- Node and edge CRUD with properties
- Labels, edge types, and schema helpers
- Fluent traversal and pathfinding (BFS, Dijkstra, Yen)
- Vector embeddings with IVF and IVF-PQ indexes
- Single-file storage format

## Install

```toml
[dependencies]
kitedb = "0.1"
```

## Quick start (Kite API)

```rust
use kitedb::api::kite::{EdgeDef, NodeDef, PropDef, Kite, KiteOptions};
use kitedb::types::PropValue;
use std::collections::HashMap;

fn main() -> kitedb::error::Result<()> {
  let user = NodeDef::new("User", "user:")
    .prop(PropDef::string("name").required());
  let knows = EdgeDef::new("KNOWS");

  let mut kite = Kite::open("my_graph.kitedb", KiteOptions::new().node(user).edge(knows))?;

  let mut alice_props = HashMap::new();
  alice_props.insert("name".to_string(), PropValue::String("Alice".into()));
  let alice = kite.create_node("User", "alice", alice_props)?;

  let bob = kite.create_node("User", "bob", HashMap::new())?;
  kite.link(alice.id, "KNOWS", bob.id)?;

  let friends = kite.neighbors_out(alice.id, Some("KNOWS"))?;
  println!("friends: {friends:?}");

  Ok(())
}
```

## Lower-level API

For direct access to storage primitives, use `kitedb::core::single_file` and the
modules under `kitedb::vector` and `kitedb::core`.

## Concurrent Access

KiteDB supports concurrent reads when wrapped in a `RwLock`. Multiple threads can read simultaneously:

```rust
use std::sync::Arc;
use parking_lot::RwLock;
use kitedb::api::kite::{Kite, KiteOptions, NodeDef};

// Wrap Kite in RwLock for concurrent access
let kite = Kite::open("graph.kitedb", KiteOptions::new().node(NodeDef::new("User", "user:")))?;
let db = Arc::new(RwLock::new(kite));

// Multiple threads can read concurrently
let db_clone = Arc::clone(&db);
std::thread::spawn(move || {
    let guard = db_clone.read();  // Shared read lock
    let user = guard.get("User", "alice");
});

// Writes require exclusive access
{
    let mut guard = db.write();  // Exclusive write lock
    guard.create_node("User", "bob", HashMap::new())?;
}
```

**Concurrency model:**

- **Reads (`&self`)**: `get()`, `exists()`, `neighbors_out()`, `from()`, traversals - concurrent via `RwLock::read()`
- **Writes (`&mut self`)**: `create_node()`, `link()`, `set_prop()` - exclusive via `RwLock::write()`
- The internal data structures use `RwLock` for thread-safe access to delta state and schema mappings

## Documentation

```text
https://kitedb.vercel.com/docs
```

## License

MIT License - see the main project LICENSE file for details.
