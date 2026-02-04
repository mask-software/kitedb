import { createFileRoute, useLocation } from '@tanstack/solid-router'
import { Show } from 'solid-js'
import DocPage from '~/components/doc-page'
import { findDocBySlug } from '~/lib/docs'

export const Route = createFileRoute('/docs/benchmarks/$')({
  component: BenchmarksSplatPage,
})

function BenchmarksSplatPage() {
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
          The benchmark page <code class="px-2 py-1 bg-slate-100 dark:bg-slate-800 rounded">{props.slug}</code> doesn't exist yet.
        </p>
        <a
          href="/docs/benchmarks"
          class="inline-flex items-center gap-2 px-6 py-3 bg-gradient-to-r from-cyan-500 to-violet-500 text-white font-semibold rounded-xl hover:shadow-lg hover:shadow-cyan-500/25 transition-all duration-200"
        >
          Back to Benchmarks
        </a>
      </div>
    </div>
  )
}

function DocPageContent(props: { slug: string }) {
  const slug = props.slug

  // Benchmarks Overview page (root level)
  if (slug === 'benchmarks') {
    return (
      <DocPage slug={slug}>
        <p>
          Performance benchmarks for KiteDB across graph operations, vector
          search, and bindings. Latest run: February 3, 2026. Raw logs live in{" "}
          <code>docs/benchmarks/results/</code>.
        </p>

        <h2 id="benchmark-categories">Benchmark Categories</h2>
        <ul>
          <li>
            <a href="/docs/benchmarks/graph">
              <strong>Graph Benchmarks</strong>
            </a>{" "}
            – Single-file raw results (Rust + Python bindings)
          </li>
          <li>
            <a href="/docs/benchmarks/vector">
              <strong>Vector Benchmarks</strong>
            </a>{" "}
            – Vector index performance (Rust)
          </li>
          <li>
            <a href="/docs/benchmarks/cross-language">
              <strong>Cross-Language Benchmarks</strong>
            </a>{" "}
            – Rust vs Python, plus TypeScript API overhead
          </li>
        </ul>

        <h2 id="test-environment">Test Environment</h2>
        <ul>
          <li>Apple M4, 16GB RAM</li>
          <li>macOS 15.3 (Darwin 25.3.0)</li>
          <li>Rust 1.88.0</li>
          <li>Node 24.12.0</li>
          <li>Bun 1.3.5</li>
          <li>Python 3.12.8</li>
        </ul>

        <h2 id="highlights">Highlights (p50)</h2>

        <h3 id="graph-highlights">Graph Operations</h3>
        <table>
          <thead>
            <tr>
              <th>Operation</th>
              <th>Rust Core</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>Key lookup (random existing)</td>
              <td>125ns</td>
            </tr>
            <tr>
              <td>1-hop traversal (out)</td>
              <td>208ns</td>
            </tr>
            <tr>
              <td>Edge exists (random)</td>
              <td>83ns</td>
            </tr>
            <tr>
              <td>Batch write (100 nodes)</td>
              <td>45.62us</td>
            </tr>
          </tbody>
        </table>
        <p>
          <a href="/docs/benchmarks/graph">View detailed graph benchmarks →</a>
        </p>

        <h3 id="vector-highlights">Vector Index</h3>
        <table>
          <thead>
            <tr>
              <th>Operation</th>
              <th>Rust Core</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>Set vectors (10k)</td>
              <td>833ns</td>
            </tr>
            <tr>
              <td>build_index()</td>
              <td>801.95ms</td>
            </tr>
            <tr>
              <td>get (random)</td>
              <td>167ns</td>
            </tr>
            <tr>
              <td>search (k=10, nProbe=10)</td>
              <td>557.54us</td>
            </tr>
          </tbody>
        </table>
        <p>
          <a href="/docs/benchmarks/vector">
            View detailed vector benchmarks →
          </a>
        </p>

        <h2 id="bindings">Bindings Snapshot (Single-File Raw, p50)</h2>
        <table>
          <thead>
            <tr>
              <th>Operation</th>
              <th>Rust</th>
              <th>Python</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>Key lookup (random existing)</td>
              <td>125ns</td>
              <td>208ns</td>
            </tr>
            <tr>
              <td>1-hop traversal (out)</td>
              <td>208ns</td>
              <td>375ns</td>
            </tr>
            <tr>
              <td>Edge exists (random)</td>
              <td>83ns</td>
              <td>125ns</td>
            </tr>
            <tr>
              <td>Batch write (100 nodes)</td>
              <td>45.62us</td>
              <td>253.08us</td>
            </tr>
          </tbody>
        </table>
        <p>
          <a href="/docs/benchmarks/cross-language">
            View cross-language benchmarks →
          </a>
        </p>

        <h2 id="running">Running Benchmarks</h2>
        <table>
          <thead>
            <tr>
              <th>Command</th>
              <th>Description</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td>
                <code>cargo run --release --example single_file_raw_bench --no-default-features</code>
              </td>
              <td>Rust single-file raw benchmark</td>
            </tr>
            <tr>
              <td>
                <code>python3 benchmark_single_file_raw.py</code>
              </td>
              <td>Python single-file raw benchmark</td>
            </tr>
            <tr>
              <td>
                <code>node --import @oxc-node/core/register benchmark/bench-fluent-vs-lowlevel.ts</code>
              </td>
              <td>TypeScript fluent vs low-level overhead</td>
            </tr>
            <tr>
              <td>
                <code>cargo run --release --example vector_bench --no-default-features</code>
              </td>
              <td>Rust vector index benchmark</td>
            </tr>
          </tbody>
        </table>
      </DocPage>
    )
  }

  // Graph Benchmarks page
  if (slug === 'benchmarks/graph') {
    return (
      <DocPage slug={slug}>
        <p>
          Measured graph performance for the single-file engine. Latest run: February 4, 2026.
          Raw logs live in <code>docs/benchmarks/results/</code>.
        </p>

        <h2 id="test-configuration">Test Configuration</h2>
        <ul>
          <li>Nodes: 10,000</li>
          <li>Edges: 50,000</li>
          <li>Iterations: 10,000</li>
          <li>Vector dims: 128</li>
          <li>Vector count: 1,000</li>
        </ul>

        <h2 id="rust-core">Rust Core (single_file_raw_bench)</h2>

        <table>
          <thead>
            <tr>
              <th>Operation</th>
              <th>p50</th>
              <th>p95</th>
            </tr>
          </thead>
          <tbody>
            <tr><td>Key lookup (random existing)</td><td>125ns</td><td>291ns</td></tr>
            <tr><td>1-hop traversal (out)</td><td>208ns</td><td>292ns</td></tr>
            <tr><td>Edge exists (random)</td><td>83ns</td><td>125ns</td></tr>
            <tr><td>Batch write (100 nodes)</td><td>34.08us</td><td>56.54us</td></tr>
            <tr><td>Batch write (100 edges)</td><td>40.25us</td><td>65.58us</td></tr>
            <tr><td>Batch write (100 edges + props)</td><td>172.33us</td><td>253.12us</td></tr>
            <tr><td>get_node_vector()</td><td>125ns</td><td>209ns</td></tr>
            <tr><td>has_node_vector()</td><td>42ns</td><td>84ns</td></tr>
            <tr><td>Set vectors (batch 100)</td><td>94.33us</td><td>195.38us</td></tr>
          </tbody>
        </table>

        <h2 id="python-bindings">Python Bindings (benchmark_single_file_raw.py)</h2>

        <table>
          <thead>
            <tr>
              <th>Operation</th>
              <th>p50</th>
              <th>p95</th>
            </tr>
          </thead>
          <tbody>
            <tr><td>Key lookup (random existing)</td><td>208ns</td><td>334ns</td></tr>
            <tr><td>1-hop traversal (out)</td><td>458ns</td><td>708ns</td></tr>
            <tr><td>Edge exists (random)</td><td>167ns</td><td>209ns</td></tr>
            <tr><td>Batch write (100 nodes)</td><td>49.71us</td><td>57.96us</td></tr>
            <tr><td>Batch write (100 edges)</td><td>53.96us</td><td>64.21us</td></tr>
            <tr><td>Batch write (100 edges + props)</td><td>436.58us</td><td>647.46us</td></tr>
            <tr><td>get_node_vector()</td><td>1.25us</td><td>2.04us</td></tr>
            <tr><td>has_node_vector()</td><td>167ns</td><td>208ns</td></tr>
            <tr><td>Set vectors (batch 100)</td><td>241.83us</td><td>658.42us</td></tr>
          </tbody>
        </table>

        <h2 id="sqlite">SQLite Baseline (single-file raw)</h2>
        <p>
          Apples-to-apples config: WAL mode, <code>synchronous=normal</code>,
          <code>temp_store=MEMORY</code>, <code>locking_mode=EXCLUSIVE</code>,
          <code>cache_size=256MB</code>, WAL autocheckpoint disabled. Edge props are
          stored in a separate table; edges use <code>INSERT OR IGNORE</code> and
          props use <code>INSERT OR REPLACE</code>. Script:{" "}
          <code>docs/benchmarks/sqlite_single_file_raw_bench.py</code>.
        </p>
        <table>
          <thead>
            <tr>
              <th>Operation</th>
              <th>p50</th>
              <th>p95</th>
            </tr>
          </thead>
          <tbody>
            <tr><td>Batch write (100 nodes)</td><td>120.67us</td><td>2.98ms</td></tr>
          </tbody>
        </table>

        <h2 id="running">Running Benchmarks</h2>
        <table>
          <thead>
            <tr>
              <th>Command</th>
              <th>Description</th>
            </tr>
          </thead>
          <tbody>
            <tr>
              <td><code>cargo run --release --example single_file_raw_bench --no-default-features -- --nodes 10000 --edges 50000 --iterations 10000</code></td>
              <td>Rust single-file raw benchmark</td>
            </tr>
            <tr>
              <td><code>python3 benchmark_single_file_raw.py --nodes 10000 --edges 50000 --iterations 10000</code></td>
              <td>Python single-file raw benchmark</td>
            </tr>
          </tbody>
        </table>
      </DocPage>
    )
  }

  // Vector Benchmarks page
  if (slug === 'benchmarks/vector') {
    return (
      <DocPage slug={slug}>
        <p>
          Vector index benchmarks for KiteDB (Rust API). Latest run: February 3, 2026.
          Raw logs live in <code>docs/benchmarks/results/2026-02-03-vector-bench-rust.txt</code>.
        </p>

        <h2 id="config">Test Configuration</h2>
        <ul>
          <li>Vectors: 10,000</li>
          <li>Dimensions: 768</li>
          <li>Iterations: 1,000</li>
          <li>k: 10</li>
          <li>nProbe: 10</li>
        </ul>

        <h2 id="results">Results (Rust)</h2>
        <table>
          <thead>
            <tr>
              <th>Operation</th>
              <th>p50</th>
              <th>p95</th>
            </tr>
          </thead>
          <tbody>
            <tr><td>Set vectors (10k)</td><td>833ns</td><td>2.12us</td></tr>
            <tr><td>build_index()</td><td>801.95ms</td><td>801.95ms</td></tr>
            <tr><td>get (random)</td><td>167ns</td><td>459ns</td></tr>
            <tr><td>search (k=10, nProbe=10)</td><td>557.54us</td><td>918.79us</td></tr>
          </tbody>
        </table>

        <h2 id="running">Running Benchmarks</h2>
        <table>
          <thead>
            <tr>
              <th>Command</th>
              <th>Description</th>
            </tr>
          </thead>
          <tbody>
            <tr><td><code>cargo run --release --example vector_bench --no-default-features -- --vectors 10000 --dimensions 768 --iterations 1000 --k 10 --n-probe 10</code></td><td>Rust vector index benchmark</td></tr>
            <tr><td><code>python3 benchmark_vector.py</code></td><td>Python vector index benchmark</td></tr>
          </tbody>
        </table>
      </DocPage>
    )
  }

  // Cross-Language Benchmarks page
  if (slug === 'benchmarks/cross-language') {
    return (
      <DocPage slug={slug}>
        <p>
          Cross-language benchmarks for KiteDB bindings. Latest run: February 4, 2026.
          Raw logs live in <code>docs/benchmarks/results/</code>.
        </p>

        <h2 id="graph-benchmarks">Single-File Raw (10k nodes / 50k edges)</h2>
        <table>
          <thead>
            <tr>
              <th>Operation</th>
              <th>Rust p50</th>
              <th>Python p50</th>
            </tr>
          </thead>
          <tbody>
            <tr><td>Key lookup (random existing)</td><td>125ns</td><td>208ns</td></tr>
            <tr><td>1-hop traversal (out)</td><td>208ns</td><td>458ns</td></tr>
            <tr><td>Edge exists (random)</td><td>83ns</td><td>167ns</td></tr>
            <tr><td>Batch write (100 nodes)</td><td>34.08us</td><td>49.71us</td></tr>
            <tr><td>Batch write (100 edges)</td><td>40.25us</td><td>53.96us</td></tr>
            <tr><td>Batch write (100 edges + props)</td><td>172.33us</td><td>436.58us</td></tr>
          </tbody>
        </table>

        <h2 id="typescript-overhead">TypeScript Fluent vs Low-Level (NAPI)</h2>
        <p>Config: 1k nodes, 5k edges, 1k iterations.</p>
        <table>
          <thead>
            <tr>
              <th>Operation</th>
              <th>Low-level p50</th>
              <th>Fluent p50</th>
              <th>Overhead</th>
            </tr>
          </thead>
          <tbody>
            <tr><td>Insert (single node + props)</td><td>115.25us</td><td>36.83us</td><td>0.32x</td></tr>
            <tr><td>Key lookup (get w/ props)</td><td>208ns</td><td>1.63us</td><td>7.81x</td></tr>
            <tr><td>Key lookup (getRef)</td><td>208ns</td><td>791ns</td><td>3.80x</td></tr>
            <tr><td>Key lookup (getId)</td><td>208ns</td><td>333ns</td><td>1.60x</td></tr>
            <tr><td>1-hop traversal (count)</td><td>1.21us</td><td>5.75us</td><td>4.76x</td></tr>
            <tr><td>1-hop traversal (nodes)</td><td>1.21us</td><td>5.83us</td><td>4.83x</td></tr>
            <tr><td>1-hop traversal (toArray)</td><td>1.21us</td><td>10.38us</td><td>8.59x</td></tr>
            <tr><td>Pathfinding BFS (depth 5)</td><td>170.79us</td><td>167.71us</td><td>0.98x</td></tr>
          </tbody>
        </table>

        <p>
          Vector index benchmarks are published on the{" "}
          <a href="/docs/benchmarks/vector">vector benchmarks</a> page.
        </p>

        <h2 id="running">Running Benchmarks</h2>
        <table>
          <thead>
            <tr>
              <th>Command</th>
              <th>Description</th>
            </tr>
          </thead>
          <tbody>
            <tr><td><code>cargo run --release --example single_file_raw_bench --no-default-features -- --nodes 10000 --edges 50000 --iterations 10000</code></td><td>Rust single-file raw benchmark</td></tr>
            <tr><td><code>python3 benchmark_single_file_raw.py --nodes 10000 --edges 50000 --iterations 10000</code></td><td>Python single-file raw benchmark</td></tr>
            <tr><td><code>node --import @oxc-node/core/register benchmark/bench-fluent-vs-lowlevel.ts</code></td><td>TypeScript fluent vs low-level overhead</td></tr>
          </tbody>
        </table>
      </DocPage>
    )
  }

  // Default fallback for unknown pages
  return (
    <DocPage slug={slug}>
      <p>This benchmark page is coming soon.</p>
    </DocPage>
  )
}
