import { createFileRoute, useLocation } from '@tanstack/solid-router'
import { Show } from 'solid-js'
import DocPage from '~/components/doc-page'
import CodeBlock from '~/components/code-block'
import { findDocBySlug } from '~/lib/docs'

export const Route = createFileRoute('/docs/$')({
  component: DocSplatPage,
})

function DocSplatPage() {
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
          The documentation page <code class="px-2 py-1 bg-slate-100 dark:bg-slate-800 rounded">{props.slug}</code> doesn't exist yet.
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

  // Benchmarks page (root level)
  if (slug === 'benchmarks') {
    return (
      <DocPage slug={slug}>
        <p>
          Performance benchmarks for RayDB bindings (NAPI, Python, Rust) using the
          single-file raw benchmark suite.
        </p>

        <h2 id="test-environment">Test Environment</h2>
        <ul>
          <li>macOS (Apple Silicon)</li>
          <li>Bun 1.3.5</li>
          <li>Python 3.12.8</li>
          <li>Rust 1.88.0</li>
          <li>RayDB 0.1.0</li>
        </ul>

        <h2 id="methodology">Methodology</h2>
        <ul>
          <li>Benchmark suite: single-file raw bindings (TypeScript, Python, Rust)</li>
          <li>Read: p50 latency for getNodeByKey / get_node_by_key</li>
          <li>Write: p50 latency for batch write of 100 nodes</li>
          <li>Mixed: full benchmark wall time (build + vector setup + compaction + reads + writes)</li>
          <li>Memory: peak RSS from /usr/bin/time -l</li>
          <li>Sizes: 10k/50k, 100k/500k, 250k/1.25M (nodes/edges)</li>
          <li>WAL: default 64MB for small/medium; 512MB for large to avoid WAL exhaustion</li>
        </ul>

        <h2 id="typescript">TypeScript</h2>
        <table>
          <thead>
            <tr>
              <th>Benchmark</th>
              <th>Nodes/Edges</th>
              <th>Time</th>
              <th>Memory</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>Read</td>
              <td>10k/50k</td>
              <td>167ns</td>
              <td>109.8MB</td>
            </tr>
            <tr>
              <td>Read</td>
              <td>100k/500k</td>
              <td>459ns</td>
              <td>419.2MB</td>
            </tr>
            <tr>
              <td>Read</td>
              <td>250k/1.25M</td>
              <td>542ns</td>
              <td>1027.4MB</td>
            </tr>
            <tr>
              <td>Write</td>
              <td>10k/50k</td>
              <td>214.75us</td>
              <td>109.8MB</td>
            </tr>
            <tr>
              <td>Write</td>
              <td>100k/500k</td>
              <td>280.04us</td>
              <td>419.2MB</td>
            </tr>
            <tr>
              <td>Write</td>
              <td>250k/1.25M</td>
              <td>444.92us</td>
              <td>1027.4MB</td>
            </tr>
            <tr>
              <td>Mixed</td>
              <td>10k/50k</td>
              <td>0.30s</td>
              <td>109.8MB</td>
            </tr>
            <tr>
              <td>Mixed</td>
              <td>100k/500k</td>
              <td>3.43s</td>
              <td>419.2MB</td>
            </tr>
            <tr>
              <td>Mixed</td>
              <td>250k/1.25M</td>
              <td>19.47s</td>
              <td>1027.4MB</td>
            </tr>
          </tbody>
        </table>

        <h2 id="python">Python</h2>
        <table>
          <thead>
            <tr>
              <th>Benchmark</th>
              <th>Nodes/Edges</th>
              <th>Time</th>
              <th>Memory</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>Read</td>
              <td>10k/50k</td>
              <td>250ns</td>
              <td>63.4MB</td>
            </tr>
            <tr>
              <td>Read</td>
              <td>100k/500k</td>
              <td>375ns</td>
              <td>372.2MB</td>
            </tr>
            <tr>
              <td>Read</td>
              <td>250k/1.25M</td>
              <td>458ns</td>
              <td>910.7MB</td>
            </tr>
            <tr>
              <td>Write</td>
              <td>10k/50k</td>
              <td>306.29us</td>
              <td>63.4MB</td>
            </tr>
            <tr>
              <td>Write</td>
              <td>100k/500k</td>
              <td>281.96us</td>
              <td>372.2MB</td>
            </tr>
            <tr>
              <td>Write</td>
              <td>250k/1.25M</td>
              <td>427.58us</td>
              <td>910.7MB</td>
            </tr>
            <tr>
              <td>Mixed</td>
              <td>10k/50k</td>
              <td>0.50s</td>
              <td>63.4MB</td>
            </tr>
            <tr>
              <td>Mixed</td>
              <td>100k/500k</td>
              <td>4.30s</td>
              <td>372.2MB</td>
            </tr>
            <tr>
              <td>Mixed</td>
              <td>250k/1.25M</td>
              <td>21.91s</td>
              <td>910.7MB</td>
            </tr>
          </tbody>
        </table>

        <h2 id="rust">Rust</h2>
        <table>
          <thead>
            <tr>
              <th>Benchmark</th>
              <th>Nodes/Edges</th>
              <th>Time</th>
              <th>Memory</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>Read</td>
              <td>10k/50k</td>
              <td>83ns</td>
              <td>38.0MB</td>
            </tr>
            <tr>
              <td>Read</td>
              <td>100k/500k</td>
              <td>291ns</td>
              <td>339.7MB</td>
            </tr>
            <tr>
              <td>Read</td>
              <td>250k/1.25M</td>
              <td>417ns</td>
              <td>899.1MB</td>
            </tr>
            <tr>
              <td>Write</td>
              <td>10k/50k</td>
              <td>160.21us</td>
              <td>38.0MB</td>
            </tr>
            <tr>
              <td>Write</td>
              <td>100k/500k</td>
              <td>240.25us</td>
              <td>339.7MB</td>
            </tr>
            <tr>
              <td>Write</td>
              <td>250k/1.25M</td>
              <td>378.83us</td>
              <td>899.1MB</td>
            </tr>
            <tr>
              <td>Mixed</td>
              <td>10k/50k</td>
              <td>0.12s</td>
              <td>38.0MB</td>
            </tr>
            <tr>
              <td>Mixed</td>
              <td>100k/500k</td>
              <td>3.03s</td>
              <td>339.7MB</td>
            </tr>
            <tr>
              <td>Mixed</td>
              <td>250k/1.25M</td>
              <td>17.97s</td>
              <td>899.1MB</td>
            </tr>
          </tbody>
        </table>
      </DocPage>
    )
  }

  // Introduction page (empty slug)
  if (slug === '') {
    return (
      <DocPage slug="">
        <p>
          Welcome to RayDB, a high-performance embedded graph database with 
          built-in vector search, designed for Bun and TypeScript.
        </p>

        <h2 id="what-is-raydb">What is RayDB?</h2>
        <p>
          RayDB is an embedded graph database that combines the power of graph 
          relationships with semantic vector search. It's designed for modern 
          TypeScript applications that need:
        </p>
        <ul>
          <li><strong>Graph relationships</strong> – Model complex connections between entities</li>
          <li><strong>Vector search</strong> – Find semantically similar content using embeddings</li>
          <li><strong>Type safety</strong> – Full TypeScript support with inferred types</li>
          <li><strong>High performance</strong> – Optimized for Bun with native bindings</li>
          <li><strong>Zero setup</strong> – No external database to manage</li>
        </ul>

        <h2 id="key-features">Key Features</h2>
        <ul>
          <li><strong>Graph-native</strong> – First-class nodes, edges, and traversals</li>
          <li><strong>Vector search</strong> – HNSW-indexed similarity queries</li>
          <li><strong>Embedded</strong> – Runs in your process, no server needed</li>
          <li><strong>Type-safe</strong> – Schemas with full TypeScript inference</li>
          <li><strong>Fast</strong> – 833k ops/sec writes, sub-ms traversals</li>
          <li><strong>ACID</strong> – Full transaction support</li>
        </ul>

        <h2 id="quick-example">Quick Example</h2>
        <CodeBlock
          code={`import { ray, defineNode, defineEdge, prop } from '@ray-db/ray';

const user = defineNode('user', {
  key: (id: string) => \`user:\${id}\`,
  props: {
    name: prop.string('name'),
    embedding: prop.vector('embedding', 1536),
  },
});

const follows = defineEdge('follows', {
  from: user,
  to: user,
});

const db = await ray('./social.raydb', {
  nodes: [user],
  edges: [follows],
});

// Create users
await db.node(user).createMany([
  { id: 'alice', name: 'Alice', embedding: [...] },
  { id: 'bob', name: 'Bob', embedding: [...] },
]);

// Find similar users
const similar = await db.node(user)
  .vector('embedding')
  .similar(queryEmbedding, { limit: 5 })
  .all();`}
          language="typescript"
        />

        <h2 id="next-steps">Next Steps</h2>
        <ul>
          <li><a href="/docs/getting-started/installation">Installation</a> – Get RayDB set up</li>
          <li><a href="/docs/getting-started/quick-start">Quick Start</a> – Build your first graph</li>
          <li><a href="/docs/guides/schema">Schema Definition</a> – Design your data model</li>
        </ul>
      </DocPage>
    )
  }

  // Default fallback for unknown pages
  return (
    <DocPage slug={slug}>
      <p>This page is coming soon.</p>
    </DocPage>
  )
}
