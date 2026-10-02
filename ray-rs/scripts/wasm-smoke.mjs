// Smoke test for the WASI build (`bun run build:wasm` or `build:wasm:debug`):
// load core.wasi.cjs in Node, write a small graph, find a path, checkpoint,
// then reopen the file and read the graph back.
import { existsSync, mkdtempSync, rmSync } from 'node:fs'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const require = createRequire(import.meta.url)
const wasmCjs = join(process.cwd(), 'core.wasi.cjs')

if (!existsSync(wasmCjs)) {
  console.error('WASM loader core.wasi.cjs not found. Run "bun run build:wasm" first.')
  process.exit(1)
}

const { Database, pathConfig } = require(wasmCjs)

function check(condition, message) {
  if (!condition) {
    throw new Error(`WASM smoke test failed: ${message}`)
  }
}

const dir = mkdtempSync(join(tmpdir(), 'kitedb-wasm-smoke-'))
const path = join(dir, 'smoke.kitedb')
try {
  const db = Database.open(path)
  db.begin()
  const a = db.createNode('a')
  const b = db.createNode('b')
  const knows = db.get_or_create_etype('knows')
  db.addEdge(a, knows, b)
  db.commit()

  const cfg = pathConfig(a, b)
  cfg.allowedEdgeTypes = [knows]
  check(db.dijkstra(cfg).found, 'path not found')
  db.checkpoint()
  db.close()

  const reopened = Database.open(path)
  check(reopened.countNodes() === 2, `expected 2 nodes after reopen, got ${reopened.countNodes()}`)
  check(reopened.get_node_by_key('b') === b, 'node "b" not found by key after reopen')
  check(reopened.edgeExists(a, knows, b), 'edge a -> b missing after reopen')
  check(reopened.check().valid, 'integrity check failed after reopen')
  reopened.close()
} finally {
  rmSync(dir, { recursive: true, force: true })
}

console.log('WASM smoke test passed')
