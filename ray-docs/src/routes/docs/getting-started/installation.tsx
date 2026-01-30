import { createFileRoute } from '@tanstack/solid-router'
import DocPage from '~/components/doc-page'
import { MultiLangCode } from '~/components/multi-lang-code'
import { InstallTabs } from '~/components/install-tabs'

export const Route = createFileRoute('/docs/getting-started/installation')({
  component: InstallationPage,
})

function InstallationPage() {
  return (
    <DocPage slug="getting-started/installation">
      <p>
        RayDB is available for JavaScript/TypeScript (via NAPI), Rust, and Python.
        Choose your preferred language below.
      </p>

      <h2 id="install">Install</h2>
      <InstallTabs />

      <h2 id="requirements">Requirements</h2>
      <ul>
        <li><strong>JavaScript/TypeScript:</strong> Bun 1.0+, Node.js 18+, or Deno</li>
        <li><strong>Rust:</strong> Rust 1.70+</li>
        <li><strong>Python:</strong> Python 3.9+</li>
      </ul>

      <h2 id="verify">Verify Installation</h2>
      <p>Create a simple test file to verify the installation works:</p>
      <MultiLangCode
        typescript={`import { ray } from '@ray-db/core';

// Open database with a simple schema
const db = ray('./test.raydb', {
  nodes: [
    {
      name: 'user',
      props: { name: { type: 'string' } },
    },
  ],
  edges: [],
});

console.log('RayDB is working!');
db.close();`}
        rust={`use raydb::ray;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Open database with a simple schema
    let db = ray("./test.raydb", RayOptions {
        nodes: vec![
            NodeSpec::new("user")
                .prop("name", PropType::String),
        ],
        edges: vec![],
        ..Default::default()
    })?;

    println!("RayDB is working!");
    db.close();
    Ok(())
}`}
        python={`from raydb import ray, define_node, prop

# Define a simple schema
user = define_node("user",
    key=lambda id: f"user:{id}",
    props={"name": prop.string("name")}
)

# Open database
with ray("./test.raydb", nodes=[user], edges=[]) as db:
    print("RayDB is working!")`}
        filename={{ ts: 'test.ts', rs: 'main.rs', py: 'test.py' }}
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

      <h2 id="next-steps">Next Steps</h2>
      <p>
        Now that RayDB is installed, head to the <a href="/docs/getting-started/quick-start">Quick Start</a> guide to build your first graph database.
      </p>
    </DocPage>
  )
}
