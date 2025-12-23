/**
 * API Client
 *
 * Fetch wrappers for all backend endpoints.
 */

import type {
  StatusResponse,
  StatsResponse,
  GraphNetwork,
  PathResponse,
  ImpactResponse,
  ApiResult,
} from "./types.ts";

const API_BASE = "/api";

// ============================================================================
// Helper
// ============================================================================

async function fetchJson<T>(url: string, options?: RequestInit): Promise<T> {
  const response = await fetch(`${API_BASE}${url}`, {
    ...options,
    headers: {
      "Content-Type": "application/json",
      ...options?.headers,
    },
  });
  
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}: ${response.statusText}`);
  }
  
  return response.json();
}

// ============================================================================
// Database Management
// ============================================================================

export async function getStatus(): Promise<StatusResponse> {
  return fetchJson<StatusResponse>("/status");
}

export async function openDatabase(path: string): Promise<ApiResult> {
  return fetchJson<ApiResult>("/db/open", {
    method: "POST",
    body: JSON.stringify({ path }),
  });
}

export async function uploadDatabase(file: File): Promise<ApiResult> {
  const formData = new FormData();
  formData.append("file", file);
  
  const response = await fetch(`${API_BASE}/db/upload`, {
    method: "POST",
    body: formData,
  });
  
  return response.json();
}

export async function createDemo(): Promise<ApiResult> {
  return fetchJson<ApiResult>("/db/demo", {
    method: "POST",
  });
}

export async function closeDatabase(): Promise<ApiResult> {
  return fetchJson<ApiResult>("/db/close", {
    method: "POST",
  });
}

// ============================================================================
// Stats
// ============================================================================

export async function getStats(): Promise<StatsResponse> {
  return fetchJson<StatsResponse>("/stats");
}

// ============================================================================
// Graph Network
// ============================================================================

export async function getGraphNetwork(): Promise<GraphNetwork> {
  return fetchJson<GraphNetwork>("/graph/network");
}

// ============================================================================
// Path Finding
// ============================================================================

export async function findPath(startKey: string, endKey: string): Promise<PathResponse> {
  return fetchJson<PathResponse>("/graph/path", {
    method: "POST",
    body: JSON.stringify({ startKey, endKey }),
  });
}

// ============================================================================
// Impact Analysis
// ============================================================================

export async function analyzeImpact(nodeKey: string): Promise<ImpactResponse> {
  return fetchJson<ImpactResponse>("/graph/impact", {
    method: "POST",
    body: JSON.stringify({ nodeKey }),
  });
}
