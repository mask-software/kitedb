import DocPage from "../../../components/DocPage";
import CodeBlock from "../../../components/CodeBlock";
import { findDocBySlug } from "../../../lib/docs";

interface DocPageProps {
  params: Promise<{ slug?: string[] }>;
}

export default async function DocPageRoute({ params }: DocPageProps) {
  const resolvedParams = await params;
  const slug = resolvedParams.slug?.join("/") ?? "";
  const doc = findDocBySlug(slug);

  if (!doc) {
    return <DocNotFound slug={slug} />;
  }

  return <DocPageContent slug={slug} />;
}

function DocNotFound({ slug }: { slug: string }) {
  return (
    <div className="max-w-4xl mx-auto px-6 py-12">
      <div className="text-center">
        <h1 className="text-4xl font-extrabold text-slate-900 dark:text-white mb-4">
          Page Not Found
        </h1>
        <p className="text-lg text-slate-600 dark:text-slate-400 mb-8">
          The documentation page{" "}
          <code className="px-2 py-1 bg-slate-100 dark:bg-slate-800 rounded">{slug}</code>
          {" "}does not exist yet.
        </p>
        <a
          href="/docs"
          className="inline-flex items-center gap-2 px-6 py-3 bg-gradient-to-r from-cyan-500 to-violet-500 text-white font-semibold rounded-xl hover:shadow-lg hover:shadow-cyan-500/25 transition-all duration-200"
        >
          Back to Documentation
        </a>
      </div>
    </div>
  );
}

function DocPageContent({ slug }: { slug: string }) {
  if (slug === "benchmarks") {
    return (
      <DocPage slug={slug}>
        <p>Performance benchmarks comparing RayDB to other graph databases.</p>

        <h2 id="test-environment">Test Environment</h2>
        <ul>
          <li>MacBook Pro M3 Max, 64GB RAM</li>
          <li>Bun 1.1.0</li>
          <li>RayDB 0.1.0</li>
        </ul>

        <h2 id="insertion">Node Insertion</h2>
        <CodeBlock
          code={`Inserting 1M nodes:
RayDB:     1.2s (833k ops/sec)
Neo4j:     8.4s (119k ops/sec)
ArangoDB:  5.1s (196k ops/sec)`}
          language="text"
        />

        <h2 id="traversal">Graph Traversal</h2>
        <CodeBlock
          code={`3-hop traversal on 1M node graph:
RayDB:     12ms
Neo4j:     45ms
ArangoDB:  38ms`}
          language="text"
        />

        <h2 id="vector-search">Vector Search</h2>
        <CodeBlock
          code={`Top-10 similarity on 100k vectors (1536 dims):
RayDB (HNSW):  0.8ms
pgvector:      2.1ms
Pinecone:      1.5ms`}
          language="text"
        />
      </DocPage>
    );
  }

  if (slug === "getting-started/quick-start") {
    return (
      <DocPage slug={slug}>
        <p>
          Lets build a simple social graph database with users and their connections. By the end of this guide,
          you will understand the core concepts of RayDB.
        </p>

        <h2 id="create-schema">1. Define Your Schema</h2>
        <p>
          RayDB uses a type-safe schema to define nodes and edges. Lets create a simple social network with users and
          follow relationships.
        </p>
        <CodeBlock
          code={`import { ray, defineNode, defineEdge, prop } from '@ray-db/ray';

// Define a user node
const user = defineNode('user', {
  key: (id: string) => \`user:\${id}\`,
  props: {
    name: prop.string('name'),
    email: prop.string('email'),
    createdAt: prop.date('created_at'),
  },
});

// Define a follow relationship
const follows = defineEdge('follows', {
  from: user,
  to: user,
  props: {
    followedAt: prop.date('followed_at'),
  },
});`}
          language="typescript"
          filename="schema.ts"
        />

        <h2 id="initialize">2. Initialize the Database</h2>
        <CodeBlock
          code={`const db = await ray('./my-app.raydb', {
  nodes: [user],
  edges: [follows],
});

console.log('Database initialized!');`}
          language="typescript"
        />

        <h2 id="add-data">3. Add Some Data</h2>
        <CodeBlock
          code={`// Create users
const alice = await db.node(user).create({
  id: 'alice',
  name: 'Alice Chen',
  email: 'alice@example.com',
  createdAt: new Date(),
});

const bob = await db.node(user).create({
  id: 'bob',
  name: 'Bob Smith',
  email: 'bob@example.com',
  createdAt: new Date(),
});

// Create a follow relationship
await db.edge(follows).create({
  from: alice,
  to: bob,
  followedAt: new Date(),
});`}
          language="typescript"
        />

        <h2 id="query">4. Query the Graph</h2>
        <CodeBlock
          code={`// Find all users Alice follows
const following = await db
  .node(user)
  .traverse(follows)
  .where({ from: alice })
  .all();

console.log('Alice follows:', following.map((u) => u.name));

// Find who follows Bob
const followers = await db
  .node(user)
  .traverse(follows)
  .where({ to: bob })
  .all();

console.log('Bob has followers:', followers.length);`}
          language="typescript"
        />

        <h2 id="cleanup">5. Close the Database</h2>
        <CodeBlock
          code={`// Always close when done
await db.close();`}
          language="typescript"
        />

        <h2 id="next-steps">Next Steps</h2>
        <p>Congratulations! You have built your first graph database with RayDB. Continue learning with these guides:</p>
        <ul>
          <li>
            <a href="/docs/guides/schema">Schema Definition</a> - Advanced schema patterns
          </li>
          <li>
            <a href="/docs/guides/queries">Queries & CRUD</a> - All query operations
          </li>
          <li>
            <a href="/docs/guides/vectors">Vector Search</a> - Semantic similarity
          </li>
        </ul>
      </DocPage>
    );
  }

  if (slug === "guides/schema") {
    return (
      <DocPage slug={slug}>
        <p>
          RayDB schemas provide type-safe definitions for your graph data. This guide covers all the ways to define
          nodes, edges, and properties.
        </p>

        <h2 id="defining-nodes">Defining Nodes</h2>
        <p>
          Nodes are the vertices in your graph. Each node type needs a unique name and a key function that generates
          unique identifiers.
        </p>
        <CodeBlock
          code={`import { defineNode, prop } from '@ray-db/ray';

const article = defineNode('article', {
  // Key function receives your input and returns a unique key
  key: (id: string) => \`article:\${id}\`,
  
  // Properties with their types
  props: {
    title: prop.string('title'),
    content: prop.text('content'),
    published: prop.boolean('published'),
    views: prop.integer('views'),
    rating: prop.float('rating'),
    tags: prop.array('tags', prop.string()),
    metadata: prop.json('metadata'),
    createdAt: prop.date('created_at'),
  },
});`}
          language="typescript"
          filename="nodes.ts"
        />

        <h2 id="property-types">Property Types</h2>
        <p>RayDB supports the following property types:</p>
        <ul>
          <li>
            <code>prop.string()</code> - Text strings
          </li>
          <li>
            <code>prop.text()</code> - Long text content
          </li>
          <li>
            <code>prop.integer()</code> - Whole numbers
          </li>
          <li>
            <code>prop.float()</code> - Decimal numbers
          </li>
          <li>
            <code>prop.boolean()</code> - True/false values
          </li>
          <li>
            <code>prop.date()</code> - Date/time values
          </li>
          <li>
            <code>prop.array()</code> - Arrays of any type
          </li>
          <li>
            <code>prop.json()</code> - Arbitrary JSON data
          </li>
          <li>
            <code>prop.vector()</code> - Float32 embedding vectors
          </li>
        </ul>

        <h2 id="defining-edges">Defining Edges</h2>
        <p>Edges connect nodes and can have their own properties.</p>
        <CodeBlock
          code={`import { defineEdge, prop } from '@ray-db/ray';

const authored = defineEdge('authored', {
  from: user,
  to: article,
  props: {
    role: prop.string('role'), // 'author' | 'contributor'
  },
});

const likes = defineEdge('likes', {
  from: user,
  to: article,
  props: {
    likedAt: prop.date('liked_at'),
  },
});`}
          language="typescript"
          filename="edges.ts"
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li>
            <a href="/docs/guides/queries">Queries & CRUD</a> - Perform operations on your schema
          </li>
          <li>
            <a href="/docs/guides/vectors">Vector Search</a> - Add embedding vectors
          </li>
        </ul>
      </DocPage>
    );
  }

  if (slug === "guides/queries") {
    return (
      <DocPage slug={slug}>
        <p>Learn how to create, read, update, and delete data in RayDB with the high-level fluent API.</p>

        <h2 id="create">Creating Nodes</h2>
        <CodeBlock
          code={`// Create a single node
const alice = await db.node(user).create({
  id: 'alice',
  name: 'Alice Chen',
  email: 'alice@example.com',
});

// Create multiple nodes
const users = await db.node(user).createMany([
  { id: 'bob', name: 'Bob', email: 'bob@example.com' },
  { id: 'carol', name: 'Carol', email: 'carol@example.com' },
]);`}
          language="typescript"
        />

        <h2 id="read">Reading Data</h2>
        <CodeBlock
          code={`// Get by key
const user = await db.node(user).get('user:alice');

// Find with conditions
const activeUsers = await db.node(user)
  .where({ status: 'active' })
  .all();

// Get first match
const admin = await db.node(user)
  .where({ role: 'admin' })
  .first();

// Count matches
const count = await db.node(user)
  .where({ verified: true })
  .count();`}
          language="typescript"
        />

        <h2 id="update">Updating Data</h2>
        <CodeBlock
          code={`// Update by key
await db.node(user)
  .update('user:alice', { name: 'Alice C.' });

// Update with conditions
await db.node(user)
  .where({ status: 'pending' })
  .update({ status: 'active' });`}
          language="typescript"
        />

        <h2 id="delete">Deleting Data</h2>
        <CodeBlock
          code={`// Delete by key
await db.node(user).delete('user:alice');

// Delete with conditions
await db.node(user)
  .where({ deleted: true })
  .delete();`}
          language="typescript"
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li>
            <a href="/docs/guides/traversal">Graph Traversal</a> - Navigate relationships
          </li>
          <li>
            <a href="/docs/guides/transactions">Transactions</a> - ACID guarantees
          </li>
        </ul>
      </DocPage>
    );
  }

  if (slug === "guides/traversal") {
    return (
      <DocPage slug={slug}>
        <p>RayDB provides powerful graph traversal capabilities to navigate relationships between nodes.</p>

        <h2 id="basic-traversal">Basic Traversal</h2>
        <CodeBlock
          code={`// Find all users that Alice follows
const following = await db
  .node(user)
  .traverse(follows)
  .from('user:alice')
  .all();

// Find all followers of Bob
const followers = await db
  .node(user)
  .traverse(follows)
  .to('user:bob')
  .all();`}
          language="typescript"
        />

        <h2 id="multi-hop">Multi-Hop Traversal</h2>
        <CodeBlock
          code={`// Find friends of friends (2-hop)
const friendsOfFriends = await db
  .node(user)
  .traverse(follows)
  .from('user:alice')
  .traverse(follows)
  .all();

// With depth limit
const network = await db
  .node(user)
  .traverse(follows)
  .from('user:alice')
  .depth({ min: 1, max: 3 })
  .all();`}
          language="typescript"
        />

        <h2 id="filtering">Filtering During Traversal</h2>
        <CodeBlock
          code={`// Find active users that Alice follows
const activeFollowing = await db
  .node(user)
  .traverse(follows)
  .from('user:alice')
  .where({ status: 'active' })
  .all();`}
          language="typescript"
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li>
            <a href="/docs/guides/vectors">Vector Search</a> - Combine with semantic search
          </li>
          <li>
            <a href="/docs/api/high-level">API Reference</a> - Full traversal API
          </li>
        </ul>
      </DocPage>
    );
  }

  if (slug === "guides/vectors") {
    return (
      <DocPage slug={slug}>
        <p>
          RayDB includes built-in vector search for semantic similarity queries. Store embeddings alongside your graph
          data and find similar nodes.
        </p>

        <h2 id="adding-vectors">Adding Vector Properties</h2>
        <CodeBlock
          code={`import { defineNode, prop } from '@ray-db/ray';

const document = defineNode('document', {
  key: (id: string) => \`doc:\${id}\`,
  props: {
    title: prop.string('title'),
    content: prop.text('content'),
    // 1536-dimensional embedding (OpenAI ada-002)
    embedding: prop.vector('embedding', 1536),
  },
});`}
          language="typescript"
        />

        <h2 id="storing-embeddings">Storing Embeddings</h2>
        <CodeBlock
          code={`// Generate embedding with your preferred provider
const embedding = await openai.embeddings.create({
  model: 'text-embedding-ada-002',
  input: 'Your document content here',
});

// Store in RayDB
await db.node(document).create({
  id: 'doc-1',
  title: 'My Document',
  content: 'Your document content here',
  embedding: embedding.data[0].embedding,
});`}
          language="typescript"
        />

        <h2 id="similarity-search">Similarity Search</h2>
        <CodeBlock
          code={`// Find similar documents
const queryEmbedding = await getEmbedding('search query');

const similar = await db
  .node(document)
  .vector('embedding')
  .similar(queryEmbedding, { limit: 10 })
  .all();

// Returns nodes with similarity scores
similar.forEach(({ node, score }) => {
  console.log(\`\${node.title}: \${score.toFixed(3)}\`);
});`}
          language="typescript"
        />

        <h2 id="hybrid-search">Hybrid Search</h2>
        <CodeBlock
          code={`// Combine vector search with graph traversal
const results = await db
  .node(document)
  .vector('embedding')
  .similar(queryEmbedding)
  .traverse(authored)  // Find authors of similar docs
  .all();`}
          language="typescript"
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li>
            <a href="/docs/api/vector-api">Vector API Reference</a> - Full vector API
          </li>
          <li>
            <a href="/docs/internals/performance">Performance</a> - Optimization tips
          </li>
        </ul>
      </DocPage>
    );
  }

  if (slug === "guides/transactions") {
    return (
      <DocPage slug={slug}>
        <p>RayDB supports ACID transactions to ensure data consistency.</p>

        <h2 id="basic-transactions">Basic Transactions</h2>
        <CodeBlock
          code={`await db.transaction(async (tx) => {
  // All operations in this block are atomic
  const alice = await tx.node(user).create({
    id: 'alice',
    name: 'Alice',
  });
  
  const bob = await tx.node(user).create({
    id: 'bob', 
    name: 'Bob',
  });
  
  await tx.edge(follows).create({
    from: alice,
    to: bob,
  });
  
  // If any operation fails, all changes are rolled back
});`}
          language="typescript"
        />

        <h2 id="isolation">Isolation Levels</h2>
        <CodeBlock
          code={`// Read committed (default)
await db.transaction(async (tx) => {
  // ...
}, { isolation: 'read-committed' });

// Serializable for strict consistency
await db.transaction(async (tx) => {
  // ...
}, { isolation: 'serializable' });`}
          language="typescript"
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li>
            <a href="/docs/api/high-level">API Reference</a> - Full transaction API
          </li>
          <li>
            <a href="/docs/internals/architecture">Architecture</a> - How transactions work
          </li>
        </ul>
      </DocPage>
    );
  }

  if (slug === "internals/architecture") {
    return (
      <DocPage slug={slug}>
        <p>Learn about RayDBs internal architecture and design decisions.</p>

        <h2 id="overview">Overview</h2>
        <p>RayDB is built on a layered architecture optimized for graph workloads:</p>
        <ul>
          <li>
            <strong>Query Layer</strong> - Fluent API and query planning
          </li>
          <li>
            <strong>Graph Layer</strong> - Node/edge management and traversal
          </li>
          <li>
            <strong>Vector Layer</strong> - HNSW index and similarity search
          </li>
          <li>
            <strong>Storage Layer</strong> - LSM-tree based persistence
          </li>
        </ul>

        <h2 id="storage-format">Storage Format</h2>
        <p>Nodes and edges are stored using a key-value model with structured keys:</p>
        <CodeBlock
          code={`// Node key format
n:{type}:{id}

// Edge key format (CSR-style)
e:{type}:{from}:{to}

// Reverse edge index
r:{type}:{to}:{from}`}
          language="text"
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li>
            <a href="/docs/internals/csr">CSR Format</a> - Edge storage details
          </li>
          <li>
            <a href="/docs/internals/performance">Performance</a> - Optimization techniques
          </li>
        </ul>
      </DocPage>
    );
  }

  if (slug === "internals/csr") {
    return (
      <DocPage slug={slug}>
        <p>RayDB uses a Compressed Sparse Row (CSR) inspired format for efficient edge storage and traversal.</p>

        <h2 id="why-csr">Why CSR?</h2>
        <p>
          CSR provides O(1) access to a nodes outgoing edges and excellent cache locality for traversals. It is the
          standard format for sparse graph algorithms.
        </p>

        <h2 id="key-structure">Key Structure</h2>
        <CodeBlock
          code={`// Forward edges (outgoing)
// Key: e:{edgeType}:{fromNode}:{toNode}
// Enables: "Find all nodes that X follows"
e:follows:user:alice:user:bob
e:follows:user:alice:user:carol

// Reverse edges (incoming)
// Key: r:{edgeType}:{toNode}:{fromNode}
// Enables: "Find all nodes that follow X"
r:follows:user:bob:user:alice
r:follows:user:carol:user:alice`}
          language="text"
        />

        <h2 id="traversal-efficiency">Traversal Efficiency</h2>
        <p>With this structure, finding all outgoing edges is a simple prefix scan:</p>
        <CodeBlock
          code={`// Find everyone Alice follows
storage.iterator({
  gte: 'e:follows:user:alice:',
  lt: 'e:follows:user:alice:\\xff',
})`}
          language="typescript"
        />
      </DocPage>
    );
  }

  if (slug === "internals/performance") {
    return (
      <DocPage slug={slug}>
        <p>Tips and techniques for getting the best performance from RayDB.</p>

        <h2 id="batch-operations">Batch Operations</h2>
        <p>Always batch writes when inserting multiple nodes:</p>
        <CodeBlock
          code={`// Slow: Individual inserts
for (const user of users) {
  await db.node(userSchema).create(user);
}

// Fast: Batch insert
await db.node(userSchema).createMany(users);`}
          language="typescript"
        />

        <h2 id="vector-indexing">Vector Indexing</h2>
        <p>Build HNSW indexes for large vector datasets:</p>
        <CodeBlock
          code={`// For datasets > 10k vectors
await db.node(document)
  .vector('embedding')
  .buildIndex({ type: 'hnsw' });`}
          language="typescript"
        />

        <h2 id="traversal-limits">Traversal Limits</h2>
        <p>Always set depth limits on multi-hop traversals:</p>
        <CodeBlock
          code={`// Potentially slow
await db.node(user).traverse(follows).all();

// Bounded traversal
await db.node(user)
  .traverse(follows)
  .depth({ max: 3 })
  .limit(100)
  .all();`}
          language="typescript"
        />
      </DocPage>
    );
  }

  if (slug === "api/high-level") {
    return (
      <DocPage slug={slug}>
        <p>The high-level API provides a Drizzle-style fluent interface for working with your graph database.</p>

        <h2 id="ray-function">ray()</h2>
        <p>Initialize the database connection.</p>
        <CodeBlock
          code={`import { ray } from '@ray-db/ray';

const db = await ray(path, options);`}
          language="typescript"
        />

        <h2 id="node-methods">Node Methods</h2>
        <CodeBlock
          code={`db.node(schema)
  .create(data)           // Create a node
  .createMany(data[])     // Create multiple nodes
  .get(key)               // Get by key
  .where(conditions)      // Filter nodes
  .first()                // Get first match
  .all()                  // Get all matches
  .count()                // Count matches
  .update(key, data)      // Update by key
  .delete(key)            // Delete by key`}
          language="typescript"
        />

        <h2 id="edge-methods">Edge Methods</h2>
        <CodeBlock
          code={`db.edge(schema)
  .create({ from, to, ...props })  // Create edge
  .get(from, to)                   // Get specific edge
  .from(key)                       // Outgoing edges
  .to(key)                         // Incoming edges
  .delete(from, to)                // Delete edge`}
          language="typescript"
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li>
            <a href="/docs/api/low-level">Low-Level API</a> - Direct storage access
          </li>
          <li>
            <a href="/docs/api/vector-api">Vector API</a> - Similarity search
          </li>
        </ul>
      </DocPage>
    );
  }

  if (slug === "api/low-level") {
    return (
      <DocPage slug={slug}>
        <p>The low-level API provides direct access to the underlying storage engine for advanced use cases.</p>

        <h2 id="storage-access">Storage Access</h2>
        <CodeBlock
          code={`import { ray } from '@ray-db/ray';

const db = await ray('./data.raydb', { nodes, edges });

// Access the underlying storage
const storage = db.storage;

// Direct key-value operations
await storage.put('custom:key', value);
const data = await storage.get('custom:key');
await storage.delete('custom:key');`}
          language="typescript"
        />

        <h2 id="batch-operations">Batch Operations</h2>
        <CodeBlock
          code={`// Efficient batch writes
await storage.batch([
  { type: 'put', key: 'key1', value: value1 },
  { type: 'put', key: 'key2', value: value2 },
  { type: 'delete', key: 'key3' },
]);`}
          language="typescript"
        />

        <h2 id="iterators">Iterators</h2>
        <CodeBlock
          code={`// Iterate over key range
for await (const { key, value } of storage.iterator({
  gte: 'user:',
  lt: 'user:\\xff',
})) {
  console.log(key, value);
}`}
          language="typescript"
        />
      </DocPage>
    );
  }

  if (slug === "api/vector-api") {
    return (
      <DocPage slug={slug}>
        <p>Complete reference for RayDBs vector search capabilities.</p>

        <h2 id="vector-property">Defining Vector Properties</h2>
        <CodeBlock
          code={`import { prop } from '@ray-db/ray';

// Define with dimensions
embedding: prop.vector('embedding', 1536)

// With custom distance metric
embedding: prop.vector('embedding', 1536, {
  metric: 'cosine' | 'euclidean' | 'dot'
})`}
          language="typescript"
        />

        <h2 id="similarity-methods">Similarity Search Methods</h2>
        <CodeBlock
          code={`db.node(schema)
  .vector('embedding')
  .similar(queryVector, options)
  .all()

// Options
{
  limit: 10,           // Max results
  threshold: 0.8,      // Min similarity score
  includeScore: true,  // Include scores in results
}`}
          language="typescript"
        />

        <h2 id="indexing">Vector Indexing</h2>
        <CodeBlock
          code={`// Build HNSW index for faster search
await db.node(schema)
  .vector('embedding')
  .buildIndex({
    type: 'hnsw',
    m: 16,
    efConstruction: 200,
  });`}
          language="typescript"
        />
      </DocPage>
    );
  }

  return (
    <DocPage slug={slug}>
      <p>This page is coming soon.</p>
    </DocPage>
  );
}
