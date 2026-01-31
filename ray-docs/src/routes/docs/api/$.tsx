import { createFileRoute, useLocation } from '@tanstack/solid-router'
import { Show } from 'solid-js'
import DocPage from '~/components/doc-page'
import CodeBlock from '~/components/code-block'
import { findDocBySlug } from '~/lib/docs'

export const Route = createFileRoute('/docs/api/$')({
  component: ApiSplatPage,
})

function ApiSplatPage() {
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
          The API reference <code class="px-2 py-1 bg-slate-100 dark:bg-slate-800 rounded">{props.slug}</code> doesn't exist yet.
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

  if (slug === 'api/high-level') {
    return (
      <DocPage slug={slug}>
        <p>
          The high-level API provides a Drizzle-style fluent interface for 
          working with your graph database.
        </p>

        <h2 id="ray-function">ray()</h2>
        <p>Initialize the database connection.</p>
        <CodeBlock
          code={`import { ray } from 'kitedb';

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
          <li><a href="/docs/api/low-level">Low-Level API</a> – Direct storage access</li>
          <li><a href="/docs/api/vector-api">Vector API</a> – Similarity search</li>
        </ul>
      </DocPage>
    )
  }

  if (slug === 'api/low-level') {
    return (
      <DocPage slug={slug}>
        <p>
          The low-level API provides direct access to the underlying storage 
          engine for advanced use cases.
        </p>

        <h2 id="storage-access">Storage Access</h2>
        <CodeBlock
          code={`import { ray } from 'kitedb';

const db = await ray('./data.kitedb', { nodes, edges });

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
    )
  }

  if (slug === 'api/vector-api') {
    return (
      <DocPage slug={slug}>
        <p>
          Complete reference for KiteDB's vector search capabilities.
        </p>

        <h2 id="vector-property">Defining Vector Properties</h2>
        <CodeBlock
          code={`import { prop } from 'kitedb';

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
    )
  }

  // Default fallback
  return (
    <DocPage slug={slug}>
      <p>This API reference is coming soon.</p>
    </DocPage>
  )
}
