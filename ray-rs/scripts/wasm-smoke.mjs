import { existsSync } from 'node:fs'
import { join } from 'node:path'
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)
const wasmCjs = join(process.cwd(), 'core.wasi.cjs')

if (!existsSync(wasmCjs)) {
  console.log('WASM loader not found. Run "npm run build:wasm" first.')
  process.exit(0)
}

const ray = require(wasmCjs)
const { Database, pathConfig } = ray

const db = Database.open('/tmp/raydb-wasm-smoke.raydb')

db.begin()
const a = db.createNode('a')
const b = db.createNode('b')
const edge = db.getOrCreateEtype('knows')

db.addEdge(a, edge, b)
db.commit()

const cfg = pathConfig(a, b)
cfg.allowedEdgeTypes = [edge]
const result = db.dijkstra(cfg)

if (!result.found) {
  console.error('WASM smoke test failed: path not found')
  process.exit(1)
}

console.log('WASM smoke test passed')

db.close()
