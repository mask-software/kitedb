// Documentation structure and utilities

export interface DocPage {
	title: string;
	description: string;
	slug: string;
	content?: string;
}

export interface DocSection {
	label: string;
	items: DocPage[];
}

export const docsStructure: DocSection[] = [
	{
		label: "Getting started",
		items: [
			{
				title: "Introduction",
				description:
					"High-performance embedded graph database with vector search",
				slug: "",
			},
			{
				title: "Installation",
				description: "How to install KiteDB in your project",
				slug: "getting-started/installation",
			},
			{
				title: "Quick start",
				description: "Build your first graph database in 5 minutes",
				slug: "getting-started/quick-start",
			},
		],
	},
	{
		label: "Guides",
		items: [
			{
				title: "Schema definition",
				description: "Define type-safe node and edge schemas",
				slug: "guides/schema",
			},
			{
				title: "Queries & CRUD",
				description: "Create, read, update, delete operations",
				slug: "guides/queries",
			},
			{
				title: "Graph traversal",
				description: "Navigate relationships in your graph",
				slug: "guides/traversal",
			},
			{
				title: "Vector search",
				description: "Semantic similarity search with embeddings",
				slug: "guides/vectors",
			},
			{
				title: "Transactions",
				description:
					"Atomic writes, snapshot isolation, conflicts, and bulk load",
				slug: "guides/transactions",
			},
			{
				title: "Performance checklist",
				description: "Choose the fastest write path and config presets",
				slug: "guides/performance",
			},
			{
				title: "Concurrency",
				description: "Parallel readers, concurrent writers, and MVCC",
				slug: "guides/concurrency",
			},
		],
	},
	{
		label: "API reference",
		items: [
			{
				title: "High-level API",
				description: "Drizzle-style fluent API",
				slug: "api/high-level",
			},
			{
				title: "Low-level API",
				description: "Direct database primitives",
				slug: "api/low-level",
			},
			{
				title: "Vector API",
				description: "Embedding and similarity search",
				slug: "api/vector-api",
			},
		],
	},
	{
		label: "Benchmarks",
		items: [
			{
				title: "Overview",
				description: "Performance benchmarks overview",
				slug: "benchmarks",
			},
			{
				title: "Graph benchmarks",
				description: "Graph database performance",
				slug: "benchmarks/graph",
			},
			{
				title: "Vector benchmarks",
				description: "Vector search performance",
				slug: "benchmarks/vector",
			},
			{
				title: "Cross-language",
				description: "Bindings performance comparison",
				slug: "benchmarks/cross-language",
			},
		],
	},
	{
		label: "Internals",
		items: [
			{
				title: "Architecture",
				description: "How KiteDB is structured internally",
				slug: "internals/architecture",
			},
			{
				title: "Snapshot and delta",
				description: "The core storage model",
				slug: "internals/snapshot-delta",
			},
			{
				title: "CSR format",
				description: "How edges are stored for fast traversal",
				slug: "internals/csr",
			},
			{
				title: "Single-file format",
				description: "The .kitedb file layout",
				slug: "internals/single-file",
			},
			{
				title: "WAL and durability",
				description: "Crash recovery and write-ahead logging",
				slug: "internals/wal",
			},
			{
				title: "MVCC and transactions",
				description: "Concurrent access and isolation",
				slug: "internals/mvcc",
			},
			{
				title: "Key index",
				description: "Fast node lookups by key",
				slug: "internals/key-index",
			},
			{
				title: "Performance",
				description: "How KiteDB keeps reads and writes fast",
				slug: "internals/performance",
			},
		],
	},
];

export function findDocBySlug(slug: string): DocPage | undefined {
	for (const section of docsStructure) {
		const page = section.items.find((item) => item.slug === slug);
		if (page) return page;
	}
	return undefined;
}

export function findSectionBySlug(slug: string): DocSection | undefined {
	for (const section of docsStructure) {
		if (section.items.some((item) => item.slug === slug)) {
			return section;
		}
	}
	return undefined;
}

export function getNextDoc(currentSlug: string): DocPage | undefined {
	const allDocs = docsStructure.flatMap((s) => s.items);
	const currentIndex = allDocs.findIndex((d) => d.slug === currentSlug);
	return currentIndex >= 0 && currentIndex < allDocs.length - 1
		? allDocs[currentIndex + 1]
		: undefined;
}

export function getPrevDoc(currentSlug: string): DocPage | undefined {
	const allDocs = docsStructure.flatMap((s) => s.items);
	const currentIndex = allDocs.findIndex((d) => d.slug === currentSlug);
	return currentIndex > 0 ? allDocs[currentIndex - 1] : undefined;
}
