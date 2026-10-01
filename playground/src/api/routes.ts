/**
 * API Routes
 *
 * All API endpoints for the playground server.
 */

import { Elysia, t } from "elysia";
import { createHash, timingSafeEqual } from "node:crypto";
import { join } from "node:path";
import {
  getDb,
  getDbPath,
  getStatus,
  type PlaygroundOpenOptions,
  openDatabase,
  openFromBuffer,
  createDemo,
  closeDatabase,
} from "./db.ts";
import { loadGraph, nodeLabel } from "./graph.ts";

// ============================================================================
// Constants
// ============================================================================

const MAX_NODES = 1000;
const MAX_FILE_SIZE = 10 * 1024 * 1024; // 10MB
const REPLICATION_PULL_MAX_FRAMES_DEFAULT = 256;
const REPLICATION_PULL_MAX_FRAMES_LIMIT = 10_000;
const REPLICATION_LOG_MAX_BYTES_DEFAULT = 1024 * 1024;
const REPLICATION_LOG_MAX_BYTES_LIMIT = 32 * 1024 * 1024;

// ============================================================================
// Types
// ============================================================================

interface VisNode {
  id: string;
  label: string;
  type: string;
  color?: string;
  degree: number;
}

interface VisEdge {
  source: string;
  target: string;
  type: string;
}

interface RawReplicationStatus {
  role?: string;
  epoch?: number;
  headLogIndex?: number;
  retainedFloor?: number;
  replicaLags?: Array<{
    replicaId: string;
    epoch: number;
    appliedLogIndex: number;
  }>;
  sidecarPath?: string;
  lastToken?: string | null;
  appendAttempts?: number;
  appendFailures?: number;
  appendSuccesses?: number;
}

interface RawReplicaStatus {
  role?: string;
  appliedEpoch?: number;
  appliedLogIndex?: number;
  needsReseed?: boolean;
  lastError?: string | null;
}

interface ParsedReplicationCursor {
  epoch: bigint;
  segmentId: bigint;
  segmentOffset: bigint;
  logIndex: bigint;
}

interface ReplicationFrameResponse {
  epoch: string;
  logIndex: string;
  segmentId: string;
  segmentOffset: string;
  payloadBase64: string;
  bytes: number;
}

type ReplicationAdminAuthMode =
  | "none"
  | "token"
  | "mtls"
  | "token_or_mtls"
  | "token_and_mtls";

interface ReplicationAdminConfig {
  mode: ReplicationAdminAuthMode;
  authEnabled: boolean;
  token: string | null;
  mtlsHeader: string;
  mtlsSubjectRegex: RegExp | null;
  mtlsNativeTlsEnabled: boolean;
  invalidConfigError: string | null;
}

// ============================================================================
// Color scheme for node types
// ============================================================================

const NODE_COLORS: Record<string, string> = {
  file: "#3B82F6",      // blue
  function: "#e6be8a",  // gold
  class: "#22C55E",     // green
  module: "#A855F7",    // purple
};

// ============================================================================
// Helpers
// ============================================================================

function getRawDb(): Record<string, unknown> | null {
  const db = getDb() as unknown as (Record<string, unknown> & { $raw?: Record<string, unknown> }) | null;
  if (!db) {
    return null;
  }
  return db.$raw ?? db;
}

function callRawMethod<T>(
  raw: Record<string, unknown>,
  names: Array<string>,
  ...args: Array<unknown>
): T {
  for (const name of names) {
    const candidate = raw[name];
    if (typeof candidate === "function") {
      return (candidate as (...values: Array<unknown>) => T).call(raw, ...args);
    }
  }

  throw new Error(`Replication method unavailable (${names.join(" | ")})`);
}

function parseBooleanEnv(raw: string | undefined, defaultValue: boolean): boolean | null {
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
  return null;
}

function resolveReplicationAdminConfig(): ReplicationAdminConfig {
  const tokenRaw = process.env.REPLICATION_ADMIN_TOKEN?.trim();
  const token = tokenRaw && tokenRaw.length > 0 ? tokenRaw : null;

  const modeRaw = process.env.REPLICATION_ADMIN_AUTH_MODE?.trim().toLowerCase();
  const mode: ReplicationAdminAuthMode = (() => {
    // Deny by default: without an explicit mode, admin routes need REPLICATION_ADMIN_TOKEN.
    // Only an explicit REPLICATION_ADMIN_AUTH_MODE=none opens them up.
    if (!modeRaw || modeRaw === "") {
      return "token";
    }

    switch (modeRaw) {
      case "none":
      case "token":
      case "mtls":
      case "token_or_mtls":
      case "token_and_mtls":
        return modeRaw;
      default:
        return "none";
    }
  })();

  if (modeRaw && mode === "none" && modeRaw !== "none") {
    return {
      mode,
      authEnabled: true,
      token,
      mtlsHeader: "x-forwarded-client-cert",
      mtlsSubjectRegex: null,
      mtlsNativeTlsEnabled: false,
      invalidConfigError:
        "Invalid REPLICATION_ADMIN_AUTH_MODE; expected none|token|mtls|token_or_mtls|token_and_mtls",
    };
  }

  const mtlsHeaderRaw = process.env.REPLICATION_MTLS_HEADER?.trim().toLowerCase();
  const mtlsHeader = mtlsHeaderRaw && mtlsHeaderRaw.length > 0
    ? mtlsHeaderRaw
    : "x-forwarded-client-cert";

  const nativeTlsMode = parseBooleanEnv(process.env.REPLICATION_MTLS_NATIVE_TLS, false);
  if (nativeTlsMode === null) {
    return {
      mode,
      authEnabled: true,
      token,
      mtlsHeader,
      mtlsSubjectRegex: null,
      mtlsNativeTlsEnabled: false,
      invalidConfigError: "Invalid REPLICATION_MTLS_NATIVE_TLS (expected boolean)",
    };
  }

  if (nativeTlsMode) {
    const tlsRequestCert = parseBooleanEnv(process.env.PLAYGROUND_TLS_REQUEST_CERT, false);
    if (tlsRequestCert === null) {
      return {
        mode,
        authEnabled: true,
        token,
        mtlsHeader,
        mtlsSubjectRegex: null,
        mtlsNativeTlsEnabled: false,
        invalidConfigError: "Invalid PLAYGROUND_TLS_REQUEST_CERT (expected boolean)",
      };
    }

    const tlsRejectUnauthorized = parseBooleanEnv(process.env.PLAYGROUND_TLS_REJECT_UNAUTHORIZED, true);
    if (tlsRejectUnauthorized === null) {
      return {
        mode,
        authEnabled: true,
        token,
        mtlsHeader,
        mtlsSubjectRegex: null,
        mtlsNativeTlsEnabled: false,
        invalidConfigError: "Invalid PLAYGROUND_TLS_REJECT_UNAUTHORIZED (expected boolean)",
      };
    }

    if (!tlsRequestCert || !tlsRejectUnauthorized) {
      return {
        mode,
        authEnabled: true,
        token,
        mtlsHeader,
        mtlsSubjectRegex: null,
        mtlsNativeTlsEnabled: false,
        invalidConfigError:
          "REPLICATION_MTLS_NATIVE_TLS requires PLAYGROUND_TLS_REQUEST_CERT=true and PLAYGROUND_TLS_REJECT_UNAUTHORIZED=true",
      };
    }
  }

  const regexRaw = process.env.REPLICATION_MTLS_SUBJECT_REGEX?.trim();
  if (regexRaw && regexRaw.length > 0) {
    try {
      return {
        mode,
        authEnabled: mode !== "none",
        token,
        mtlsHeader,
        mtlsSubjectRegex: new RegExp(regexRaw),
        mtlsNativeTlsEnabled: nativeTlsMode,
        invalidConfigError: null,
      };
    } catch {
      return {
        mode,
        authEnabled: true,
        token,
        mtlsHeader,
        mtlsSubjectRegex: null,
        mtlsNativeTlsEnabled: nativeTlsMode,
        invalidConfigError: "Invalid REPLICATION_MTLS_SUBJECT_REGEX",
      };
    }
  }

  return {
    mode,
    authEnabled: mode !== "none",
    token,
    mtlsHeader,
    mtlsSubjectRegex: null,
    mtlsNativeTlsEnabled: nativeTlsMode,
    invalidConfigError: null,
  };
}

function matchesMtlsRequest(request: Request, config: ReplicationAdminConfig): boolean {
  const headerValue = request.headers.get(config.mtlsHeader);
  if (headerValue && headerValue.trim() !== "") {
    if (!config.mtlsSubjectRegex) {
      return true;
    }
    return config.mtlsSubjectRegex.test(headerValue);
  }

  if (!config.mtlsNativeTlsEnabled || config.mtlsSubjectRegex) {
    return false;
  }

  try {
    return new URL(request.url).protocol === "https:";
  } catch {
    return false;
  }
}

/** Constant-time bearer token check; both sides are hashed so their lengths always match. */
function bearerTokenMatches(authHeader: string | null, token: string): boolean {
  if (!authHeader) {
    return false;
  }
  const actual = createHash("sha256").update(authHeader).digest();
  const expected = createHash("sha256").update(`Bearer ${token}`).digest();
  return timingSafeEqual(actual, expected);
}

function requireReplicationAdmin(
  request: Request,
  set: { status?: number },
): { ok: true } | { ok: false; error: string } {
  const config = resolveReplicationAdminConfig();
  if (config.invalidConfigError) {
    set.status = 500;
    return { ok: false, error: config.invalidConfigError };
  }

  if (config.mode === "none") {
    return { ok: true };
  }

  const tokenRequired = config.mode === "token" || config.mode === "token_and_mtls";
  if (tokenRequired && !config.token) {
    set.status = 403;
    return {
      ok: false,
      error: "Replication admin endpoints are disabled: set REPLICATION_ADMIN_TOKEN to enable them",
    };
  }

  const tokenOk = config.token
    ? bearerTokenMatches(request.headers.get("authorization"), config.token)
    : false;
  const mtlsOk = matchesMtlsRequest(request, config);

  const authorized = (() => {
    switch (config.mode) {
      case "token":
        return tokenOk;
      case "mtls":
        return mtlsOk;
      case "token_or_mtls":
        return tokenOk || mtlsOk;
      case "token_and_mtls":
        return tokenOk && mtlsOk;
      case "none":
      default:
        return true;
    }
  })();

  if (authorized) {
    return { ok: true };
  }

  set.status = 401;
  return {
    ok: false,
    error: `Unauthorized: replication admin auth mode '${config.mode}' not satisfied`,
  };
}

function resolveReplicationStatus(
  raw: Record<string, unknown>,
): {
  role: "primary" | "replica" | "disabled";
  primary: RawReplicationStatus | null;
  replica: RawReplicaStatus | null;
} {
  const primary = callRawMethod<RawReplicationStatus | null>(
    raw,
    ["primaryReplicationStatus", "primary_replication_status"],
  );
  const replica = callRawMethod<RawReplicaStatus | null>(
    raw,
    ["replicaReplicationStatus", "replica_replication_status"],
  );

  const role = primary
    ? "primary"
    : replica
      ? "replica"
      : "disabled";

  return { role, primary, replica };
}

function parsePositiveInt(
  value: unknown,
  fallback: number,
  min: number,
  max: number,
): number {
  if (value === undefined || value === null || value === "") {
    return fallback;
  }

  const parsed = Number(value);
  if (!Number.isFinite(parsed)) {
    return fallback;
  }

  return Math.min(Math.max(Math.floor(parsed), min), max);
}

function parseBoolean(value: unknown, fallback: boolean): boolean {
  if (value === undefined || value === null || value === "") {
    return fallback;
  }

  if (typeof value === "boolean") {
    return value;
  }

  const text = String(value).toLowerCase().trim();
  if (text === "1" || text === "true" || text === "yes") {
    return true;
  }
  if (text === "0" || text === "false" || text === "no") {
    return false;
  }

  return fallback;
}

function parseReplicationCursor(raw: unknown): ParsedReplicationCursor | null {
  if (typeof raw !== "string" || raw.trim() === "") {
    return null;
  }

  const token = raw.trim();
  const parts = token.split(":");
  if (parts.length === 2) {
    const epoch = BigInt(parts[0]);
    const logIndex = BigInt(parts[1]);
    return {
      epoch,
      segmentId: 0n,
      segmentOffset: 0n,
      logIndex,
    };
  }

  if (parts.length === 4) {
    return {
      epoch: BigInt(parts[0]),
      segmentId: BigInt(parts[1]),
      segmentOffset: BigInt(parts[2]),
      logIndex: BigInt(parts[3]),
    };
  }

  throw new Error(
    "invalid cursor format; expected 'epoch:logIndex' or 'epoch:segmentId:segmentOffset:logIndex'",
  );
}

function cursorAfterFrame(
  cursor: ParsedReplicationCursor | null,
  epoch: bigint,
  segmentId: bigint,
  segmentOffset: bigint,
  logIndex: bigint,
): boolean {
  if (!cursor) {
    return true;
  }

  if (epoch > cursor.epoch) {
    return true;
  }
  if (epoch < cursor.epoch) {
    return false;
  }

  if (logIndex > cursor.logIndex) {
    return true;
  }
  if (logIndex < cursor.logIndex) {
    return false;
  }

  if (cursor.segmentId === 0n) {
    return false;
  }
  if (segmentId > cursor.segmentId) {
    return true;
  }
  if (segmentId < cursor.segmentId) {
    return false;
  }

  return segmentOffset > cursor.segmentOffset;
}

function formatSegmentFileName(id: bigint): string {
  return `segment-${id.toString().padStart(20, "0")}.rlog`;
}

async function readFileBytes(path: string): Promise<Uint8Array> {
  const arrayBuffer = await Bun.file(path).arrayBuffer();
  return new Uint8Array(arrayBuffer);
}

async function readManifestEnvelope(sidecarPath: string): Promise<{
  version: number;
  payload_crc32: number;
  manifest: {
    epoch: number;
    head_log_index: number;
    retained_floor: number;
    active_segment_id: number;
    segments: Array<{
      id: number;
      start_log_index: number;
      end_log_index: number;
      size_bytes: number;
    }>;
  };
}> {
  const manifestPath = join(sidecarPath, "manifest.json");
  const text = await Bun.file(manifestPath).text();
  return JSON.parse(text);
}

function escapePrometheusLabelValue(value: string): string {
  return value
    .replaceAll("\\", "\\\\")
    .replaceAll("\"", "\\\"")
    .replaceAll("\n", "\\n");
}

function formatPrometheusLabels(labels: Record<string, string | number>): string {
  const entries = Object.entries(labels);
  if (entries.length === 0) {
    return "";
  }
  const rendered = entries.map(
    ([key, value]) => `${key}="${escapePrometheusLabelValue(String(value))}"`,
  );
  return `{${rendered.join(",")}}`;
}

function toMetricNumber(value: unknown, fallback = 0): number {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) {
    return fallback;
  }
  return parsed;
}

function pushPrometheusMetricHelp(
  lines: Array<string>,
  metricName: string,
  metricType: "gauge" | "counter",
  helpText: string,
): void {
  lines.push(`# HELP ${metricName} ${helpText}`);
  lines.push(`# TYPE ${metricName} ${metricType}`);
}

function pushPrometheusMetricSample(
  lines: Array<string>,
  metricName: string,
  value: number,
  labels: Record<string, string | number> = {},
): void {
  lines.push(`${metricName}${formatPrometheusLabels(labels)} ${value}`);
}

function renderReplicationPrometheusMetrics(
  resolved: {
    role: "primary" | "replica" | "disabled";
    primary: RawReplicationStatus | null;
    replica: RawReplicaStatus | null;
  },
  authEnabled: boolean,
): string {
  const lines: Array<string> = [];

  pushPrometheusMetricHelp(
    lines,
    "raydb_replication_enabled",
    "gauge",
    "Whether replication is enabled for the connected database (1 enabled, 0 disabled).",
  );
  pushPrometheusMetricSample(lines, "raydb_replication_enabled", resolved.role === "disabled" ? 0 : 1, {
    role: resolved.role,
  });

  pushPrometheusMetricHelp(
    lines,
    "raydb_replication_auth_enabled",
    "gauge",
    "Whether replication admin token auth is enabled for admin endpoints.",
  );
  pushPrometheusMetricSample(lines, "raydb_replication_auth_enabled", authEnabled ? 1 : 0);

  if (resolved.primary) {
    const epoch = toMetricNumber(resolved.primary.epoch, 0);
    const headLogIndex = toMetricNumber(resolved.primary.headLogIndex, 0);
    const retainedFloor = toMetricNumber(resolved.primary.retainedFloor, 0);
    const replicaLags = resolved.primary.replicaLags ?? [];

    let staleReplicaCount = 0;
    let maxReplicaLag = 0;

    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_primary_epoch",
      "gauge",
      "Primary replication epoch.",
    );
    pushPrometheusMetricSample(lines, "raydb_replication_primary_epoch", epoch);

    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_primary_head_log_index",
      "gauge",
      "Primary replication head log index.",
    );
    pushPrometheusMetricSample(lines, "raydb_replication_primary_head_log_index", headLogIndex);

    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_primary_retained_floor",
      "gauge",
      "Primary replication retained floor log index.",
    );
    pushPrometheusMetricSample(lines, "raydb_replication_primary_retained_floor", retainedFloor);

    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_primary_replica_count",
      "gauge",
      "Number of replicas reporting progress to the primary.",
    );
    pushPrometheusMetricSample(lines, "raydb_replication_primary_replica_count", replicaLags.length);

    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_primary_replica_lag",
      "gauge",
      "Replica lag in frames relative to primary head index.",
    );
    for (const lag of replicaLags) {
      const replicaEpoch = toMetricNumber(lag.epoch, 0);
      const appliedLogIndex = toMetricNumber(lag.appliedLogIndex, 0);
      const lagFrames = replicaEpoch === epoch
        ? Math.max(0, headLogIndex - appliedLogIndex)
        : Math.max(0, headLogIndex);
      if (replicaEpoch !== epoch) {
        staleReplicaCount += 1;
      }
      maxReplicaLag = Math.max(maxReplicaLag, lagFrames);
      pushPrometheusMetricSample(
        lines,
        "raydb_replication_primary_replica_lag",
        lagFrames,
        {
          replica_id: lag.replicaId,
          replica_epoch: replicaEpoch,
        },
      );
    }

    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_primary_stale_epoch_replica_count",
      "gauge",
      "Count of replicas reporting progress from a stale epoch.",
    );
    pushPrometheusMetricSample(
      lines,
      "raydb_replication_primary_stale_epoch_replica_count",
      staleReplicaCount,
    );

    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_primary_max_replica_lag",
      "gauge",
      "Maximum replica lag in frames among replicas reporting progress.",
    );
    pushPrometheusMetricSample(lines, "raydb_replication_primary_max_replica_lag", maxReplicaLag);

    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_primary_append_attempts_total",
      "counter",
      "Total replication append attempts on primary commit path.",
    );
    pushPrometheusMetricSample(
      lines,
      "raydb_replication_primary_append_attempts_total",
      toMetricNumber(resolved.primary.appendAttempts, 0),
    );

    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_primary_append_failures_total",
      "counter",
      "Total replication append failures on primary commit path.",
    );
    pushPrometheusMetricSample(
      lines,
      "raydb_replication_primary_append_failures_total",
      toMetricNumber(resolved.primary.appendFailures, 0),
    );

    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_primary_append_successes_total",
      "counter",
      "Total replication append successes on primary commit path.",
    );
    pushPrometheusMetricSample(
      lines,
      "raydb_replication_primary_append_successes_total",
      toMetricNumber(resolved.primary.appendSuccesses, 0),
    );
  }

  if (resolved.replica) {
    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_replica_applied_epoch",
      "gauge",
      "Replica applied epoch.",
    );
    pushPrometheusMetricSample(
      lines,
      "raydb_replication_replica_applied_epoch",
      toMetricNumber(resolved.replica.appliedEpoch, 0),
    );

    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_replica_applied_log_index",
      "gauge",
      "Replica applied log index.",
    );
    pushPrometheusMetricSample(
      lines,
      "raydb_replication_replica_applied_log_index",
      toMetricNumber(resolved.replica.appliedLogIndex, 0),
    );

    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_replica_needs_reseed",
      "gauge",
      "Whether replica currently requires reseed (1 yes, 0 no).",
    );
    pushPrometheusMetricSample(
      lines,
      "raydb_replication_replica_needs_reseed",
      resolved.replica.needsReseed ? 1 : 0,
    );

    pushPrometheusMetricHelp(
      lines,
      "raydb_replication_replica_last_error_present",
      "gauge",
      "Whether replica has a non-empty last_error value (1 yes, 0 no).",
    );
    const hasError = resolved.replica.lastError ? 1 : 0;
    pushPrometheusMetricSample(lines, "raydb_replication_replica_last_error_present", hasError);
  }

  return `${lines.join("\n")}\n`;
}

// ============================================================================
// API Routes
// ============================================================================

export const apiRoutes = new Elysia({ prefix: "/api" })
  // --------------------------------------------------------------------------
  // Status
  // --------------------------------------------------------------------------
  .get("/status", async () => {
    return await getStatus();
  })

  // --------------------------------------------------------------------------
  // Replication (status / pull / promote)
  // --------------------------------------------------------------------------
  .get("/replication/status", async () => {
    const raw = getRawDb();
    if (!raw) {
      return {
        connected: false,
        error: "No database connected",
      };
    }

    try {
      const resolved = resolveReplicationStatus(raw);
      return {
        connected: true,
        authEnabled: resolveReplicationAdminConfig().authEnabled,
        role: resolved.role,
        primary: resolved.primary,
        replica: resolved.replica,
      };
    } catch (error) {
      return {
        connected: true,
        error:
          error instanceof Error
            ? error.message
            : "Failed to query replication status",
      };
    }
  })

  .get("/replication/metrics", async ({ request, set }) => {
    const auth = requireReplicationAdmin(request, set);
    if (!auth.ok) {
      return new Response(auth.error, {
        status: set.status ?? 401,
        headers: { "Content-Type": "text/plain; charset=utf-8" },
      });
    }

    const raw = getRawDb();
    if (!raw) {
      return new Response("No database connected", {
        status: 503,
        headers: { "Content-Type": "text/plain; charset=utf-8" },
      });
    }

    try {
      const resolved = resolveReplicationStatus(raw);
      const text = renderReplicationPrometheusMetrics(
        resolved,
        resolveReplicationAdminConfig().authEnabled,
      );
      return new Response(text, {
        headers: {
          "Content-Type": "text/plain; version=0.0.4; charset=utf-8",
          "Cache-Control": "no-store",
        },
      });
    } catch (error) {
      return new Response(
        error instanceof Error ? error.message : "Failed to render replication metrics",
        {
          status: 500,
          headers: { "Content-Type": "text/plain; charset=utf-8" },
        },
      );
    }
  })

  .get("/replication/snapshot/latest", async ({ query, request, set }) => {
    const auth = requireReplicationAdmin(request, set);
    if (!auth.ok) {
      return { success: false, error: auth.error };
    }

    const raw = getRawDb();
    if (!raw) {
      return { success: false, error: "No database connected" };
    }

    try {
      const resolved = resolveReplicationStatus(raw);
      if (resolved.role !== "primary" || !resolved.primary) {
        return {
          success: false,
          error: "Replication snapshot endpoint requires primary role",
        };
      }

      const dbPath = getDbPath();
      if (!dbPath) {
        return { success: false, error: "Database path unavailable" };
      }

      const includeData = parseBoolean((query as Record<string, unknown>).includeData, false);
      const bytes = await readFileBytes(dbPath);
      const sha256 = createHash("sha256").update(bytes).digest("hex");

      return {
        success: true,
        role: resolved.role,
        epoch: resolved.primary.epoch ?? null,
        headLogIndex: resolved.primary.headLogIndex ?? null,
        snapshot: {
          format: "single-file-db-copy",
          dbPath,
          byteLength: bytes.byteLength,
          sha256,
          generatedAt: new Date().toISOString(),
          dataBase64: includeData ? Buffer.from(bytes).toString("base64") : undefined,
        },
      };
    } catch (error) {
      return {
        success: false,
        error:
          error instanceof Error
            ? error.message
            : "Failed to prepare replication snapshot",
      };
    }
  })

  .get("/replication/log", async ({ query, request, set }) => {
    const auth = requireReplicationAdmin(request, set);
    if (!auth.ok) {
      return { success: false, error: auth.error };
    }

    const raw = getRawDb();
    if (!raw) {
      return { success: false, error: "No database connected" };
    }

    try {
      const resolved = resolveReplicationStatus(raw);
      if (resolved.role !== "primary" || !resolved.primary?.sidecarPath) {
        return {
          success: false,
          error: "Replication log endpoint requires primary role with sidecar",
        };
      }

      const queryObject = query as Record<string, unknown>;
      const maxBytes = parsePositiveInt(
        queryObject.maxBytes,
        REPLICATION_LOG_MAX_BYTES_DEFAULT,
        1,
        REPLICATION_LOG_MAX_BYTES_LIMIT,
      );
      const maxFrames = parsePositiveInt(
        queryObject.maxFrames,
        REPLICATION_PULL_MAX_FRAMES_DEFAULT,
        1,
        REPLICATION_PULL_MAX_FRAMES_LIMIT,
      );
      const includePayload = parseBoolean(queryObject.includePayload, true);
      const cursor = parseReplicationCursor(queryObject.cursor);

      const envelope = await readManifestEnvelope(resolved.primary.sidecarPath);
      const manifest = envelope.manifest;
      const segments = [...manifest.segments].sort((left, right) => left.id - right.id);

      const frames: Array<ReplicationFrameResponse> = [];
      let totalBytes = 0;
      let nextCursor = typeof queryObject.cursor === "string" ? queryObject.cursor : null;
      let limited = false;

      outer: for (const segment of segments) {
        const segmentId = BigInt(segment.id);
        const segmentPath = join(
          resolved.primary.sidecarPath,
          formatSegmentFileName(segmentId),
        );

        const segmentBytes = await readFileBytes(segmentPath);
        const view = new DataView(
          segmentBytes.buffer,
          segmentBytes.byteOffset,
          segmentBytes.byteLength,
        );

        let offset = 0;
        while (offset + 32 <= segmentBytes.byteLength) {
          const magic = view.getUint32(offset, true);
          if (magic !== 0x474f4c52) {
            break;
          }

          const _version = view.getUint16(offset + 4, true);
          const _flags = view.getUint16(offset + 6, true);
          const epoch = view.getBigUint64(offset + 8, true);
          const logIndex = view.getBigUint64(offset + 16, true);
          const payloadLength = view.getUint32(offset + 24, true);
          const payloadOffset = offset + 32;
          const payloadEnd = payloadOffset + payloadLength;
          if (payloadEnd > segmentBytes.byteLength) {
            break;
          }

          const frameBytes = payloadEnd - offset;
          const frameOffset = BigInt(offset);
          const frameAfterCursor = cursorAfterFrame(
            cursor,
            epoch,
            segmentId,
            frameOffset,
            logIndex,
          );

          if (frameAfterCursor) {
            if ((totalBytes + frameBytes > maxBytes && frames.length > 0) || frames.length >= maxFrames) {
              limited = true;
              break outer;
            }

            const payload = segmentBytes.subarray(payloadOffset, payloadEnd);
            const nextOffset = BigInt(payloadEnd);
            nextCursor = `${epoch}:${segmentId}:${nextOffset}:${logIndex}`;

            frames.push({
              epoch: epoch.toString(),
              logIndex: logIndex.toString(),
              segmentId: segmentId.toString(),
              segmentOffset: frameOffset.toString(),
              payloadBase64: includePayload
                ? Buffer.from(payload).toString("base64")
                : "",
              bytes: frameBytes,
            });
            totalBytes += frameBytes;
          }

          offset = payloadEnd;
        }
      }

      return {
        success: true,
        role: resolved.role,
        epoch: manifest.epoch,
        headLogIndex: manifest.head_log_index,
        retainedFloor: manifest.retained_floor,
        cursor: typeof queryObject.cursor === "string" ? queryObject.cursor : null,
        nextCursor,
        eof: !limited,
        frameCount: frames.length,
        totalBytes,
        frames,
      };
    } catch (error) {
      return {
        success: false,
        error:
          error instanceof Error
            ? error.message
            : "Failed to fetch replication log",
      };
    }
  })

  .get("/replication/transport/snapshot", async ({ query, request, set }) => {
    const auth = requireReplicationAdmin(request, set);
    if (!auth.ok) {
      return { success: false, error: auth.error };
    }

    const raw = getRawDb();
    if (!raw) {
      return { success: false, error: "No database connected" };
    }

    try {
      const includeData = parseBoolean((query as Record<string, unknown>).includeData, false);
      const exported = callRawMethod<string>(
        raw,
        [
          "exportReplicationSnapshotTransportJson",
          "export_replication_snapshot_transport_json",
        ],
        includeData,
      );
      const snapshot = JSON.parse(exported) as Record<string, unknown>;
      return {
        success: true,
        snapshot,
      };
    } catch (error) {
      return {
        success: false,
        error:
          error instanceof Error
            ? error.message
            : "Failed to export replication transport snapshot",
      };
    }
  })

  .get("/replication/transport/log", async ({ query, request, set }) => {
    const auth = requireReplicationAdmin(request, set);
    if (!auth.ok) {
      return { success: false, error: auth.error };
    }

    const raw = getRawDb();
    if (!raw) {
      return { success: false, error: "No database connected" };
    }

    try {
      const queryObject = query as Record<string, unknown>;
      const maxBytes = parsePositiveInt(
        queryObject.maxBytes,
        REPLICATION_LOG_MAX_BYTES_DEFAULT,
        1,
        REPLICATION_LOG_MAX_BYTES_LIMIT,
      );
      const maxFrames = parsePositiveInt(
        queryObject.maxFrames,
        REPLICATION_PULL_MAX_FRAMES_DEFAULT,
        1,
        REPLICATION_PULL_MAX_FRAMES_LIMIT,
      );
      const includePayload = parseBoolean(queryObject.includePayload, true);
      const cursor = typeof queryObject.cursor === "string" ? queryObject.cursor : null;

      const exported = callRawMethod<string>(
        raw,
        [
          "exportReplicationLogTransportJson",
          "export_replication_log_transport_json",
        ],
        cursor,
        maxFrames,
        maxBytes,
        includePayload,
      );
      const payload = JSON.parse(exported) as Record<string, unknown>;
      return {
        success: true,
        ...(payload as object),
      };
    } catch (error) {
      return {
        success: false,
        error:
          error instanceof Error
            ? error.message
            : "Failed to export replication transport log",
      };
    }
  })

  .post(
    "/replication/pull",
    async ({ body, request, set }) => {
      const auth = requireReplicationAdmin(request, set);
      if (!auth.ok) {
        return { success: false, error: auth.error };
      }

      const raw = getRawDb();
      if (!raw) {
        return { success: false, error: "No database connected" };
      }

      const maxFrames = Math.min(
        Math.max(body.maxFrames ?? REPLICATION_PULL_MAX_FRAMES_DEFAULT, 1),
        REPLICATION_PULL_MAX_FRAMES_LIMIT,
      );

      try {
        const applied = callRawMethod<number>(
          raw,
          ["replicaCatchUpOnce", "replica_catch_up_once"],
          maxFrames,
        );
        const resolved = resolveReplicationStatus(raw);

        return {
          success: true,
          appliedFrames: applied,
          role: resolved.role,
          replica: resolved.replica,
        };
      } catch (error) {
        return {
          success: false,
          error:
            error instanceof Error
              ? error.message
              : "Replication pull failed",
        };
      }
    },
    {
      body: t.Object({
        maxFrames: t.Optional(t.Number()),
      }),
    },
  )

  .post("/replication/reseed", async ({ request, set }) => {
    const auth = requireReplicationAdmin(request, set);
    if (!auth.ok) {
      return { success: false, error: auth.error };
    }

    const raw = getRawDb();
    if (!raw) {
      return { success: false, error: "No database connected" };
    }

    try {
      callRawMethod<void>(
        raw,
        ["replicaReseedFromSnapshot", "replica_reseed_from_snapshot"],
      );
      const resolved = resolveReplicationStatus(raw);

      return {
        success: true,
        role: resolved.role,
        replica: resolved.replica,
      };
    } catch (error) {
      return {
        success: false,
        error:
          error instanceof Error
            ? error.message
            : "Replica reseed failed",
      };
    }
  })

  .post("/replication/promote", async ({ request, set }) => {
    const auth = requireReplicationAdmin(request, set);
    if (!auth.ok) {
      return { success: false, error: auth.error };
    }

    const raw = getRawDb();
    if (!raw) {
      return { success: false, error: "No database connected" };
    }

    try {
      const epoch = callRawMethod<number>(
        raw,
        ["primaryPromoteToNextEpoch", "primary_promote_to_next_epoch"],
      );
      const resolved = resolveReplicationStatus(raw);

      return {
        success: true,
        epoch,
        role: resolved.role,
        primary: resolved.primary,
      };
    } catch (error) {
      return {
        success: false,
        error:
          error instanceof Error
            ? error.message
            : "Primary promote failed",
      };
    }
  })

  // --------------------------------------------------------------------------
  // Database Management
  // --------------------------------------------------------------------------
  .post(
    "/db/open",
    async ({ body }) => {
      return await openDatabase(body.path, body.options as PlaygroundOpenOptions | undefined);
    },
    {
      body: t.Object({
        path: t.String(),
        options: t.Optional(
          t.Object({
            readOnly: t.Optional(t.Boolean()),
            createIfMissing: t.Optional(t.Boolean()),
            mvcc: t.Optional(t.Boolean()),
            mvccGcIntervalMs: t.Optional(t.Number()),
            mvccRetentionMs: t.Optional(t.Number()),
            mvccMaxChainDepth: t.Optional(t.Number()),
            syncMode: t.Optional(t.Union([t.Literal("Full"), t.Literal("Normal"), t.Literal("Off")])),
            groupCommitEnabled: t.Optional(t.Boolean()),
            groupCommitWindowMs: t.Optional(t.Number()),
            walSizeMb: t.Optional(t.Number()),
            checkpointThreshold: t.Optional(t.Number()),
            replicationRole: t.Optional(
              t.Union([
                t.Literal("disabled"),
                t.Literal("primary"),
                t.Literal("replica"),
              ]),
            ),
            replicationSidecarPath: t.Optional(t.String()),
            replicationSourceDbPath: t.Optional(t.String()),
            replicationSourceSidecarPath: t.Optional(t.String()),
            replicationSegmentMaxBytes: t.Optional(t.Number()),
            replicationRetentionMinEntries: t.Optional(t.Number()),
            replicationRetentionMinMs: t.Optional(t.Number()),
          }),
        ),
      }),
    }
  )

  .post(
    "/db/upload",
    async ({ body }) => {
      const file = body.file;
      if (!file) {
        return { success: false, error: "No file provided" };
      }

      if (file.size > MAX_FILE_SIZE) {
        return { success: false, error: `File too large. Maximum size is ${MAX_FILE_SIZE / 1024 / 1024}MB` };
      }

      const buffer = new Uint8Array(await file.arrayBuffer());
      return await openFromBuffer(buffer);
    },
    {
      body: t.Object({
        file: t.File(),
      }),
    }
  )

  .post("/db/demo", async () => {
    return await createDemo();
  })

  .post("/db/close", async () => {
    return await closeDatabase();
  })

  // --------------------------------------------------------------------------
  // Stats
  // --------------------------------------------------------------------------
  .get("/stats", async () => {
    const db = getDb();
    if (!db) {
      return { error: "No database connected" };
    }

    const stats = await db.stats();
    const nodeCount = Number(stats.snapshotNodes) + stats.deltaNodesCreated - stats.deltaNodesDeleted;
    const edgeCount = Number(stats.snapshotEdges) + stats.deltaEdgesAdded - stats.deltaEdgesDeleted;

    return {
      nodes: nodeCount,
      edges: edgeCount,
      snapshotGen: stats.snapshotGen.toString(),
      walSegment: stats.walSegment.toString(),
      walBytes: Number(stats.walBytes),
      recommendCompact: stats.recommendCompact,
    };
  })

  // --------------------------------------------------------------------------
  // Graph Network (for visualization)
  // --------------------------------------------------------------------------
  .get("/graph/network", async () => {
    const db = getDb();
    if (!db) {
      return { nodes: [], edges: [], truncated: false, error: "No database connected" };
    }

    const graph = loadGraph(db);
    const truncated = graph.nodes.length > MAX_NODES;
    const shownNodes = graph.nodes.slice(0, MAX_NODES);
    const shownIds = new Set(shownNodes.map((node) => node.id));

    const visEdges: VisEdge[] = [];
    const degrees = new Map<number, number>();
    for (const edge of graph.edges) {
      if (!shownIds.has(edge.src) || !shownIds.has(edge.dst)) {
        continue;
      }
      visEdges.push({
        source: graph.keyById.get(edge.src)!,
        target: graph.keyById.get(edge.dst)!,
        type: edge.type,
      });
      degrees.set(edge.src, (degrees.get(edge.src) ?? 0) + 1);
      degrees.set(edge.dst, (degrees.get(edge.dst) ?? 0) + 1);
    }

    const visNodes: VisNode[] = shownNodes.map((node) => ({
      id: node.key,
      label: nodeLabel(node),
      type: node.type,
      color: NODE_COLORS[node.type] || "#94a3b8",
      degree: degrees.get(node.id) ?? 0,
    }));

    return { nodes: visNodes, edges: visEdges, truncated };
  })

  // --------------------------------------------------------------------------
  // Path Finding
  // --------------------------------------------------------------------------
  .post(
    "/graph/path",
    async ({ body }) => {
      const db = getDb();
      if (!db) {
        return { error: "No database connected" };
      }

      const graph = loadGraph(db);
      const startNodeId = graph.idByKey.get(body.startKey);
      const endNodeId = graph.idByKey.get(body.endKey);
      if (startNodeId === undefined || endNodeId === undefined) {
        return { error: "Start or end node not found" };
      }

      // Shortest path by hop count along outgoing edges
      const result = db.path(startNodeId, endNodeId).bfs();
      if (!result.found) {
        return { error: "No path found" };
      }

      return {
        path: result.path.map((nodeId) => graph.keyById.get(nodeId) ?? String(nodeId)),
        edges: result.edges.map((edge) => graph.edgeTypeNames.get(edge.etype) ?? "unknown"),
      };
    },
    {
      body: t.Object({
        startKey: t.String(),
        endKey: t.String(),
      }),
    }
  )

  // --------------------------------------------------------------------------
  // Impact Analysis
  // --------------------------------------------------------------------------
  .post(
    "/graph/impact",
    async ({ body }) => {
      const db = getDb();
      if (!db) {
        return { error: "No database connected" };
      }

      const graph = loadGraph(db);
      const startNodeId = graph.idByKey.get(body.nodeKey);
      if (startNodeId === undefined) {
        return { error: "Node not found" };
      }

      const incoming = new Map<number, Array<{ src: number; type: string }>>();
      for (const edge of graph.edges) {
        const list = incoming.get(edge.dst);
        if (list) {
          list.push(edge);
        } else {
          incoming.set(edge.dst, [edge]);
        }
      }

      // BFS over incoming edges: everything that (transitively) depends on this node
      const impacted = new Set<string>();
      const edgeTypes = new Set<string>();
      const visited = new Set<number>([startNodeId]);
      const queue: number[] = [startNodeId];

      while (queue.length > 0) {
        const current = queue.shift()!;
        for (const edge of incoming.get(current) ?? []) {
          if (visited.has(edge.src)) {
            continue;
          }
          visited.add(edge.src);
          const key = graph.keyById.get(edge.src);
          if (key) {
            impacted.add(key);
            edgeTypes.add(edge.type);
          }
          queue.push(edge.src);
        }
      }

      return {
        impacted: Array.from(impacted),
        edges: Array.from(edgeTypes),
      };
    },
    {
      body: t.Object({
        nodeKey: t.String(),
      }),
    }
  );
