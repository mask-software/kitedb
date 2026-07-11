import test from 'ava'

import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

import {
  Database,
  collectReplicationLogTransportJson,
  collectReplicationMetricsPrometheus,
  collectReplicationSnapshotTransportJson,
} from '../index'
import {
  createReplicationAdminAuthorizer,
  createReplicationTransportAdapter,
  type ReplicationAdminAuthRequest,
} from '../ts/replication_transport'

function makePaths() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'kitedb-repl-flow-'))
  return {
    primaryPath: path.join(dir, 'primary.kitedb'),
    primarySidecar: path.join(dir, 'primary.sidecar'),
    replicaPath: path.join(dir, 'replica.kitedb'),
    replicaSidecar: path.join(dir, 'replica.sidecar'),
  }
}

function drainReplica(replica: Database, maxFrames: number, maxLoops = 64): void {
  for (let i = 0; i < maxLoops; i += 1) {
    const applied = replica.replicaCatchUpOnce(maxFrames)
    if (applied === 0) return
  }
}

test('host-runtime replication transport/admin flow is consistent', (t) => {
  const paths = makePaths()
  const primary = Database.open(paths.primaryPath, {
    replicationRole: 'Primary',
    replicationSidecarPath: paths.primarySidecar,
    replicationSegmentMaxBytes: 1,
    replicationRetentionMinEntries: 1,
    autoCheckpoint: false,
  })
  // Second live primary on one path simulates multi-machine split-brain so
  // epoch fencing can be exercised; requires the test-only lock bypass.
  process.env.KITEDB_DANGER_ALLOW_MULTI_NODE_SIMULATION = '1'
  const stale = Database.open(paths.primaryPath, {
    replicationRole: 'Primary',
    replicationSidecarPath: paths.primarySidecar,
    replicationSegmentMaxBytes: 1,
    replicationRetentionMinEntries: 1,
    autoCheckpoint: false,
    dangerBypassFileLockForMultiNodeSimulation: true,
  })
  const replica = Database.open(paths.replicaPath, {
    replicationRole: 'Replica',
    replicationSidecarPath: paths.replicaSidecar,
    replicationSourceDbPath: paths.primaryPath,
    replicationSourceSidecarPath: paths.primarySidecar,
    autoCheckpoint: false,
  })

  t.teardown(() => {
    for (const db of [replica, stale, primary]) {
      try {
        db.close()
      } catch {}
    }
  })

  primary.begin()
  primary.createNode('n:base')
  const tokenBase = primary.commitWithToken()
  t.true(tokenBase.startsWith('1:'))

  replica.replicaBootstrapFromSnapshot()
  const replicaAfterBootstrap = replica.replicaReplicationStatus()
  t.false(replicaAfterBootstrap.needsReseed)
  t.is(replicaAfterBootstrap.appliedLogIndex, 1)

  const adapter = createReplicationTransportAdapter(primary)
  const snapshot = adapter.snapshot(false)
  const snapshotDirect = JSON.parse(collectReplicationSnapshotTransportJson(primary, false))
  t.is(snapshot.epoch, snapshotDirect.epoch)
  t.is(snapshot.head_log_index, snapshotDirect.head_log_index)
  t.truthy(snapshot.start_cursor)

  const logPage = adapter.log({
    cursor: null,
    maxFrames: 128,
    maxBytes: 1024 * 1024,
    includePayload: false,
  })
  const logPageDirect = JSON.parse(
    collectReplicationLogTransportJson(primary, null, 128, 1024 * 1024, false),
  )
  t.is(logPage.frame_count, logPageDirect.frame_count)
  t.true(logPage.frame_count >= 1)

  const metricsProm = adapter.metricsPrometheus()
  const metricsPromDirect = collectReplicationMetricsPrometheus(primary)
  t.true(metricsProm.includes('kitedb_replication_'))
  t.is(metricsProm, metricsPromDirect)

  const requireAdmin = createReplicationAdminAuthorizer<ReplicationAdminAuthRequest>({
    mode: 'token',
    token: 'secret-token',
  })
  t.notThrows(() =>
    requireAdmin({ headers: { authorization: 'Bearer secret-token' } }),
  )
  const authErr = t.throws(() =>
    requireAdmin({ headers: { authorization: 'Bearer wrong-token' } }),
  )
  t.truthy(authErr)

  for (let i = 0; i < 6; i += 1) {
    primary.begin()
    primary.createNode(`n:lag-${i}`)
    primary.commitWithToken()
  }

  const lagStatus = replica.replicaReplicationStatus()
  primary.primaryReportReplicaProgress(
    'replica-a',
    lagStatus.appliedEpoch,
    lagStatus.appliedLogIndex,
  )
  primary.primaryRunRetention()

  // Retention respects the reported replica cursor and lagging replicas
  // recover automatically (reseeding from snapshot internally when a gap
  // exists), so catch-up succeeds instead of throwing a reseed error.
  drainReplica(replica, 64)
  t.false(replica.replicaReplicationStatus().needsReseed)
  t.is(replica.countNodes(), primary.countNodes())

  // The explicit reseed admin API remains available and idempotent.
  primary.checkpoint()
  replica.replicaReseedFromSnapshot()
  t.false(replica.replicaReplicationStatus().needsReseed)
  t.is(replica.countNodes(), primary.countNodes())

  const beforePromote = primary.primaryReplicationStatus().epoch
  const promotedEpoch = primary.primaryPromoteToNextEpoch()
  t.true(promotedEpoch > beforePromote)

  stale.begin()
  stale.createNode('n:stale-write')
  const staleErr = t.throws(() => stale.commitWithToken())
  t.truthy(staleErr)
  t.regex(String(staleErr?.message), /stale primary/i)
  if (stale.hasTransaction()) {
    stale.rollback()
  }

  primary.begin()
  primary.createNode('n:post-promote')
  const promotedToken = primary.commitWithToken()
  t.true(promotedToken.startsWith(`${promotedEpoch}:`))

  t.false(replica.waitForToken(promotedToken, 5))
  drainReplica(replica, 128)
  t.true(replica.waitForToken(promotedToken, 2000))
  t.is(replica.countNodes(), primary.countNodes())
})
