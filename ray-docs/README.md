# KiteDB docs site

Source for the KiteDB homepage and documentation. Built with SolidJS, TanStack Start, and Tailwind CSS v4. Deployed to Vercel (see `vercel.json`).

## Develop

```bash
bun install
bun run dev --port 5311   # http://localhost:5311
bun run build             # production build into .output/
bun run start             # serve the build (node .output/server/index.mjs)
bun run lint              # Biome lint
```

The version shown in the docs sidebar is read from `../ray-rs/package.json` at build time.

## Test

The tests make HTTP requests against a running server (they check, for example, that unknown docs pages return 404). Start a server first, then run them.

Against the dev server (the default URL is `http://localhost:5311`):

```bash
bun run dev --port 5311
bun run test
```

Against a production build:

```bash
bun run build
PORT=5398 node .output/server/index.mjs &
DOCS_URL=http://localhost:5398 bun run test
```

## Layout

- `src/routes/`: file-based routes. `index.tsx` is the homepage, `docs.tsx` is the docs layout (sidebar and nav), and `docs/` holds the pages. Each section (`getting-started`, `guides`, `api`, `benchmarks`, `internals`) has a `$.tsx` splat route that renders its pages by slug. Slugs not listed in `lib/docs.ts` return a 404.
- `src/lib/docs.ts`: the docs navigation (`docsStructure`). To add a page, add an entry here and a matching branch in the section's `$.tsx`.
- `src/lib/benchmarks.ts`: every benchmark number on the site, each tied to a raw log in `docs/benchmarks/results/`.
- `src/components/home/`: homepage sections.
- `src/components/`: shared pieces (doc page shell, code blocks, search, nav).
- `src/styles.css`: Tailwind setup and design tokens (`--color-kite-*` in `@theme`).

See `AGENTS.md` for the Solid rules this codebase follows.
