import { existsSync, renameSync } from 'node:fs'
import { join } from 'node:path'

const root = process.cwd()
const from = join(root, 'core.wasm')
const to = join(root, 'core.wasm32-wasi.wasm')

if (existsSync(from) && !existsSync(to)) {
  renameSync(from, to)
  console.log(`Renamed ${from} -> ${to}`)
}

const fromDebug = join(root, 'core.debug.wasm')
const toDebug = join(root, 'core.wasm32-wasi.debug.wasm')
if (existsSync(fromDebug) && !existsSync(toDebug)) {
  renameSync(fromDebug, toDebug)
  console.log(`Renamed ${fromDebug} -> ${toDebug}`)
}
