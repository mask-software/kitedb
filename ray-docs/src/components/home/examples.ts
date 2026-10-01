import type { Language } from "~/lib/language-store";

export type LangId = Language["id"];
export type Snippet = Record<LangId, string>;

export const SHIKI_LANG: Record<LangId, string> = {
	typescript: "typescript",
	python: "python",
	rust: "rust",
};

export const FILE_EXT: Record<LangId, string> = {
	typescript: "ts",
	python: "py",
	rust: "rs",
};

// ---------------------------------------------------------------------------
// Hero "query theater" scenes. Each language lists which code lines light up
// at each animation step, so the graph and the code advance together.
// ---------------------------------------------------------------------------

export interface SceneCode {
	code: string;
	/** stepLines[step] = zero-based line indexes emphasized during that step */
	stepLines: number[][];
}

export interface Scene {
	id: "traverse" | "vector";
	label: string;
	file: string;
	captions: string[];
	result: { label: string; items: { text: string; meta?: string }[] };
	code: Record<LangId, SceneCode>;
}

export const SCENES: Scene[] = [
	{
		id: "traverse",
		label: "Traverse",
		file: "topics",
		captions: [
			"Start from a node. Key lookups hit a hash index: ~125 ns.",
			"Follow `wrote` edges. Adjacency is stored contiguously (CSR), so a hop is a slice read.",
			"Hop again along `discusses`. Each hop is another slice of the same CSR adjacency arrays.",
			"Collect the result: three topics, each listed once.",
		],
		result: {
			label: "3 nodes",
			items: [
				{ text: "topic:graphs" },
				{ text: "topic:storage" },
				{ text: "topic:search" },
			],
		},
		code: {
			typescript: {
				code: `const topics = db
  .from(alice)
  .out('wrote')
  .out('discusses')
  .nodes()`,
				stepLines: [[0, 1], [2], [3], [4]],
			},
			python: {
				code: `topics = (db
    .from_(alice)
    .out(wrote)
    .out(discusses)
    .nodes()
    .to_list())`,
				stepLines: [[0, 1], [2], [3], [4, 5]],
			},
			rust: {
				code: `let topics = db
    .from(alice.id())
    .out(Some("wrote"))?
    .out(Some("discusses"))?
    .to_vec();`,
				stepLines: [[0, 1], [2], [3], [4]],
			},
		},
	},
	{
		id: "vector",
		label: "Vector search",
		file: "similar",
		captions: [
			"Embed the question with any model you like.",
			"The IVF index scans only the clusters closest to the query.",
			"Results are ranked by cosine similarity. Each hit is a node id you can traverse from.",
		],
		result: {
			label: "k = 3",
			items: [
				{ text: "doc:wal-recovery", meta: "0.93" },
				{ text: "doc:csr-layout", meta: "0.81" },
				{ text: "doc:ivf-recall", meta: "0.74" },
			],
		},
		code: {
			typescript: {
				code: `const query = await embed('how does crash recovery work?')

const hits = index.search(query, { k: 3 })`,
				stepLines: [[0], [2], [2]],
			},
			python: {
				code: `query = embed("how does crash recovery work?")

hits = index.search(query, k=3)`,
				stepLines: [[0], [2], [2]],
			},
			rust: {
				code: `let query = embed("how does crash recovery work?")?;

let hits = index.search(&query, SimilarOptions::new(3))?;`,
				stepLines: [[0], [2], [2]],
			},
		},
	},
];

// ---------------------------------------------------------------------------
// Code tour
// ---------------------------------------------------------------------------

export interface TourItem {
	id: string;
	title: string;
	blurb: string;
	file: string;
	code: Snippet;
}

export const TOUR: TourItem[] = [
	{
		id: "schema",
		title: "Define a schema",
		blurb: "Declare node and edge types with typed properties.",
		file: "schema",
		code: {
			typescript: `import { kite } from '@kitedb/core'

const db = await kite('./knowledge.kitedb', {
  nodes: [
    {
      name: 'document',
      props: {
        title: { type: 'string' },
        content: { type: 'string' },
        embedding: { type: 'vector' },
      },
    },
    { name: 'topic', props: { name: { type: 'string' } } },
  ],
  edges: [
    { name: 'discusses', props: { relevance: { type: 'float' } } },
  ],
})`,
			python: `from kitedb import kite, define_node, define_edge, prop

document = define_node("document",
    key=lambda id: f"doc:{id}",
    props={
        "title": prop.string("title"),
        "content": prop.string("content"),
        "embedding": prop.vector("embedding"),
    },
)

topic = define_node("topic",
    key=lambda name: f"topic:{name}",
    props={"name": prop.string("name")},
)

discusses = define_edge("discusses", {"relevance": prop.float("relevance")})

db = kite("./knowledge.kitedb", nodes=[document, topic], edges=[discusses])`,
			rust: `use kitedb::api::kite::{kite, EdgeDef, KiteOptions, NodeDef, PropDef};

// Embeddings live in a VectorIndex keyed by node ID
let document = NodeDef::new("document", "doc:")
    .prop(PropDef::string("title"))
    .prop(PropDef::string("content"));
let topic = NodeDef::new("topic", "topic:")
    .prop(PropDef::string("name"));
let discusses = EdgeDef::new("discusses")
    .prop(PropDef::float("relevance"));

let db = kite(
    "./knowledge.kitedb",
    KiteOptions::new().node(document).node(topic).edge(discusses),
)?;`,
		},
	},
	{
		id: "write",
		title: "Write",
		blurb:
			"Insert, link, and update through builders. Each commit is written to the WAL before it becomes visible.",
		file: "write",
		code: {
			typescript: `const doc = db
  .insert('document')
  .values('doc-1', {
    title: 'Getting Started',
    content: 'Welcome to KiteDB...',
  })
  .returning()

db.link(doc.id, 'discusses', topic.id, { relevance: 0.95 })

const update = db.update('document', 'doc-1')
update.set('title', 'Updated Title')
update.execute()`,
			python: `doc = (db.insert(document)
    .values(key="doc-1", title="Getting Started", content="Welcome to KiteDB...")
    .returning())

db.link(doc, discusses, topic, relevance=0.95)

(db.update(doc)
    .set(title="Updated Title")
    .execute())`,
			rust: `let doc = db
    .insert("document")?
    .values("doc-1", HashMap::from([
        ("title".into(), PropValue::String("Getting Started".into())),
        ("content".into(), PropValue::String("Welcome to KiteDB...".into())),
    ]))?
    .returning()?;

db.link_with_props(doc.id(), "discusses", topic.id(), HashMap::from([
    ("relevance".into(), PropValue::F64(0.95)),
]))?;

db.update_by_id(doc.id())?
    .set("title", PropValue::String("Updated Title".into()))
    .execute()?;`,
		},
	},
	{
		id: "traverse",
		title: "Traverse",
		blurb:
			"Chain out() and in() calls to walk the graph, with filters and limits along the way.",
		file: "traverse",
		code: {
			typescript: `// Topics discussed by Alice's documents
const topics = db
  .from(alice.id)
  .out('wrote')
  .out('discusses')
  .nodes()

// Multi-hop with a limit
const colleagues = db
  .from(alice.id)
  .out('knows')
  .out('worksAt')
  .take(10)
  .nodes()`,
			python: `# Topics discussed by Alice's documents
topics = (db
    .from_(alice)
    .out(wrote)
    .out(discusses)
    .nodes()
    .to_list())

# Multi-hop with a limit
colleagues = (db
    .from_(alice)
    .out(knows)
    .out(works_at)
    .take(10)
    .nodes()
    .to_list())`,
			rust: `// Topics discussed by Alice's documents
let topics = db
    .from(alice.id())
    .out(Some("wrote"))?
    .out(Some("discusses"))?
    .to_vec();

// Multi-hop with a limit
let colleagues = db
    .from(alice.id())
    .out(Some("knows"))?
    .out(Some("worksAt"))?
    .take(10)
    .to_vec();`,
		},
	},
	{
		id: "vector",
		title: "Search by meaning",
		blurb:
			"Build an IVF index over your embeddings. Search results are node ids you can traverse from.",
		file: "search",
		code: {
			typescript: `import { createVectorIndex, DistanceMetric } from '@kitedb/core'

const index = createVectorIndex({
  dimensions: 1536,
  metric: DistanceMetric.Cosine,
})

index.set(doc.id, embedding)

const similar = index.search(queryEmbedding, {
  k: 10,
  threshold: 0.8,
})

for (const hit of similar) {
  console.log(hit.nodeId, hit.similarity)
}`,
			python: `from kitedb import create_vector_index, VectorIndexOptions

index = create_vector_index(VectorIndexOptions(
    dimensions=1536,
    metric="cosine",
))

index.set(doc, embedding)

similar = index.search(query_embedding, k=10, threshold=0.8)

for hit in similar:
    print(hit.node.key, hit.similarity)`,
			rust: `let mut index = VectorIndex::new(VectorIndexOptions {
    dimensions: 1536,
    metric: DistanceMetric::Cosine,
    ..Default::default()
});

index.set(doc.id(), &embedding)?;

let similar = index.search(
    &query_embedding,
    SimilarOptions::new(10).with_threshold(0.8),
)?;

for hit in similar {
    println!("{} {}", hit.node_id, hit.similarity);
}`,
		},
	},
];
