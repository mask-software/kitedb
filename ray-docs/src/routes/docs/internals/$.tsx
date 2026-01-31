import { createFileRoute, useLocation } from '@tanstack/solid-router'
import { Show } from 'solid-js'
import DocPage from '~/components/doc-page'
import CodeBlock from '~/components/code-block'
import { findDocBySlug } from '~/lib/docs'

export const Route = createFileRoute('/docs/internals/$')({
  component: InternalsSplatPage,
})

function InternalsSplatPage() {
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
          The internals page <code class="px-2 py-1 bg-slate-100 dark:bg-slate-800 rounded">{props.slug}</code> doesn't exist yet.
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

  if (slug === 'internals/architecture') {
    return (
      <DocPage slug={slug}>
        <p>
          Learn about KiteDB's internal architecture and design decisions.
        </p>

        <h2 id="overview">Overview</h2>
        <p>
          KiteDB is built on a layered architecture optimized for graph workloads:
        </p>
        <ul>
          <li><strong>Query Layer</strong> – Fluent API and query planning</li>
          <li><strong>Graph Layer</strong> – Node/edge management and traversal</li>
          <li><strong>Vector Layer</strong> – HNSW index and similarity search</li>
          <li><strong>Storage Layer</strong> – LSM-tree based persistence</li>
        </ul>

        <h2 id="storage-format">Storage Format</h2>
        <p>
          Nodes and edges are stored using a key-value model with structured keys:
        </p>
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
          <li><a href="/docs/internals/csr">CSR Format</a> – Edge storage details</li>
          <li><a href="/docs/internals/performance">Performance</a> – Optimization techniques</li>
        </ul>
      </DocPage>
    )
  }

  if (slug === 'internals/csr') {
    return (
      <DocPage slug={slug}>
        <p>
          KiteDB uses a Compressed Sparse Row (CSR) inspired format for 
          efficient edge storage and traversal.
        </p>

        <h2 id="why-csr">Why CSR?</h2>
        <p>
          CSR provides O(1) access to a node's outgoing edges and excellent 
          cache locality for traversals. It's the standard format for sparse 
          graph algorithms.
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
        <p>
          With this structure, finding all outgoing edges is a simple prefix scan:
        </p>
        <CodeBlock
          code={`// Find everyone Alice follows
storage.iterator({
  gte: 'e:follows:user:alice:',
  lt: 'e:follows:user:alice:\\xff',
})`}
          language="typescript"
        />
      </DocPage>
    )
  }

  if (slug === 'internals/performance') {
    return (
      <DocPage slug={slug}>
        <p>
          Tips and techniques for getting the best performance from KiteDB.
        </p>

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
    )
  }

  // Default fallback
  return (
    <DocPage slug={slug}>
      <p>This internals documentation is coming soon.</p>
    </DocPage>
  )
}
