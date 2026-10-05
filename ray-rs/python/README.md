# KiteDB for Python

KiteDB is a high-performance embedded graph database with built-in vector search.
This package provides the Python bindings to the Rust core.

## Features

- ACID transactions with commit/rollback
- MVCC (on by default): snapshot-isolated transactions and concurrent writers
- Node and edge CRUD operations with properties
- Labels, edge types, and property keys
- Fluent traversal and pathfinding (BFS, Dijkstra, A\*)
- Vector embeddings with IVF and IVF-PQ indexes
- Single-file storage format

## Install

### From PyPI

```bash
pip install kitedb
```

### From source

```bash
# Install maturin (Rust extension build tool)
python -m pip install -U maturin

# Build and install in development mode
maturin develop --features python

# Or build a wheel
maturin build --features python --release
pip install target/wheels/kitedb-*.whl
```

## Quick start (fluent API)

The fluent API provides a high-level, type-safe interface:

```python
from kitedb import kite, node, edge, prop, optional

# Define your schema
User = node("user",
    key=lambda id: f"user:{id}",
    props={
        "name": prop.string("name"),
        "email": prop.string("email"),
        "age": optional(prop.int("age")),
    }
)

Knows = edge("knows", {
    "since": prop.int("since"),
})

# Open database
with kite("./social.kitedb", nodes=[User], edges=[Knows]) as db:
    # Insert nodes
    alice = db.insert(User).values(key="alice", name="Alice", email="alice@example.com").returning()
    bob = db.insert(User).values(key="bob", name="Bob", email="bob@example.com").returning()

    # Create edges
    db.link(alice, Knows, bob, since=2024)

    # Traverse
    friends = db.from_(alice).out(Knows).nodes().to_list()

    # Pathfinding
    path = db.shortest_path(alice).via(Knows).to(bob).dijkstra()
```

## Quick start (low-level API)

For direct control, use the low-level `Database` class:

```python
from kitedb import Database, PropValue

with Database("my_graph.kitedb") as db:
    db.begin()

    alice = db.create_node("user:alice")
    bob = db.create_node("user:bob")

    name_key = db.get_or_create_propkey("name")
    db.set_node_prop(alice, name_key, PropValue.string("Alice"))
    db.set_node_prop(bob, name_key, PropValue.string("Bob"))

    knows = db.get_or_create_etype("knows")
    db.add_edge(alice, knows, bob)

    db.commit()

    print("nodes:", db.count_nodes())
    print("edges:", db.count_edges())
```

## Bulk ingest (max throughput)

Use bulk-load transactions + batch APIs to maximize write throughput. A bulk load
works with MVCC on (the default) or off, at the same speed. It runs alone among
writers: it waits for open write transactions to finish, and write transactions that
begin while it is open wait for it. Readers never wait for it; a read transaction
that began before its commit does not see it. Avoid holding a read transaction open
across a large load: the load's commits then record version history for that
reader, which slows them down. `Kite.bulk()` uses a bulk-load transaction too, as does
`batch_create_nodes()` (which multi-row fluent inserts use) outside a transaction.

```python
from kitedb import Database

db = Database("my_graph.kitedb")
db.begin_bulk()

node_ids = db.create_nodes_batch(keys)  # keys: List[Optional[str]]
db.add_edges_batch(edges)               # edges: List[Tuple[int, int, int]]
db.add_edges_with_props_batch(edges_with_props)

db.commit()
```

## Fluent traversal

```python
from kitedb import TraverseOptions

friends = db.from_(alice).out(knows).to_list()

results = db.from_(alice).traverse(
    knows,
    TraverseOptions(max_depth=3, min_depth=1, direction="out", unique=True),
).to_list()
```

## Concurrent Access

Threads can share one `Database` (or `Kite`). Reads don't wait for other readers or for open write
transactions, but each read call holds the GIL, so reads from Python threads run one at a time:

```python
import threading
from concurrent.futures import ThreadPoolExecutor

# Threads can share the handle; their reads run one at a time
def read_user(key):
    return db.get_node_by_key(key)

with ThreadPoolExecutor(max_workers=4) as executor:
    futures = [executor.submit(read_user, f"user:{i}") for i in range(100)]
    results = [f.result() for f in futures]

# Or with asyncio (keeps the event loop free; the reads still run one at a time)
import asyncio

async def read_users():
    loop = asyncio.get_event_loop()
    tasks = [
        loop.run_in_executor(None, db.get_node_by_key, f"user:{i}")
        for i in range(100)
    ]
    return await asyncio.gather(*tasks)
```

**Concurrency model:**

- **Reads don't wait for writers**: `get_node_by_key()`, `get_out_edges()`, traversals, etc. don't wait for
  other threads' open transactions, but they hold the GIL while they run, so they don't run in parallel
- **Writes are concurrent too** (MVCC, on by default): each thread's `begin()` opens its own
  transaction, and write transactions on different threads run at the same time (commits
  that arrive together are written as one group, each applied whole and in order). A
  transaction reads the state as of its `begin()`
  plus its own writes; reads outside a transaction see the latest committed state. A commit
  that overlaps a write committed since its transaction began raises `ConflictError` (see
  [Errors](#errors)); retry it
- **Thread safety**: The `Database` object is safe to share across threads

`OpenOptions(mvcc=False)` turns MVCC off. It is deprecated and will be removed in a later
release (there is no runtime warning). Without MVCC, write transactions run one at a time
(a second writer's `begin()` waits, with the GIL released, until the first finishes) and
transactions read the latest committed state. The file format is the same in both modes.
Each writable open database runs a background thread that prunes MVCC version history
(`mvcc_gc_interval_ms`, default 5000; read-only opens start none); `mvcc_retention_ms`
defaults to 0.

The GIL: point reads and single writes (`get_node_by_key()`, `create_node()`, `set_node_prop()`, ...)
hold it while they run. Calls that can block or run long release it: `open`/`close`,
`begin`/`begin_bulk`/`commit`, savepoints, `batch_create_nodes`, `checkpoint`/`optimize`/`vacuum`,
JSON export/import, backups, replication catch-up and export, `wait_for_token`, streaming chunks and
vector index training. Other Python threads keep running while a commit syncs the WAL, but read-heavy
code gets no faster with more threads. For parallel reads, open the file read-only in several processes
(read-only handles share the file lock; a writable handle holds it alone).

`close()` never deadlocks against a transaction open on another thread. A close-time
checkpoint (`close_with_checkpoint_if_wal_over`, and `Kite.close()`) waits for such a
transaction to finish first; a transaction still open when the database closes is discarded.

## Errors

Failed operations raise `kitedb.KiteError` or one of its subclasses. `KiteError` subclasses
`RuntimeError`, so existing `except RuntimeError` handlers keep working. Invalid arguments
(an unknown `direction`, `metric` or `aggregation`, out-of-range numbers) raise `ValueError`.

| Exception | Raised when |
| --- | --- |
| `ConflictError` | an MVCC transaction conflicts with a concurrent commit (retry it) |
| `ReadOnlyError` | a write is attempted on a read-only database |
| `NotFoundError` | a node, edge or key doesn't exist |
| `ClosedError` | the database handle is closed |
| `TransactionError` | no transaction is open on this thread, or one already is |
| `DuplicateKeyError` | a node with the key already exists |
| `LockError` | another process holds the database file lock |
| `CorruptionError` | on-disk data fails validation |
| `WalFullError` | the WAL and its WAL segments are full and no checkpoint can make room for this write now: automatic checkpoints are off, or blocking (one runs once the transaction ends); open write transactions hold the segments' records; a blocking checkpoint, optimize, vacuum or WAL resize waits for this writer's transaction; the database is closing, or the checkpoint thread cannot start; or the checkpoint that answered the write freed nothing. Checkpoint, or end those transactions, before writing more |
| `CheckpointError` | a write needs WAL segment space only a checkpoint frees, and the last automatic checkpoint failed (`Database.checkpoint_error()`); committed data is safe |
| `CheckpointDeclinedError` | `background_checkpoint()` made no checkpoint, and the message says why: a blocking checkpoint, optimize, vacuum or WAL resize waits for the gate (it checkpoints anyway), or open write transactions hold every WAL segment (segments nothing needs may have been freed); no commit is affected |
| `WritesRefusedError` | the handle refuses writes, `close()` included, until the database is reopened: an operation panicked mid-way through its writes, so memory and disk may disagree (reads go on; the reopen recovers every acknowledged commit) |

```python
from kitedb import ConflictError

while True:
    db.begin()
    try:
        db.set_node_prop(node_id, key_id, PropValue.int(1))
    except BaseException:
        db.rollback()
        raise
    try:
        db.commit()
        break
    except ConflictError:
        # The failed commit applied nothing and ended the transaction: run it again
        continue
```

## Streaming

`stream_nodes`, `stream_nodes_with_props`, `stream_edges` and `stream_edges_with_props`
return lazy iterators of batches (`StreamOptions(batch_size=...)`, default 1000). Each batch
is built when you ask for it, so memory stays proportional to one batch.

```python
from kitedb import StreamOptions

for batch in db.stream_nodes_with_props(StreamOptions(batch_size=500)):
    for node in batch:
        print(node.id, node.key, len(node.props))
```

## Vector search

```python
from kitedb import IvfIndex, IvfConfig, SearchOptions

index = IvfIndex(dimensions=128, config=IvfConfig(n_clusters=100))

training_data = [0.1] * (128 * 1000)
index.add_training_vectors(training_data, num_vectors=1000)
index.train()

index.insert(vector_id=1, vector=[0.1] * 128)

results = index.search(
    manifest_json='{"vectors": {...}}',
    query=[0.1] * 128,
    k=10,
    options=SearchOptions(n_probe=20),
)

for result in results:
    print(result.node_id, result.distance)
```

## Replication admin (low-level API)

Phase D replication controls are available on `Database`:

```python
from kitedb import (
    Database,
    OpenOptions,
    collect_replication_log_transport_json,
    collect_replication_metrics_otel_json,
    collect_replication_metrics_prometheus,
    collect_replication_snapshot_transport_json,
    push_replication_metrics_otel_json,
)

primary = Database(
    "cluster-primary.kitedb",
    OpenOptions(
        replication_role="primary",
        replication_sidecar_path="./cluster-primary.sidecar",
        replication_segment_max_bytes=64 * 1024 * 1024,
        replication_retention_min_entries=1024,
    ),
)

primary.begin()
primary.create_node("n:1")
token = primary.commit_with_token()

primary.primary_report_replica_progress("replica-a", 1, 42)
pruned_segments, retained_floor = primary.primary_run_retention()
primary_status = primary.primary_replication_status()

replica = Database(
    "cluster-replica.kitedb",
    OpenOptions(
        replication_role="replica",
        replication_sidecar_path="./cluster-replica.sidecar",
        replication_source_db_path="cluster-primary.kitedb",
        replication_source_sidecar_path="./cluster-primary.sidecar",
    ),
)

replica.replica_bootstrap_from_snapshot()
replica.replica_catch_up_once(256)
if token:
    replica.wait_for_token(token, 2000)
replica_status = replica.replica_replication_status()
if replica_status and replica_status["needs_reseed"]:
    replica.replica_reseed_from_snapshot()

prometheus = collect_replication_metrics_prometheus(primary)
print(prometheus)

otel_json = collect_replication_metrics_otel_json(primary)
print(otel_json)

status_code, response_body = push_replication_metrics_otel_json(
    primary,
    "http://127.0.0.1:4318/v1/metrics",
    timeout_ms=5000,
)
print(status_code, response_body)

secure_status, secure_body = push_replication_metrics_otel_json(
    primary,
    "https://collector.internal:4318/v1/metrics",
    timeout_ms=5000,
    https_only=True,
    ca_cert_pem_path="./tls/collector-ca.pem",
    client_cert_pem_path="./tls/client.pem",
    client_key_pem_path="./tls/client-key.pem",
)
print(secure_status, secure_body)

snapshot_json = collect_replication_snapshot_transport_json(primary, include_data=False)
print(snapshot_json)

log_json = collect_replication_log_transport_json(
    primary,
    cursor=None,
    max_frames=128,
    max_bytes=1024 * 1024,
    include_payload=False,
)
print(log_json)

replica.close()
primary.close()
```

To guard host HTTP endpoints for these controls, use `create_replication_admin_authorizer`
with a `ReplicationAdminAuthConfig`. `mode` is required (`"none"` disables auth explicitly),
and a config that can't be checked safely raises `ValueError`. Tokens are compared in constant
time. The mTLS modes need a check: prefer `mtls_matcher=create_asgi_tls_mtls_matcher()`, which
reads the server's verified TLS state. A client-certificate header forwarded by a
TLS-terminating proxy (`mtls_header`, default `x-forwarded-client-cert`) counts only with
`trust_forwarded_client_cert=True` and an `mtls_subject_regex` that matches the whole header
value; any client can send that header, so enable it only behind a proxy that verifies client
certificates and overwrites it on every request.

```python
import os

from kitedb import ReplicationAdminAuthConfig, create_replication_admin_authorizer

require_admin = create_replication_admin_authorizer(
    ReplicationAdminAuthConfig(mode="token", token=os.environ["REPLICATION_ADMIN_TOKEN"])
)
require_admin(request)  # raises PermissionError when unauthorized
```

## Documentation

```text
https://kitedb.vercel.com/docs
```

## License

MIT License - see the main project LICENSE file for details.
