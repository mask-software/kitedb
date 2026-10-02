import { createHash, timingSafeEqual } from 'node:crypto'

import {
  collectReplicationLogTransportJson,
  collectReplicationMetricsOtelJson,
  collectReplicationMetricsPrometheus,
  collectReplicationSnapshotTransportJson,
} from '../index'
import type { Database } from '../index'

export interface ReplicationSnapshotTransport {
  format: string
  db_path: string
  byte_length: number
  checksum_crc32c: string
  generated_at_ms: number
  epoch: number
  head_log_index: number
  retained_floor: number
  start_cursor: string
  data_base64?: string | null
}

export interface ReplicationLogTransportFrame {
  epoch: number
  log_index: number
  segment_id: number
  segment_offset: number
  bytes: number
  payload_base64?: string | null
}

export interface ReplicationLogTransportPage {
  epoch: number
  head_log_index: number
  retained_floor: number
  cursor?: string | null
  next_cursor?: string | null
  eof: boolean
  frame_count: number
  total_bytes: number
  frames: ReplicationLogTransportFrame[]
}

export interface ReplicationLogTransportOptions {
  cursor?: string | null
  maxFrames?: number
  maxBytes?: number
  includePayload?: boolean
}

export interface ReplicationTransportAdapter {
  snapshot(includeData?: boolean): ReplicationSnapshotTransport
  log(options?: ReplicationLogTransportOptions): ReplicationLogTransportPage
  metricsPrometheus(): string
  metricsOtelJson(): string
}

export type ReplicationAdminAuthMode = 'none' | 'token' | 'mtls' | 'token_or_mtls' | 'token_and_mtls'

export interface ReplicationAdminAuthRequest {
  headers?: Record<string, string | undefined> | null
}

/**
 * Replication admin auth policy.
 *
 * The mTLS modes need a check: an `mtlsMatcher` (e.g.
 * `createNodeTlsMtlsMatcher()` or `createForwardedTlsMtlsMatcher()`), or
 * `trustForwardedClientCert` with `mtlsSubjectRegex`. A config that cannot be
 * satisfied safely is rejected when it is used.
 */
export interface ReplicationAdminAuthConfig<
  TRequest extends ReplicationAdminAuthRequest = ReplicationAdminAuthRequest,
> {
  /** Required; `'none'` disables auth explicitly. */
  mode: ReplicationAdminAuthMode
  /** Bearer token for the token modes, compared in constant time. */
  token?: string | null
  /** Header carrying the proxy-verified client certificate (default `x-forwarded-client-cert`). */
  mtlsHeader?: string
  /**
   * Pattern the whole `mtlsHeader` value must match: it is anchored at both
   * ends (as `^(?:pattern)$`), so `/CN=admin/` does not match `CN=x,OU=CN=admin`.
   */
  mtlsSubjectRegex?: RegExp | null
  /**
   * Accept `mtlsHeader` as proof of a client certificate when its value matches
   * `mtlsSubjectRegex` (required). Enable it only behind a TLS-terminating proxy
   * that verifies client certificates and overwrites this header on every
   * request: otherwise any client can send it.
   */
  trustForwardedClientCert?: boolean
  /** Custom mTLS check; takes precedence over the forwarded header. */
  mtlsMatcher?: (request: TRequest) => boolean
}

export interface ReplicationNodeTlsLikeSocket {
  authorized?: boolean | null
  getPeerCertificate?: () => unknown
}

export interface ReplicationNodeTlsLikeRequest extends ReplicationAdminAuthRequest {
  socket?: ReplicationNodeTlsLikeSocket | null
  client?: ReplicationNodeTlsLikeSocket | null
  raw?: { socket?: ReplicationNodeTlsLikeSocket | null } | null
  req?: { socket?: ReplicationNodeTlsLikeSocket | null } | null
}

export interface ReplicationNodeMtlsMatcherOptions {
  requirePeerCertificate?: boolean
}

export interface ReplicationForwardedMtlsMatcherOptions {
  requirePeerCertificate?: boolean
  requireVerifyHeader?: boolean
  verifyHeaders?: string[]
  certHeaders?: string[]
  successValues?: string[]
}

const REPLICATION_ADMIN_AUTH_MODES = new Set<ReplicationAdminAuthMode>([
  'none',
  'token',
  'mtls',
  'token_or_mtls',
  'token_and_mtls',
])

const DEFAULT_FORWARDED_VERIFY_HEADERS = ['x-client-verify', 'ssl-client-verify']
const DEFAULT_FORWARDED_CERT_HEADERS = ['x-forwarded-client-cert', 'x-client-cert']
const DEFAULT_FORWARDED_SUCCESS_VALUES = ['success', 'successful', 'true', '1', 'yes', 'verified', 'ok']

function hasPeerCertificate(socket: ReplicationNodeTlsLikeSocket): boolean {
  if (!socket.getPeerCertificate) return false
  try {
    const certificate = socket.getPeerCertificate()
    if (!certificate || typeof certificate !== 'object') return false
    return Object.keys(certificate as Record<string, unknown>).length > 0
  } catch {
    return false
  }
}

function isSocketAuthorized(
  socket: ReplicationNodeTlsLikeSocket | null | undefined,
  options: Required<ReplicationNodeMtlsMatcherOptions>,
): boolean {
  if (!socket || socket.authorized !== true) return false
  if (!options.requirePeerCertificate) return true
  return hasPeerCertificate(socket)
}

export function isNodeTlsClientAuthorized(
  request: ReplicationNodeTlsLikeRequest,
  options: ReplicationNodeMtlsMatcherOptions = {},
): boolean {
  const resolved: Required<ReplicationNodeMtlsMatcherOptions> = {
    requirePeerCertificate: options.requirePeerCertificate ?? false,
  }
  return (
    isSocketAuthorized(request.socket, resolved) ||
    isSocketAuthorized(request.client, resolved) ||
    isSocketAuthorized(request.raw?.socket, resolved) ||
    isSocketAuthorized(request.req?.socket, resolved)
  )
}

export function createNodeTlsMtlsMatcher(
  options: ReplicationNodeMtlsMatcherOptions = {},
): (request: ReplicationNodeTlsLikeRequest) => boolean {
  return (request: ReplicationNodeTlsLikeRequest): boolean => isNodeTlsClientAuthorized(request, options)
}

function normalizeHeaderNames(headers: string[] | undefined, fallback: string[]): string[] {
  const names = (headers ?? fallback)
    .map((name) => name.trim().toLowerCase())
    .filter((name) => name.length > 0)
  if (names.length > 0) return names
  return fallback
}

function normalizeHeaderValues(values: string[] | undefined, fallback: string[]): Set<string> {
  const normalized = (values ?? fallback)
    .map((value) => value.trim().toLowerCase())
    .filter((value) => value.length > 0)
  if (normalized.length > 0) return new Set(normalized)
  return new Set(fallback)
}

export function isForwardedTlsClientAuthorized(
  request: ReplicationAdminAuthRequest,
  options: ReplicationForwardedMtlsMatcherOptions = {},
): boolean {
  const verifyHeaders = normalizeHeaderNames(options.verifyHeaders, DEFAULT_FORWARDED_VERIFY_HEADERS)
  const certHeaders = normalizeHeaderNames(options.certHeaders, DEFAULT_FORWARDED_CERT_HEADERS)
  const successValues = normalizeHeaderValues(options.successValues, DEFAULT_FORWARDED_SUCCESS_VALUES)
  const requireVerifyHeader = options.requireVerifyHeader ?? true
  const requirePeerCertificate = options.requirePeerCertificate ?? false

  const verifyValues: string[] = []
  for (const header of verifyHeaders) {
    const value = getHeaderValue(request, header)
    if (value) verifyValues.push(value.toLowerCase())
  }
  const verifyOk = verifyValues.length > 0
    ? verifyValues.some((value) => successValues.has(value))
    : !requireVerifyHeader
  if (!verifyOk) return false

  if (!requirePeerCertificate) return true
  for (const header of certHeaders) {
    if (getHeaderValue(request, header)) return true
  }
  return false
}

export function createForwardedTlsMtlsMatcher(
  options: ReplicationForwardedMtlsMatcherOptions = {},
): (request: ReplicationAdminAuthRequest) => boolean {
  return (request: ReplicationAdminAuthRequest): boolean =>
    isForwardedTlsClientAuthorized(request, options)
}

type NormalizedReplicationAdminAuthConfig<TRequest extends ReplicationAdminAuthRequest = ReplicationAdminAuthRequest> =
  {
    mode: ReplicationAdminAuthMode
    token: string | null
    mtlsHeader: string
    /** The subject pattern anchored at both ends; set only when the header is trusted. */
    trustedSubject: RegExp | null
    mtlsMatcher: ((request: TRequest) => boolean) | null
  }

/** `pattern` anchored to match a whole string, without stateful (g, y) or multiline flags. */
function wholeValuePattern(pattern: RegExp): RegExp {
  return new RegExp(`^(?:${pattern.source})$`, pattern.flags.replace(/[gmy]/g, ''))
}

function normalizeReplicationAdminAuthConfig<
  TRequest extends ReplicationAdminAuthRequest = ReplicationAdminAuthRequest,
>(config: ReplicationAdminAuthConfig<TRequest>): NormalizedReplicationAdminAuthConfig<TRequest> {
  const modeRaw: unknown = config.mode
  if (modeRaw === undefined || modeRaw === null) {
    throw new Error(
      "replication admin auth requires a mode (none|token|mtls|token_or_mtls|token_and_mtls); use mode: 'none' to disable auth explicitly",
    )
  }
  if (!REPLICATION_ADMIN_AUTH_MODES.has(modeRaw as ReplicationAdminAuthMode)) {
    throw new Error(
      `Invalid replication admin auth mode '${String(modeRaw)}'; expected none|token|mtls|token_or_mtls|token_and_mtls`,
    )
  }
  const mode = modeRaw as ReplicationAdminAuthMode
  const token = config.token?.trim() || null
  if ((mode === 'token' || mode === 'token_or_mtls' || mode === 'token_and_mtls') && !token) {
    throw new Error(`replication admin auth mode '${mode}' requires a non-empty token`)
  }
  const mtlsHeaderRaw = config.mtlsHeader?.trim().toLowerCase()
  const mtlsHeader = mtlsHeaderRaw && mtlsHeaderRaw.length > 0 ? mtlsHeaderRaw : 'x-forwarded-client-cert'
  const mtlsMatcher = config.mtlsMatcher ?? null

  let trustedSubject: RegExp | null = null
  if (config.trustForwardedClientCert) {
    if (!config.mtlsSubjectRegex) {
      throw new Error(
        `replication admin auth: trustForwardedClientCert requires mtlsSubjectRegex, the pattern a trusted '${mtlsHeader}' value must match`,
      )
    }
    trustedSubject = wholeValuePattern(config.mtlsSubjectRegex)
  }
  const usesMtls = mode === 'mtls' || mode === 'token_or_mtls' || mode === 'token_and_mtls'
  if (usesMtls && !mtlsMatcher && !trustedSubject) {
    throw new Error(
      `replication admin auth mode '${mode}' needs an mTLS check: an mtlsMatcher (e.g. createNodeTlsMtlsMatcher()) ` +
        `or trustForwardedClientCert: true with mtlsSubjectRegex. A client certificate header is not trusted by default, ` +
        'since any client can send it.',
    )
  }
  return { mode, token, mtlsHeader, trustedSubject, mtlsMatcher }
}

function getHeaderValue(request: ReplicationAdminAuthRequest, name: string): string | null {
  const headers = request.headers
  if (!headers) return null
  const direct = headers[name]
  if (typeof direct === 'string' && direct.trim().length > 0) {
    return direct.trim()
  }
  for (const [key, value] of Object.entries(headers)) {
    if (key.toLowerCase() !== name) continue
    if (typeof value !== 'string') continue
    const trimmed = value.trim()
    if (trimmed.length > 0) return trimmed
  }
  return null
}

function sha256(value: string): Uint8Array {
  return createHash('sha256').update(value).digest()
}

function isTokenMatch(request: ReplicationAdminAuthRequest, token: string | null): boolean {
  if (!token) return false
  const authorization = getHeaderValue(request, 'authorization')
  if (!authorization) return false
  // Equal-length digests: the comparison takes the same time for any input.
  return timingSafeEqual(sha256(authorization), sha256(`Bearer ${token}`))
}

function isMtlsMatch<TRequest extends ReplicationAdminAuthRequest = ReplicationAdminAuthRequest>(
  request: TRequest,
  config: NormalizedReplicationAdminAuthConfig<TRequest>,
): boolean {
  if (config.mtlsMatcher) {
    return config.mtlsMatcher(request)
  }
  if (!config.trustedSubject) return false
  const certValue = getHeaderValue(request, config.mtlsHeader)
  return certValue !== null && config.trustedSubject.test(certValue)
}

function isAuthorizedWithNormalized<TRequest extends ReplicationAdminAuthRequest = ReplicationAdminAuthRequest>(
  request: TRequest,
  config: NormalizedReplicationAdminAuthConfig<TRequest>,
): boolean {
  switch (config.mode) {
    case 'none':
      return true
    case 'token':
      return isTokenMatch(request, config.token)
    case 'mtls':
      return isMtlsMatch(request, config)
    case 'token_or_mtls':
      return isTokenMatch(request, config.token) || isMtlsMatch(request, config)
    case 'token_and_mtls':
      return isTokenMatch(request, config.token) && isMtlsMatch(request, config)
  }
}

export function isReplicationAdminAuthorized<
  TRequest extends ReplicationAdminAuthRequest = ReplicationAdminAuthRequest,
>(request: TRequest, config: ReplicationAdminAuthConfig<TRequest>): boolean {
  const normalized = normalizeReplicationAdminAuthConfig(config)
  return isAuthorizedWithNormalized(request, normalized)
}

export function authorizeReplicationAdminRequest<
  TRequest extends ReplicationAdminAuthRequest = ReplicationAdminAuthRequest,
>(request: TRequest, config: ReplicationAdminAuthConfig<TRequest>): void {
  const normalized = normalizeReplicationAdminAuthConfig(config)
  if (isAuthorizedWithNormalized(request, normalized)) {
    return
  }
  throw new Error(`Unauthorized: replication admin auth mode '${normalized.mode}' not satisfied`)
}

export function createReplicationAdminAuthorizer<
  TRequest extends ReplicationAdminAuthRequest = ReplicationAdminAuthRequest,
>(config: ReplicationAdminAuthConfig<TRequest>): (request: TRequest) => void {
  const normalized = normalizeReplicationAdminAuthConfig(config)
  return (request: TRequest): void => {
    if (isAuthorizedWithNormalized(request, normalized)) {
      return
    }
    throw new Error(`Unauthorized: replication admin auth mode '${normalized.mode}' not satisfied`)
  }
}

function parseJson<T>(raw: string, label: string): T {
  try {
    return JSON.parse(raw) as T
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    throw new Error(`Failed to parse ${label}: ${message}`)
  }
}

export function readReplicationSnapshotTransport(db: Database, includeData = false): ReplicationSnapshotTransport {
  const raw = collectReplicationSnapshotTransportJson(db, includeData)
  return parseJson<ReplicationSnapshotTransport>(raw, 'replication snapshot transport JSON')
}

export function readReplicationLogTransport(
  db: Database,
  options: ReplicationLogTransportOptions = {},
): ReplicationLogTransportPage {
  const raw = collectReplicationLogTransportJson(
    db,
    options.cursor ?? null,
    options.maxFrames ?? 128,
    options.maxBytes ?? 1024 * 1024,
    options.includePayload ?? true,
  )
  return parseJson<ReplicationLogTransportPage>(raw, 'replication log transport JSON')
}

export function createReplicationTransportAdapter(db: Database): ReplicationTransportAdapter {
  return {
    snapshot(includeData = false): ReplicationSnapshotTransport {
      return readReplicationSnapshotTransport(db, includeData)
    },
    log(options: ReplicationLogTransportOptions = {}): ReplicationLogTransportPage {
      return readReplicationLogTransport(db, options)
    },
    metricsPrometheus(): string {
      return collectReplicationMetricsPrometheus(db)
    },
    metricsOtelJson(): string {
      return collectReplicationMetricsOtelJson(db)
    },
  }
}
