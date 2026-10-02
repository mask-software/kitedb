// raydb-b4 replication-core lane: binary replication transports (X5), the
// sidecar generation and no db_path in the transports (P2, P9), removing a
// replica's progress (P2), and the replication export methods on Kite (Kite
// had none, so the playground's transport endpoints always failed). Each test
// fails against the unfixed bindings.

import test from 'ava'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import {
  Database,
  collectReplicationLogTransport,
  collectReplicationLogTransportJson,
  collectReplicationSnapshotTransport,
  collectReplicationSnapshotTransportJson,
} from '../index'
import { kiteSync, node, string } from '../ts/index'
import { createReplicationTransportAdapter } from '../ts/replication_transport'

const File = node('file', {
  key: (id: string) => `file:${id}`,
  props: { path: string('path') },
})

function makeDir(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-b4-repl-core-'))
}

function openPrimary(dir: string, extra: Record<string, unknown> = {}): Database {
  return Database.open(path.join(dir, 'primary.kitedb'), {
    replicationRole: 'Primary',
    autoCheckpoint: false,
    ...extra,
  })
}

function commitNodes(db: Database, count: number, prefix = 'n'): void {
  for (let i = 0; i < count; i += 1) {
    db.begin()
    db.createNode(`${prefix}:${i}`)
    db.commitWithToken()
  }
}

test('binary snapshot transport matches the JSON export and has no db_path', (t) => {
  const dir = makeDir()
  const primary = openPrimary(dir)
  t.teardown(() => primary.close())
  commitNodes(primary, 3)

  const json = JSON.parse(primary.exportReplicationSnapshotTransportJson(true))
  t.false('db_path' in json, 'the JSON must not expose the primary path')
  t.false(JSON.stringify(json).includes(dir), 'no path in the JSON')
  t.regex(json.generation, /^[0-9a-f]{16}$/)

  const binary = primary.exportReplicationSnapshotTransport(true)
  t.true(Buffer.isBuffer(binary.data))
  t.deepEqual(binary.data, Buffer.from(json.data_base64, 'base64'))
  t.is(binary.byteLength, json.byte_length)
  t.is(binary.checksumCrc32.toString(16).padStart(8, '0'), json.checksum_crc32c)
  t.is(binary.headLogIndex, 3)
  t.is(binary.startCursor, json.start_cursor)
  t.is(binary.generation, json.generation)
  t.is(binary.format, json.format)

  const direct = collectReplicationSnapshotTransport(primary, false)
  t.is(direct.data, undefined)
  t.is(direct.checksumCrc32, binary.checksumCrc32)
  t.is(JSON.parse(collectReplicationSnapshotTransportJson(primary, false)).generation, binary.generation)
})

test('binary log transport pages like the JSON export, with raw payloads', (t) => {
  const dir = makeDir()
  const primary = openPrimary(dir)
  t.teardown(() => primary.close())
  commitNodes(primary, 5)

  const page = primary.exportReplicationLogTransport(null, 3, 1 << 20, true)
  const json = JSON.parse(primary.exportReplicationLogTransportJson(null, 3, 1 << 20, true))
  t.is(page.frames.length, 3)
  t.is(page.frameCount, 3)
  t.false(page.eof)
  t.is(page.nextCursor, json.next_cursor)
  t.is(page.generation, json.generation)
  t.regex(page.generation, /^[0-9a-f]{16}$/)
  page.frames.forEach((frame, index) => {
    t.is(frame.logIndex, json.frames[index].log_index)
    t.deepEqual(frame.payload, Buffer.from(json.frames[index].payload_base64, 'base64'))
  })

  const rest = collectReplicationLogTransport(primary, page.nextCursor, 64, 1 << 20, false)
  t.true(rest.eof)
  t.deepEqual(
    rest.frames.map((frame) => frame.logIndex),
    [4, 5],
  )
  t.true(rest.frames.every((frame) => frame.payload === undefined))
  const restJson = JSON.parse(collectReplicationLogTransportJson(primary, page.nextCursor, 64, 1 << 20, false))
  t.is(restJson.frame_count, 2)
})

test('primaryRemoveReplicaProgress forgets a decommissioned replica', (t) => {
  const dir = makeDir()
  const primary = openPrimary(dir, {
    replicationSegmentMaxBytes: 1,
    replicationRetentionMinEntries: 2,
  })
  t.teardown(() => primary.close())
  commitNodes(primary, 1)
  primary.primaryReportReplicaProgress('gone', 1, 1)
  commitNodes(primary, 9, 'more')
  t.is(primary.primaryRunRetention().retainedFloor, 2)

  t.true(primary.primaryRemoveReplicaProgress('gone'))
  t.false(primary.primaryRemoveReplicaProgress('gone'))
  t.is(primary.primaryRunRetention().retainedFloor, 8)
  t.false(primary.primaryReplicationStatus()!.replicaLags.some((lag) => lag.replicaId === 'gone'))
})

test('Kite exports the replication transports and works with the TS adapter', (t) => {
  const dir = makeDir()
  const db = kiteSync(path.join(dir, 'kite-primary.kitedb'), {
    nodes: [File],
    edges: [],
    replicationRole: 'primary',
  })
  t.teardown(() => db.close())
  for (let i = 0; i < 3; i += 1) {
    db.insert(File).values({ key: `f${i}`, path: `src/f${i}.ts` }).returning()
  }

  const json = JSON.parse(db.exportReplicationSnapshotTransportJson(true))
  t.is(json.head_log_index, db.primaryReplicationStatus()!.headLogIndex)
  const binary = db.exportReplicationSnapshotTransport(true)
  t.deepEqual(binary.data, Buffer.from(json.data_base64, 'base64'))

  const log = db.exportReplicationLogTransport(null, 64, 1 << 20, true)
  const logJson = JSON.parse(db.exportReplicationLogTransportJson(null, 64, 1 << 20, true))
  t.is(log.frames.length, logJson.frame_count)
  t.true(log.frames.length >= 3)

  const adapter = createReplicationTransportAdapter(db)
  const snapshot = adapter.snapshot(true)
  t.false('db_path' in snapshot)
  t.is(snapshot.generation, binary.generation)
  t.is(snapshot.start_cursor, binary.startCursor)
  t.deepEqual(Buffer.from(snapshot.data_base64!, 'base64'), binary.data)
  const adapterLog = adapter.log({ includePayload: true })
  t.is(adapterLog.generation, log.generation)
  t.is(adapterLog.frame_count, log.frames.length)
  t.deepEqual(Buffer.from(adapterLog.frames[0].payload_base64!, 'base64'), log.frames[0].payload)
})
