import solidPlugin from "@opentui/solid/bun-plugin";

const result = await Bun.build({
  entrypoints: ["src/main.tsx"],
  outdir: "dist",
  target: "bun",
  sourcemap: "inline",
  minify: false,
  plugins: [solidPlugin],
});

if (!result.success) {
  for (const message of result.logs) {
    console.error(message);
  }
  process.exit(1);
}

console.log("Built to dist/");
