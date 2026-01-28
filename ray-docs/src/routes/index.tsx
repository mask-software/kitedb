import { createFileRoute, Link } from "@tanstack/solid-router";
import {
	Zap,
	Database,
	GitBranch,
	Shield,
	Search,
	Sparkles,
	ArrowRight,
	BookOpen,
	Rocket,
	Code,
	Terminal,
	Cpu,
	Network,
	Box,
} from "lucide-solid";
import Logo from "~/components/Logo";
import ThemeToggle from "~/components/ThemeToggle";
import { StatCard } from "~/components/StatCard";
import { Card } from "~/components/Card";
import CodeBlock from "~/components/CodeBlock";
import { Tabs } from "~/components/Tabs";

export const Route = createFileRoute("/")({
	component: HomePage,
});

function HomePage() {
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
  .filter(company => company.props.employees > 100)
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

	return (
		<div class="min-h-screen bg-[#05070d] speed-page">
			{/* Skip link */}
			<a
				href="#main-content"
				class="sr-only focus:not-sr-only focus:absolute focus:top-4 focus:left-4 focus:z-50 focus:px-4 focus:py-2 focus:bg-[#00d4ff] focus:text-black focus:rounded-lg focus:font-semibold"
			>
				Skip to main content
			</a>

			{/* Header */}
			<header class="sticky top-0 z-50 border-b border-[#1a2a42]/70 speed-glass speed-nav">
				<nav
					class="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8"
					aria-label="Main navigation"
				>
					<div class="flex items-center justify-between h-16">
						<Link
							to="/"
							class="flex items-center gap-2.5 group"
							aria-label="RayDB Home"
						>
							<Logo size={32} />
							<span class="text-xl font-bold text-gradient">RayDB</span>
						</Link>

						<div class="hidden md:flex items-center gap-1">
							<Link
								to="/docs"
								class="px-4 py-2 text-sm font-medium text-slate-400 hover:text-white rounded-lg hover:bg-white/5 transition-colors duration-150"
							>
								Documentation
							</Link>
							<a
								href="/docs/api/high-level"
								class="px-4 py-2 text-sm font-medium text-slate-400 hover:text-white rounded-lg hover:bg-white/5 transition-colors duration-150"
							>
								API Reference
							</a>
							<a
								href="/docs/benchmarks"
								class="px-4 py-2 text-sm font-medium text-slate-400 hover:text-white rounded-lg hover:bg-white/5 transition-colors duration-150"
							>
								Benchmarks
							</a>
						</div>

						<div class="flex items-center gap-1">
							<ThemeToggle />
							<a
								href="https://github.com/maskdotdev/ray"
								target="_blank"
								rel="noopener noreferrer"
								class="p-2.5 rounded-lg text-slate-400 hover:bg-white/5 hover:text-white transition-colors duration-150"
								aria-label="View RayDB on GitHub"
							>
								<svg
									class="w-5 h-5"
									fill="currentColor"
									viewBox="0 0 24 24"
									aria-hidden="true"
								>
									<path
										fill-rule="evenodd"
										d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
										clip-rule="evenodd"
									/>
								</svg>
							</a>
						</div>
					</div>
				</nav>
			</header>

			<main id="main-content">
				{/* Hero Section */}
				<section
					class="relative pt-24 pb-32 sm:pt-32 sm:pb-40 overflow-hidden"
					aria-labelledby="hero-heading"
				>
					{/* Background glow orbs */}
					<div
						class="absolute inset-0 -z-10 overflow-hidden"
						aria-hidden="true"
					>
						<div class="hero-glow w-[800px] h-[800px] -top-[400px] left-1/2 -translate-x-1/2 animate-glow-pulse" />
						<div class="hero-glow w-[600px] h-[600px] top-[100px] -left-[200px] animate-glow-pulse animate-delay-200" />
						<div class="hero-glow w-[500px] h-[500px] top-[200px] -right-[100px] animate-glow-pulse animate-delay-400" />
						<div class="speed-grid" />
						<div class="speed-lines" />
						<div class="speed-sheen" />
					</div>

					<div class="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
						<div class="text-center">
							{/* Main heading */}
							<h1
								id="hero-heading"
								class="text-5xl sm:text-6xl md:text-7xl lg:text-8xl font-black tracking-tight text-balance animate-slide-up"
							>
								<span class="block text-white">The Graph Database</span>
								<span class="block mt-2 speed-text neon-glow-subtle">
									Built for Speed
								</span>
							</h1>

							{/* Tagline */}
							<p class="mt-8 max-w-2xl mx-auto text-lg sm:text-xl text-slate-400 text-pretty leading-relaxed animate-slide-up animate-delay-100">
								A high-performance embedded graph database with vector search.
								Rust core with bindings for TypeScript, Python, and more.
							</p>

							{/* CTA buttons */}
							<div class="mt-12 flex flex-col sm:flex-row items-center justify-center gap-4 animate-slide-up animate-delay-200">
								<Link
									to="/docs/getting-started/installation"
									class="group inline-flex items-center gap-2 px-8 py-4 text-base font-semibold text-black bg-[#2af2ff] rounded-xl shadow-[0_0_30px_rgba(42,242,255,0.4)] hover:shadow-[0_0_50px_rgba(42,242,255,0.6)] hover:scale-[1.02] active:scale-[0.98] transition-[box-shadow,transform] duration-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white focus-visible:ring-offset-2 focus-visible:ring-offset-[#05070d] speed-cta"
								>
									Get Started
									<ArrowRight
										size={18}
										class="group-hover:translate-x-0.5 transition-transform duration-150"
										aria-hidden="true"
									/>
								</Link>
								<a
									href="https://github.com/maskdotdev/ray"
									target="_blank"
									rel="noopener noreferrer"
									class="inline-flex items-center gap-2 px-8 py-4 text-base font-semibold text-white bg-white/5 border border-white/10 rounded-xl hover:bg-white/10 hover:border-white/20 active:scale-[0.98] transition-[background-color,border-color,transform] duration-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[#2af2ff] focus-visible:ring-offset-2 focus-visible:ring-offset-[#05070d]"
								>
									<svg
										class="w-5 h-5"
										fill="currentColor"
										viewBox="0 0 24 24"
										aria-hidden="true"
									>
										<path
											fill-rule="evenodd"
											d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
											clip-rule="evenodd"
										/>
									</svg>
									View on GitHub
								</a>
							</div>

							{/* Install command */}
							<div class="mt-12 flex justify-center animate-slide-up animate-delay-300">
								<div class="group relative inline-flex items-center gap-4 px-6 py-4 bg-[#0b1220] rounded-xl border border-[#1a2a42] shadow-[0_0_30px_rgba(0,0,0,0.3)] speed-card">
									<Terminal
										size={18}
										class="text-slate-500"
										aria-hidden="true"
									/>
									<code class="text-sm font-mono">
										<span class="text-slate-500">$</span>
										<span class="text-[#00d4ff] ml-2">bun add</span>
										<span class="text-white ml-2">@ray-db/ray</span>
									</code>
									<button
										type="button"
										class="p-2 rounded-lg text-slate-500 hover:text-[#00d4ff] hover:bg-white/5 transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[#00d4ff]"
										aria-label="Copy install command"
										onClick={() =>
											navigator.clipboard.writeText("bun add @ray-db/ray")
										}
									>
										<svg
											class="w-4 h-4"
											fill="none"
											stroke="currentColor"
											viewBox="0 0 24 24"
											aria-hidden="true"
										>
											<path
												stroke-linecap="round"
												stroke-linejoin="round"
												stroke-width="2"
												d="M8 16H6a2 2 0 01-2-2V6a2 2 0 012-2h8a2 2 0 012 2v2m-6 12h8a2 2 0 002-2v-8a2 2 0 00-2-2h-8a2 2 0 00-2 2v8a2 2 0 002 2z"
											/>
										</svg>
									</button>
								</div>
							</div>
						</div>
					</div>
				</section>

				{/* Stats Section - Staggered Layout */}
				<section
					class="py-20 border-y border-[#1a2a42]/60 bg-[#0b1220]/55"
					aria-labelledby="stats-heading"
				>
					<h2 id="stats-heading" class="sr-only">
						Performance Statistics
					</h2>
					<div class="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
						<div class="grid grid-cols-2 lg:grid-cols-4 gap-4 md:gap-6">
							{/* First stat - offset up */}
							<div class="lg:-translate-y-4">
								<StatCard value="~125ns" label="Node Lookup" />
							</div>
							{/* Second stat - normal */}
							<div class="lg:translate-y-4">
								<StatCard value="~1.1μs" label="1-Hop Traversal" />
							</div>
							{/* Third stat - offset up */}
							<div class="lg:-translate-y-4">
								<StatCard value="Zero" label="Dependencies" />
							</div>
							{/* Fourth stat - normal */}
							<div class="lg:translate-y-4">
								<StatCard value="Rust" label="Native Core" />
							</div>
						</div>
					</div>
				</section>

				{/* Features - Guided Flow */}
				<section class="py-28" aria-labelledby="features-heading">
					<div class="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
						<div class="text-center mb-16">
							<h2
								id="features-heading"
								class="text-3xl sm:text-4xl font-bold text-white text-balance"
							>
								Built for Modern AI Applications
							</h2>
							<p class="mt-4 text-lg text-slate-400 max-w-2xl mx-auto text-pretty">
								A clear, three‑step stack: model your data, query it fast, and
								ship everywhere.
							</p>
						</div>

						<div class="space-y-12">
							<article class="grid lg:grid-cols-12 gap-8 lg:gap-12 items-start">
								<div class="lg:col-span-4">
									<div class="flex items-center gap-3 mb-4">
										<span class="text-xs font-semibold tracking-[0.3em] text-[#2af2ff] font-mono">
											01
										</span>
										<div class="w-10 h-10 icon-tile rounded-lg bg-[#2af2ff]/10 text-[#2af2ff]">
											<Database class="w-5 h-5" aria-hidden="true" />
										</div>
										<h3 class="text-2xl font-bold text-white">
											Unified Data Model
										</h3>
									</div>
									<p class="text-slate-400 leading-relaxed">
										Combine graph relationships and vector similarity in one
										coherent API—no glue code, no extra services.
									</p>
								</div>
								<div class="lg:col-span-8 grid sm:grid-cols-2 gap-4">
									<Card
										title="Graph + Vector"
										description="Traverse relationships and run similarity search in the same query chain."
										icon={<Database class="w-5 h-5" aria-hidden="true" />}
									/>
									<Card
										title="HNSW Vector Index"
										description="Log‑time nearest neighbor search with high recall at scale."
										icon={<Search class="w-5 h-5" aria-hidden="true" />}
									/>
								</div>
							</article>

							<div class="h-px w-full bg-gradient-to-r from-transparent via-[#1a2a42] to-transparent" />

							<article class="grid lg:grid-cols-12 gap-8 lg:gap-12 items-start">
								<div class="lg:col-span-4">
									<div class="flex items-center gap-3 mb-4">
										<span class="text-xs font-semibold tracking-[0.3em] text-[#2af2ff] font-mono">
											02
										</span>
										<div class="w-10 h-10 icon-tile rounded-lg bg-[#2af2ff]/10 text-[#2af2ff]">
											<Zap class="w-5 h-5" aria-hidden="true" />
										</div>
										<h3 class="text-2xl font-bold text-white">
											Blazing Performance
										</h3>
									</div>
									<p class="text-slate-400 leading-relaxed">
										Memory‑mapped storage + zero‑copy reads keep latency
										ultra‑low without external processes.
									</p>
								</div>
								<div class="lg:col-span-8 grid sm:grid-cols-2 gap-4">
									<Card
										title="Blazing Fast"
										description="~125ns node lookups, ~1.1μs traversals. 118× faster than Memgraph."
										icon={<Zap class="w-5 h-5" aria-hidden="true" />}
									/>
									<Card
										title="Zero Dependencies"
										description="Single‑file storage that’s easy to back up, sync, and deploy."
										icon={<Sparkles class="w-5 h-5" aria-hidden="true" />}
									/>
								</div>
							</article>

							<div class="h-px w-full bg-gradient-to-r from-transparent via-[#1a2a42] to-transparent" />

							<article class="grid lg:grid-cols-12 gap-8 lg:gap-12 items-start">
								<div class="lg:col-span-4">
									<div class="flex items-center gap-3 mb-4">
										<span class="text-xs font-semibold tracking-[0.3em] text-[#2af2ff] font-mono">
											03
										</span>
										<div class="w-10 h-10 icon-tile rounded-lg bg-[#2af2ff]/10 text-[#2af2ff]">
											<Shield class="w-5 h-5" aria-hidden="true" />
										</div>
										<h3 class="text-2xl font-bold text-white">
											Developer Experience
										</h3>
									</div>
									<p class="text-slate-400 leading-relaxed">
										Rust core with idiomatic bindings, MVCC transactions, and
										type‑safe schemas across languages.
									</p>
								</div>
								<div class="lg:col-span-8 grid sm:grid-cols-2 gap-4">
									<Card
										title="Multi‑Language"
										description="First‑class bindings for TypeScript, Python, and more."
										icon={<Shield class="w-5 h-5" aria-hidden="true" />}
									/>
									<Card
										title="MVCC Transactions"
										description="Snapshot isolation with non‑blocking readers by default."
										icon={<GitBranch class="w-5 h-5" aria-hidden="true" />}
									/>
								</div>
							</article>
						</div>
					</div>
				</section>

				{/* Workflow - Schema to Queries */}
				<section
					class="py-28 bg-[#0b1220]/35"
					aria-labelledby="workflow-heading"
				>
					<div class="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
						<div class="text-center mb-16">
							<h2
								id="workflow-heading"
								class="text-3xl sm:text-4xl font-bold text-white text-balance"
							>
								From Schema to Query
							</h2>
							<p class="mt-4 text-lg text-slate-400 text-pretty max-w-2xl mx-auto">
								A clean two‑step workflow: define the model, then compose fast,
								readable queries.
							</p>
						</div>

						<div class="grid lg:grid-cols-12 gap-12 items-center">
							<div class="lg:col-span-5">
								<div class="flex items-center gap-3 mb-4">
									<span class="text-xs font-semibold tracking-[0.3em] text-[#2af2ff] font-mono">
										01
									</span>
									<h3 class="text-2xl font-bold text-white">
										Schema‑First Design
									</h3>
								</div>
								<p class="mt-4 text-lg text-slate-400 text-pretty leading-relaxed">
									Define your schema once, get idiomatic APIs in every language.
									Type safety where your language supports it.
								</p>
								<ul class="mt-6 space-y-3">
									<li class="flex items-start gap-3">
										<div class="flex-shrink-0 w-6 h-6 rounded-full bg-[#2af2ff]/15 icon-tile mt-0.5">
											<div class="w-2 h-2 rounded-full bg-[#2af2ff]" />
										</div>
										<span class="text-slate-300">
											Typed nodes with properties and vector embeddings
										</span>
									</li>
									<li class="flex items-start gap-3">
										<div class="flex-shrink-0 w-6 h-6 rounded-full bg-[#2af2ff]/15 icon-tile mt-0.5">
											<div class="w-2 h-2 rounded-full bg-[#2af2ff]" />
										</div>
										<span class="text-slate-300">
											Typed edges with relationship properties
										</span>
									</li>
									<li class="flex items-start gap-3">
										<div class="flex-shrink-0 w-6 h-6 rounded-full bg-[#2af2ff]/15 icon-tile mt-0.5">
											<div class="w-2 h-2 rounded-full bg-[#2af2ff]" />
										</div>
										<span class="text-slate-300">
											Single‑file storage with SQLite‑inspired simplicity
										</span>
									</li>
								</ul>
							</div>
							<div class="lg:col-span-7">
								<CodeBlock
									code={schemaCode}
									language="typescript"
									filename="schema.ts"
								/>
							</div>
						</div>

						<div class="mt-16 grid lg:grid-cols-12 gap-12 items-start">
							<div class="lg:col-span-5 order-2 lg:order-1">
								<div class="flex items-center gap-3 mb-4">
									<span class="text-xs font-semibold tracking-[0.3em] text-[#2af2ff] font-mono">
										02
									</span>
									<h3 class="text-2xl font-bold text-white">
										Intuitive Query API
									</h3>
								</div>
								<p class="mt-4 text-lg text-slate-400 text-pretty leading-relaxed">
									Fluent, chainable queries that read like the graph—traversal,
									vectors, and CRUD in one place.
								</p>
								<div class="mt-6 flex flex-wrap gap-2">
									<span class="px-3 py-1 text-xs font-semibold tracking-wide text-[#2af2ff] bg-[#2af2ff]/10 rounded-full">
										Traversal
									</span>
									<span class="px-3 py-1 text-xs font-semibold tracking-wide text-[#2af2ff] bg-[#2af2ff]/10 rounded-full">
										Vector Search
									</span>
									<span class="px-3 py-1 text-xs font-semibold tracking-wide text-[#2af2ff] bg-[#2af2ff]/10 rounded-full">
										CRUD
									</span>
								</div>
							</div>
							<div class="lg:col-span-7 order-1 lg:order-2">
								<Tabs
									items={[
										{
											label: "Traversal",
											code: traversalCode,
											language: "typescript",
										},
										{
											label: "Vector Search",
											code: vectorCode,
											language: "typescript",
										},
										{ label: "CRUD", code: crudCode, language: "typescript" },
									]}
								/>
							</div>
						</div>
					</div>
				</section>

				{/* Architecture Section - Bento Grid */}
				<section
					class="py-28 bg-[#0b1220]/35"
					aria-labelledby="architecture-heading"
				>
					<div class="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
						<div class="mb-16 max-w-xl">
							<h2
								id="architecture-heading"
								class="text-3xl sm:text-4xl font-bold text-white text-balance"
							>
								Under the Hood
							</h2>
							<p class="mt-4 text-lg text-slate-400 text-pretty">
								Purpose-built internals for maximum performance with minimal
								complexity.
							</p>
						</div>

						{/* Bento-style asymmetric grid */}
						<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
							{/* Large card - spans 2 columns */}
							<article class="md:col-span-2 lg:col-span-2 group relative p-8 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
								<div class="flex items-start gap-5">
									<div class="flex-shrink-0 w-14 h-14 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
										<Cpu class="w-7 h-7" aria-hidden="true" />
									</div>
									<div>
										<h3 class="text-xl font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
											CSR Storage Format
										</h3>
										<p class="mt-3 text-slate-400 leading-relaxed max-w-lg">
											Compressed Sparse Row format stores adjacency data
											contiguously for cache-efficient traversal. Memory-mapped
											files enable zero-copy reads with minimal memory
											footprint.
										</p>
									</div>
								</div>
							</article>

							{/* Regular card */}
							<article class="group relative p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
								<div class="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200 mb-4">
									<Terminal class="w-6 h-6" aria-hidden="true" />
								</div>
								<h3 class="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
									Rust Core
								</h3>
								<p class="mt-2 text-sm text-slate-400 leading-relaxed">
									Written in Rust for memory safety and predictable performance.
									Zero-cost FFI bindings.
								</p>
							</article>

							{/* Regular card */}
							<article class="group relative p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
								<div class="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200 mb-4">
									<Network class="w-6 h-6" aria-hidden="true" />
								</div>
								<h3 class="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
									Layered Navigation
								</h3>
								<p class="mt-2 text-sm text-slate-400 leading-relaxed">
									HNSW builds a hierarchy of proximity graphs, enabling O(log n)
									approximate nearest neighbor queries.
								</p>
							</article>

							{/* Wide card - spans 2 columns on lg */}
							<article class="md:col-span-2 group relative p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
								<div class="flex items-start gap-4">
									<div class="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
										<Box class="w-6 h-6" aria-hidden="true" />
									</div>
									<div>
										<h3 class="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
											Append-Only WAL
										</h3>
										<p class="mt-2 text-sm text-slate-400 leading-relaxed">
											Write-ahead logging ensures durability. Periodic
											compaction reclaims space while maintaining consistent
											read performance.
										</p>
									</div>
								</div>
							</article>
						</div>
					</div>
				</section>

				{/* Use Cases - Asymmetric Grid */}
				<section class="py-28" aria-labelledby="usecases-heading">
					<div class="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
						<div class="flex flex-col lg:flex-row lg:items-end lg:justify-between gap-6 mb-16">
							<div>
								<h2
									id="usecases-heading"
									class="text-3xl sm:text-4xl font-bold text-white text-balance"
								>
									Perfect For
								</h2>
								<p class="mt-4 text-lg text-slate-400 max-w-xl">
									From AI applications to local-first software, RayDB adapts to
									your needs.
								</p>
							</div>
							<Link
								to="/docs"
								class="inline-flex items-center gap-2 text-[#00d4ff] hover:text-white transition-colors font-medium"
							>
								Explore all use cases
								<ArrowRight size={16} aria-hidden="true" />
							</Link>
						</div>

						{/* Asymmetric grid: 1 featured + 3 regular */}
						<div class="grid lg:grid-cols-3 gap-6">
							{/* Featured large card */}
							<article class="lg:row-span-2 group relative p-8 rounded-2xl bg-gradient-to-br from-[#0b1220] to-[#0b1220]/60 border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
								<div class="h-full flex flex-col">
									<div class="flex-shrink-0 w-14 h-14 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 group-hover:shadow-[0_0_20px_rgba(0,212,255,0.3)] transition-[background-color,transform,box-shadow] duration-200 mb-6">
										<BookOpen class="w-7 h-7" aria-hidden="true" />
									</div>
									<h3 class="text-xl font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
										RAG Pipelines
									</h3>
									<p class="mt-4 text-slate-400 leading-relaxed flex-grow">
										Store document chunks with embeddings and traverse
										relationships for context-aware retrieval. Combine vector
										similarity with graph context for superior RAG results.
									</p>
									<div class="mt-6 pt-6 border-t border-[#1a2a42]">
										<div class="flex items-center gap-2 text-sm text-slate-500">
											<span class="w-2 h-2 rounded-full bg-[#00d4ff]" />
											Vector embeddings + Graph traversal
										</div>
									</div>
								</div>
							</article>

							{/* Regular cards */}
							<article class="group relative p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
								<div class="flex items-start gap-4">
									<div class="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
										<GitBranch class="w-6 h-6" aria-hidden="true" />
									</div>
									<div>
										<h3 class="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
											Knowledge Graphs
										</h3>
										<p class="mt-2 text-sm text-slate-400 leading-relaxed">
											Model complex relationships between entities with semantic
											similarity search.
										</p>
									</div>
								</div>
							</article>

							<article class="group relative p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
								<div class="flex items-start gap-4">
									<div class="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
										<Sparkles class="w-6 h-6" aria-hidden="true" />
									</div>
									<div>
										<h3 class="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
											Recommendations
										</h3>
										<p class="mt-2 text-sm text-slate-400 leading-relaxed">
											Combine user-item graphs with embedding similarity for
											hybrid recommendations.
										</p>
									</div>
								</div>
							</article>

							<article class="lg:col-span-2 group relative p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/40 hover:shadow-[0_0_40px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-300 speed-card">
								<div class="flex items-start gap-4">
									<div class="flex-shrink-0 w-12 h-12 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
										<Database class="w-6 h-6" aria-hidden="true" />
									</div>
									<div>
										<h3 class="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
											Local-First Apps
										</h3>
										<p class="mt-2 text-sm text-slate-400 leading-relaxed max-w-lg">
											Embedded architecture with single-file storage. No
											external database needed. Perfect for desktop apps, CLI
											tools, and edge computing.
										</p>
									</div>
								</div>
							</article>
						</div>
					</div>
				</section>

				{/* CTA Section */}
				<section class="py-28 bg-[#0b1220]/35" aria-labelledby="cta-heading">
					<div class="max-w-4xl mx-auto px-4 sm:px-6 lg:px-8 text-center">
						<h2
							id="cta-heading"
							class="text-3xl sm:text-4xl font-bold text-white text-balance"
						>
							Ready to Get Started?
						</h2>
						<p class="mt-4 text-lg text-slate-400 text-pretty">
							Build your first graph database in 5 minutes with our Quick Start
							guide.
						</p>

						<div class="mt-12 grid sm:grid-cols-2 gap-6 max-w-2xl mx-auto">
							<Link
								to="/docs/getting-started/installation"
								class="group flex items-center gap-4 p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/55 hover:shadow-[0_0_30px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-200 speed-card"
							>
								<div class="flex-shrink-0 w-14 h-14 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
									<Rocket class="w-7 h-7" aria-hidden="true" />
								</div>
								<div class="text-left min-w-0">
									<div class="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
										Installation Guide
									</div>
									<div class="text-sm text-slate-500">Set up in 2 minutes</div>
								</div>
							</Link>

							<a
								href="/docs/getting-started/quick-start"
								class="group flex items-center gap-4 p-6 rounded-2xl bg-[#0b1220] border border-[#1a2a42] hover:border-[#2af2ff]/55 hover:shadow-[0_0_30px_rgba(42,242,255,0.12)] transition-[border-color,box-shadow] duration-200 speed-card"
							>
								<div class="flex-shrink-0 w-14 h-14 icon-tile rounded-xl bg-[#00d4ff]/10 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:scale-110 transition-[background-color,transform] duration-200">
									<Code class="w-7 h-7" aria-hidden="true" />
								</div>
								<div class="text-left min-w-0">
									<div class="font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
										Quick Start Tutorial
									</div>
									<div class="text-sm text-slate-500">
										Build your first graph
									</div>
								</div>
							</a>
						</div>
					</div>
				</section>
			</main>

			{/* Footer */}
			<footer class="border-t border-[#1a2a42]/60 py-12 bg-[#05070d]">
				<div class="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
					<div class="flex flex-col sm:flex-row items-center justify-between gap-6">
						<div class="flex items-center gap-2.5">
							<Logo size={24} />
							<span class="font-semibold text-gradient">RayDB</span>
						</div>

						<p class="text-sm text-slate-500">MIT License. Built with Rust.</p>

						<div class="flex items-center gap-4">
							<a
								href="https://github.com/maskdotdev/ray"
								target="_blank"
								rel="noopener noreferrer"
								class="text-slate-500 hover:text-[#00d4ff] transition-colors duration-150"
								aria-label="RayDB on GitHub"
							>
								<svg
									class="w-5 h-5"
									fill="currentColor"
									viewBox="0 0 24 24"
									aria-hidden="true"
								>
									<path
										fill-rule="evenodd"
										d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
										clip-rule="evenodd"
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
