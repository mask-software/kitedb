/**
 * Build Script for Playground Client
 *
 * Uses Bun.build() to bundle the React app.
 */

import { join } from "node:path";
import { cp } from "node:fs/promises";

const srcDir = join(import.meta.dir, "src/client");
const distDir = join(import.meta.dir, "dist");

console.log("Building playground client...");

// Build the React app
const result = await Bun.build({
  entrypoints: [join(srcDir, "index.tsx")],
  outdir: distDir,
  minify: process.env.NODE_ENV === "production",
  sourcemap: process.env.NODE_ENV !== "production" ? "inline" : "none",
  target: "browser",
  define: {
    "process.env.NODE_ENV": JSON.stringify(process.env.NODE_ENV || "development"),
  },
});

if (!result.success) {
  console.error("Build failed:");
  for (const log of result.logs) {
    console.error(log);
  }
  process.exit(1);
}

// Copy index.html
await cp(join(srcDir, "index.html"), join(distDir, "index.html"));

console.log(`Build complete! Output: ${distDir}`);
console.log(`  - index.html`);
for (const output of result.outputs) {
  console.log(`  - ${output.path.replace(distDir + "/", "")}`);
}
