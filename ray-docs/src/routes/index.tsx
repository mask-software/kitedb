import { createFileRoute, Link } from "@tanstack/solid-router";
import { For, createSignal, onMount, onCleanup } from "solid-js";
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
	Activity,
	Server,
	Layers,
} from "lucide-solid";
import Logo from "~/components/Logo";
import ThemeToggle from "~/components/ThemeToggle";
import CodeBlock from "~/components/CodeBlock";
import { Tabs } from "~/components/Tabs";

export const Route = createFileRoute("/")({
	component: HomePage,
});

// Console-style stat card
function ConsoleStat(props: { label: string; value: string; unit?: string }) {
	return (
		<div class="console-container p-4 hover:scale-[1.02] transition-transform duration-200">
			<div class="console-scanlines" aria-hidden="true" />
			<div class="relative">
				<div class="terminal-stat-label mb-1">{props.label}</div>
				<div class="terminal-stat-value flex items-baseline gap-1">
					{props.value}
					{props.unit && (
						<span class="text-sm text-slate-500">{props.unit}</span>
					)}
				</div>
			</div>
		</div>
	);
}

// Electric feature card
function ElectricCard(props: {
	title: string;
	description: string;
	icon: any;
}) {
	return (
		<article class="group relative p-6 rounded-xl console-container electric-glow transition-all duration-300">
			<div class="console-scanlines opacity-10" aria-hidden="true" />
			<div class="relative flex items-start gap-4">
				<div class="flex-shrink-0 w-12 h-12 icon-tile rounded-lg bg-[#00d4ff]/10 border border-[#00d4ff]/20 text-[#00d4ff] group-hover:bg-[#00d4ff]/20 group-hover:border-[#00d4ff]/40 group-hover:shadow-[0_0_20px_rgba(0,212,255,0.4)] transition-all duration-300">
					{props.icon}
				</div>
				<div class="min-w-0">
					<h3 class="font-mono font-semibold text-white group-hover:text-[#00d4ff] transition-colors duration-150">
						{props.title}
					</h3>
					<p class="mt-2 text-sm text-slate-400 leading-relaxed">
						{props.description}
					</p>
				</div>
			</div>
			{/* Electric border effect on hover */}
			<div
				class="absolute inset-0 rounded-xl opacity-0 group-hover:opacity-100 transition-opacity duration-300 electric-border pointer-events-none"
				aria-hidden="true"
			/>
		</article>
	);
}

function HomePage() {
	const [typedText, setTypedText] = createSignal("");
	const fullText = "High-performance embedded graph database with vector search";
	let typingInterval: ReturnType<typeof setInterval>;

	onMount(() => {
		let i = 0;
		typingInterval = setInterval(() => {
			if (i < fullText.length) {
				setTypedText(fullText.slice(0, i + 1));
				i++;
			} else {
				clearInterval(typingInterval);
			}
		}, 40);
	});

	onCleanup(() => {
		if (typingInterval) clearInterval(typingInterval);
	});

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
		<div class="min-h-screen bg-[#030712] circuit-pattern">
			{/* Skip link */}
			<a
				href="#main-content"
				class="sr-only focus:not-sr-only focus:absolute focus:top-4 focus:left-4 focus:z-50 focus:px-4 focus:py-2 focus:bg-[#00d4ff] focus:text-black focus:rounded-lg focus:font-semibold"
			>
				Skip to main content
			</a>

			{/* Electric background effects */}
			<div class="fixed inset-0 pointer-events-none z-0" aria-hidden="true">
				{/* Gradient orbs */}
				<div class="absolute top-0 left-1/4 w-[600px] h-[600px] bg-[#00d4ff]/5 rounded-full blur-[120px] animate-glow-pulse" />
				<div class="absolute bottom-0 right-1/4 w-[500px] h-[500px] bg-[#7c3aed]/5 rounded-full blur-[100px] animate-glow-pulse animate-delay-200" />
				{/* Scanlines overlay */}
				<div class="absolute inset-0 console-scanlines opacity-20" />
			</div>

			{/* Console-style Header */}
			<header class="sticky top-0 z-50 border-b border-[#1a2a42]/70 bg-[#030712]/90 backdrop-blur-xl">
				<nav
					class="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8"
					aria-label="Main navigation"
				>
					<div class="flex items-center justify-between h-14">
						{/* Terminal-style logo */}
						<Link
							to="/"
							class="flex items-center gap-3 group"
							aria-label="RayDB Home"
						>
							<div class="flex items-center gap-2 px-3 py-1.5 rounded-md bg-[#0a1628] border border-[#1a2a42] group-hover:border-[#00d4ff]/50 transition-colors">
								<span class="text-[#00d4ff] font-mono text-sm">❯</span>
								<Logo size={20} />
								<span class="font-mono font-bold text-white">raydb</span>
								<span class="console-cursor h-3 w-1.5" aria-hidden="true" />
							</div>
						</Link>

						<div class="hidden md:flex items-center gap-1 font-mono text-sm">
							<Link
								to="/docs"
								class="px-3 py-1.5 text-slate-400 hover:text-[#00d4ff] hover:bg-[#00d4ff]/5 rounded transition-colors duration-150"
							>
								./docs
							</Link>
							<a
								href="/docs/api/high-level"
								class="px-3 py-1.5 text-slate-400 hover:text-[#00d4ff] hover:bg-[#00d4ff]/5 rounded transition-colors duration-150"
							>
								./api
							</a>
							<a
								href="/docs/benchmarks"
								class="px-3 py-1.5 text-slate-400 hover:text-[#00d4ff] hover:bg-[#00d4ff]/5 rounded transition-colors duration-150"
							>
								./bench
							</a>
						</div>

						<div class="flex items-center gap-2">
							<ThemeToggle />
							<a
								href="https://github.com/maskdotdev/ray"
								target="_blank"
								rel="noopener noreferrer"
								class="flex items-center gap-2 px-3 py-1.5 rounded-md text-slate-400 hover:text-white bg-[#0a1628] border border-[#1a2a42] hover:border-[#00d4ff]/50 transition-colors duration-150 font-mono text-sm"
								aria-label="View RayDB on GitHub"
							>
								<svg
									class="w-4 h-4"
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
								<span class="hidden sm:inline">clone</span>
							</a>
						</div>
					</div>
				</nav>
				{/* Electric top border */}
				<div
					class="absolute bottom-0 left-0 right-0 h-px bg-gradient-to-r from-transparent via-[#00d4ff] to-transparent opacity-50"
					aria-hidden="true"
				/>
			</header>

			<main id="main-content">
				{/* Hero Section - Console Style */}
				<section
					class="relative pt-16 pb-24 sm:pt-24 sm:pb-32 overflow-hidden"
					aria-labelledby="hero-heading"
				>
					<div class="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
						{/* Main console window */}
						<div class="console-container max-w-4xl mx-auto">
							<div class="console-header">
								<div class="console-dots">
									<div class="console-dot console-dot--red" />
									<div class="console-dot console-dot--yellow" />
									<div class="console-dot console-dot--green" />
								</div>
								<div class="console-title">raydb — bash — 120×40</div>
								<div class="w-12" /> {/* Spacer for symmetry */}
							</div>

							<div class="p-6 sm:p-8 space-y-4 font-mono">
								{/* ASCII Art Logo */}
								<pre
									class="ascii-art text-center hidden sm:block select-none"
									aria-hidden="true"
								>
									{`
  ██████╗  █████╗ ██╗   ██╗██████╗ ██████╗ 
  ██╔══██╗██╔══██╗╚██╗ ██╔╝██╔══██╗██╔══██╗
  ██████╔╝███████║ ╚████╔╝ ██║  ██║██████╔╝
  ██╔══██╗██╔══██║  ╚██╔╝  ██║  ██║██╔══██╗
  ██║  ██║██║  ██║   ██║   ██████╔╝██████╔╝
  ╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝   ╚═════╝ ╚═════╝ 
                                            `}
								</pre>

								{/* Mobile logo */}
								<div class="sm:hidden text-center">
									<h1 class="text-4xl font-black electric-text">RAYDB</h1>
								</div>

								{/* Version info */}
								<div class="flex items-center justify-center gap-4 text-sm text-slate-500">
									<span>v0.1.0</span>
									<span class="text-[#28c840]">● online</span>
									<span>rust core</span>
								</div>

								{/* Typing effect tagline */}
								<div class="text-center py-4">
									<h1 id="hero-heading" class="sr-only">
										RayDB - The Graph Database Built for Speed
									</h1>
									<p class="text-lg sm:text-xl text-slate-300">
										<span class="text-[#00d4ff]">❯</span> {typedText()}
										<span
											class="console-cursor inline-block w-2 h-5 ml-1 align-middle"
											aria-hidden="true"
										/>
									</p>
								</div>

								{/* Command line install */}
								<div class="bg-[#0a1628] rounded-lg p-4 border border-[#1a2a42]">
									<div class="flex items-center gap-3 flex-wrap">
										<span class="text-[#00d4ff]">$</span>
										<span class="text-[#febc2e]">bun</span>
										<span class="text-white">add</span>
										<span class="text-[#28c840]">@ray-db/ray</span>
										<button
											type="button"
											class="ml-auto px-3 py-1 text-xs rounded bg-[#1a2a42] text-slate-400 hover:text-[#00d4ff] hover:bg-[#1a2a42]/80 transition-colors"
											aria-label="Copy install command"
											onClick={() =>
												navigator.clipboard.writeText("bun add @ray-db/ray")
											}
										>
											copy
										</button>
									</div>
								</div>

								{/* CTA buttons */}
								<div class="flex flex-col sm:flex-row items-center justify-center gap-4 pt-4">
									<Link
										to="/docs/getting-started/installation"
										class="group relative w-full sm:w-auto inline-flex items-center justify-center gap-2 px-6 py-3 font-mono font-semibold text-black bg-[#00d4ff] rounded-lg overflow-hidden hover:shadow-[0_0_30px_rgba(0,212,255,0.5)] transition-shadow duration-300 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white focus-visible:ring-offset-2 focus-visible:ring-offset-[#030712]"
									>
										<span class="relative z-10 flex items-center gap-2">
											./install
											<ArrowRight
												size={16}
												class="group-hover:translate-x-0.5 transition-transform"
												aria-hidden="true"
											/>
										</span>
										{/* Electric shimmer */}
										<div
											class="absolute inset-0 bg-gradient-to-r from-transparent via-white/20 to-transparent -translate-x-full group-hover:translate-x-full transition-transform duration-700"
											aria-hidden="true"
										/>
									</Link>
									<a
										href="https://github.com/maskdotdev/ray"
										target="_blank"
										rel="noopener noreferrer"
										class="w-full sm:w-auto inline-flex items-center justify-center gap-2 px-6 py-3 font-mono text-slate-300 bg-[#0a1628] border border-[#1a2a42] rounded-lg hover:border-[#00d4ff]/50 hover:text-[#00d4ff] transition-colors duration-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[#00d4ff] focus-visible:ring-offset-2 focus-visible:ring-offset-[#030712]"
									>
										<svg
											class="w-4 h-4"
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
										git clone
									</a>
								</div>
							</div>
						</div>
					</div>
				</section>

				{/* Stats Section - Console Metrics */}
				<section
					class="py-16 border-y border-[#1a2a42]/50"
					aria-labelledby="stats-heading"
				>
					<h2 id="stats-heading" class="sr-only">
						Performance Statistics
					</h2>
					<div class="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
						<div class="flex items-center gap-3 mb-8">
							<Activity size={20} class="text-[#00d4ff]" aria-hidden="true" />
							<span class="font-mono text-sm text-slate-400">
								SYSTEM_METRICS
							</span>
							<div class="flex-1 h-px bg-gradient-to-r from-[#1a2a42] to-transparent" />
						</div>

						<div class="grid grid-cols-2 lg:grid-cols-4 gap-4">
							<ConsoleStat label="NODE_LOOKUP" value="~125" unit="ns" />
							<ConsoleStat label="1_HOP_TRAVERSAL" value="~1.1" unit="μs" />
							<ConsoleStat label="DEPENDENCIES" value="0" />
							<ConsoleStat label="CORE" value="Rust" />
						</div>

						{/* Voltage bar */}
						<div class="mt-8 max-w-2xl mx-auto">
							<div class="flex items-center justify-between text-xs font-mono text-slate-500 mb-2">
								<span>PERFORMANCE_LEVEL</span>
								<span class="text-[#00d4ff]">118× faster than Memgraph</span>
							</div>
							<div class="voltage-bar">
								<div class="voltage-bar-fill" style="width: 95%" />
							</div>
						</div>
					</div>
				</section>

				{/* Features - Electric Cards */}
				<section class="py-20" aria-labelledby="features-heading">
					<div class="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
						<div class="flex items-center gap-3 mb-12">
							<Zap size={20} class="text-[#00d4ff]" aria-hidden="true" />
							<span class="font-mono text-sm text-slate-400">
								CORE_FEATURES
							</span>
							<div class="flex-1 h-px bg-gradient-to-r from-[#1a2a42] to-transparent" />
						</div>

						{/* Feature sections */}
						<div class="space-y-16">
							{/* Section 1 */}
							<div>
								<div class="flex items-center gap-3 mb-6">
									<span class="font-mono text-xs text-[#00d4ff] px-2 py-1 rounded bg-[#00d4ff]/10 border border-[#00d4ff]/20">
										01
									</span>
									<h3 class="text-xl font-bold text-white font-mono">
										UNIFIED_DATA_MODEL
									</h3>
								</div>
								<p class="text-slate-400 mb-6 max-w-2xl">
									Combine graph relationships and vector similarity in one
									coherent API—no glue code, no extra services.
								</p>
								<div class="grid sm:grid-cols-2 gap-4">
									<ElectricCard
										title="Graph + Vector"
										description="Traverse relationships and run similarity search in the same query chain."
										icon={<Database class="w-5 h-5" aria-hidden="true" />}
									/>
									<ElectricCard
										title="HNSW Vector Index"
										description="Log-time nearest neighbor search with high recall at scale."
										icon={<Search class="w-5 h-5" aria-hidden="true" />}
									/>
								</div>
							</div>

							{/* Section 2 */}
							<div>
								<div class="flex items-center gap-3 mb-6">
									<span class="font-mono text-xs text-[#00d4ff] px-2 py-1 rounded bg-[#00d4ff]/10 border border-[#00d4ff]/20">
										02
									</span>
									<h3 class="text-xl font-bold text-white font-mono">
										BLAZING_PERFORMANCE
									</h3>
								</div>
								<p class="text-slate-400 mb-6 max-w-2xl">
									Memory-mapped storage + zero-copy reads keep latency ultra-low
									without external processes.
								</p>
								<div class="grid sm:grid-cols-2 gap-4">
									<ElectricCard
										title="~125ns Lookups"
										description="~125ns node lookups, ~1.1μs traversals. 118× faster than Memgraph."
										icon={<Zap class="w-5 h-5" aria-hidden="true" />}
									/>
									<ElectricCard
										title="Zero Dependencies"
										description="Single-file storage that's easy to back up, sync, and deploy."
										icon={<Sparkles class="w-5 h-5" aria-hidden="true" />}
									/>
								</div>
							</div>

							{/* Section 3 */}
							<div>
								<div class="flex items-center gap-3 mb-6">
									<span class="font-mono text-xs text-[#00d4ff] px-2 py-1 rounded bg-[#00d4ff]/10 border border-[#00d4ff]/20">
										03
									</span>
									<h3 class="text-xl font-bold text-white font-mono">
										DEVELOPER_EXPERIENCE
									</h3>
								</div>
								<p class="text-slate-400 mb-6 max-w-2xl">
									Rust core with idiomatic bindings, MVCC transactions, and
									type-safe schemas across languages.
								</p>
								<div class="grid sm:grid-cols-2 gap-4">
									<ElectricCard
										title="Multi-Language"
										description="First-class bindings for TypeScript, Python, and more."
										icon={<Shield class="w-5 h-5" aria-hidden="true" />}
									/>
									<ElectricCard
										title="MVCC Transactions"
										description="Snapshot isolation with non-blocking readers by default."
										icon={<GitBranch class="w-5 h-5" aria-hidden="true" />}
									/>
								</div>
							</div>
						</div>
					</div>
				</section>

				{/* Code Examples - Console Style */}
				<section
					class="py-20 bg-[#050810]/50"
					aria-labelledby="workflow-heading"
				>
					<div class="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
						<div class="flex items-center gap-3 mb-12">
							<Terminal size={20} class="text-[#00d4ff]" aria-hidden="true" />
							<span class="font-mono text-sm text-slate-400">CODE_EXAMPLES</span>
							<div class="flex-1 h-px bg-gradient-to-r from-[#1a2a42] to-transparent" />
						</div>

						<div class="grid lg:grid-cols-12 gap-12 items-start">
							<div class="lg:col-span-5">
								<div class="flex items-center gap-3 mb-4">
									<span class="font-mono text-xs text-[#00d4ff] px-2 py-1 rounded bg-[#00d4ff]/10 border border-[#00d4ff]/20">
										01
									</span>
									<h2
										id="workflow-heading"
										class="text-xl font-bold text-white font-mono"
									>
										SCHEMA_FIRST
									</h2>
								</div>
								<p class="text-slate-400 leading-relaxed mb-6">
									Define your schema once, get idiomatic APIs in every language.
									Type safety where your language supports it.
								</p>
								<ul class="space-y-3">
									<li class="flex items-start gap-3 font-mono text-sm">
										<span class="text-[#28c840]">✓</span>
										<span class="text-slate-300">
											Typed nodes with vector embeddings
										</span>
									</li>
									<li class="flex items-start gap-3 font-mono text-sm">
										<span class="text-[#28c840]">✓</span>
										<span class="text-slate-300">
											Typed edges with properties
										</span>
									</li>
									<li class="flex items-start gap-3 font-mono text-sm">
										<span class="text-[#28c840]">✓</span>
										<span class="text-slate-300">
											Single-file storage
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
									<span class="font-mono text-xs text-[#00d4ff] px-2 py-1 rounded bg-[#00d4ff]/10 border border-[#00d4ff]/20">
										02
									</span>
									<h3 class="text-xl font-bold text-white font-mono">
										QUERY_API
									</h3>
								</div>
								<p class="text-slate-400 leading-relaxed mb-6">
									Fluent, chainable queries that read like the graph—traversal,
									vectors, and CRUD in one place.
								</p>
								<div class="flex flex-wrap gap-2">
									<span class="px-3 py-1 text-xs font-mono text-[#00d4ff] bg-[#00d4ff]/10 rounded border border-[#00d4ff]/20">
										traversal
									</span>
									<span class="px-3 py-1 text-xs font-mono text-[#7c3aed] bg-[#7c3aed]/10 rounded border border-[#7c3aed]/20">
										vector_search
									</span>
									<span class="px-3 py-1 text-xs font-mono text-[#28c840] bg-[#28c840]/10 rounded border border-[#28c840]/20">
										crud
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

				{/* Architecture Section */}
				<section class="py-20" aria-labelledby="architecture-heading">
					<div class="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
						<div class="flex items-center gap-3 mb-12">
							<Cpu size={20} class="text-[#00d4ff]" aria-hidden="true" />
							<span class="font-mono text-sm text-slate-400">ARCHITECTURE</span>
							<div class="flex-1 h-px bg-gradient-to-r from-[#1a2a42] to-transparent" />
						</div>

						<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
							{/* Large card */}
							<article class="md:col-span-2 console-container p-6 group">
								<div class="console-scanlines opacity-10" aria-hidden="true" />
								<div class="relative flex items-start gap-5">
									<div class="flex-shrink-0 w-14 h-14 icon-tile rounded-xl bg-[#00d4ff]/10 border border-[#00d4ff]/20 text-[#00d4ff] group-hover:shadow-[0_0_20px_rgba(0,212,255,0.3)] transition-all">
										<Layers class="w-7 h-7" aria-hidden="true" />
									</div>
									<div>
										<h3 class="text-lg font-mono font-semibold text-white group-hover:text-[#00d4ff] transition-colors">
											CSR_STORAGE_FORMAT
										</h3>
										<p class="mt-2 text-slate-400 leading-relaxed max-w-lg text-sm">
											Compressed Sparse Row format stores adjacency data
											contiguously for cache-efficient traversal. Memory-mapped
											files enable zero-copy reads.
										</p>
									</div>
								</div>
							</article>

							{/* Small cards */}
							<article class="console-container p-5 group">
								<div class="console-scanlines opacity-10" aria-hidden="true" />
								<div class="relative">
									<div class="w-12 h-12 icon-tile rounded-lg bg-[#00d4ff]/10 border border-[#00d4ff]/20 text-[#00d4ff] mb-4 group-hover:shadow-[0_0_15px_rgba(0,212,255,0.3)] transition-all">
										<Server class="w-6 h-6" aria-hidden="true" />
									</div>
									<h3 class="font-mono font-semibold text-white text-sm group-hover:text-[#00d4ff] transition-colors">
										RUST_CORE
									</h3>
									<p class="mt-2 text-xs text-slate-400">
										Memory safety and predictable performance with zero-cost FFI.
									</p>
								</div>
							</article>

							<article class="console-container p-5 group">
								<div class="console-scanlines opacity-10" aria-hidden="true" />
								<div class="relative">
									<div class="w-12 h-12 icon-tile rounded-lg bg-[#00d4ff]/10 border border-[#00d4ff]/20 text-[#00d4ff] mb-4 group-hover:shadow-[0_0_15px_rgba(0,212,255,0.3)] transition-all">
										<Network class="w-6 h-6" aria-hidden="true" />
									</div>
									<h3 class="font-mono font-semibold text-white text-sm group-hover:text-[#00d4ff] transition-colors">
										HNSW_INDEX
									</h3>
									<p class="mt-2 text-xs text-slate-400">
										O(log n) approximate nearest neighbor queries.
									</p>
								</div>
							</article>

							<article class="md:col-span-2 console-container p-5 group">
								<div class="console-scanlines opacity-10" aria-hidden="true" />
								<div class="relative flex items-start gap-4">
									<div class="w-12 h-12 icon-tile rounded-lg bg-[#00d4ff]/10 border border-[#00d4ff]/20 text-[#00d4ff] group-hover:shadow-[0_0_15px_rgba(0,212,255,0.3)] transition-all">
										<Box class="w-6 h-6" aria-hidden="true" />
									</div>
									<div>
										<h3 class="font-mono font-semibold text-white text-sm group-hover:text-[#00d4ff] transition-colors">
											APPEND_ONLY_WAL
										</h3>
										<p class="mt-2 text-xs text-slate-400">
											Write-ahead logging for durability. Periodic compaction
											reclaims space.
										</p>
									</div>
								</div>
							</article>
						</div>
					</div>
				</section>

				{/* Use Cases */}
				<section
					class="py-20 bg-[#050810]/50"
					aria-labelledby="usecases-heading"
				>
					<div class="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
						<div class="flex items-center gap-3 mb-12">
							<Rocket size={20} class="text-[#00d4ff]" aria-hidden="true" />
							<span class="font-mono text-sm text-slate-400">USE_CASES</span>
							<div class="flex-1 h-px bg-gradient-to-r from-[#1a2a42] to-transparent" />
						</div>

						<div class="grid md:grid-cols-2 lg:grid-cols-3 gap-4">
							{/* Featured card */}
							<article class="lg:row-span-2 console-container p-6 group flex flex-col">
								<div class="console-scanlines opacity-10" aria-hidden="true" />
								<div class="relative flex-1">
									<div class="w-14 h-14 icon-tile rounded-xl bg-[#7c3aed]/10 border border-[#7c3aed]/20 text-[#7c3aed] mb-6 group-hover:shadow-[0_0_20px_rgba(124,58,237,0.4)] transition-all">
										<BookOpen class="w-7 h-7" aria-hidden="true" />
									</div>
									<h3 class="text-lg font-mono font-semibold text-white group-hover:text-[#7c3aed] transition-colors">
										RAG_PIPELINES
									</h3>
									<p class="mt-4 text-sm text-slate-400 leading-relaxed">
										Store document chunks with embeddings and traverse
										relationships for context-aware retrieval. Combine vector
										similarity with graph context for superior RAG results.
									</p>
									<div class="mt-6 pt-6 border-t border-[#1a2a42]">
										<div class="flex items-center gap-2 text-xs font-mono text-slate-500">
											<span class="w-2 h-2 rounded-full bg-[#7c3aed]" />
											Vector embeddings + Graph traversal
										</div>
									</div>
								</div>
							</article>

							<article class="console-container p-5 group">
								<div class="console-scanlines opacity-10" aria-hidden="true" />
								<div class="relative flex items-start gap-4">
									<div class="w-12 h-12 icon-tile rounded-lg bg-[#28c840]/10 border border-[#28c840]/20 text-[#28c840] group-hover:shadow-[0_0_15px_rgba(40,200,64,0.4)] transition-all">
										<GitBranch class="w-6 h-6" aria-hidden="true" />
									</div>
									<div>
										<h3 class="font-mono font-semibold text-white text-sm group-hover:text-[#28c840] transition-colors">
											KNOWLEDGE_GRAPHS
										</h3>
										<p class="mt-2 text-xs text-slate-400">
											Model complex relationships with semantic similarity.
										</p>
									</div>
								</div>
							</article>

							<article class="console-container p-5 group">
								<div class="console-scanlines opacity-10" aria-hidden="true" />
								<div class="relative flex items-start gap-4">
									<div class="w-12 h-12 icon-tile rounded-lg bg-[#febc2e]/10 border border-[#febc2e]/20 text-[#febc2e] group-hover:shadow-[0_0_15px_rgba(254,188,46,0.4)] transition-all">
										<Sparkles class="w-6 h-6" aria-hidden="true" />
									</div>
									<div>
										<h3 class="font-mono font-semibold text-white text-sm group-hover:text-[#febc2e] transition-colors">
											RECOMMENDATIONS
										</h3>
										<p class="mt-2 text-xs text-slate-400">
											Hybrid user-item graphs with embedding similarity.
										</p>
									</div>
								</div>
							</article>

							<article class="md:col-span-2 console-container p-5 group">
								<div class="console-scanlines opacity-10" aria-hidden="true" />
								<div class="relative flex items-start gap-4">
									<div class="w-12 h-12 icon-tile rounded-lg bg-[#ff5f57]/10 border border-[#ff5f57]/20 text-[#ff5f57] group-hover:shadow-[0_0_15px_rgba(255,95,87,0.4)] transition-all">
										<Database class="w-6 h-6" aria-hidden="true" />
									</div>
									<div>
										<h3 class="font-mono font-semibold text-white text-sm group-hover:text-[#ff5f57] transition-colors">
											LOCAL_FIRST_APPS
										</h3>
										<p class="mt-2 text-xs text-slate-400 max-w-md">
											Embedded architecture with single-file storage. Perfect
											for desktop apps, CLI tools, and edge computing.
										</p>
									</div>
								</div>
							</article>
						</div>
					</div>
				</section>

				{/* CTA Section */}
				<section class="py-20" aria-labelledby="cta-heading">
					<div class="max-w-4xl mx-auto px-4 sm:px-6 lg:px-8">
						<div class="console-container p-8 sm:p-12 text-center">
							<div class="console-scanlines opacity-10" aria-hidden="true" />
							<div class="relative">
								<pre
									class="ascii-art text-center mb-6 hidden sm:block select-none"
									aria-hidden="true"
								>
									{`
  ⚡ READY TO START? ⚡
                        `}
								</pre>
								<h2
									id="cta-heading"
									class="text-2xl sm:text-3xl font-mono font-bold text-white sm:hidden"
								>
									Ready to Start?
								</h2>
								<p class="mt-4 text-slate-400 font-mono text-sm max-w-md mx-auto">
									Build your first graph database in 5 minutes with our Quick
									Start guide.
								</p>

								<div class="mt-10 grid sm:grid-cols-2 gap-4 max-w-lg mx-auto">
									<Link
										to="/docs/getting-started/installation"
										class="group flex items-center gap-4 p-4 rounded-lg bg-[#0a1628] border border-[#1a2a42] hover:border-[#00d4ff]/50 transition-colors"
									>
										<div class="w-12 h-12 icon-tile rounded-lg bg-[#00d4ff]/10 border border-[#00d4ff]/20 text-[#00d4ff] group-hover:shadow-[0_0_15px_rgba(0,212,255,0.4)] transition-all">
											<Rocket class="w-6 h-6" aria-hidden="true" />
										</div>
										<div class="text-left">
											<div class="font-mono text-sm font-semibold text-white group-hover:text-[#00d4ff] transition-colors">
												./install
											</div>
											<div class="text-xs text-slate-500">2 min setup</div>
										</div>
									</Link>

									<a
										href="/docs/getting-started/quick-start"
										class="group flex items-center gap-4 p-4 rounded-lg bg-[#0a1628] border border-[#1a2a42] hover:border-[#7c3aed]/50 transition-colors"
									>
										<div class="w-12 h-12 icon-tile rounded-lg bg-[#7c3aed]/10 border border-[#7c3aed]/20 text-[#7c3aed] group-hover:shadow-[0_0_15px_rgba(124,58,237,0.4)] transition-all">
											<Code class="w-6 h-6" aria-hidden="true" />
										</div>
										<div class="text-left">
											<div class="font-mono text-sm font-semibold text-white group-hover:text-[#7c3aed] transition-colors">
												./quickstart
											</div>
											<div class="text-xs text-slate-500">First graph</div>
										</div>
									</a>
								</div>
							</div>
						</div>
					</div>
				</section>
			</main>

			{/* Footer */}
			<footer class="border-t border-[#1a2a42]/50 py-8 bg-[#030712]">
				<div class="max-w-6xl mx-auto px-4 sm:px-6 lg:px-8">
					<div class="flex flex-col sm:flex-row items-center justify-between gap-4 font-mono text-sm">
						<div class="flex items-center gap-3">
							<span class="text-[#00d4ff]">❯</span>
							<Logo size={20} />
							<span class="text-slate-400">raydb</span>
							<span class="text-slate-600">v0.1.0</span>
						</div>

						<p class="text-slate-500">
							MIT License • Built with{" "}
							<span class="text-[#ff5f57]">Rust</span>
						</p>

						<a
							href="https://github.com/maskdotdev/ray"
							target="_blank"
							rel="noopener noreferrer"
							class="text-slate-500 hover:text-[#00d4ff] transition-colors"
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
			</footer>
		</div>
	);
}
