/**
 * Playground Server
 *
 * Elysia server that serves both the API and static files.
 */

import { Elysia } from "elysia";
import { cors } from "@elysiajs/cors";
import { apiRoutes } from "./api/routes.ts";
import { join } from "node:path";

const PORT = process.env.PORT ? parseInt(process.env.PORT) : 3000;
const DEV = process.env.NODE_ENV !== "production";
const DIST_DIR = join(import.meta.dir, "../dist");

// Helper to get content type
const getContentType = (path: string): string => {
  if (path.endsWith(".js")) return "application/javascript";
  if (path.endsWith(".css")) return "text/css";
  if (path.endsWith(".html")) return "text/html";
  if (path.endsWith(".json")) return "application/json";
  if (path.endsWith(".png")) return "image/png";
  if (path.endsWith(".svg")) return "image/svg+xml";
  return "application/octet-stream";
};

export const app = new Elysia()
  // Enable CORS for development
  .use(cors({
    origin: DEV ? true : false,
  }))
  
  // Mount API routes first
  .use(apiRoutes)
  
  // Serve static files explicitly
  .get("/index.js", () => {
    const file = Bun.file(join(DIST_DIR, "index.js"));
    return new Response(file, {
      headers: { "Content-Type": "application/javascript" },
    });
  })
  .get("/index.css", () => {
    const file = Bun.file(join(DIST_DIR, "index.css"));
    return new Response(file, {
      headers: { "Content-Type": "text/css" },
    });
  })
  
  // Serve index.html for root
  .get("/", () => {
    const file = Bun.file(join(DIST_DIR, "index.html"));
    return new Response(file, {
      headers: { "Content-Type": "text/html" },
    });
  });

let server: ReturnType<typeof app.listen> | null = null;

if (import.meta.main) {
  try {
    server = app.listen({
      port: PORT,
      hostname: "0.0.0.0",
    });
    const actualPort = server.server?.port ?? PORT;
    console.log(`RayDB Playground running at http://localhost:${actualPort}`);
  } catch (err) {
    console.error("Failed to start server", err);
    process.exit(1);
  }
}

const shutdown = () => {
  if (server) {
    server.stop();
    server = null;
  }
  process.exit(0);
};

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
