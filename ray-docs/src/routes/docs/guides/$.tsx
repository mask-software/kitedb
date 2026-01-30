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
          RayDB schemas define the structure of your graph data. 
          This guide covers how to define nodes, edges, and properties.
        </p>

        <h2 id="defining-nodes">Defining Nodes</h2>
        <p>
          Nodes are the vertices in your graph. Each node type needs a unique name 
          and can have typed properties.
        </p>
        <MultiLangCode
          typescript={`import { ray } from '@ray-db/core';

const db = ray('./blog.raydb', {
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
          rust={`use raydb::ray;

let db = ray("./blog.raydb", RayOptions {
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
          python={`from raydb import ray, define_node, prop

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

db = ray("./blog.raydb", nodes=[article], edges=[])`}
          filename={{ ts: 'schema.ts', rs: 'schema.rs', py: 'schema.py' }}
        />

        <h2 id="property-types">Property Types</h2>
        <p>RayDB supports the following property types:</p>
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
          typescript={`const db = ray('./blog.raydb', {
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
          rust={`let db = ray("./blog.raydb", RayOptions {
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
          python={`from raydb import ray, define_node, define_edge, prop

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

db = ray("./blog.raydb", nodes=[user, article], edges=[authored, likes])`}
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
          Learn how to create, read, update, and delete data in RayDB.
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
          RayDB provides powerful graph traversal capabilities to navigate 
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
          RayDB includes built-in vector search for semantic similarity queries. 
          Store embeddings and find similar nodes using IVF indexing.
        </p>

        <h2 id="creating-index">Creating a Vector Index</h2>
        <MultiLangCode
          typescript={`import { createVectorIndex } from '@ray-db/core';

// Create an index for 1536-dimensional vectors (OpenAI embeddings)
const index = createVectorIndex({
  dimensions: 1536,
  metric: 'Cosine',  // or 'Euclidean', 'DotProduct'
});`}
          rust={`use raydb::{VectorIndex, VectorIndexOptions, DistanceMetric};

// Create an index for 1536-dimensional vectors
let mut index = VectorIndex::new(VectorIndexOptions {
    dimensions: 1536,
    metric: DistanceMetric::Cosine,
    ..Default::default()
})?;`}
          python={`from raydb import create_vector_index

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
          RayDB supports transactions for atomic operations. The low-level 
          API provides explicit transaction control.
        </p>

        <h2 id="basic-transactions">Basic Transactions</h2>
        <MultiLangCode
          typescript={`import { Database } from '@ray-db/core';

const db = Database.open('./my.raydb');

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
          rust={`use raydb::Database;

let db = Database::open("./my.raydb", None)?;

// Begin a read-write transaction
db.begin(false)?;

// All operations are part of the transaction
let node_id = db.create_node(Some("user:alice"))?;
db.set_node_prop_by_name(node_id, "name", PropValue::String("Alice".into()))?;

// Commit the transaction
db.commit()?;

// Or rollback on error
// db.rollback()?;`}
          python={`from raydb import Database

db = Database("./my.raydb")

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

// The Ray high-level API auto-manages transactions
// Each operation is atomic by default`}
          rust={`// Check if there's an active transaction
if db.has_transaction() {
    println!("Transaction is active");
}

// The Ray high-level API auto-manages transactions`}
          python={`# Check if there's an active transaction
if db.has_transaction():
    print("Transaction is active")

# The Ray high-level API auto-manages transactions`}
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li><a href="/docs/api/high-level">API Reference</a> – Full transaction API</li>
          <li><a href="/docs/internals/architecture">Architecture</a> – How transactions work</li>
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
