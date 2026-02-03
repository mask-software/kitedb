import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import { fromBinary } from '@bufbuild/protobuf'

import { Database, PropType } from '../index'
import { IndexSchema } from './scip/scip_pb'

type Mode = 'clean' | 'incremental'
type ProfileName = 'minimal' | 'argus' | 'argus-fast'
type EdgeCleanup = 'clear' | 'allowlist'

type Args = {
  repo?: string
  scipPath?: string
  scipBasePath?: string
  scipNextPath?: string
  dbPath?: string
  mode: Mode
  profile: ProfileName
  edgeCleanup: EdgeCleanup
  chunkTarget: number
  chunkMax: number
  importEdges: number
  exportSymbols: number
  callEdgesPerChunk: number
  nodeProps: number
  edgeProps: number
  indexer: string
  indexerArgs: string[]
  skipIndex: boolean
  batchSize: number
  fileBatch: number
  retryBatch: boolean
  changeRatio: number
  deleteRatio: number
  walSizeMb: number
  checkpointThreshold: number
  backgroundCheckpoint: boolean
}

type GraphProfile = {
  edgeCleanup: EdgeCleanup
  chunkTarget: number
  chunkMax: number
  importEdges: number
  exportSymbols: number
  callEdgesPerChunk: number
  nodeProps: number
  edgeProps: number
}

function scanProfile(argv: string[]): ProfileName {
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === '--profile') {
      return (argv[i + 1] as ProfileName) ?? 'minimal'
    }
  }
  return (process.env.SCIP_PROFILE as ProfileName | undefined) ?? 'minimal'
}

function profileDefaults(name: ProfileName): GraphProfile {
  if (name === 'argus') {
    return {
      edgeCleanup: 'allowlist',
      chunkTarget: 40,
      chunkMax: 20,
      importEdges: 3,
      exportSymbols: 4,
      callEdgesPerChunk: 4,
      nodeProps: 2,
      edgeProps: 1,
    }
  }
  if (name === 'argus-fast') {
    return {
      edgeCleanup: 'clear',
      chunkTarget: 120,
      chunkMax: 8,
      importEdges: 1,
      exportSymbols: 1,
      callEdgesPerChunk: 1,
      nodeProps: 0,
      edgeProps: 0,
    }
  }
  return {
    edgeCleanup: 'clear',
    chunkTarget: 10_000,
    chunkMax: 1,
    importEdges: 0,
    exportSymbols: 0,
    callEdgesPerChunk: 0,
    nodeProps: 0,
    edgeProps: 0,
  }
}

function parseArgs(argv: string[]): Args {
  const profile = scanProfile(argv)
  const defaults = profileDefaults(profile)
  const fileBatchDefault = profile === 'argus' || profile === 'argus-fast' ? 3000 : 0
  const retryBatchDefault = process.env.SCIP_BATCH_RETRY
    ? process.env.SCIP_BATCH_RETRY === '1'
    : fileBatchDefault > 0

  const args: Args = {
    mode: 'clean',
    profile,
    edgeCleanup: (process.env.SCIP_EDGE_CLEANUP as EdgeCleanup | undefined) ?? defaults.edgeCleanup,
    chunkTarget: Number.parseInt(process.env.SCIP_CHUNK_TARGET ?? String(defaults.chunkTarget), 10),
    chunkMax: Number.parseInt(process.env.SCIP_CHUNK_MAX ?? String(defaults.chunkMax), 10),
    importEdges: Number.parseInt(process.env.SCIP_IMPORT_EDGES ?? String(defaults.importEdges), 10),
    exportSymbols: Number.parseInt(process.env.SCIP_EXPORT_SYMBOLS ?? String(defaults.exportSymbols), 10),
    callEdgesPerChunk: Number.parseInt(process.env.SCIP_CALL_EDGES ?? String(defaults.callEdgesPerChunk), 10),
    nodeProps: Number.parseInt(process.env.SCIP_NODE_PROPS ?? String(defaults.nodeProps), 10),
    edgeProps: Number.parseInt(process.env.SCIP_EDGE_PROPS ?? String(defaults.edgeProps), 10),
    indexer: process.env.SCIP_INDEX_CMD ?? 'scip-typescript',
    indexerArgs: process.env.SCIP_INDEX_ARGS?.split(' ').filter(Boolean) ?? [],
    skipIndex: process.env.SCIP_SKIP_INDEX === '1',
    scipPath: process.env.SCIP_PATH,
    scipBasePath: process.env.SCIP_BASE,
    scipNextPath: process.env.SCIP_NEXT,
    batchSize: Number.parseInt(process.env.SCIP_BATCH_SIZE ?? '5000', 10),
    fileBatch: Number.parseInt(process.env.SCIP_FILE_BATCH ?? String(fileBatchDefault), 10),
    retryBatch: retryBatchDefault,
    changeRatio: Number.parseFloat(process.env.SCIP_CHANGE_RATIO ?? '0.8'),
    deleteRatio: Number.parseFloat(process.env.SCIP_DELETE_RATIO ?? '0.35'),
    walSizeMb: Number.parseInt(process.env.SCIP_WAL_SIZE_MB ?? '512', 10),
    checkpointThreshold: Number.parseFloat(process.env.SCIP_CHECKPOINT_THRESHOLD ?? '0.4'),
    backgroundCheckpoint: process.env.SCIP_BACKGROUND_CHECKPOINT === '1',
  }

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i]
    switch (arg) {
      case '--repo':
        args.repo = argv[++i]
        break
      case '--scip':
        args.scipPath = argv[++i]
        break
      case '--scip-base':
        args.scipBasePath = argv[++i]
        break
      case '--scip-next':
        args.scipNextPath = argv[++i]
        break
      case '--db':
        args.dbPath = argv[++i]
        break
      case '--mode':
        args.mode = (argv[++i] as Mode) ?? 'clean'
        break
      case '--profile':
        args.profile = (argv[++i] as ProfileName) ?? args.profile
        break
      case '--edge-cleanup':
        args.edgeCleanup = (argv[++i] as EdgeCleanup) ?? args.edgeCleanup
        break
      case '--chunk-target':
        args.chunkTarget = Number.parseInt(argv[++i] ?? String(args.chunkTarget), 10)
        break
      case '--chunk-max':
        args.chunkMax = Number.parseInt(argv[++i] ?? String(args.chunkMax), 10)
        break
      case '--import-edges':
        args.importEdges = Number.parseInt(argv[++i] ?? String(args.importEdges), 10)
        break
      case '--export-symbols':
        args.exportSymbols = Number.parseInt(argv[++i] ?? String(args.exportSymbols), 10)
        break
      case '--call-edges':
        args.callEdgesPerChunk = Number.parseInt(argv[++i] ?? String(args.callEdgesPerChunk), 10)
        break
      case '--node-props':
        args.nodeProps = Number.parseInt(argv[++i] ?? String(args.nodeProps), 10)
        break
      case '--edge-props':
        args.edgeProps = Number.parseInt(argv[++i] ?? String(args.edgeProps), 10)
        break
      case '--indexer':
        args.indexer = argv[++i] ?? args.indexer
        break
      case '--indexer-args':
        args.indexerArgs = (argv[++i] ?? '')
          .split(' ')
          .map((item) => item.trim())
          .filter(Boolean)
        break
      case '--skip-index':
        args.skipIndex = true
        break
      case '--batch-size':
        args.batchSize = Number.parseInt(argv[++i] ?? String(args.batchSize), 10)
        break
      case '--file-batch':
        args.fileBatch = Number.parseInt(argv[++i] ?? String(args.fileBatch), 10)
        break
      case '--retry-batch':
        args.retryBatch = true
        break
      case '--no-retry-batch':
        args.retryBatch = false
        break
      case '--change-ratio':
        args.changeRatio = Number.parseFloat(argv[++i] ?? String(args.changeRatio))
        break
      case '--delete-ratio':
        args.deleteRatio = Number.parseFloat(argv[++i] ?? String(args.deleteRatio))
        break
      case '--wal-size-mb':
        args.walSizeMb = Number.parseInt(argv[++i] ?? String(args.walSizeMb), 10)
        break
      case '--checkpoint-threshold':
        args.checkpointThreshold = Number.parseFloat(argv[++i] ?? String(args.checkpointThreshold))
        break
      case '--background-checkpoint':
        args.backgroundCheckpoint = true
        break
      default:
        break
    }
  }

  return args
}

function resolveProfile(args: Args): GraphProfile {
  return {
    edgeCleanup: args.edgeCleanup,
    chunkTarget: Math.max(1, args.chunkTarget),
    chunkMax: Math.max(1, args.chunkMax),
    importEdges: Math.max(0, args.importEdges),
    exportSymbols: Math.max(0, args.exportSymbols),
    callEdgesPerChunk: Math.max(0, args.callEdgesPerChunk),
    nodeProps: Math.max(0, args.nodeProps),
    edgeProps: Math.max(0, args.edgeProps),
  }
}

function ensureDir(dir: string): void {
  fs.mkdirSync(dir, { recursive: true })
}

function runIndexer(args: Args, repoRoot: string, scipPath: string): void {
  const indexer = args.indexer
  const indexArgs = [...args.indexerArgs]
  if (indexArgs.length === 0 && indexer === 'scip-typescript') {
    indexArgs.push('index', '--output', scipPath, '--cwd', repoRoot, '--infer-tsconfig')
  }

  let result = spawnSync(indexer, indexArgs, {
    cwd: repoRoot,
    stdio: 'inherit',
  })

  if (result.error && indexer === 'scip-typescript' && (result.error as NodeJS.ErrnoException).code === 'ENOENT') {
    result = spawnSync('npx', ['@sourcegraph/scip-typescript', ...indexArgs], {
      cwd: repoRoot,
      stdio: 'inherit',
    })
  }

  if (result.error) throw result.error
  if (result.status !== 0) {
    throw new Error(`indexer failed with exit code ${result.status ?? 'unknown'}`)
  }
}

type ScipDocument = {
  relativePath: string
  language: string
  occurrences: Array<{ symbol: string; symbolRoles: number }>
  symbols: Array<{ symbol: string; displayName?: string; kind?: number }>
}

type ScipIndex = {
  documents: ScipDocument[]
}

function loadIndex(scipPath: string): ScipIndex {
  const bytes = fs.readFileSync(scipPath)
  return fromBinary(IndexSchema, bytes) as ScipIndex
}

type HashValue = string | null

type DocFingerprint = {
  doc: ScipDocument
  hash: string
}

function collectCounts(values: string[]): Map<string, number> {
  const counts = new Map<string, number>()
  for (const value of values) {
    if (!value) continue
    counts.set(value, (counts.get(value) ?? 0) + 1)
  }
  return counts
}

function collectOccurrenceCounts(occurrences: Array<{ symbol: string; symbolRoles: number }>): Map<string, number> {
  const counts = new Map<string, number>()
  for (const occ of occurrences) {
    if (!occ.symbol) continue
    const key = `${occ.symbol}|${occ.symbolRoles ?? 0}`
    counts.set(key, (counts.get(key) ?? 0) + 1)
  }
  return counts
}

function hashCounts(language: string, defCounts: Map<string, number>, refCounts: Map<string, number>): string {
  const hash = createHash('sha256')
  hash.update(language)

  const defEntries = Array.from(defCounts.entries()).sort((a, b) => a[0].localeCompare(b[0]))
  for (const [symbol, count] of defEntries) {
    hash.update('D')
    hash.update(symbol)
    hash.update('#')
    hash.update(String(count))
  }

  const refEntries = Array.from(refCounts.entries()).sort((a, b) => a[0].localeCompare(b[0]))
  for (const [key, count] of refEntries) {
    hash.update('R')
    hash.update(key)
    hash.update('#')
    hash.update(String(count))
  }

  return hash.digest('hex')
}

function fingerprint(doc: ScipDocument): DocFingerprint {
  const defCounts = collectCounts(doc.symbols.map((sym) => sym.symbol))
  const refCounts = collectOccurrenceCounts(doc.occurrences)
  return {
    doc,
    hash: hashCounts(doc.language, defCounts, refCounts),
  }
}

function hashFile(filePath: string): HashValue {
  try {
    const data = fs.readFileSync(filePath)
    return createHash('sha256').update(data).digest('hex')
  } catch {
    return null
  }
}

function computeFileHashes(repoRoot: string, index: ScipIndex): Map<string, HashValue> {
  const hashes = new Map<string, HashValue>()
  for (const doc of index.documents) {
    const filePath = path.join(repoRoot, doc.relativePath)
    hashes.set(doc.relativePath, hashFile(filePath))
  }
  return hashes
}

function writeHashes(filePath: string, hashes: Map<string, HashValue>): void {
  const entries = Array.from(hashes.entries()).sort((a, b) => a[0].localeCompare(b[0]))
  const payload = Object.fromEntries(entries)
  fs.writeFileSync(filePath, JSON.stringify(payload, null, 2))
}

function loadHashes(filePath: string): Map<string, HashValue> | null {
  if (!fs.existsSync(filePath)) return null
  const payload = JSON.parse(fs.readFileSync(filePath, 'utf8')) as Record<string, HashValue>
  return new Map(Object.entries(payload))
}

function diffIndices(
  base: ScipIndex,
  next: ScipIndex,
  baseHashes?: Map<string, HashValue> | null,
  nextHashes?: Map<string, HashValue> | null,
) {
  const baseDocs = new Map(base.documents.map((doc) => [doc.relativePath, doc]))
  const nextDocs = new Map(next.documents.map((doc) => [doc.relativePath, doc]))

  if (baseHashes && nextHashes) {
    const deleted: ScipDocument[] = []
    const added: ScipDocument[] = []
    const updated: ScipDocument[] = []
    let unchanged = 0

    for (const [pathKey, baseHash] of baseHashes.entries()) {
      const nextHash = nextHashes.get(pathKey)
      if (nextHash === undefined) {
        const baseDoc = baseDocs.get(pathKey)
        if (baseDoc) deleted.push(baseDoc)
        continue
      }

      if (baseHash !== null && nextHash !== null) {
        if (baseHash === nextHash) {
          unchanged += 1
        } else {
          const nextDoc = nextDocs.get(pathKey)
          if (nextDoc) updated.push(nextDoc)
        }
        continue
      }

      const baseDoc = baseDocs.get(pathKey)
      const nextDoc = nextDocs.get(pathKey)
      if (!baseDoc || !nextDoc) {
        unchanged += 1
        continue
      }
      const baseFinger = fingerprint(baseDoc)
      const nextFinger = fingerprint(nextDoc)
      if (baseFinger.hash === nextFinger.hash) {
        unchanged += 1
      } else {
        updated.push(nextDoc)
      }
    }

    for (const [pathKey] of nextHashes.entries()) {
      if (!baseHashes.has(pathKey)) {
        const nextDoc = nextDocs.get(pathKey)
        if (nextDoc) added.push(nextDoc)
      }
    }

    return { added, updated, deleted, unchanged, mode: 'hash' as const }
  }

  const baseMap = new Map<string, DocFingerprint>()
  const nextMap = new Map<string, DocFingerprint>()

  for (const doc of base.documents) {
    baseMap.set(doc.relativePath, fingerprint(doc))
  }
  for (const doc of next.documents) {
    nextMap.set(doc.relativePath, fingerprint(doc))
  }

  const deleted: ScipDocument[] = []
  const added: ScipDocument[] = []
  const updated: ScipDocument[] = []
  let unchanged = 0

  for (const [pathKey, baseEntry] of baseMap.entries()) {
    const nextEntry = nextMap.get(pathKey)
    if (!nextEntry) {
      deleted.push(baseEntry.doc)
      continue
    }
    if (baseEntry.hash === nextEntry.hash) {
      unchanged += 1
    } else {
      updated.push(nextEntry.doc)
    }
  }

  for (const [pathKey, nextEntry] of nextMap.entries()) {
    if (!baseMap.has(pathKey)) {
      added.push(nextEntry.doc)
    }
  }

  return { added, updated, deleted, unchanged, mode: 'symbol' as const }
}

function pickRandom<T>(values: T[], count: number): T[] {
  if (count <= 0) return []
  if (count >= values.length) return values.slice()
  const result: T[] = []
  const used = new Set<number>()
  while (result.length < count) {
    const idx = Math.floor(Math.random() * values.length)
    if (used.has(idx)) continue
    used.add(idx)
    result.push(values[idx])
  }
  return result
}

function normalizeFileBatch(value: number): number {
  if (!Number.isFinite(value)) return 0
  if (value <= 0) return 0
  return Math.max(1, Math.floor(value))
}

function isWalFull(err: unknown): boolean {
  if (!err) return false
  const message = err instanceof Error ? err.message : String(err)
  return message.toLowerCase().includes('wal buffer full')
}

function stringHash(value: string): number {
  let hash = 2166136261
  for (let i = 0; i < value.length; i += 1) {
    hash ^= value.charCodeAt(i)
    hash = Math.imul(hash, 16777619)
  }
  return hash >>> 0
}

function pickStableTargets(
  seed: string,
  values: string[],
  count: number,
  skip?: string,
): string[] {
  if (count <= 0 || values.length === 0) return []
  const result: string[] = []
  const used = new Set<string>()
  const seedHash = stringHash(seed)
  for (let i = 0; i < values.length && result.length < count; i += 1) {
    const idx = (seedHash + i * 9973) % values.length
    const value = values[idx]
    if (!value || value === skip || used.has(value)) continue
    used.add(value)
    result.push(value)
  }
  return result
}

function openDb(dbPath: string, args: Args): Database {
  return Database.open(dbPath, {
    walSize: Math.max(args.walSizeMb, 8) * 1024 * 1024,
    autoCheckpoint: true,
    checkpointThreshold: args.checkpointThreshold,
    backgroundCheckpoint: args.backgroundCheckpoint,
  })
}

type OpState = { ops: number }

type EdgeSpec = { etype: number; dst: number; seed: string }

type EdgeSet = Map<string, EdgeSpec>

function edgeKey(etype: number, dst: number): string {
  return `${etype}:${dst}`
}

function flushIfNeeded(db: Database, state: OpState, batchSize: number, autoCommit: boolean): void {
  if (!autoCommit) return
  if (state.ops >= batchSize) {
    db.commit()
    db.begin()
    state.ops = 0
  }
}

function bumpOps(state: OpState, count = 1): void {
  state.ops += count
}

function applyExtraNodeProps(db: Database, nodeId: number, seed: string, count: number, state: OpState): void {
  for (let i = 0; i < count; i += 1) {
    const value = stringHash(`${seed}:${i}`)
    db.setNodePropByName(nodeId, `p${i}`, { propType: PropType.Int, intValue: value })
    bumpOps(state)
  }
}

function applyExtraEdgeProps(
  db: Database,
  src: number,
  etype: number,
  dst: number,
  seed: string,
  count: number,
  state: OpState,
): void {
  for (let i = 0; i < count; i += 1) {
    const value = stringHash(`${seed}:${i}`)
    db.setEdgePropByName(src, etype, dst, `p${i}`, {
      propType: PropType.Int,
      intValue: value,
    })
    bumpOps(state)
  }
}

function syncEdges(
  db: Database,
  src: number,
  desired: EdgeSet,
  profile: GraphProfile,
  state: OpState,
  batchSize: number,
  autoCommit: boolean,
): void {
  const existingEdges = db.getOutEdges(src)
  const existing = new Map<string, EdgeSpec>()
  for (const edge of existingEdges) {
    existing.set(edgeKey(edge.etype, edge.nodeId), {
      etype: edge.etype,
      dst: edge.nodeId,
      seed: `${src}:${edge.etype}:${edge.nodeId}`,
    })
  }

  if (profile.edgeCleanup === 'clear') {
    for (const edge of existing.values()) {
      db.deleteEdge(src, edge.etype, edge.dst)
      bumpOps(state)
      flushIfNeeded(db, state, batchSize, autoCommit)
    }
    existing.clear()
  } else {
    for (const edge of existing.values()) {
      if (!desired.has(edgeKey(edge.etype, edge.dst))) {
        db.deleteEdge(src, edge.etype, edge.dst)
        bumpOps(state)
        flushIfNeeded(db, state, batchSize, autoCommit)
      }
    }
  }

  for (const edge of desired.values()) {
    const key = edgeKey(edge.etype, edge.dst)
    if (!existing.has(key)) {
      db.addEdge(src, edge.etype, edge.dst)
      bumpOps(state)
    }
    if (profile.edgeProps > 0) {
      applyExtraEdgeProps(db, src, edge.etype, edge.dst, edge.seed, profile.edgeProps, state)
    }
    flushIfNeeded(db, state, batchSize, autoCommit)
  }
}

function clearFileEdges(
  db: Database,
  fileId: number,
  state: OpState,
  batchSize: number,
  autoCommit: boolean,
): void {
  const outEdges = db.getOutEdges(fileId)
  for (const edge of outEdges) {
    db.deleteEdge(fileId, edge.etype, edge.nodeId)
    bumpOps(state)
    flushIfNeeded(db, state, batchSize, autoCommit)
  }

  const inEdges = db.getInEdges(fileId)
  for (const edge of inEdges) {
    db.deleteEdge(edge.nodeId, edge.etype, fileId)
    bumpOps(state)
    flushIfNeeded(db, state, batchSize, autoCommit)
  }
}

function applyDoc(
  db: Database,
  doc: ScipDocument,
  nodeCache: Map<string, number>,
  state: OpState,
  batchSize: number,
  autoCommit: boolean,
  profile: GraphProfile,
  allDocPaths: string[],
  updateMode: boolean,
): void {
  const fileProp = 'path'
  const langProp = 'language'
  const indexedProp = 'indexed_at'
  const nameProp = 'name'
  const kindProp = 'kind'

  const contains = db.getOrCreateEtype('CONTAINS')
  const defines = db.getOrCreateEtype('DEFINES')
  const references = db.getOrCreateEtype('REFERENCES')
  const imports = db.getOrCreateEtype('IMPORTS')
  const exports = db.getOrCreateEtype('EXPORTS')
  const calls = db.getOrCreateEtype('CALLS')

  const ensureNode = (key: string): number => {
    const cached = nodeCache.get(key)
    if (cached !== undefined) return cached
    const existing = db.getNodeByKey(key)
    if (existing !== null) {
      nodeCache.set(key, existing)
      return existing
    }
    const created = db.createNode(key)
    nodeCache.set(key, created)
    return created
  }

  const fileKey = `file:${doc.relativePath}`
  const fileId = ensureNode(fileKey)
  db.setNodePropByName(fileId, fileProp, { propType: PropType.String, stringValue: doc.relativePath })
  bumpOps(state)
  db.setNodePropByName(fileId, langProp, { propType: PropType.String, stringValue: doc.language })
  bumpOps(state)
  if (updateMode) {
    db.setNodePropByName(fileId, indexedProp, { propType: PropType.Int, intValue: Date.now() })
    bumpOps(state)
  }
  if (profile.nodeProps > 0) {
    applyExtraNodeProps(db, fileId, fileKey, profile.nodeProps, state)
  }

  const symbolIds: number[] = []
  for (const sym of doc.symbols) {
    if (!sym.symbol) continue
    const symKey = `sym:${sym.symbol}`
    const symId = ensureNode(symKey)
    symbolIds.push(symId)
    if (sym.displayName) {
      db.setNodePropByName(symId, nameProp, { propType: PropType.String, stringValue: sym.displayName })
      bumpOps(state)
    }
    if (sym.kind !== undefined) {
      db.setNodePropByName(symId, kindProp, { propType: PropType.Int, intValue: sym.kind })
      bumpOps(state)
    }
    if (profile.nodeProps > 0) {
      applyExtraNodeProps(db, symId, symKey, profile.nodeProps, state)
    }
  }

  const occurrenceSymbols = doc.occurrences
    .map((occ) => occ.symbol)
    .filter((symbol) => Boolean(symbol)) as string[]

  const totalItems = doc.symbols.length + doc.occurrences.length
  const chunkCount = Math.min(profile.chunkMax, Math.max(1, Math.ceil(totalItems / profile.chunkTarget)))
  const chunkIds: number[] = []

  for (let i = 0; i < chunkCount; i += 1) {
    const chunkKey = `chunk:${doc.relativePath}:${i}`
    const chunkId = ensureNode(chunkKey)
    db.setNodePropByName(chunkId, fileProp, { propType: PropType.String, stringValue: doc.relativePath })
    bumpOps(state)
    db.setNodePropByName(chunkId, langProp, { propType: PropType.String, stringValue: doc.language })
    bumpOps(state)
    db.setNodePropByName(chunkId, 'index', { propType: PropType.Int, intValue: i })
    bumpOps(state)
    if (profile.nodeProps > 0) {
      applyExtraNodeProps(db, chunkId, chunkKey, profile.nodeProps, state)
    }
    chunkIds.push(chunkId)
  }

  if (updateMode && profile.chunkMax > chunkCount) {
    for (let i = chunkCount; i < profile.chunkMax; i += 1) {
      const chunkKey = `chunk:${doc.relativePath}:${i}`
      const chunkId = db.getNodeByKey(chunkKey)
      if (chunkId === null) continue
      clearFileEdges(db, chunkId, state, batchSize, autoCommit)
      db.deleteNode(chunkId)
      bumpOps(state)
      flushIfNeeded(db, state, batchSize, autoCommit)
    }
  }

  const desiredFileEdges: EdgeSet = new Map()
  for (const chunkId of chunkIds) {
    desiredFileEdges.set(edgeKey(contains, chunkId), {
      etype: contains,
      dst: chunkId,
      seed: `${fileId}:${contains}:${chunkId}`,
    })
  }

  for (const symId of symbolIds) {
    desiredFileEdges.set(edgeKey(defines, symId), {
      etype: defines,
      dst: symId,
      seed: `${fileId}:${defines}:${symId}`,
    })
  }

  if (profile.exportSymbols > 0) {
    for (const symId of symbolIds.slice(0, profile.exportSymbols)) {
      desiredFileEdges.set(edgeKey(exports, symId), {
        etype: exports,
        dst: symId,
        seed: `${fileId}:${exports}:${symId}`,
      })
    }
  }

  if (profile.importEdges > 0) {
    const importTargets = pickStableTargets(doc.relativePath, allDocPaths, profile.importEdges, doc.relativePath)
    for (const targetPath of importTargets) {
      const targetId = ensureNode(`file:${targetPath}`)
      desiredFileEdges.set(edgeKey(imports, targetId), {
        etype: imports,
        dst: targetId,
        seed: `${fileId}:${imports}:${targetId}`,
      })
    }
  }

  syncEdges(db, fileId, desiredFileEdges, profile, state, batchSize, autoCommit)

  for (let i = 0; i < chunkIds.length; i += 1) {
    const chunkId = chunkIds[i]
    const desiredChunkEdges: EdgeSet = new Map()

    const symbolsForChunk = symbolIds.filter((_, idx) => idx % chunkCount === i)
    for (const symId of symbolsForChunk) {
      desiredChunkEdges.set(edgeKey(contains, symId), {
        etype: contains,
        dst: symId,
        seed: `${chunkId}:${contains}:${symId}`,
      })
    }

    const occurrenceForChunk = occurrenceSymbols.filter((_, idx) => idx % chunkCount === i)
    const occurrenceIds = occurrenceForChunk.map((symbol) => ensureNode(`sym:${symbol}`))
    for (const symId of occurrenceIds) {
      desiredChunkEdges.set(edgeKey(references, symId), {
        etype: references,
        dst: symId,
        seed: `${chunkId}:${references}:${symId}`,
      })
    }

    if (profile.callEdgesPerChunk > 0) {
      for (const symId of occurrenceIds.slice(0, profile.callEdgesPerChunk)) {
        desiredChunkEdges.set(edgeKey(calls, symId), {
          etype: calls,
          dst: symId,
          seed: `${chunkId}:${calls}:${symId}`,
        })
      }
    }

    syncEdges(db, chunkId, desiredChunkEdges, profile, state, batchSize, autoCommit)
  }
}

function deleteDoc(
  db: Database,
  doc: ScipDocument,
  state: OpState,
  batchSize: number,
  autoCommit: boolean,
  profile: GraphProfile,
): void {
  const fileKey = `file:${doc.relativePath}`
  const fileId = db.getNodeByKey(fileKey)
  if (fileId === null) return
  clearFileEdges(db, fileId, state, batchSize, autoCommit)
  if (profile.chunkMax > 0) {
    for (let i = 0; i < profile.chunkMax; i += 1) {
      const chunkId = db.getNodeByKey(`chunk:${doc.relativePath}:${i}`)
      if (chunkId === null) continue
      clearFileEdges(db, chunkId, state, batchSize, autoCommit)
      db.deleteNode(chunkId)
      bumpOps(state)
      flushIfNeeded(db, state, batchSize, autoCommit)
    }
  }
  db.deleteNode(fileId)
  bumpOps(state)
  flushIfNeeded(db, state, batchSize, autoCommit)
}

function runFileBatches<T>(
  db: Database,
  items: T[],
  fileBatch: number,
  retryBatch: boolean,
  autoCommit: boolean,
  state: OpState,
  handler: (item: T) => void,
): void {
  if (items.length === 0) return
  const normalized = normalizeFileBatch(fileBatch)
  const batchSize = normalized === 0 ? items.length : normalized
  const allowRetry = retryBatch && !autoCommit
  if (retryBatch && autoCommit) {
    console.warn('[batch] auto-commit enabled; retry disabled')
  }

  const queue: T[][] = []
  for (let i = 0; i < items.length; i += batchSize) {
    queue.push(items.slice(i, i + batchSize))
  }

  while (queue.length > 0) {
    const batch = queue.shift()
    if (!batch) continue
    state.ops = 0
    if (!db.hasTransaction()) db.begin()

    try {
      for (const item of batch) {
        handler(item)
      }
      if (db.hasTransaction()) db.commit()
      state.ops = 0
    } catch (err) {
      if (db.hasTransaction()) {
        db.rollback()
      }
      state.ops = 0
      if (allowRetry && isWalFull(err) && batch.length > 1) {
        try {
          db.checkpoint()
        } catch {
          // ignore checkpoint failures before retry
        }
        const mid = Math.ceil(batch.length / 2)
        queue.unshift(batch.slice(mid))
        queue.unshift(batch.slice(0, mid))
        continue
      }
      throw err
    }
  }
}

function buildGraph(db: Database, index: ScipIndex, batchSize: number, profile: GraphProfile, autoCommit: boolean, fileBatch: number, retryBatch: boolean): void {
  const nodeCache = new Map<string, number>()
  const state: OpState = { ops: 0 }
  const allDocPaths = index.documents.map((doc) => doc.relativePath)

  runFileBatches(
    db,
    index.documents,
    fileBatch,
    retryBatch,
    autoCommit,
    state,
    (doc) => applyDoc(db, doc, nodeCache, state, batchSize, autoCommit, profile, allDocPaths, false),
  )
}

function incrementalGraphSynthetic(
  db: Database,
  index: ScipIndex,
  args: Args,
  profile: GraphProfile,
  autoCommit: boolean,
  fileBatch: number,
  retryBatch: boolean,
): void {
  const total = index.documents.length
  const changeCount = Math.floor(total * args.changeRatio)
  const deleteCount = Math.floor(changeCount * args.deleteRatio)
  const updateCount = Math.max(changeCount - deleteCount, 0)

  const changed = pickRandom(index.documents, changeCount)
  const toDelete = changed.slice(0, deleteCount)
  const toUpdate = changed.slice(deleteCount, deleteCount + updateCount)

  const nodeCache = new Map<string, number>()
  const state: OpState = { ops: 0 }
  const allDocPaths = index.documents.map((doc) => doc.relativePath)

  runFileBatches(
    db,
    toDelete,
    fileBatch,
    retryBatch,
    autoCommit,
    state,
    (doc) => deleteDoc(db, doc, state, args.batchSize, autoCommit, profile),
  )

  runFileBatches(
    db,
    toUpdate,
    fileBatch,
    retryBatch,
    autoCommit,
    state,
    (doc) => applyDoc(db, doc, nodeCache, state, args.batchSize, autoCommit, profile, allDocPaths, true),
  )
}

function incrementalGraphDiff(
  db: Database,
  base: ScipIndex,
  next: ScipIndex,
  args: Args,
  profile: GraphProfile,
  autoCommit: boolean,
  fileBatch: number,
  retryBatch: boolean,
  baseHashes?: Map<string, HashValue> | null,
  nextHashes?: Map<string, HashValue> | null,
  diffMode?: string,
): void {
  const diff = diffIndices(base, next, baseHashes, nextHashes)
  const modeLabel = diffMode ? `${diff.mode}/${diffMode}` : diff.mode
  console.log(
    `[diff] mode=${modeLabel} added=${diff.added.length} updated=${diff.updated.length} deleted=${diff.deleted.length} unchanged=${diff.unchanged}`,
  )

  const nodeCache = new Map<string, number>()
  const state: OpState = { ops: 0 }
  const allDocPaths = next.documents.map((doc) => doc.relativePath)

  runFileBatches(
    db,
    diff.deleted,
    fileBatch,
    retryBatch,
    autoCommit,
    state,
    (doc) => deleteDoc(db, doc, state, args.batchSize, autoCommit, profile),
  )

  runFileBatches(
    db,
    [...diff.added, ...diff.updated],
    fileBatch,
    retryBatch,
    autoCommit,
    state,
    (doc) => applyDoc(db, doc, nodeCache, state, args.batchSize, autoCommit, profile, allDocPaths, true),
  )
}

function main(): void {
  const args = parseArgs(process.argv.slice(2))
  const profile = resolveProfile(args)
  const fileBatch = normalizeFileBatch(args.fileBatch)
  const autoCommitOps = fileBatch === 0

  const repoRoot = args.repo ? path.resolve(args.repo) : undefined
  const basePath = args.scipBasePath ?? args.scipPath
  const nextPath = args.scipNextPath

  if (!repoRoot && !basePath && !nextPath) {
    throw new Error('missing --repo or --scip/--scip-base')
  }

  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-scip-'))
  const scipBasePath = basePath ? path.resolve(basePath) : path.join(tempDir, 'index-base.scip')
  const scipNextPath = nextPath ? path.resolve(nextPath) : path.join(tempDir, 'index-next.scip')
  const dbPath = args.dbPath ? path.resolve(args.dbPath) : path.join(tempDir, 'stress.kitedb')

  if (args.mode === 'clean') {
    if (!args.skipIndex) {
      if (!repoRoot) throw new Error('missing --repo for indexing')
      ensureDir(path.dirname(scipBasePath))
      const start = Date.now()
      runIndexer(args, repoRoot, scipBasePath)
      const elapsed = Date.now() - start
      console.log(`[scip] indexer ms=${elapsed}`)
    } else if (!fs.existsSync(scipBasePath)) {
      throw new Error(`missing scip index at ${scipBasePath}`)
    }

    const parseStart = Date.now()
    const index = loadIndex(scipBasePath)
    console.log(`[scip] documents=${index.documents.length} parse_ms=${Date.now() - parseStart}`)

    if (repoRoot) {
      const hashes = computeFileHashes(repoRoot, index)
      writeHashes(`${scipBasePath}.hashes.json`, hashes)
    }

    fs.rmSync(dbPath, { force: true })
    const db = openDb(dbPath, args)

    try {
      const start = Date.now()
      buildGraph(db, index, args.batchSize, profile, autoCommitOps, fileBatch, args.retryBatch)
      console.log(`[graph] clean_ms=${Date.now() - start}`)
      console.log(`[db] nodes=${db.countNodes()} edges=${db.countEdges()}`)
    } finally {
      db.close()
    }
    return
  }

  if (!basePath) {
    throw new Error('incremental mode requires --scip-base or --scip')
  }
  if (!fs.existsSync(scipBasePath)) {
    throw new Error(`missing scip index at ${scipBasePath}`)
  }

  const db = openDb(dbPath, args)

  try {
    if (nextPath) {
      if (!args.skipIndex) {
        if (!repoRoot) throw new Error('missing --repo for indexing')
        ensureDir(path.dirname(scipNextPath))
        const start = Date.now()
        runIndexer(args, repoRoot, scipNextPath)
        const elapsed = Date.now() - start
        console.log(`[scip] indexer ms=${elapsed}`)
      } else if (!fs.existsSync(scipNextPath)) {
        throw new Error(`missing scip index at ${scipNextPath}`)
      }

      const baseStart = Date.now()
      const baseIndex = loadIndex(scipBasePath)
      console.log(`[scip] base_documents=${baseIndex.documents.length} parse_ms=${Date.now() - baseStart}`)

      const nextStart = Date.now()
      const nextIndex = loadIndex(scipNextPath)
      console.log(`[scip] next_documents=${nextIndex.documents.length} parse_ms=${Date.now() - nextStart}`)

      let baseHashes: Map<string, HashValue> | null = null
      let nextHashes: Map<string, HashValue> | null = null
      let diffMode: string | undefined

      if (repoRoot) {
        baseHashes = loadHashes(`${scipBasePath}.hashes.json`)
        nextHashes = computeFileHashes(repoRoot, nextIndex)
        writeHashes(`${scipNextPath}.hashes.json`, nextHashes)
        if (baseHashes && nextHashes) {
          diffMode = 'filehash'
        }
      }

      const start = Date.now()
      incrementalGraphDiff(
        db,
        baseIndex,
        nextIndex,
        args,
        profile,
        autoCommitOps,
        fileBatch,
        args.retryBatch,
        baseHashes,
        nextHashes,
        diffMode,
      )
      console.log(`[graph] incremental_ms=${Date.now() - start}`)
    } else {
      console.warn('[scip] no --scip-next provided; using synthetic churn based on current index')
      const parseStart = Date.now()
      const index = loadIndex(scipBasePath)
      console.log(`[scip] documents=${index.documents.length} parse_ms=${Date.now() - parseStart}`)
      const start = Date.now()
      incrementalGraphSynthetic(db, index, args, profile, autoCommitOps, fileBatch, args.retryBatch)
      console.log(`[graph] incremental_ms=${Date.now() - start}`)
    }

    console.log(`[db] nodes=${db.countNodes()} edges=${db.countEdges()}`)
  } finally {
    db.close()
  }
}

main()
