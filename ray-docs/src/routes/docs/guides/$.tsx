import { createFileRoute } from "@tanstack/solid-router";
import { DocNotFound } from "~/components/doc-not-found";
import DocPage from "~/components/doc-page";
import { MultiLangCode } from "~/components/multi-lang-code";
import { loadDocSlug } from "~/lib/doc-route";

export const Route = createFileRoute("/docs/guides/$")({
	loader: loadDocSlug,
	component: GuidesSplatPage,
	notFoundComponent: () => <DocNotFound />,
});

function GuidesSplatPage() {
	const data = Route.useLoaderData();
	return <DocPageContent slug={data().slug} />;
}

function DocPageContent(props: { slug: string }) {
	const slug = props.slug;

	if (slug === "guides/schema") {
		return (
			<DocPage slug={slug}>
				<p>
					A schema declares the node and edge types in your graph and the typed
					properties each type carries. This guide covers nodes, edges, and the
					available property types.
				</p>

				<h2 id="defining-nodes">Defining nodes</h2>
				<p>
					Nodes are the vertices of the graph. Each node type needs a unique
					name and can declare typed properties.
				</p>
				<MultiLangCode
					typescript={`import { kite } from '@kitedb/core';

const db = await kite('./blog.kitedb', {
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
					rust={`use kitedb::api::kite::{kite, KiteOptions, NodeDef, PropDef};

let db = kite(
    "./blog.kitedb",
    KiteOptions::new().node(
        NodeDef::new("article", "article:")
            .prop(PropDef::string("title"))
            .prop(PropDef::string("content"))
            .prop(PropDef::bool("published"))
            .prop(PropDef::int("views"))
            .prop(PropDef::float("rating")),
    ),
)?;`}
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
					filename={{ ts: "schema.ts", rs: "schema.rs", py: "schema.py" }}
				/>

				<h2 id="property-types">Property types</h2>
				<p>KiteDB supports the following property types:</p>
				<ul>
					<li>
						<code>string</code> – Text strings
					</li>
					<li>
						<code>int</code> – 64-bit integers
					</li>
					<li>
						<code>float</code> – 64-bit floating point numbers
					</li>
					<li>
						<code>bool</code> – Boolean values
					</li>
					<li>
						<code>vector</code> – Float32 embedding vectors
					</li>
				</ul>
				<p>
					TypeScript builders are available as top-level exports (e.g.{" "}
					<code>string()</code>) or under <code>prop</code> (e.g.{" "}
					<code>prop.string()</code>).
				</p>

				<h2 id="defining-edges">Defining edges</h2>
				<p>
					Edges connect two nodes. Like nodes, they can carry their own
					properties.
				</p>
				<MultiLangCode
					typescript={`const db = await kite('./blog.kitedb', {
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
					rust={`use kitedb::api::kite::{kite, EdgeDef, KiteOptions, NodeDef, PropDef};

let db = kite(
    "./blog.kitedb",
    KiteOptions::new()
        .node(NodeDef::new("user", "user:").prop(PropDef::string("name")))
        .node(NodeDef::new("article", "article:").prop(PropDef::string("title")))
        .edge(EdgeDef::new("authored").prop(PropDef::string("role")))
        .edge(EdgeDef::new("likes").prop(PropDef::int("likedAt"))),
)?;`}
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

				<h2 id="next-steps">Next steps</h2>
				<ul>
					<li>
						<a href="/docs/guides/queries">Queries & CRUD</a>: read and write
						nodes of these types
					</li>
					<li>
						<a href="/docs/guides/vectors">Vector search</a>: attach embedding
						vectors to nodes
					</li>
				</ul>
			</DocPage>
		);
	}

	if (slug === "guides/queries") {
		return (
			<DocPage slug={slug}>
				<p>
					Create, read, update, and delete nodes with the high-level API. The
					examples use the <code>user</code> schema from the{" "}
					<a href="/docs/getting-started/quick-start">quick start</a>.
				</p>

				<h2 id="create">Creating nodes</h2>
				<MultiLangCode
					typescript={`// Create a single node with returning
const alice = db.insert('user')
  .values('alice', { name: 'Alice Chen', email: 'alice@example.com' })
  .returning();

// Create without returning (slightly faster)
db.insert('user')
  .values('bob', { name: 'Bob Smith', email: 'bob@example.com' })
  .execute();`}
					rust={`use kitedb::types::PropValue;
use std::collections::HashMap;

// Create a single node with returning
let alice = db
    .insert("user")?
    .values("alice", HashMap::from([
        ("name".into(), PropValue::String("Alice Chen".into())),
        ("email".into(), PropValue::String("alice@example.com".into())),
    ]))?
    .returning()?;

// Create without returning (slightly faster)
db.insert("user")?
    .values("bob", HashMap::from([
        ("name".into(), PropValue::String("Bob Smith".into())),
        ("email".into(), PropValue::String("bob@example.com".into())),
    ]))?
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

				<h2 id="read">Reading data</h2>
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
let user_by_id = db.node_by_id(alice.id())?;

// Check if exists
let exists = db.exists(alice.id());

// List all nodes of a type
let all_users: Vec<_> = db.all("user")?.collect();

// Count nodes
let user_count = db.count_nodes_by_type("user")?;`}
					python={`# Get by key
alice = db.get(user, "alice")

# Get lightweight ref by key (no properties loaded)
alice_ref = db.get_ref(user, "alice")

# Check if exists
exists = alice is not None and db.exists(alice)

# List all nodes of a type
all_users = list(db.all(user))

# Count nodes
user_count = db.count(user)`}
				/>

				<h2 id="update">Updating data</h2>
				<MultiLangCode
					typescript={`// Update by key. The update builder's methods return void,
// so call them on the builder rather than chaining.
const rename = db.update('user', 'alice');
rename.set('name', 'Alice C.');
rename.execute();

// Update multiple properties
const edit = db.update('user', 'alice');
edit.setAll({ name: 'Alice Chen', email: 'newemail@example.com' });
edit.execute();

// Remove a property
const clear = db.update('user', 'alice');
clear.unset('email');
clear.execute();`}
					rust={`// Update by node ID
db.update_by_id(alice.id())?
    .set("name", PropValue::String("Alice C.".into()))
    .execute()?;

// Update multiple properties
db.update_by_id(alice.id())?
    .set("name", PropValue::String("Alice Chen".into()))
    .set("email", PropValue::String("newemail@example.com".into()))
    .execute()?;

// Remove a property
db.update_by_id(alice.id())?
    .unset("email")
    .execute()?;`}
					python={`# Update by node reference
(db.update(alice)
    .set(name="Alice C.")
    .execute())

# Update multiple properties
(db.update(alice)
    .set({"name": "Alice Chen", "email": "newemail@example.com"})
    .execute())

# Update another property
(db.update(alice)
    .set(email="newemail@example.com")
    .execute())`}
				/>

				<h2 id="delete">Deleting data</h2>
				<MultiLangCode
					typescript={`// Delete by node ID
db.deleteById(alice.id);

// Delete by key
db.deleteByKey('user', 'alice');`}
					rust={`// Delete by node ID
db.delete_node(alice.id())?;

// Delete by key (lookup then delete)
if let Some(node) = db.get("user", "alice")? {
    db.delete_node(node.id())?;
}`}
					python={`# Delete by node reference
db.delete(alice)

# Delete by key (lookup then delete)
node = db.get(user, "alice")
if node is not None:
    db.delete(node)`}
				/>

				<h2 id="next-steps">Next steps</h2>
				<ul>
					<li>
						<a href="/docs/guides/traversal">Graph traversal</a>: follow edges
						between nodes
					</li>
					<li>
						<a href="/docs/guides/transactions">Transactions</a>: commit several
						writes atomically
					</li>
				</ul>
			</DocPage>
		);
	}

	if (slug === "guides/traversal") {
		return (
			<DocPage slug={slug}>
				<p>
					A traversal starts at a node and follows edges outgoing, incoming, or
					in both directions. Steps chain, so a multi-hop query reads left to
					right.
				</p>

				<h2 id="basic-traversal">Basic traversal</h2>
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
    .from(alice.id())
    .out(Some("follows"))?
    .to_vec();

// Find all followers of Alice (incoming edges)
let followers = db
    .from(alice.id())
    .r#in(Some("follows"))?
    .to_vec();

// Follow edges in both directions
let connections = db
    .from(alice.id())
    .both(Some("knows"))?
    .to_vec();`}
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

				<h2 id="multi-hop">Multi-hop traversal</h2>
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
    .from(alice.id())
    .out(Some("follows"))?
    .out(Some("follows"))?
    .to_vec();

// Chain different edge types
let authors_of_liked = db
    .from(alice.id())
    .out(Some("likes"))?       // Alice -> Articles
    .r#in(Some("authored"))?   // Articles <- Users
    .to_vec();`}
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

				<h2 id="variable-depth">Variable-depth traversal</h2>
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
					rust={`use kitedb::api::traversal::TraverseOptions;

// Traverse 1-3 hops
let network = db
    .from(alice.id())
    .traverse(Some("follows"), TraverseOptions {
        min_depth: 1,
        max_depth: 3,
        ..Default::default()
    })?
    .to_vec();

// Limit results
let top_connections = db
    .from(alice.id())
    .out(Some("follows"))?
    .take(10)
    .to_vec();`}
					python={`from kitedb import TraverseOptions

# Traverse 1-3 hops
network = (db
    .from_(alice)
    .traverse(follows, TraverseOptions(min_depth=1, max_depth=3))
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

				<h2 id="next-steps">Next steps</h2>
				<ul>
					<li>
						<a href="/docs/guides/vectors">Vector search</a>: find similar
						nodes, then traverse from them
					</li>
					<li>
						<a href="/docs/api/high-level">High-level API</a>: the full
						traversal API
					</li>
				</ul>
			</DocPage>
		);
	}

	if (slug === "guides/vectors") {
		return (
			<DocPage slug={slug}>
				<p>
					KiteDB has built-in vector search for similarity queries. You store an
					embedding per node, and an approximate nearest-neighbor index (IVF, or
					IVF-PQ for large high-dimensional collections) finds the nearest
					vectors to a query embedding.
				</p>

				<h2 id="creating-index">Creating a vector index</h2>
				<MultiLangCode
					typescript={`import { createVectorIndex, DistanceMetric } from '@kitedb/core';

// Create an index for 1536-dimensional vectors (OpenAI embeddings)
const index = createVectorIndex({
  dimensions: 1536,
  metric: DistanceMetric.Cosine, // or Euclidean, DotProduct
});`}
					rust={`use kitedb::api::vector_search::{VectorIndex, VectorIndexOptions};
use kitedb::vector::DistanceMetric;

// Create an index for 1536-dimensional vectors
let mut index = VectorIndex::new(VectorIndexOptions {
    dimensions: 1536,
    metric: DistanceMetric::Cosine,
    ..Default::default()
});`}
					python={`from kitedb import create_vector_index, VectorIndexOptions

# Create an index for 1536-dimensional vectors
index = create_vector_index(VectorIndexOptions(
    dimensions=1536,
    metric="cosine",  # or "euclidean", "dot_product"
))`}
				/>

				<h2 id="storing-embeddings">Storing embeddings</h2>
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
index.set(doc.id(), &embedding)?;`}
					python={`# Generate embedding with your preferred provider
response = openai.embeddings.create(
    model="text-embedding-ada-002",
    input="Your document content here",
)
embedding = response.data[0].embedding

# Store the vector, associated with a node reference
index.set(doc, embedding)`}
				/>

				<h2 id="similarity-search">Similarity search</h2>
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
					rust={`use kitedb::api::vector_search::SimilarOptions;

// Search for similar vectors
let query_embedding = get_embedding("search query")?;

let results = index.search(
    &query_embedding,
    SimilarOptions::new(10).with_threshold(0.7),
)?;

// Results contain node_id, distance, and similarity
for hit in results {
    println!("Node {}: similarity={:.3}", hit.node_id, hit.similarity);
}`}
					python={`# Search for similar vectors
query_embedding = get_embedding("search query")

results = index.search(query_embedding, k=10, threshold=0.7)

# Results contain node, distance, and similarity
for hit in results:
    print(f"Node {hit.node.key}: similarity={hit.similarity:.3f}")`}
				/>

				<h2 id="index-management">Index management</h2>
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
let has_vector = index.has(doc.id());

// Get a stored vector
let vector = index.get(doc.id());

// Delete a vector
index.delete(doc.id())?;

// Build/rebuild the IVF index for faster search
index.build_index()?;

// Get index statistics
let stats = index.stats();
println!("Total vectors: {}", stats.total_vectors);`}
					python={`# Check if a node has a vector
has_vector = index.has(doc)

# Get a stored vector
vector = index.get(doc)

# Delete a vector
index.delete(doc)

# Build/rebuild the IVF index for faster search
index.build_index()

# Get index statistics
stats = index.stats()
print(f"Total vectors: {stats['totalVectors']}")`}
				/>

				<h2 id="index-backend">Choosing the index backend</h2>
				<p>
					By default (<code>auto</code>) the index picks its backend each time it
					builds, from the live vector count and dimensions at that point:
				</p>
				<ul>
					<li>
						<strong>Plain IVF</strong> while the collection has fewer than 50,000
						vectors or fewer than 512 dimensions. Results carry exact distances,
						and builds are fast.
					</li>
					<li>
						<strong>IVF-PQ</strong> from 512 dimensions and 50,000 vectors on.
						Searches scan compact product-quantization codes (about 2x faster at
						that size in our measurements) and re-rank the best candidates by
						exact distance. Raise <code>rerankFactor</code> for higher recall.
					</li>
				</ul>
				<p>
					The index rebuilds itself on the next search as it grows (at 4x the
					size it was built at, or when it crosses the threshold), so a growing
					collection moves from IVF to IVF-PQ on its own. Set the backend
					explicitly to keep one:
				</p>
				<MultiLangCode
					typescript={`import { AnnAlgorithm, createVectorIndex } from '@kitedb/core';

const index = createVectorIndex({
  dimensions: 1536,
  annAlgorithm: AnnAlgorithm.Ivf, // 'ivf' | 'ivf_pq' | 'auto' (default)
});

index.buildIndex();
console.log(index.stats().indexAlgorithm); // 'ivf'`}
					rust={`use kitedb::api::vector_search::{AnnAlgorithm, VectorIndex, VectorIndexOptions};

let mut index = VectorIndex::new(
    VectorIndexOptions::new(1536).with_ann_algorithm(AnnAlgorithm::Ivf),
);

index.build_index()?;
assert_eq!(index.stats().index_algorithm, Some(AnnAlgorithm::Ivf));`}
					python={`from kitedb import create_vector_index, VectorIndexOptions

index = create_vector_index(VectorIndexOptions(
    dimensions=1536,
    ann_algorithm="ivf",  # "ivf" | "ivf_pq" | "auto" (default)
))

index.build_index()
print(index.stats()["indexAlgorithm"])  # "ivf"`}
				/>

				<h2 id="next-steps">Next steps</h2>
				<ul>
					<li>
						<a href="/docs/api/vector-api">Vector API</a>: every vector index
						method and option
					</li>
					<li>
						<a href="/docs/internals/performance">Performance</a>: tuning notes
					</li>
				</ul>
			</DocPage>
		);
	}

	if (slug === "guides/transactions") {
		return (
			<DocPage slug={slug}>
				<p>
					A transaction groups writes so they commit together or not at all. The
					high-level API wraps this in <code>transaction()</code> and{" "}
					<code>batch()</code>; the low-level <code>Database</code> API exposes
					begin, commit, and rollback directly.
				</p>

				<h2 id="high-level">High-level transactions (Kite)</h2>
				<p>
					The high-level <code>Kite</code> API supports explicit transactions
					for batching multiple operations into a single commit. When the
					callback completes, the transaction commits; on error it rolls back.
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
    ctx.link(alice.id(), "follows", bob.id())?;
    Ok(())
})?;`}
					python={`from kitedb import kite

db = kite("./my.kitedb", nodes=[user], edges=[follows])

with db.transaction():
    alice = db.insert(user).values(key="alice", name="Alice").returning()
    bob = db.insert(user).values(key="bob", name="Bob").returning()
    db.link(alice, follows, bob, since=2024)`}
				/>

				<h2 id="batch">Batch operations</h2>
				<p>
					A batch runs its operations in a single transaction, so it is atomic
					and pays the commit cost once. Use it for ingestion, such as indexing
					a codebase.
				</p>
				<MultiLangCode
					typescript={`// Batch with builder/executor operations (sync)
db.batch([
  db.insert('user').values('alice', { name: 'Alice' }),
  db.insert('user').values('bob', { name: 'Bob' }),
  () => db.link(aliceId, 'follows', bobId, { since: 2024 }),
]);`}
					rust={`use kitedb::api::kite::BatchOp;
use std::collections::HashMap;

db.batch(vec![
    BatchOp::CreateNode {
        node_type: "user".into(),
        key_suffix: "alice".into(),
        props: HashMap::new(),
    },
    BatchOp::CreateNode {
        node_type: "user".into(),
        key_suffix: "bob".into(),
        props: HashMap::new(),
    },
])?;`}
					python={`db.batch([
    db.insert(user).values(key="alice", name="Alice"),
    db.insert(user).values(key="bob", name="Bob"),
])`}
				/>

				<h2 id="bulk-load">Bulk load (max throughput)</h2>
				<p>
					A bulk-load transaction bypasses MVCC (if you enabled it) to minimize
					per-write overhead. Use it for one-shot ingest or ETL jobs, and avoid
					concurrent readers and writers while it runs.
				</p>
				<MultiLangCode
					typescript={`import { Database } from '@kitedb/core';

const db = Database.open('./my.kitedb');
db.beginBulk();
const nodeIds = db.createNodesBatch(keys); // keys: Array<string | null>
db.addEdgesBatch(edges); // edges: { src, etype, dst }[]
db.addEdgesWithPropsBatch(edgesWithProps);
db.commit();`}
					rust={`use kitedb::core::single_file::{open_single_file, SingleFileOpenOptions};

let db = open_single_file("./my.kitedb", SingleFileOpenOptions::default())?;
db.begin_bulk()?;
let node_ids = db.create_nodes_batch(&keys)?; // keys: &[Option<&str>]
db.add_edges_batch(&edges)?;                  // edges: &[(NodeId, ETypeId, NodeId)]
db.add_edges_with_props_batch(edges_with_props)?;
db.commit()?;`}
					python={`from kitedb import Database

db = Database("./my.kitedb")
db.begin_bulk()
node_ids = db.create_nodes_batch(keys)  # keys: List[Optional[str]]
db.add_edges_batch(edges)               # edges: List[Tuple[int, int, int]]
db.add_edges_with_props_batch(edges_with_props)
db.commit()`}
				/>

				<h2 id="write-path">Choose a write path</h2>
				<table>
					<thead>
						<tr>
							<th>Goal</th>
							<th>Recommended API</th>
						</tr>
					</thead>
					<tbody>
						<tr>
							<td>Max throughput, single writer</td>
							<td>
								<code>beginBulk()</code> + batch APIs
							</td>
						</tr>
						<tr>
							<td>Atomic ingest (MVCC on or off)</td>
							<td>
								<code>batch()</code> / <code>transaction()</code>
							</td>
						</tr>
						<tr>
							<td>Several writer threads</td>
							<td>
								<code>mvcc: true</code> + <code>syncMode: 'Normal'</code> + group
								commit + chunked batches (without MVCC, write transactions run one
								at a time). One writer sending batches is usually faster
							</td>
						</tr>
					</tbody>
				</table>

				<h2 id="limitations">Current limitations</h2>
				<ul>
					<li>
						Traversal and path queries read the committed view. If you need to
						traverse newly written edges, commit first.
					</li>
					<li>
						JavaScript <code>batch()</code> is synchronous; avoid async work
						inside a batch (do async work first, then batch the writes).
					</li>
				</ul>

				<h2 id="basic-transactions">Basic transactions</h2>
				<MultiLangCode
					typescript={`import { Database, PropValueType } from '@kitedb/core';

const db = Database.open('./my.kitedb');

// Begin a read-write transaction
db.begin();

try {
  // All operations are part of the transaction
  const nodeId = db.createNode('user:alice');
  db.setNodePropByName(nodeId, 'name', {
    propType: PropValueType.String,
    stringValue: 'Alice',
  });

  // Commit the transaction
  db.commit();
} catch (e) {
  // Rollback on error
  db.rollback();
  throw e;
}`}
					rust={`use kitedb::core::single_file::{open_single_file, SingleFileOpenOptions};
use kitedb::types::PropValue;

let db = open_single_file("./my.kitedb", SingleFileOpenOptions::default())?;

// Begin a read-write transaction
db.begin(false)?;

// All operations are part of the transaction
let node_id = db.create_node(Some("user:alice"))?;
db.set_node_prop_by_name(node_id, "name", PropValue::String("Alice".into()))?;

// Commit the transaction
db.commit()?;

// Or rollback on error
// db.rollback()?;`}
					python={`from kitedb import Database, PropValue

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

				<h2 id="read-only">Read-only transactions</h2>
				<MultiLangCode
					typescript={`// Begin a read-only transaction
db.begin(true);

const node = db.get_node_by_key('user:alice');
const props = node !== null ? db.get_node_props(node) : null;

// Read-only transactions still need to be ended
db.commit();  // or db.rollback() - same effect for read-only`}
					rust={`// Begin a read-only transaction
db.begin(true)?;

if let Some(node) = db.node_by_key("user:alice") {
    let props = db.node_props(node);
}

// Read-only transactions still need to be ended
db.commit()?;`}
					python={`# Begin a read-only transaction
db.begin(read_only=True)

node = db.get_node_by_key("user:alice")
props = db.get_node_props(node) if node is not None else None

# Read-only transactions still need to be ended
db.commit()  # or db.rollback() - same effect for read-only`}
				/>

				<h2 id="transaction-status">Transaction status</h2>
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

				<h2 id="next-steps">Next steps</h2>
				<ul>
					<li>
						<a href="/docs/api/high-level">High-level API</a>: the full
						transaction API
					</li>
					<li>
						<a href="/docs/internals/mvcc">MVCC and transactions</a>: how
						transactions are isolated
					</li>
				</ul>
			</DocPage>
		);
	}

	if (slug === "guides/performance") {
		return (
			<DocPage slug={slug}>
				<p>
					Use this checklist to pick a write path and a durability preset for
					your workload. Most write-path gains come from three things: fewer WAL
					syncs, fewer per-operation allocations, and larger batches.
				</p>

				<h2 id="decision">Decision matrix</h2>
				<table>
					<thead>
						<tr>
							<th>Goal</th>
							<th>Best path</th>
						</tr>
					</thead>
					<tbody>
						<tr>
							<td>Max ingest throughput, single writer</td>
							<td>
								<code>beginBulk()</code> + batch APIs
							</td>
						</tr>
						<tr>
							<td>Atomic ingest (MVCC on or off)</td>
							<td>
								<code>transaction()</code> / <code>batch()</code>
							</td>
						</tr>
						<tr>
							<td>Several writer threads</td>
							<td>
								<code>mvcc: true</code> + <code>syncMode: 'Normal'</code> + group
								commit
							</td>
						</tr>
						<tr>
							<td>Strong durability per commit</td>
							<td>
								<code>syncMode: 'Full'</code>
							</td>
						</tr>
						<tr>
							<td>Throwaway or test data</td>
							<td>
								<code>syncMode: 'Off'</code>
							</td>
						</tr>
					</tbody>
				</table>

				<h2 id="bulk">Bulk ingest (fastest path)</h2>
				<p>
					A bulk-load transaction bypasses MVCC (if you enabled it) to minimize
					overhead. Use it for one-shot ingest or ETL jobs, and avoid concurrent
					readers and writers while it runs.
				</p>
				<MultiLangCode
					typescript={`import { Database } from '@kitedb/core';

const db = Database.open('./my.kitedb');
db.beginBulk();
const nodeIds = db.createNodesBatch(keys);
db.addEdgesBatch(edges);
db.addEdgesWithPropsBatch(edgesWithProps);
db.commit();`}
					rust={`use kitedb::core::single_file::{open_single_file, SingleFileOpenOptions};

let db = open_single_file("./my.kitedb", SingleFileOpenOptions::default())?;
db.begin_bulk()?;
let node_ids = db.create_nodes_batch(&keys)?;
db.add_edges_batch(&edges)?;
db.add_edges_with_props_batch(edges_with_props)?;
db.commit()?;`}
					python={`from kitedb import Database

db = Database("./my.kitedb")
db.begin_bulk()
node_ids = db.create_nodes_batch(keys)
db.add_edges_batch(edges)
db.add_edges_with_props_batch(edges_with_props)
db.commit()`}
				/>

				<h2 id="presets">Config presets</h2>
				<table>
					<thead>
						<tr>
							<th>Preset</th>
							<th>Settings</th>
						</tr>
					</thead>
					<tbody>
						<tr>
							<td>Single-writer ingest</td>
							<td>
								<code>syncMode: 'Normal'</code>,{" "}
								<code>groupCommitEnabled: false</code>, WAL ≥ 256MB,{" "}
								<code>autoCheckpoint: false</code>
							</td>
						</tr>
						<tr>
							<td>Several writer threads</td>
							<td>
								<code>mvcc: true</code>, <code>syncMode: 'Normal'</code>,{" "}
								<code>groupCommitEnabled: true</code>, chunked batches
							</td>
						</tr>
						<tr>
							<td>Max durability</td>
							<td>
								<code>syncMode: 'Full'</code>, smaller batches
							</td>
						</tr>
						<tr>
							<td>Max speed (test)</td>
							<td>
								<code>syncMode: 'Off'</code>
							</td>
						</tr>
					</tbody>
				</table>

				<h2 id="checklist">Checklist</h2>
				<ul>
					<li>
						Use batch APIs: <code>createNodesBatch</code>,{" "}
						<code>addEdgesBatch</code>, <code>addEdgesWithPropsBatch</code>
					</li>
					<li>
						Prefer <code>beginBulk()</code> for ingest; commit in chunks
					</li>
					<li>Increase WAL size for large ingest (256MB+)</li>
					<li>
						Disable auto-checkpoint during ingest; checkpoint once at the end
					</li>
					<li>Use low-level API for hot paths in JS/TS</li>
					<li>
						Avoid per-edge property sets when you can batch props with the edge
					</li>
				</ul>

				<h2 id="verify">Verify with benchmarks</h2>
				<ul>
					<li>
						<a href="/docs/benchmarks/graph">Graph benchmarks</a>: baselines to
						compare your numbers against
					</li>
					<li>
						<a href="/docs/internals/performance">Performance</a>: deeper tuning
						notes
					</li>
				</ul>
			</DocPage>
		);
	}

	if (slug === "guides/concurrency") {
		return (
			<DocPage slug={slug}>
				<p>
					Within one process, many threads can read a KiteDB database at the
					same time. Without MVCC (the default), one write transaction is open
					at a time: a second writer waits in <code>begin</code> until the first
					commits or rolls back. With the <code>mvcc</code> open option, write
					transactions run concurrently and conflicts are detected at commit.
				</p>

				<h2 id="concurrency-model">Concurrency model</h2>
				<p>
					KiteDB uses a <strong>readers-writer lock</strong> pattern:
				</p>
				<ul>
					<li>
						<strong>Multiple concurrent readers</strong> – Any number of threads
						can read simultaneously
					</li>
					<li>
						<strong>One writer at a time (default)</strong> – A write
						transaction waits for the open one to finish, so concurrent
						read-modify-write transactions cannot lose updates. Read-only
						transactions never wait for it
					</li>
					<li>
						<strong>MVCC (opt-in)</strong> – With the <code>mvcc</code> open
						option, transactions read from consistent snapshots, and write
						transactions run concurrently; a commit that conflicts with one
						committed since its transaction began fails with a conflict error, so
						retry it
					</li>
				</ul>

				<MultiLangCode
					typescript={`// Reads are synchronous. Calls on one handle run one after
// another on the JS thread, so there is nothing to await.
const alice = db.get(user, 'alice');
const bob = db.get(user, 'bob');
const following = alice ? db.from(alice).out('follows').toArray() : [];

// For parallel reads, open the file in each worker thread with
// readOnly: true. Read-only handles share the file lock; a writable
// handle holds it exclusively.
const reader = await kite('./data.kitedb', {
  nodes: [user],
  edges: [follows],
  readOnly: true,
});`}
					rust={`use std::sync::{Arc, RwLock};
use std::thread;
use kitedb::api::kite::Kite;

let db = Arc::new(RwLock::new(Kite::open("./data.kitedb", options)?));

let handles: Vec<_> = (0..4).map(|i| {
    let db = Arc::clone(&db);
    thread::spawn(move || {
        // Multiple threads can acquire read locks simultaneously
        let key = format!("user{}", i);
        let guard = db.read().unwrap();
        guard.get("user", &key).ok().flatten()
    })
}).collect();

// Collect results
let results: Vec<_> = handles.into_iter()
    .map(|h| h.join().unwrap())
    .collect();`}
					python={`import threading
from kitedb import kite

db = kite("./data.kitedb", nodes=[user], edges=[])
results = {}

def read_user(user_id: str):
    # Threads can share one handle safely
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

# The binding holds the GIL during each call,
# so these reads run one at a time
print(results)`}
				/>

				<h2 id="performance">Performance notes</h2>
				<p>
					Read throughput typically improves with parallel readers. Without
					MVCC, writer threads take turns for whole transactions, so more writer
					threads add no write throughput. With MVCC, writers build their
					transactions in parallel and only the commits are applied one at a
					time, but its version bookkeeping costs throughput too: for several
					writer threads, use <code>mvcc: true</code> with{" "}
					<code>syncMode: 'Normal'</code> and group commit; for the most write
					throughput, prepare data in parallel and send it through one writer in
					batched transactions. Measure with your workload and tune batch sizes
					and sync mode accordingly.
				</p>

				<h2 id="best-practices">Best practices</h2>
				<ul>
					<li>
						<strong>Batch writes</strong> – Group multiple writes into single
						operations to minimize exclusive lock time
					</li>
					<li>
						<strong>Use transactions for atomicity</strong> – Group related
						writes; with MVCC enabled, readers also see consistent snapshots
						while those writes commit
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

				<h2 id="mvcc">MVCC and transaction semantics</h2>
				<p>
					MVCC (multi-version concurrency control) is off by default. Turn it on
					with the <code>mvcc</code> open option when readers need snapshot
					isolation or several threads write at once:
				</p>
				<ul>
					<li>Multiple readers can run concurrently</li>
					<li>
						Without MVCC, write transactions run one at a time: a write{" "}
						<code>begin</code> waits until no other write transaction is open
						(in Python it releases the GIL while it waits)
					</li>
					<li>
						With MVCC, write transactions run concurrently, and write conflicts
						are detected at commit time
					</li>
					<li>
						Commits are applied one at a time; a commit briefly blocks new reads
						while it publishes its changes
					</li>
					<li>Each committed transaction is atomic</li>
				</ul>

				<MultiLangCode
					typescript={`// Atomic transaction (auto-commit on success, rollback on error)
await db.transaction(async (ctx) => {
  const alice = ctx.get(user, 'alice');
  if (alice) {
    const update = ctx.update(user, 'alice');
    update.set('name', 'Alice Updated');
    update.execute();
  }
});`}
					rust={`use kitedb::types::PropValue;

// Atomic transaction with TxContext
db.transaction(|ctx| {
    let alice = ctx.get("user", "alice")?;
    if let Some(node) = alice {
        ctx.set_prop(node.id(), "name", PropValue::String("Alice Updated".into()))?;
    }
    Ok(())
})?;`}
					python={`# Atomic transaction (context manager handles commit/rollback)
with db.transaction():
    alice = db.get(user, "alice")
    if alice is not None:
        db.update(alice).set(name="Alice Updated").execute()`}
				/>

				<h2 id="limitations">Limitations</h2>
				<ul>
					<li>
						<strong>Single-process only</strong> – Concurrent access is within a
						single process; multi-process access requires external coordination
					</li>
					<li>
						<strong>Write serialization</strong> – Without MVCC, write
						transactions run one at a time; with MVCC, commits are applied one
						at a time. High-write workloads may see contention
					</li>
					<li>
						<strong>Memory overhead</strong> – With MVCC enabled, version
						history uses additional memory
					</li>
				</ul>

				<h2 id="next-steps">Next steps</h2>
				<ul>
					<li>
						<a href="/docs/guides/transactions">Transactions</a>: atomic writes,
						batches, and bulk load
					</li>
					<li>
						<a href="/docs/benchmarks">Benchmarks</a>: measured read and write
						latencies
					</li>
					<li>
						<a href="/docs/internals/architecture">Architecture</a>: how the
						storage engine is put together
					</li>
				</ul>
			</DocPage>
		);
	}

	// Default fallback
	return (
		<DocPage slug={slug}>
			<p>This guide is coming soon.</p>
		</DocPage>
	);
}
