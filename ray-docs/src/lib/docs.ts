// Documentation structure and utilities

export interface DocPage {
  title: string;
  description: string;
  slug: string;
  content?: string;
}

export interface DocSection {
  label: string;
  collapsed?: boolean;
  items: DocPage[];
}

export const docsStructure: DocSection[] = [
  {
    label: "Getting Started",
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
        title: "Quick Start",
        description: "Build your first graph database in 5 minutes",
        slug: "getting-started/quick-start",
      },
    ],
  },
  {
    label: "Guides",
    items: [
      {
        title: "Schema Definition",
        description: "Define type-safe node and edge schemas",
        slug: "guides/schema",
      },
      {
        title: "Queries & CRUD",
        description: "Create, read, update, delete operations",
        slug: "guides/queries",
      },
      {
        title: "Graph Traversal",
        description: "Navigate relationships in your graph",
        slug: "guides/traversal",
      },
      {
        title: "Vector Search",
        description: "Semantic similarity search with embeddings",
        slug: "guides/vectors",
      },
      {
        title: "Transactions",
        description: "ACID transactions and isolation levels",
        slug: "guides/transactions",
      },
      {
        title: "Concurrency",
        description: "Multi-threaded access and parallel reads",
        slug: "guides/concurrency",
      },
    ],
  },
  {
    label: "API Reference",
    items: [
      {
        title: "High-Level API",
        description: "Drizzle-style fluent API",
        slug: "api/high-level",
      },
      {
        title: "Low-Level API",
        description: "Direct storage access",
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
        title: "Graph Benchmarks",
        description: "Graph database performance",
        slug: "benchmarks/graph",
      },
      {
        title: "Vector Benchmarks",
        description: "Vector search performance",
        slug: "benchmarks/vector",
      },
      {
        title: "Cross-Language",
        description: "Bindings performance comparison",
        slug: "benchmarks/cross-language",
      },
    ],
  },
  {
    label: "Deep Dive",
    collapsed: true,
    items: [
      {
        title: "Architecture",
        description: "Internal design and data structures",
        slug: "internals/architecture",
      },
      {
        title: "CSR Format",
        description: "Compressed Sparse Row storage",
        slug: "internals/csr",
      },
      {
        title: "Performance",
        description: "Optimization techniques",
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
