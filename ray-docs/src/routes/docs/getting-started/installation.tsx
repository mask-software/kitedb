import { createFileRoute } from "@tanstack/solid-router";
import DocPage from "~/components/doc-page";
import { MultiLangCode } from "~/components/multi-lang-code";
import { InstallTabs } from "~/components/install-tabs";

export const Route = createFileRoute("/docs/getting-started/installation")({
	component: InstallationPage,
});

function InstallationPage() {
	return (
		<DocPage slug="getting-started/installation">
			<p>
				KiteDB ships packages for JavaScript/TypeScript (native N-API bindings),
				Rust, and Python. The language you pick here also sets the language of
				every code sample in the docs.
			</p>

			<h2 id="install">Install</h2>
			<InstallTabs />

			<h2 id="requirements">Requirements</h2>
			<ul>
				<li>
					<strong>JavaScript/TypeScript:</strong> Bun 1.0+ or Node.js 16+
				</li>
				<li>
					<strong>Rust:</strong> Stable Rust toolchain. Add the crate with{" "}
					<code>--no-default-features</code> (or{" "}
					<code>default-features = false</code> in <code>Cargo.toml</code>): the
					default <code>napi</code> feature compiles the Node.js binding layer,
					which Rust programs don't use.
				</li>
				<li>
					<strong>Python:</strong> Python 3.9 to 3.13 (prebuilt wheels for Linux
					x86_64/aarch64, macOS arm64, and Windows x64)
				</li>
			</ul>

			<h2 id="verify">Verify the installation</h2>
			<p>
				Save this as a test file. It opens a database with a one-type schema and
				closes it again.
			</p>
			<MultiLangCode
				typescript={`import { kite } from '@kitedb/core';

// Open database with a simple schema
const db = await kite('./test.kitedb', {
  nodes: [
    {
      name: 'user',
      props: { name: { type: 'string' } },
    },
  ],
  edges: [],
});

console.log('KiteDB is working!');
db.close();`}
				rust={`use kitedb::api::kite::{kite, KiteOptions, NodeDef, PropDef};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Open database with a simple schema
    let db = kite(
        "./test.kitedb",
        KiteOptions::new()
            .node(NodeDef::new("user", "user:").prop(PropDef::string("name"))),
    )?;

    println!("KiteDB is working!");
    db.close()?;
    Ok(())
}`}
				python={`from kitedb import kite, define_node, prop

# Define a simple schema
user = define_node("user",
    key=lambda id: f"user:{id}",
    props={"name": prop.string("name")}
)

# Open database
with kite("./test.kitedb", nodes=[user], edges=[]) as db:
    print("KiteDB is working!")`}
				filename={{ ts: "test.ts", rs: "main.rs", py: "test.py" }}
			/>

			<p>Run it:</p>
			<MultiLangCode
				typescript={`bun run test.ts
# or
npx tsx test.ts`}
				rust={`cargo run`}
				python={`python test.py`}
				inline
			/>

			<h2 id="next-steps">Next steps</h2>
			<p>
				The <a href="/docs/getting-started/quick-start">quick start</a> walks
				through a schema, a few inserts, and your first traversal.
			</p>
		</DocPage>
	);
}
