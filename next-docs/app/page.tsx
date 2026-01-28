import Link from "next/link";
import {
  ArrowRight,
  BookOpen,
  Box,
  Code,
  Cpu,
  Database,
  GitBranch,
  Network,
  Rocket,
  Search,
  Sparkles,
  Terminal,
} from "lucide-react";
import Header from "../components/Header";
import Logo from "../components/Logo";
import { Card } from "../components/Card";
import CodeBlock from "../components/CodeBlock";
import InstallCommand from "../components/InstallCommand";
import { StatCard } from "../components/StatCard";
import Tabs from "../components/Tabs";
import { highlightCode } from "../lib/highlighter";

export default async function HomePage() {
  const schemaCode = `import { ray, defineNode, defineEdge, prop } from '@ray-db/ray';

// Define nodes with typed properties
const Document = defineNode('document', {
  key: (id: string) => \`doc:\${id}\`,
  props: {
    title: prop.string('title'),
    content: prop.string('content'),
    embedding: prop.vector('embedding', 1536),
  },
});

const Topic = defineNode('topic', {
  key: (name: string) => \`topic:\${name}\`,
  props: { name: prop.string('name') },
});

// Define typed edges
const discusses = defineEdge('discusses', {
  relevance: prop.float('relevance'),
});

// Open database with schema
const db = await ray('./knowledge.raydb', {
  nodes: [Document, Topic],
  edges: [discusses],
});`;

  const traversalCode = `// Find all topics discussed by Alice's documents
const topics = await db
  .from(alice)
  .out('wrote')           // Alice -> Document
  .out('discusses')       // Document -> Topic
  .unique()
  .toArray();

// Multi-hop with filtering
const results = await db
  .from(startNode)
  .out('knows', { where: { since: { gt: 2020n } } })
  .out('worksAt')
  .filter((company) => company.props.employees > 100)
  .limit(10)
  .toArray();`;

  const vectorCode = `// Find similar documents
const similar = await db.similar(Document, queryEmbedding, {
  k: 10,
  threshold: 0.8,
});

// Combine with graph context
const contextual = await Promise.all(
  similar.map(async (doc) => ({
    document: doc,
    topics: await db.from(doc).out('discusses').toArray(),
    related: await db.from(doc).out('relatedTo').limit(5).toArray(),
  }))
);`;

  const crudCode = `// Insert with returning
const doc = await db.insert(Document)
  .values({
    key: 'doc-1',
    title: 'Getting Started',
    content: 'Welcome to RayDB...',
    embedding: await embed('Welcome to RayDB...'),
  })
  .returning();

// Create relationships
await db.link(doc, discusses, topic, { relevance: 0.95 });

// Update properties
await db.update(Document)
  .set({ title: 'Updated Title' })
  .where({ key: 'doc-1' });`;

  const tabItems = await Promise.all(
    [
      { label: "Traversal", code: traversalCode, language: "typescript" },
      { label: "Vector Search", code: vectorCode, language: "typescript" },
      { label: "CRUD", code: crudCode, language: "typescript" },
    ].map(async (item) => ({
      ...item,
      html: await highlightCode(item.code, item.language || "text"),
    }))
  );

  return (
    <div className="min-h-screen bg-[#05070d] speed-page">

      <Header />

      <main id="main-content">
        <section className="relative pb-28 sm:pb-36 overflow-hidden speed-hero" aria-labelledby="hero-heading">

          <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
            <div className="grid gap-12 lg:grid-cols-[1.1fr,0.9fr] items-center">
              <div className="text-left">
                <div className="speed-badge animate-slide-up">Embedded Graph + Vector Engine</div>
                <h1
                  id="hero-heading"
                  className="mt-6 text-5xl sm:text-6xl md:text-7xl font-black tracking-tight text-balance animate-slide-up animate-delay-100"
                >
                  <span className="block text-white">Instant Graph + Vector Search</span>
                  <span className="block mt-3 speed-text neon-glow-subtle">Nanosecond Latency by Design</span>
                </h1>

                <p className="mt-8 max-w-xl text-lg sm:text-xl text-slate-400 text-pretty leading-relaxed animate-slide-up animate-delay-200">
                  RayDB is an embedded graph database with vector search, built in Rust for fast, local, predictable
                  performance in every app.
                </p>

                <div className="mt-10 flex flex-wrap items-center gap-4 animate-slide-up animate-delay-300">
                  <Link
                    href="/docs/getting-started/installation"
                    className="group inline-flex items-center gap-2 px-8 py-4 text-base font-semibold text-black bg-[#2af2ff] rounded-xl shadow-[0_0_30px_rgba(42,242,255,0.4)] hover:shadow-[0_0_50px_rgba(42,242,255,0.6)] hover:scale-[1.02] active:scale-[0.98] transition-[box-shadow,transform] duration-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white focus-visible:ring-offset-2 focus-visible:ring-offset-[#05070d] speed-cta"
                  >
                    Get Started
                    <ArrowRight size={18} className="group-hover:translate-x-0.5 transition-transform duration-150" aria-hidden="true" />
                  </Link>
                  <a
                    href="https://github.com/maskdotdev/ray"
                    target="_blank"
                    rel="noopener noreferrer"
                    className="inline-flex items-center gap-2 px-8 py-4 text-base font-semibold text-white bg-white/5 border border-white/10 rounded-xl hover:bg-white/10 hover:border-white/20 active:scale-[0.98] transition-[background-color,border-color,transform] duration-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[#2af2ff] focus-visible:ring-offset-2 focus-visible:ring-offset-[#05070d]"
                  >
                    <svg className="w-5 h-5" fill="currentColor" viewBox="0 0 24 24" aria-hidden="true">
                      <path
                        fillRule="evenodd"
                        d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
                        clipRule="evenodd"
                      />
                    </svg>
                    View on GitHub
                  </a>
                </div>

                <div className="mt-6 flex flex-wrap gap-2 animate-slide-up animate-delay-400">
                  <span className="speed-chip">125ns lookups</span>
                  <span className="speed-chip">1.1us traversals</span>
                  <span className="speed-chip">Zero dependencies</span>
                  <span className="speed-chip">Rust core</span>
                </div>

                <div className="mt-10 animate-slide-up animate-delay-500">
                  <InstallCommand command="bun add @ray-db/ray" />
                </div>
              </div>

              <div className="relative">
                <div className="speed-panel p-8 sm:p-10 animate-slide-up animate-delay-200">
                  <div className="relative z-10 flex items-center justify-between">
                    <div>
                      <p className="text-xs font-semibold tracking-[0.35em] text-slate-500 uppercase">Performance Console</p>
                      <h2 className="mt-3 text-2xl font-semibold text-white text-balance">Nanosecond Graph Paths</h2>
                    </div>
                    <span className="speed-signal">Live</span>
                  </div>

                  <div className="relative z-10 mt-8 grid grid-cols-2 gap-4">
                    <div className="speed-metric">
                      <div className="text-2xl font-semibold text-white tabular-nums">125ns</div>
                      <div className="mt-1 text-xs text-slate-400 uppercase tracking-[0.2em]">Node lookup</div>
                    </div>
                    <div className="speed-metric">
                      <div className="text-2xl font-semibold text-white tabular-nums">1.1us</div>
                      <div className="mt-1 text-xs text-slate-400 uppercase tracking-[0.2em]">1 hop traverse</div>
                    </div>
                    <div className="speed-metric">
                      <div className="text-2xl font-semibold text-white tabular-nums">0</div>
                      <div className="mt-1 text-xs text-slate-400 uppercase tracking-[0.2em]">Network hops</div>
                    </div>
                    <div className="speed-metric">
                      <div className="text-2xl font-semibold text-white tabular-nums">118x</div>
                      <div className="mt-1 text-xs text-slate-400 uppercase tracking-[0.2em]">Memgraph speedup</div>
                    </div>
                  </div>

                  <div className="relative z-10 mt-6 speed-meter" aria-hidden="true" />

                  <p className="relative z-10 mt-4 text-xs text-slate-500">
                    Runs inside your process with memory-mapped storage and zero copy reads.
                  </p>

                  <div className="relative z-10 mt-6 grid gap-3 text-sm">
                    <div className="flex items-center justify-between text-slate-400">
                      <span>Cold start</span>
                      <span className="text-white tabular-nums">4ms</span>
                    </div>
                    <div className="flex items-center justify-between text-slate-400">
                      <span>Vector recall</span>
                      <span className="text-white tabular-nums">0.98</span>
                    </div>
                    <div className="flex items-center justify-between text-slate-400">
                      <span>Disk footprint</span>
                      <span className="text-white tabular-nums">single file</span>
                    </div>
                  </div>
                </div>

                <div className="hidden lg:block absolute -bottom-8 right-8">
                  <div className="speed-ghost-card p-5">
                    <div className="text-xs uppercase tracking-[0.3em] text-slate-500">Edge Ready</div>
                    <div className="mt-2 text-white font-semibold">Single file. Zero services.</div>
                    <p className="mt-2 text-sm text-slate-400">Ship a full graph + vector stack with your app.</p>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </section>

        <section className="py-20 border-y border-[#1a2a42]/60 bg-[#0b1220]/55" aria-labelledby="stats-heading">
          <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
            <div className="flex flex-col lg:flex-row lg:items-end lg:justify-between gap-6 mb-12">
              <div>
                <h2 id="stats-heading" className="text-3xl sm:text-4xl font-bold text-white text-balance">
                  Performance Snapshot
                </h2>
                <p className="mt-4 text-lg text-slate-400 max-w-xl text-pretty">
                  The speed profile stays consistent because everything runs local with zero-copy reads.
                </p>
              </div>
              <Link href="/docs/benchmarks" className="inline-flex items-center gap-2 text-[#00d4ff] hover:text-white transition-colors font-medium">
                View benchmarks
                <ArrowRight size={16} aria-hidden="true" />
              </Link>
            </div>

            <div className="grid grid-cols-2 lg:grid-cols-4 gap-4 md:gap-6">
              <div className="lg:-translate-y-4">
                <StatCard value="125ns" label="Node Lookup" />
              </div>
              <div className="lg:translate-y-4">
                <StatCard value="1.1us" label="1-Hop Traversal" />
              </div>
              <div className="lg:-translate-y-4">
                <StatCard value="Zero" label="Dependencies" />
              </div>
              <div className="lg:translate-y-4">
                <StatCard value="Rust" label="Native Core" />
              </div>
            </div>
          </div>
        </section>

        <section className="py-28" aria-labelledby="features-heading">
          <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
            <div className="grid lg:grid-cols-[0.95fr,1.05fr] gap-12 items-start">
              <div>
                <h2 id="features-heading" className="text-3xl sm:text-4xl font-bold text-white text-balance">
                  Fast by Design, End to End
                </h2>
                <p className="mt-4 text-lg text-slate-400 text-pretty">
                  Model, traverse, and ship without latency spikes. Every layer is tuned for speed.
                </p>

                <div className="mt-10 space-y-6">
                  <div className="flex items-start gap-4">
                    <div className="w-11 h-11 rounded-full bg-[#2af2ff]/10 text-[#2af2ff] font-mono text-xs tracking-[0.3em] flex items-center justify-center">
                      01
                    </div>
                    <div>
                      <h3 className="text-xl font-semibold text-white">Unified Data Model</h3>
                      <p className="mt-2 text-slate-400 leading-relaxed">
                        Graph relationships and vector similarity live in one coherent API.
                      </p>
                    </div>
                  </div>
                  <div className="flex items-start gap-4">
                    <div className="w-11 h-11 rounded-full bg-[#2af2ff]/10 text-[#2af2ff] font-mono text-xs tracking-[0.3em] flex items-center justify-center">
                      02
                    </div>
                    <div>
                      <h3 className="text-xl font-semibold text-white">Zero Copy Performance</h3>
                      <p className="mt-2 text-slate-400 leading-relaxed">
                        Memory-mapped storage keeps reads hot without external processes.
                      </p>
                    </div>
                  </div>
                  <div className="flex items-start gap-4">
                    <div className="w-11 h-11 rounded-full bg-[#2af2ff]/10 text-[#2af2ff] font-mono text-xs tracking-[0.3em] flex items-center justify-center">
                      03
                    </div>
                    <div>
                      <h3 className="text-xl font-semibold text-white">Developer Friendly</h3>
                      <p className="mt-2 text-slate-400 leading-relaxed">
                        Type-safe schemas and bindings for TypeScript, Python, and more.
                      </p>
                    </div>
                  </div>
                </div>
              </div>

              <div className="grid sm:grid-cols-2 gap-4">
                <Card
                  title="Graph + Vector"
                  description="Traverse relationships and run similarity search in the same query chain."
                  icon={<Database className="w-5 h-5" aria-hidden="true" />}
                />
                <Card
                  title="HNSW Vector Index"
                  description="Log-time nearest neighbor search with high recall at scale."
                  icon={<Search className="w-5 h-5" aria-hidden="true" />}
                />
                <Card
                  title="Zero Dependencies"
                  description="Single-file storage that is easy to back up, sync, and deploy."
                  icon={<Sparkles className="w-5 h-5" aria-hidden="true" />}
                />
                <Card
                  title="MVCC Transactions"
                  description="Snapshot isolation with non-blocking readers by default."
                  icon={<GitBranch className="w-5 h-5" aria-hidden="true" />}
                />
              </div>
            </div>
          </div>
        </section>

        <section className="py-28 bg-[#0b1220]/35" aria-labelledby="workflow-heading">
          <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
            <div className="text-center mb-16">
              <h2 id="workflow-heading" className="text-3xl sm:text-4xl font-bold text-white text-balance">
                From Schema to Query
              </h2>
              <p className="mt-4 text-lg text-slate-400 text-pretty max-w-2xl mx-auto">
                A clean two-step workflow: define the model, then compose fast, readable queries.
              </p>
            </div>

            <div className="grid lg:grid-cols-12 gap-12 items-center">
              <div className="lg:col-span-5">
                <div className="flex items-center gap-3 mb-4">
                  <span className="text-xs font-semibold tracking-[0.3em] text-[#2af2ff] font-mono">01</span>
                  <h3 className="text-2xl font-bold text-white">Schema-First Design</h3>
                </div>
                <p className="mt-4 text-lg text-slate-400 text-pretty leading-relaxed">
                  Define your schema once, get idiomatic APIs in every language. Type safety where your language
                  supports it.
                </p>
                <ul className="mt-6 space-y-3">
                  <li className="flex items-start gap-3">
                    <div className="flex-shrink-0 w-6 h-6 rounded-full bg-[#2af2ff]/15 icon-tile mt-0.5">
                      <div className="w-2 h-2 rounded-full bg-[#2af2ff]" />
                    </div>
                    <span className="text-slate-300">Typed nodes with properties and vector embeddings</span>
                  </li>
                  <li className="flex items-start gap-3">
                    <div className="flex-shrink-0 w-6 h-6 rounded-full bg-[#2af2ff]/15 icon-tile mt-0.5">
                      <div className="w-2 h-2 rounded-full bg-[#2af2ff]" />
                    </div>
                    <span className="text-slate-300">Typed edges with relationship properties</span>
                  </li>
                  <li className="flex items-start gap-3">
                    <div className="flex-shrink-0 w-6 h-6 rounded-full bg-[#2af2ff]/15 icon-tile mt-0.5">
                      <div className="w-2 h-2 rounded-full bg-[#2af2ff]" />
                    </div>
                    <span className="text-slate-300">Single-file storage with SQLite-inspired simplicity</span>
                  </li>
                </ul>
              </div>
              <div className="lg:col-span-7">
                <CodeBlock code={schemaCode} language="typescript" filename="schema.ts" />
              </div>
            </div>

            <div className="mt-16 grid lg:grid-cols-12 gap-12 items-start">
              <div className="lg:col-span-5 order-2 lg:order-1">
                <div className="flex items-center gap-3 mb-4">
                  <span className="text-xs font-semibold tracking-[0.3em] text-[#2af2ff] font-mono">02</span>
                  <h3 className="text-2xl font-bold text-white">Intuitive Query API</h3>
                </div>
                <p className="mt-4 text-lg text-slate-400 text-pretty leading-relaxed">
                  Fluent, chainable queries that read like the graph - traversal, vectors, and CRUD in one place.
                </p>
                <div className="mt-6 flex flex-wrap gap-2">
                  <span className="px-3 py-1 text-xs font-semibold tracking-wide text-[#2af2ff] bg-[#2af2ff]/10 rounded-full">
                    Traversal
                  </span>
                  <span className="px-3 py-1 text-xs font-semibold tracking-wide text-[#2af2ff] bg-[#2af2ff]/10 rounded-full">
                    Vector Search
                  </span>
                  <span className="px-3 py-1 text-xs font-semibold tracking-wide text-[#2af2ff] bg-[#2af2ff]/10 rounded-full">
                    CRUD
                  </span>
                </div>
              </div>
              <div className="lg:col-span-7 order-1 lg:order-2">
                <Tabs items={tabItems} />
              </div>
            </div>
          </div>
        </section>

        <section className="py-28 bg-[#0b1220]/35" aria-labelledby="architecture-heading">
          <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
            <div className="mb-16 max-w-xl">
              <h2 id="architecture-heading" className="text-3xl sm:text-4xl font-bold text-white text-balance">
                Under the Hood
              </h2>
              <p className="mt-4 text-lg text-slate-400 text-pretty">
                Purpose-built internals for maximum performance with minimal complexity.
              </p>
            </div>

            <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
              <article className="md:col-span-2 lg:col-span-2 group relative p-8 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
                <div className="flex items-start gap-5">
                  <div className="flex-shrink-0 w-14 h-14 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
                    <Cpu className="w-7 h-7" aria-hidden="true" />
                  </div>
                  <div>
                    <h3 className="text-xl font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
                      CSR Storage Format
                    </h3>
                    <p className="mt-3 text-slate-400 leading-relaxed max-w-lg">
                      Compressed Sparse Row format stores adjacency data contiguously for cache-efficient traversal.
                      Memory-mapped files enable zero-copy reads with minimal memory footprint.
                    </p>
                  </div>
                </div>
              </article>

              <article className="group relative p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
                <div className="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200 mb-4">
                  <Terminal className="w-6 h-6" aria-hidden="true" />
                </div>
                <h3 className="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
                  Rust Core
                </h3>
                <p className="mt-2 text-sm text-slate-400 leading-relaxed">
                  Written in Rust for memory safety and predictable performance. Zero-cost FFI bindings.
                </p>
              </article>

              <article className="group relative p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
                <div className="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200 mb-4">
                  <Network className="w-6 h-6" aria-hidden="true" />
                </div>
                <h3 className="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
                  Layered Navigation
                </h3>
                <p className="mt-2 text-sm text-slate-400 leading-relaxed">
                  HNSW builds a hierarchy of proximity graphs, enabling O(log n) approximate nearest neighbor queries.
                </p>
              </article>

              <article className="md:col-span-2 group relative p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
                <div className="flex items-start gap-4">
                  <div className="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
                    <Box className="w-6 h-6" aria-hidden="true" />
                  </div>
                  <div>
                    <h3 className="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
                      Append-Only WAL
                    </h3>
                    <p className="mt-2 text-sm text-slate-400 leading-relaxed">
                      Write-ahead logging ensures durability. Periodic compaction reclaims space while maintaining
                      consistent read performance.
                    </p>
                  </div>
                </div>
              </article>
            </div>
          </div>
        </section>

        <section className="py-28" aria-labelledby="usecases-heading">
          <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
            <div className="flex flex-col lg:flex-row lg:items-end lg:justify-between gap-6 mb-16">
              <div>
                <h2 id="usecases-heading" className="text-3xl sm:text-4xl font-bold text-white text-balance">
                  Perfect For
                </h2>
                <p className="mt-4 text-lg text-slate-400 max-w-xl">
                  From AI applications to local-first software, RayDB adapts to your needs.
                </p>
              </div>
              <Link href="/docs" className="inline-flex items-center gap-2 text-[#00d4ff] hover:text-white transition-colors font-medium">
                Explore all use cases
                <ArrowRight size={16} aria-hidden="true" />
              </Link>
            </div>

            <div className="grid lg:grid-cols-3 gap-6">
              <article className="lg:row-span-2 group relative p-8 rounded-2xl bg-gradient-to-br from-[#0b1220] to-[#0b1220]/60 border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
                <div className="h-full flex flex-col">
                  <div className="flex-shrink-0 w-14 h-14 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 group-hover:shadow-[0_0_20px_rgba(0,212,255,0.3)] transition-[background-color,transform,box-shadow] duration-200 mb-6">
                    <BookOpen className="w-7 h-7" aria-hidden="true" />
                  </div>
                  <h3 className="text-xl font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
                    RAG Pipelines
                  </h3>
                  <p className="mt-4 text-slate-400 leading-relaxed flex-grow">
                    Store document chunks with embeddings and traverse relationships for context-aware retrieval.
                    Combine vector similarity with graph context for superior RAG results.
                  </p>
                  <div className="mt-6 pt-6 border-t border-[#1a2a42]">
                    <div className="flex items-center gap-2 text-sm text-slate-500">
                      <span className="w-2 h-2 rounded-full bg-[#00d4ff]" />
                      Vector embeddings + Graph traversal
                    </div>
                  </div>
                </div>
              </article>

              <article className="group relative p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
                <div className="flex items-start gap-4">
                  <div className="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
                    <GitBranch className="w-6 h-6" aria-hidden="true" />
                  </div>
                  <div>
                    <h3 className="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
                      Knowledge Graphs
                    </h3>
                    <p className="mt-2 text-sm text-slate-400 leading-relaxed">
                      Model complex relationships between entities with semantic similarity search.
                    </p>
                  </div>
                </div>
              </article>

              <article className="group relative p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
                <div className="flex items-start gap-4">
                  <div className="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
                    <Sparkles className="w-6 h-6" aria-hidden="true" />
                  </div>
                  <div>
                    <h3 className="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
                      Recommendations
                    </h3>
                    <p className="mt-2 text-sm text-slate-400 leading-relaxed">
                      Combine user-item graphs with embedding similarity for hybrid recommendations.
                    </p>
                  </div>
                </div>
              </article>

              <article className="lg:col-span-2 group relative p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
                <div className="flex items-start gap-4">
                  <div className="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
                    <Database className="w-6 h-6" aria-hidden="true" />
                  </div>
                  <div>
                    <h3 className="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
                      Local-First Apps
                    </h3>
                    <p className="mt-2 text-sm text-slate-400 leading-relaxed max-w-lg">
                      Embedded architecture with single-file storage. No external database needed. Perfect for desktop
                      apps, CLI tools, and edge computing.
                    </p>
                  </div>
                </div>
              </article>
            </div>
          </div>
        </section>

        <section className="py-28 bg-[#0b1220]/35" aria-labelledby="cta-heading">
          <div className="max-w-4xl mx-auto px-4 sm:px-6 lg:px-8 text-center">
            <h2 id="cta-heading" className="text-3xl sm:text-4xl font-bold text-white text-balance">
              Ready to Get Started?
            </h2>
            <p className="mt-4 text-lg text-slate-400 text-pretty">
              Build your first graph database in 5 minutes with our Quick Start guide.
            </p>

            <div className="mt-12 grid sm:grid-cols-2 gap-6 max-w-2xl mx-auto">
              <Link
                href="/docs/getting-started/installation"
                className="group flex items-center gap-4 p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/55 hover:shadow-[0_0_30px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-200 speed-card"
              >
                <div className="flex-shrink-0 w-14 h-14 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
                  <Rocket className="w-7 h-7" aria-hidden="true" />
                </div>
                <div className="text-left min-w-0">
                  <div className="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
                    Installation Guide
                  </div>
                  <div className="text-sm text-slate-500">Set up in 2 minutes</div>
                </div>
              </Link>

              <Link
                href="/docs/getting-started/quick-start"
                className="group flex items-center gap-4 p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/55 hover:shadow-[0_0_30px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-200 speed-card"
              >
                <div className="flex-shrink-0 w-14 h-14 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
                  <Code className="w-7 h-7" aria-hidden="true" />
                </div>
                <div className="text-left min-w-0">
                  <div className="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
                    Quick Start Tutorial
                  </div>
                  <div className="text-sm text-slate-500">Build your first graph</div>
                </div>
              </Link>
            </div>
          </div>
        </section>
      </main>

      <footer className="border-t border-[#1a2a42]/60 py-12 bg-[#05070d]">
        <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
          <div className="flex flex-col sm:flex-row items-center justify-between gap-6">
            <div className="flex items-center gap-2.5">
              <Logo size={24} />
              <span className="font-semibold text-gradient">RayDB</span>
            </div>

            <p className="text-sm text-slate-500">MIT License. Built with Rust.</p>

            <div className="flex items-center gap-4">
              <a
                href="https://github.com/maskdotdev/ray"
                target="_blank"
                rel="noopener noreferrer"
                className="text-slate-500 hover:text-[#00d4ff] transition-colors duration-150"
                aria-label="RayDB on GitHub"
              >
                <svg className="w-5 h-5" fill="currentColor" viewBox="0 0 24 24" aria-hidden="true">
                  <path
                    fillRule="evenodd"
                    d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
                    clipRule="evenodd"
                  />
                </svg>
              </a>
            </div>
          </div>
        </div>
      </footer>
    </div>
  );
}
