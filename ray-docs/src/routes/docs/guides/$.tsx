import { createFileRoute, useLocation } from '@tanstack/solid-router'
import { Show } from 'solid-js'
import DocPage from '~/components/doc-page'
import { MultiLangCode } from '~/components/multi-lang-code'
import { findDocBySlug } from '~/lib/docs'

export const Route = createFileRoute('/docs/guides/$')({
  component: GuidesSplatPage,
})

function GuidesSplatPage() {
  const location = useLocation()
  const slug = () => {
    const path = location().pathname
    const match = path.match(/^\/docs\/(.+)$/)
    return match ? match[1] : ''
  }
  const doc = () => findDocBySlug(slug())

  return (
    <Show
      when={doc()}
      fallback={<DocNotFound slug={slug()} />}
    >
      <DocPageContent slug={slug()} />
    </Show>
  )
}

function DocNotFound(props: { slug: string }) {
  return (
    <div class="max-w-4xl mx-auto px-6 py-12">
      <div class="text-center">
        <h1 class="text-4xl font-extrabold text-slate-900 dark:text-white mb-4">
          Page Not Found
        </h1>
        <p class="text-lg text-slate-600 dark:text-slate-400 mb-8">
          The guide <code class="px-2 py-1 bg-slate-100 dark:bg-slate-800 rounded">{props.slug}</code> doesn't exist yet.
        </p>
        <a
          href="/docs"
          class="inline-flex items-center gap-2 px-6 py-3 bg-gradient-to-r from-cyan-500 to-violet-500 text-white font-semibold rounded-xl hover:shadow-lg hover:shadow-cyan-500/25 transition-all duration-200"
        >
          Back to Documentation
        </a>
      </div>
    </div>
  )
}

function DocPageContent(props: { slug: string }) {
  const slug = props.slug

  if (slug === 'guides/schema') {
    return (
      <DocPage slug={slug}>
        <p>
          KiteDB schemas define the structure of your graph data. 
          This guide covers how to define nodes, edges, and properties.
        </p>

        <h2 id="defining-nodes">Defining Nodes</h2>
        <p>
          Nodes are the vertices in your graph. Each node type needs a unique name 
          and can have typed properties.
        </p>
        <MultiLangCode
          typescript={`import { kite } from '@kitedb/core';

const db = kite('./blog.kitedb', {
  nodes: [
    {
      name: 'article',
      props: {
        title: { type: 'string' },
        content: { type: 'string' },
        published: { type: 'bool' },
        views: { type: 'int' },
        rating: { type: 'float' },
      },
    },
  ],
  edges: [],
});`}
          rust={`use kitedb::kite;

let db = kite("./blog.kitedb", KiteOptions {
    nodes: vec![
        NodeSpec::new("article")
            .prop("title", PropType::String)
            .prop("content", PropType::String)
            .prop("published", PropType::Bool)
            .prop("views", PropType::Int)
            .prop("rating", PropType::Float),
    ],
    edges: vec![],
    ..Default::default()
})?;`}
          python={`from kitedb import kite, define_node, prop

article = define_node("article",
    key=lambda id: f"article:{id}",
    props={
        "title": prop.string("title"),
        "content": prop.string("content"),
        "published": prop.bool("published"),
        "views": prop.int("views"),
        "rating": prop.float("rating"),
    }
)

db = kite("./blog.kitedb", nodes=[article], edges=[])`}
          filename={{ ts: 'schema.ts', rs: 'schema.rs', py: 'schema.py' }}
        />

        <h2 id="property-types">Property Types</h2>
        <p>KiteDB supports the following property types:</p>
        <ul>
          <li><code>string</code> – Text strings</li>
          <li><code>int</code> – 64-bit integers</li>
          <li><code>float</code> – 64-bit floating point numbers</li>
          <li><code>bool</code> – Boolean values</li>
          <li><code>vector</code> – Float32 embedding vectors</li>
        </ul>

        <h2 id="defining-edges">Defining Edges</h2>
        <p>
          Edges connect nodes and can have their own properties.
        </p>
        <MultiLangCode
          typescript={`const db = kite('./blog.kitedb', {
  nodes: [
    { name: 'user', props: { name: { type: 'string' } } },
    { name: 'article', props: { title: { type: 'string' } } },
  ],
  edges: [
    {
      name: 'authored',
      props: {
        role: { type: 'string' },  // 'author' | 'contributor'
      },
    },
    {
      name: 'likes',
      props: {
        likedAt: { type: 'int' },  // Unix timestamp
      },
    },
  ],
});`}
          rust={`let db = kite("./blog.kitedb", KiteOptions {
    nodes: vec![
        NodeSpec::new("user").prop("name", PropType::String),
        NodeSpec::new("article").prop("title", PropType::String),
    ],
    edges: vec![
        EdgeSpec::new("authored")
            .prop("role", PropType::String),
        EdgeSpec::new("likes")
            .prop("likedAt", PropType::Int),
    ],
    ..Default::default()
})?;`}
          python={`from kitedb import kite, define_node, define_edge, prop

user = define_node("user",
    key=lambda id: f"user:{id}",
    props={"name": prop.string("name")}
)

article = define_node("article",
    key=lambda id: f"article:{id}",
    props={"title": prop.string("title")}
)

authored = define_edge("authored", {"role": prop.string("role")})
likes = define_edge("likes", {"likedAt": prop.int("likedAt")})

db = kite("./blog.kitedb", nodes=[user, article], edges=[authored, likes])`}
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li><a href="/docs/guides/queries">Queries & CRUD</a> – Perform operations on your schema</li>
          <li><a href="/docs/guides/vectors">Vector Search</a> – Add embedding vectors</li>
        </ul>
      </DocPage>
    )
  }

  if (slug === 'guides/queries') {
    return (
      <DocPage slug={slug}>
        <p>
          Learn how to create, read, update, and delete data in KiteDB.
        </p>

        <h2 id="create">Creating Nodes</h2>
        <MultiLangCode
          typescript={`// Create a single node with returning
const alice = db.insert('user')
  .values('alice', { name: 'Alice Chen', email: 'alice@example.com' })
  .returning();

// Create without returning (slightly faster)
db.insert('user')
  .values('bob', { name: 'Bob Smith', email: 'bob@example.com' })
  .execute();`}
          rust={`// Create a single node with returning
let alice = db.insert("user")
    .values("alice", json!({
        "name": "Alice Chen",
        "email": "alice@example.com"
    }))
    .returning()?;

// Create without returning (slightly faster)
db.insert("user")
    .values("bob", json!({
        "name": "Bob Smith",
        "email": "bob@example.com"
    }))
    .execute()?;`}
          python={`# Create a single node with returning
alice = (db.insert(user)
    .values(key="alice", name="Alice Chen", email="alice@example.com")
    .returning())

# Create without returning (slightly faster)
(db.insert(user)
    .values(key="bob", name="Bob Smith", email="bob@example.com")
    .execute())`}
        />

        <h2 id="read">Reading Data</h2>
        <MultiLangCode
          typescript={`// Get by key
const user = db.get('user', 'alice');

// Get by node ID
const userById = db.getById(alice.id);

// Check if exists
const exists = db.exists(alice.id);

// List all nodes of a type
const allUsers = db.all('user');

// Count nodes
const userCount = db.countNodes('user');`}
          rust={`// Get by key
let user = db.get("user", "alice")?;

// Get by node ID
let user_by_id = db.get_by_id(alice.id)?;

// Check if exists
let exists = db.exists(alice.id)?;

// List all nodes of a type
let all_users = db.all("user")?;

// Count nodes
let user_count = db.count_nodes(Some("user"))?;`}
          python={`# Get by key
user = db.get(user, "alice")

# Get by node ID
user_by_id = db.get_by_id(alice.id)

# Check if exists
exists = db.exists(alice.id)

# List all nodes of a type
all_users = db.all(user)

# Count nodes
user_count = db.count_nodes("user")`}
        />

        <h2 id="update">Updating Data</h2>
        <MultiLangCode
          typescript={`// Update by node ID
db.updateById(alice.id)
  .set('name', 'Alice C.')
  .execute();

// Update multiple properties
db.updateById(alice.id)
  .setAll({ name: 'Alice Chen', email: 'newemail@example.com' })
  .execute();

// Remove a property
db.updateById(alice.id)
  .unset('email')
  .execute();`}
          rust={`// Update by node ID
db.update_by_id(alice.id)
    .set("name", "Alice C.")
    .execute()?;

// Update multiple properties
db.update_by_id(alice.id)
    .set_all(json!({
        "name": "Alice Chen",
        "email": "newemail@example.com"
    }))
    .execute()?;

// Remove a property
db.update_by_id(alice.id)
    .unset("email")
    .execute()?;`}
          python={`# Update by node ID
(db.update_by_id(alice.id)
    .set("name", "Alice C.")
    .execute())

# Update multiple properties
(db.update_by_id(alice.id)
    .set_all({"name": "Alice Chen", "email": "newemail@example.com"})
    .execute())

# Remove a property
(db.update_by_id(alice.id)
    .unset("email")
    .execute())`}
        />

        <h2 id="delete">Deleting Data</h2>
        <MultiLangCode
          typescript={`// Delete by node ID
db.deleteById(alice.id);

// Delete by key
db.deleteByKey('user', 'alice');`}
          rust={`// Delete by node ID
db.delete_by_id(alice.id)?;

// Delete by key
db.delete_by_key("user", "alice")?;`}
          python={`# Delete by node ID
db.delete_by_id(alice.id)

# Delete by key
db.delete_by_key(user, "alice")`}
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li><a href="/docs/guides/traversal">Graph Traversal</a> – Navigate relationships</li>
          <li><a href="/docs/guides/transactions">Transactions</a> – ACID guarantees</li>
        </ul>
      </DocPage>
    )
  }

  if (slug === 'guides/traversal') {
    return (
      <DocPage slug={slug}>
        <p>
          KiteDB provides powerful graph traversal capabilities to navigate 
          relationships between nodes.
        </p>

        <h2 id="basic-traversal">Basic Traversal</h2>
        <MultiLangCode
          typescript={`// Find all users that Alice follows (outgoing edges)
const following = db
  .from(alice.id)
  .out('follows')
  .nodes();

// Find all followers of Alice (incoming edges)
const followers = db
  .from(alice.id)
  .in('follows')
  .nodes();

// Follow edges in both directions
const connections = db
  .from(alice.id)
  .both('knows')
  .nodes();`}
          rust={`// Find all users that Alice follows (outgoing edges)
let following = db
    .from(alice.id)
    .out(Some("follows"))
    .nodes()?;

// Find all followers of Alice (incoming edges)
let followers = db
    .from(alice.id)
    .in_(Some("follows"))
    .nodes()?;

// Follow edges in both directions
let connections = db
    .from(alice.id)
    .both(Some("knows"))
    .nodes()?;`}
          python={`# Find all users that Alice follows (outgoing edges)
following = (db
    .from_(alice)
    .out(follows)
    .nodes()
    .to_list())

# Find all followers of Alice (incoming edges)
followers = (db
    .from_(alice)
    .in_(follows)
    .nodes()
    .to_list())

# Follow edges in both directions
connections = (db
    .from_(alice)
    .both(knows)
    .nodes()
    .to_list())`}
        />

        <h2 id="multi-hop">Multi-Hop Traversal</h2>
        <MultiLangCode
          typescript={`// Find friends of friends (2-hop)
const friendsOfFriends = db
  .from(alice.id)
  .out('follows')
  .out('follows')
  .nodes();

// Chain different edge types
const authorsOfLikedArticles = db
  .from(alice.id)
  .out('likes')     // Alice -> Articles
  .in('authored')   // Articles <- Users
  .nodes();`}
          rust={`// Find friends of friends (2-hop)
let friends_of_friends = db
    .from(alice.id)
    .out(Some("follows"))
    .out(Some("follows"))
    .nodes()?;

// Chain different edge types
let authors_of_liked = db
    .from(alice.id)
    .out(Some("likes"))     // Alice -> Articles
    .in_(Some("authored"))  // Articles <- Users
    .nodes()?;`}
          python={`# Find friends of friends (2-hop)
friends_of_friends = (db
    .from_(alice)
    .out(follows)
    .out(follows)
    .nodes()
    .to_list())

# Chain different edge types
authors_of_liked = (db
    .from_(alice)
    .out(likes)       # Alice -> Articles
    .in_(authored)    # Articles <- Users
    .nodes()
    .to_list())`}
        />

        <h2 id="variable-depth">Variable Depth Traversal</h2>
        <MultiLangCode
          typescript={`// Traverse 1-3 hops
const network = db
  .from(alice.id)
  .traverse('follows', { minDepth: 1, maxDepth: 3 })
  .nodes();

// Limit results
const topConnections = db
  .from(alice.id)
  .out('follows')
  .take(10)
  .nodes();`}
          rust={`// Traverse 1-3 hops
let network = db
    .from(alice.id)
    .traverse(Some("follows"), TraverseOptions {
        min_depth: Some(1),
        max_depth: 3,
        ..Default::default()
    })
    .nodes()?;

// Limit results
let top_connections = db
    .from(alice.id)
    .out(Some("follows"))
    .take(10)
    .nodes()?;`}
          python={`# Traverse 1-3 hops
network = (db
    .from_(alice)
    .traverse(follows, min_depth=1, max_depth=3)
    .nodes()
    .to_list())

# Limit results
top_connections = (db
    .from_(alice)
    .out(follows)
    .take(10)
    .nodes()
    .to_list())`}
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li><a href="/docs/guides/vectors">Vector Search</a> – Combine with semantic search</li>
          <li><a href="/docs/api/high-level">API Reference</a> – Full traversal API</li>
        </ul>
      </DocPage>
    )
  }

  if (slug === 'guides/vectors') {
    return (
      <DocPage slug={slug}>
        <p>
          KiteDB includes built-in vector search for semantic similarity queries. 
          Store embeddings and find similar nodes using IVF indexing.
        </p>

        <h2 id="creating-index">Creating a Vector Index</h2>
        <MultiLangCode
          typescript={`import { createVectorIndex } from '@kitedb/core';

// Create an index for 1536-dimensional vectors (OpenAI embeddings)
const index = createVectorIndex({
  dimensions: 1536,
  metric: 'Cosine',  // or 'Euclidean', 'DotProduct'
});`}
          rust={`use kitedb::{VectorIndex, VectorIndexOptions, DistanceMetric};

// Create an index for 1536-dimensional vectors
let mut index = VectorIndex::new(VectorIndexOptions {
    dimensions: 1536,
    metric: DistanceMetric::Cosine,
    ..Default::default()
})?;`}
          python={`from kitedb import create_vector_index

# Create an index for 1536-dimensional vectors
index = create_vector_index(
    dimensions=1536,
    metric="Cosine",  # or "Euclidean", "DotProduct"
)`}
        />

        <h2 id="storing-embeddings">Storing Embeddings</h2>
        <MultiLangCode
          typescript={`// Generate embedding with your preferred provider
const response = await openai.embeddings.create({
  model: 'text-embedding-ada-002',
  input: 'Your document content here',
});
const embedding = response.data[0].embedding;

// Store the vector, associated with a node ID
index.set(doc.id, embedding);`}
          rust={`// Get embedding from your provider
let embedding: Vec<f32> = get_embedding("Your document content")?;

// Store the vector, associated with a node ID
index.set(doc.id, &embedding)?;`}
          python={`# Generate embedding with your preferred provider
response = openai.embeddings.create(
    model="text-embedding-ada-002",
    input="Your document content here",
)
embedding = response.data[0].embedding

# Store the vector, associated with a node ID
index.set(doc.id, embedding)`}
        />

        <h2 id="similarity-search">Similarity Search</h2>
        <MultiLangCode
          typescript={`// Search for similar vectors
const queryEmbedding = await getEmbedding('search query');

const results = index.search(queryEmbedding, {
  k: 10,           // Return top 10 results
  threshold: 0.7,  // Minimum similarity (0-1)
});

// Results contain nodeId, distance, and similarity
for (const hit of results) {
  console.log(\`Node \${hit.nodeId}: similarity=\${hit.similarity.toFixed(3)}\`);
}`}
          rust={`// Search for similar vectors
let query_embedding = get_embedding("search query")?;

let results = index.search(&query_embedding, SimilarOptions {
    k: 10,
    threshold: Some(0.7),
    ..Default::default()
})?;

// Results contain node_id, distance, and similarity
for hit in results {
    println!("Node {}: similarity={:.3}", hit.node_id, hit.similarity);
}`}
          python={`# Search for similar vectors
query_embedding = get_embedding("search query")

results = index.search(query_embedding, k=10, threshold=0.7)

# Results contain node_id, distance, and similarity
for hit in results:
    print(f"Node {hit.node_id}: similarity={hit.similarity:.3f}")`}
        />

        <h2 id="index-management">Index Management</h2>
        <MultiLangCode
          typescript={`// Check if a node has a vector
const hasVector = index.has(doc.id);

// Get a stored vector
const vector = index.get(doc.id);

// Delete a vector
index.delete(doc.id);

// Build/rebuild the IVF index for faster search
index.buildIndex();

// Get index statistics
const stats = index.stats();
console.log(\`Total vectors: \${stats.totalVectors}\`);`}
          rust={`// Check if a node has a vector
let has_vector = index.has(doc.id)?;

// Get a stored vector
let vector = index.get(doc.id)?;

// Delete a vector
index.delete(doc.id)?;

// Build/rebuild the IVF index for faster search
index.build_index()?;

// Get index statistics
let stats = index.stats()?;
println!("Total vectors: {}", stats.total_vectors);`}
          python={`# Check if a node has a vector
has_vector = index.has(doc.id)

# Get a stored vector
vector = index.get(doc.id)

# Delete a vector
index.delete(doc.id)

# Build/rebuild the IVF index for faster search
index.build_index()

# Get index statistics
stats = index.stats()
print(f"Total vectors: {stats.total_vectors}")`}
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li><a href="/docs/api/vector-api">Vector API Reference</a> – Full vector API</li>
          <li><a href="/docs/internals/performance">Performance</a> – Optimization tips</li>
        </ul>
      </DocPage>
    )
  }

  if (slug === 'guides/transactions') {
    return (
      <DocPage slug={slug}>
        <p>
          KiteDB supports transactions for atomic operations. The low-level 
          API provides explicit transaction control.
        </p>

        <h2 id="high-level">High-Level Transactions (Kite)</h2>
        <p>
          The high-level <code>Kite</code> API supports explicit transactions for batching
          multiple operations into a single commit. When the callback completes, the
          transaction commits; on error it rolls back.
        </p>
        <MultiLangCode
          typescript={`import { kite } from '@kitedb/core';

const db = await kite('./my.kitedb', { nodes: [User], edges: [follows] });

await db.transaction(async (ctx) => {
  const alice = ctx.insert('user').values('alice', { name: 'Alice' }).returning();
  const bob = ctx.insert('user').values('bob', { name: 'Bob' }).returning();
  ctx.link(alice.id, 'follows', bob.id, { since: 2024 });
});`}
          rust={`use kitedb::api::kite::Kite;
use std::collections::HashMap;

let mut db = Kite::open("./my.kitedb", options)?;

db.transaction(|ctx| {
    let alice = ctx.create_node("user", "alice", HashMap::new())?;
    let bob = ctx.create_node("user", "bob", HashMap::new())?;
    ctx.link(alice.id, "follows", bob.id)?;
    Ok(())
})?;`}
          python={`from kitedb import kite

db = kite("./my.kitedb", nodes=[user], edges=[follows])

with db.transaction():
    alice = db.insert(user).values(key="alice", name="Alice").returning()
    bob = db.insert(user).values(key="bob", name="Bob").returning()
    db.link(alice, follows, bob, since=2024)`}
        />

        <h2 id="batch">Batch Operations</h2>
        <p>
          Batch operations run in a single transaction. This is ideal for fast
          ingestion (e.g., indexing a codebase) and guarantees atomicity.
        </p>
        <MultiLangCode
          typescript={`// Batch with builder/executor operations (sync)
db.batch([
  db.insert('user').values('alice', { name: 'Alice' }),
  db.insert('user').values('bob', { name: 'Bob' }),
  () => db.link(aliceId, 'follows', bobId, { since: 2024 }),
]);`}
          rust={`use kitedb::api::kite::BatchOp;

db.batch(vec![
    BatchOp::CreateNode { node_type: "user".into(), key_suffix: "alice".into(), props: HashMap::new() },
    BatchOp::CreateNode { node_type: "user".into(), key_suffix: "bob".into(), props: HashMap::new() },
])?;`}
          python={`db.batch([
    db.insert(user).values(key="alice", name="Alice"),
    db.insert(user).values(key="bob", name="Bob"),
])`}
        />

        <h2 id="limitations">Current Limitations</h2>
        <ul>
          <li>
            Traversal and path queries read the committed view. If you need to
            traverse newly written edges, commit first.
          </li>
          <li>
            JavaScript <code>batch()</code> is synchronous; avoid async work inside
            a batch (do async work first, then batch the writes).
          </li>
        </ul>

        <h2 id="basic-transactions">Basic Transactions</h2>
        <MultiLangCode
          typescript={`import { Database } from '@kitedb/core';

const db = Database.open('./my.kitedb');

// Begin a read-write transaction
db.begin();

try {
  // All operations are part of the transaction
  const nodeId = db.createNode('user:alice');
  const nameKey = db.getOrCreatePropkey('name');
  db.setNodePropByName(nodeId, 'name', { propType: 'String', stringValue: 'Alice' });
  
  // Commit the transaction
  db.commit();
} catch (e) {
  // Rollback on error
  db.rollback();
  throw e;
}`}
          rust={`use kitedb::Database;

let db = Database::open("./my.kitedb", None)?;

// Begin a read-write transaction
db.begin(false)?;

// All operations are part of the transaction
let node_id = db.create_node(Some("user:alice"))?;
db.set_node_prop_by_name(node_id, "name", PropValue::String("Alice".into()))?;

// Commit the transaction
db.commit()?;

// Or rollback on error
// db.rollback()?;`}
          python={`from kitedb import Database

db = Database("./my.kitedb")

# Begin a read-write transaction
db.begin()

try:
    # All operations are part of the transaction
    node_id = db.create_node("user:alice")
    db.set_node_prop_by_name(node_id, "name", PropValue.string("Alice"))
    
    # Commit the transaction
    db.commit()
except Exception as e:
    # Rollback on error
    db.rollback()
    raise e`}
        />

        <h2 id="read-only">Read-Only Transactions</h2>
        <MultiLangCode
          typescript={`// Begin a read-only transaction (faster, no locking)
db.begin(true);

const node = db.getNodeByKey('user:alice');
const props = db.getNodeProps(node);

// Read-only transactions still need to be ended
db.commit();  // or db.rollback() - same effect for read-only`}
          rust={`// Begin a read-only transaction (faster, no locking)
db.begin(true)?;

let node = db.get_node_by_key("user:alice")?;
let props = db.get_node_props(node)?;

// Read-only transactions still need to be ended
db.commit()?;`}
          python={`# Begin a read-only transaction (faster, no locking)
db.begin(read_only=True)

node = db.get_node_by_key("user:alice")
props = db.get_node_props(node)

# Read-only transactions still need to be ended
db.commit()  # or db.rollback() - same effect for read-only`}
        />

        <h2 id="transaction-status">Transaction Status</h2>
        <MultiLangCode
          typescript={`// Check if there's an active transaction
if (db.hasTransaction()) {
  console.log('Transaction is active');
}

// The Kite high-level API auto-manages transactions
// and also supports explicit db.transaction()/db.batch()`}
          rust={`// Check if there's an active transaction
if db.has_transaction() {
    println!("Transaction is active");
}

// The Kite high-level API auto-manages transactions`}
          python={`# Check if there's an active transaction
if db.has_transaction():
    print("Transaction is active")

# The Kite high-level API auto-manages transactions`}
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li><a href="/docs/api/high-level">API Reference</a> – Full transaction API</li>
          <li><a href="/docs/internals/architecture">Architecture</a> – How transactions work</li>
        </ul>
      </DocPage>
    )
  }

  if (slug === 'guides/concurrency') {
    return (
      <DocPage slug={slug}>
        <p>
          KiteDB supports concurrent access from multiple threads, enabling
          parallel reads for improved throughput in multi-threaded applications.
        </p>

        <h2 id="concurrency-model">Concurrency Model</h2>
        <p>
          KiteDB uses a <strong>readers-writer lock</strong> pattern:
        </p>
        <ul>
          <li>
            <strong>Multiple concurrent readers</strong> – Any number of threads
            can read simultaneously
          </li>
          <li>
            <strong>Exclusive writer</strong> – Write operations acquire
            exclusive access
          </li>
          <li>
            <strong>MVCC isolation</strong> – Transactions see consistent
            snapshots
          </li>
        </ul>

        <MultiLangCode
          typescript={`// Concurrent reads from multiple async operations
const results = await Promise.all([
  db.get(user, 'alice'),
  db.get(user, 'bob'),
  db.get(user, 'charlie'),
  db.from(user).out('follows').toArray(),
]);

// With worker threads, share the db path (each worker opens independently)
// Workers can read concurrently from the same database file`}
          rust={`use std::sync::{Arc, RwLock};
use std::thread;

let db = Arc::new(RwLock::new(Kite::open("./data.kitedb")?));

let handles: Vec<_> = (0..4).map(|i| {
    let db = Arc::clone(&db);
    thread::spawn(move || {
        // Multiple threads can acquire read locks simultaneously
        let guard = db.read().unwrap();
        guard.get_node(format!("user:{}", i))
    })
}).collect();

// Collect results
let results: Vec<_> = handles.into_iter()
    .map(|h| h.join().unwrap())
    .collect();`}
          python={`import threading

db = kite("./data.kitedb", nodes=[user])
results = {}

def read_user(user_id: str):
    """Each thread can read concurrently"""
    results[user_id] = db.get(user, user_id)

# Spawn multiple reader threads
threads = [
    threading.Thread(target=read_user, args=(uid,))
    for uid in ["alice", "bob", "charlie", "dave"]
]

for t in threads:
    t.start()
for t in threads:
    t.join()

# All reads completed in parallel
print(results)`}
        />

        <h2 id="performance">Performance Scaling</h2>
        <p>
          Benchmarks show ~1.5-1.8x throughput improvement with 4-8 reader
          threads:
        </p>
        <table>
          <thead>
            <tr>
              <th>Threads</th>
              <th>Relative Throughput</th>
              <th>Notes</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>1</td>
              <td>1.0x (baseline)</td>
              <td>Single-threaded</td>
            </tr>
            <tr>
              <td>2</td>
              <td>~1.3x</td>
              <td>Good scaling</td>
            </tr>
            <tr>
              <td>4</td>
              <td>~1.5-1.6x</td>
              <td>Sweet spot for most workloads</td>
            </tr>
            <tr>
              <td>8</td>
              <td>~1.6-1.8x</td>
              <td>Diminishing returns</td>
            </tr>
            <tr>
              <td>16</td>
              <td>~1.7-1.9x</td>
              <td>Lock contention increases</td>
            </tr>
          </tbody>
        </table>

        <h2 id="best-practices">Best Practices</h2>
        <ul>
          <li>
            <strong>Batch writes</strong> – Group multiple writes into single
            operations to minimize exclusive lock time
          </li>
          <li>
            <strong>Use transactions for consistency</strong> – MVCC ensures
            readers see consistent snapshots even during concurrent writes
          </li>
          <li>
            <strong>Profile your workload</strong> – The optimal thread count
            depends on your read/write ratio and data access patterns
          </li>
          <li>
            <strong>Avoid long-held locks</strong> – Keep critical sections
            short; do processing outside the lock
          </li>
        </ul>

        <h2 id="mvcc">MVCC and Snapshot Isolation</h2>
        <p>
          KiteDB uses Multi-Version Concurrency Control (MVCC) to provide
          snapshot isolation:
        </p>
        <ul>
          <li>Readers never block writers</li>
          <li>Writers never block readers</li>
          <li>
            Each transaction sees a consistent snapshot from its start time
          </li>
          <li>Write conflicts are detected and one transaction is aborted</li>
        </ul>

        <MultiLangCode
          typescript={`// Transaction isolation example
const tx1 = db.beginTransaction();
const tx2 = db.beginTransaction();

// tx1 reads value
const value1 = tx1.get(user, 'alice');

// tx2 modifies same value
tx2.update(user, 'alice', { name: 'Alice Updated' });
tx2.commit();

// tx1 still sees original value (snapshot isolation)
const value2 = tx1.get(user, 'alice');
console.log(value1.name === value2.name); // true`}
          rust={`// Transaction isolation example
let tx1 = db.begin_transaction()?;
let tx2 = db.begin_transaction()?;

// tx1 reads value
let value1 = tx1.get("user", "alice")?;

// tx2 modifies same value
tx2.update("user", "alice", json!({"name": "Alice Updated"}))?;
tx2.commit()?;

// tx1 still sees original value (snapshot isolation)
let value2 = tx1.get("user", "alice")?;
assert_eq!(value1.name, value2.name); // true`}
          python={`# Transaction isolation example
tx1 = db.begin_transaction()
tx2 = db.begin_transaction()

# tx1 reads value
value1 = tx1.get(user, "alice")

# tx2 modifies same value
tx2.update(user, "alice", {"name": "Alice Updated"})
tx2.commit()

# tx1 still sees original value (snapshot isolation)
value2 = tx1.get(user, "alice")
print(value1.name == value2.name)  # True`}
        />

        <h2 id="limitations">Limitations</h2>
        <ul>
          <li>
            <strong>Single-process only</strong> – Concurrent access is within a
            single process; multi-process access requires external coordination
          </li>
          <li>
            <strong>Write serialization</strong> – All writes are serialized;
            high-write workloads may see contention
          </li>
          <li>
            <strong>Memory overhead</strong> – MVCC maintains version history,
            using additional memory
          </li>
        </ul>

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li>
            <a href="/docs/guides/transactions">Transactions</a> – Learn about
            ACID guarantees
          </li>
          <li>
            <a href="/docs/benchmarks">Benchmarks</a> – See detailed performance
            numbers
          </li>
          <li>
            <a href="/docs/internals/architecture">Architecture</a> – Understand
            the internal design
          </li>
        </ul>
      </DocPage>
    )
  }

  // Default fallback
  return (
    <DocPage slug={slug}>
      <p>This guide is coming soon.</p>
    </DocPage>
  )
}
