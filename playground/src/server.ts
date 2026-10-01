/**
 * Playground Server
 *
 * Elysia server that serves both the API and static files.
 *
 * The API has no user authentication, so the defaults keep it local:
 *
 *   PORT                          Listen port (default 3000; 0 picks a free port).
 *   PLAYGROUND_HOST               Bind address (default 127.0.0.1). Anything else exposes the API to that network.
 *   PLAYGROUND_ALLOWED_ORIGINS    Comma-separated origins allowed cross-origin (CORS) access, e.g.
 *                                 "http://localhost:5173". Default: none (same-origin only).
 *   PLAYGROUND_DATA_DIR           The only directory /api/db/open may read or create databases in
 *                                 (default: playground/data). Relative paths are resolved against it;
 *                                 paths outside it are rejected. Uploads and the demo use private temp dirs.
 *   REPLICATION_ADMIN_TOKEN       Bearer token for the replication admin routes (snapshot, log, pull,
 *                                 reseed, promote, metrics). Unset: those routes are disabled, unless
 *                                 REPLICATION_ADMIN_AUTH_MODE selects mTLS or explicitly sets "none".
 *   REPLICATION_ADMIN_AUTH_MODE   token | mtls | token_or_mtls | token_and_mtls | none (default: token).
 *   PLAYGROUND_TLS_*, REPLICATION_MTLS_*   TLS / mTLS settings, see resolvePlaygroundTlsConfig() and
 *                                 resolveReplicationAdminConfig() in api/routes.ts.
 */

import { Elysia } from "elysia";
import { cors } from "@elysiajs/cors";
import { apiRoutes } from "./api/routes.ts";
import { existsSync } from "node:fs";
import { join } from "node:path";

const PORT = process.env.PORT ? parseInt(process.env.PORT) : 3000;
const HOST = process.env.PLAYGROUND_HOST?.trim() || "127.0.0.1";
const LOOPBACK_HOSTS = new Set(["127.0.0.1", "::1", "localhost"]);
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

type TlsFile = ReturnType<typeof Bun.file>;

interface PlaygroundTlsConfig {
  enabled: boolean;
  protocol: "http" | "https";
  tls?: {
    cert: TlsFile;
    key: TlsFile;
    ca?: TlsFile;
    requestCert: boolean;
    rejectUnauthorized: boolean;
  };
}

function parseBooleanEnv(name: string, raw: string | undefined, defaultValue: boolean): boolean {
  if (raw === undefined) {
    return defaultValue;
  }

  const normalized = raw.trim().toLowerCase();
  if (normalized === "") {
    return defaultValue;
  }
  if (normalized === "1" || normalized === "true" || normalized === "yes" || normalized === "on") {
    return true;
  }
  if (normalized === "0" || normalized === "false" || normalized === "no" || normalized === "off") {
    return false;
  }
  throw new Error(`Invalid ${name} (expected boolean)`);
}

export function resolvePlaygroundTlsConfig(env: NodeJS.ProcessEnv = process.env): PlaygroundTlsConfig {
  const certFile = env.PLAYGROUND_TLS_CERT_FILE?.trim();
  const keyFile = env.PLAYGROUND_TLS_KEY_FILE?.trim();
  const caFile = env.PLAYGROUND_TLS_CA_FILE?.trim();

  const hasCert = Boolean(certFile && certFile.length > 0);
  const hasKey = Boolean(keyFile && keyFile.length > 0);
  if (hasCert !== hasKey) {
    throw new Error("PLAYGROUND_TLS_CERT_FILE and PLAYGROUND_TLS_KEY_FILE must both be set for TLS");
  }

  if (!hasCert || !hasKey) {
    return { enabled: false, protocol: "http" };
  }

  if (!existsSync(certFile!)) {
    throw new Error(`PLAYGROUND_TLS_CERT_FILE does not exist: ${certFile}`);
  }
  if (!existsSync(keyFile!)) {
    throw new Error(`PLAYGROUND_TLS_KEY_FILE does not exist: ${keyFile}`);
  }
  if (caFile && caFile.length > 0 && !existsSync(caFile)) {
    throw new Error(`PLAYGROUND_TLS_CA_FILE does not exist: ${caFile}`);
  }

  const requestCert = parseBooleanEnv("PLAYGROUND_TLS_REQUEST_CERT", env.PLAYGROUND_TLS_REQUEST_CERT, false);
  const rejectUnauthorized = parseBooleanEnv(
    "PLAYGROUND_TLS_REJECT_UNAUTHORIZED",
    env.PLAYGROUND_TLS_REJECT_UNAUTHORIZED,
    true,
  );

  return {
    enabled: true,
    protocol: "https",
    tls: {
      cert: Bun.file(certFile!),
      key: Bun.file(keyFile!),
      ...(caFile && caFile.length > 0 ? { ca: Bun.file(caFile) } : {}),
      requestCert,
      rejectUnauthorized,
    },
  };
}

/** Origins from PLAYGROUND_ALLOWED_ORIGINS, read per request. */
function isAllowedOrigin(request: Request): boolean {
  const origin = request.headers.get("origin");
  if (!origin) {
    return false;
  }
  const allowed = (process.env.PLAYGROUND_ALLOWED_ORIGINS ?? "")
    .split(",")
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0);
  return allowed.includes(origin);
}

export const app = new Elysia()
  // Same-origin only unless PLAYGROUND_ALLOWED_ORIGINS lists the origin
  .use(cors({
    origin: isAllowedOrigin,
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
    const tlsConfig = resolvePlaygroundTlsConfig();
    server = app.listen({
      port: PORT,
      hostname: HOST,
      ...(tlsConfig.tls ? { tls: tlsConfig.tls } : {}),
    });
    const actualPort = server.server?.port ?? PORT;
    const urlHost = HOST.includes(":") ? `[${HOST}]` : HOST;
    console.log(`RayDB Playground running at ${tlsConfig.protocol}://${urlHost}:${actualPort}`);
    if (!LOOPBACK_HOSTS.has(HOST)) {
      console.warn(
        `Warning: PLAYGROUND_HOST=${HOST} exposes the unauthenticated playground API beyond this machine.`,
      );
    }
    if (tlsConfig.enabled) {
      console.log(
        `TLS enabled (requestCert=${tlsConfig.tls?.requestCert ? "true" : "false"}, rejectUnauthorized=${tlsConfig.tls?.rejectUnauthorized ? "true" : "false"})`,
      );
    }
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
