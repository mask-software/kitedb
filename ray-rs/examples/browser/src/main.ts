const runButton = document.querySelector('#run') as HTMLButtonElement | null
const logEl = document.querySelector('#log') as HTMLPreElement | null

const log = (message: string) => {
  if (!logEl) return
  logEl.textContent += message + '\n'
}

const DB_PATH = '/demo.raydb'
const DB_IDB_KEY = 'raydb-demo'

const openDb = (): Promise<IDBDatabase> =>
  new Promise((resolve, reject) => {
    const request = indexedDB.open('raydb-wasm-demo', 1)
    request.onupgradeneeded = () => {
      const db = request.result
      if (!db.objectStoreNames.contains('files')) {
        db.createObjectStore('files')
      }
    }
    request.onsuccess = () => resolve(request.result)
    request.onerror = () => reject(request.error)
  })

const loadPersistedFile = async (): Promise<Uint8Array | ArrayBuffer | null> => {
  if (!('indexedDB' in window)) return null
  const db = await openDb()
  return new Promise((resolve, reject) => {
    const tx = db.transaction('files', 'readonly')
    const store = tx.objectStore('files')
    const request = store.get(DB_IDB_KEY)
    request.onsuccess = () => resolve(request.result ?? null)
    request.onerror = () => reject(request.error)
  })
}

const savePersistedFile = async (data: Uint8Array): Promise<void> => {
  if (!('indexedDB' in window)) return
  const db = await openDb()
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction('files', 'readwrite')
    const store = tx.objectStore('files')
    store.put(data, DB_IDB_KEY)
    tx.oncomplete = () => resolve()
    tx.onerror = () => reject(tx.error)
  })
}

const runDemo = async () => {
  if (logEl) logEl.textContent = ''
  log('Loading WASM...')

  const ray = await import('../../core.wasi-browser.js')
  const { Database, pathConfig, JsTraversalDirection, __fs } = ray

  const persisted = await loadPersistedFile()
  if (persisted) {
    log('Restoring persisted DB from IndexedDB...')
    const bytes = persisted instanceof Uint8Array ? persisted : new Uint8Array(persisted)
    __fs.writeFileSync(DB_PATH, bytes)
  }

  const db = Database.open(DB_PATH)
  db.begin()

  const alice = db.createNode('user:alice')
  const bob = db.createNode('user:bob')
  const carol = db.createNode('user:carol')

  const knows = db.getOrCreateEtype('knows')
  db.addEdge(alice, knows, bob)
  db.addEdge(bob, knows, carol)
  db.commit()

  const config = pathConfig(alice, carol)
  config.allowedEdgeTypes = [knows]
  const dijkstra = db.dijkstra(config)

  log(`Dijkstra found: ${dijkstra.found}`)
  log(`Path: ${dijkstra.path.join(' -> ')}`)

  const reachable = db.reachableNodes(alice, 2, knows)
  log(`Reachable within 2 hops: ${reachable.join(', ')}`)

  const singleHop = db.traverseSingle([alice], JsTraversalDirection.Out, knows)
  log(`Single-hop count: ${singleHop.length}`)

  db.close()

  const bytes = __fs.readFileSync(DB_PATH) as Uint8Array
  await savePersistedFile(bytes)
  log('Persisted DB to IndexedDB')
  log('Done.')
}

runButton?.addEventListener('click', () => {
  void runDemo().catch((err) => log(err?.stack ?? String(err)))
})
