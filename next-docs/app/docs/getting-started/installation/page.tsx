import DocPage from "../../../../components/DocPage";
import CodeBlock from "../../../../components/CodeBlock";

export default function InstallationPage() {
  return (
    <DocPage slug="getting-started/installation">
      <p>RayDB is available on npm and works with Bun, Node.js, and any TypeScript project.</p>

      <h2 id="requirements">Requirements</h2>
      <ul>
        <li>
          <strong>Bun</strong> 1.0+ (recommended) or <strong>Node.js</strong> 18+
        </li>
        <li>
          <strong>TypeScript</strong> 5.0+
        </li>
      </ul>

      <h2 id="install-bun">Install with Bun</h2>
      <CodeBlock code="bun add @ray-db/ray" language="bash" />

      <h2 id="install-npm">Install with npm</h2>
      <CodeBlock code="npm install @ray-db/ray" language="bash" />

      <h2 id="install-pnpm">Install with pnpm</h2>
      <CodeBlock code="pnpm add @ray-db/ray" language="bash" />

      <h2 id="install-yarn">Install with Yarn</h2>
      <CodeBlock code="yarn add @ray-db/ray" language="bash" />

      <h2 id="typescript">TypeScript Configuration</h2>
      <p>
        RayDB is written in TypeScript and includes type definitions. For the best experience, ensure your
        <code>tsconfig.json</code> has:
      </p>
      <CodeBlock
        code={`{
  "compilerOptions": {
    "strict": true,
    "moduleResolution": "bundler",
    "target": "ES2022",
    "module": "ESNext"
  }
}`}
        language="json"
        filename="tsconfig.json"
      />

      <h2 id="verify">Verify Installation</h2>
      <p>Create a simple test file:</p>
      <CodeBlock
        code={`import { ray, defineNode, prop } from '@ray-db/ray';

const user = defineNode('user', {
  key: (id: string) => \`user:\${id}\`,
  props: {
    name: prop.string('name'),
  },
});

const db = await ray('./test.raydb', {
  nodes: [user],
  edges: [],
});

console.log('RayDB is working!');
await db.close();`}
        language="typescript"
        filename="test.ts"
      />

      <p>Run it:</p>
      <CodeBlock
        code={`bun run test.ts
# or
npx tsx test.ts`}
        language="bash"
      />

      <h2 id="next-steps">Next Steps</h2>
      <p>
        Now that RayDB is installed, head to the <a href="/docs/getting-started/quick-start">Quick Start</a> guide to
        build your first graph database.
      </p>
    </DocPage>
  );
}
