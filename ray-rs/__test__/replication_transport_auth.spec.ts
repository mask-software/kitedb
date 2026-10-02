import test from 'ava'

import {
  authorizeReplicationAdminRequest,
  createForwardedTlsMtlsMatcher,
  createNodeTlsMtlsMatcher,
  createReplicationAdminAuthorizer,
  isForwardedTlsClientAuthorized,
  isReplicationAdminAuthorized,
  isNodeTlsClientAuthorized,
  type ReplicationAdminAuthRequest,
} from '../ts/replication_transport'

type RequestLike = ReplicationAdminAuthRequest & {
  tlsAuthorized?: boolean
}

function request(headers: Record<string, string | undefined> = {}): RequestLike {
  return { headers }
}

test('replication admin auth none mode always allows', (t) => {
  t.true(isReplicationAdminAuthorized(request(), { mode: 'none' }))
  t.notThrows(() => authorizeReplicationAdminRequest(request(), { mode: 'none' }))
})

test('replication admin auth token mode requires bearer token', (t) => {
  const cfg = { mode: 'token', token: 'abc123' } as const
  t.true(isReplicationAdminAuthorized(request({ authorization: 'Bearer abc123' }), cfg))
  t.false(isReplicationAdminAuthorized(request({ authorization: 'Bearer no' }), cfg))
  t.false(isReplicationAdminAuthorized(request({}), cfg))
})

test('replication admin auth mtls mode supports a trusted header + whole-value subject regex', (t) => {
  const cfg = {
    mode: 'mtls',
    mtlsHeader: 'x-client-cert',
    trustForwardedClientCert: true,
    mtlsSubjectRegex: /CN=replication-admin,O=RayDB/,
  } as const
  t.true(isReplicationAdminAuthorized(request({ 'x-client-cert': 'CN=replication-admin,O=RayDB' }), cfg))
  t.false(isReplicationAdminAuthorized(request({ 'x-client-cert': 'CN=viewer,O=RayDB' }), cfg))
  t.false(isReplicationAdminAuthorized(request({ 'x-client-cert': 'CN=replication-admin,O=RayDB,OU=x' }), cfg))
  // The header is never trusted implicitly.
  const { trustForwardedClientCert: _, ...untrusted } = cfg
  t.throws(
    () => isReplicationAdminAuthorized(request({ 'x-client-cert': 'CN=replication-admin,O=RayDB' }), untrusted),
    {
      message: /mTLS check/,
    },
  )
})

test('replication admin auth token_or_mtls accepts either', (t) => {
  const cfg = {
    mode: 'token_or_mtls',
    token: 'abc123',
    mtlsHeader: 'x-client-cert',
    trustForwardedClientCert: true,
    mtlsSubjectRegex: /CN=replication-admin,O=RayDB/,
  } as const
  t.true(isReplicationAdminAuthorized(request({ authorization: 'Bearer abc123' }), cfg))
  t.true(isReplicationAdminAuthorized(request({ 'x-client-cert': 'CN=replication-admin,O=RayDB' }), cfg))
  t.false(isReplicationAdminAuthorized(request({}), cfg))
})

test('replication admin auth token_and_mtls requires both', (t) => {
  const cfg = {
    mode: 'token_and_mtls',
    token: 'abc123',
    mtlsHeader: 'x-client-cert',
    trustForwardedClientCert: true,
    mtlsSubjectRegex: /CN=replication-admin,O=RayDB/,
  } as const
  t.false(isReplicationAdminAuthorized(request({ authorization: 'Bearer abc123' }), cfg))
  t.false(isReplicationAdminAuthorized(request({ 'x-client-cert': 'CN=replication-admin,O=RayDB' }), cfg))
  t.true(
    isReplicationAdminAuthorized(
      request({
        authorization: 'Bearer abc123',
        'x-client-cert': 'CN=replication-admin,O=RayDB',
      }),
      cfg,
    ),
  )
})

test('replication admin auth supports custom mtls matcher hook', (t) => {
  const cfg = {
    mode: 'mtls',
    mtlsMatcher: (req: RequestLike) => req.tlsAuthorized === true,
  }
  t.true(isReplicationAdminAuthorized({ headers: {}, tlsAuthorized: true }, cfg))
  t.false(isReplicationAdminAuthorized({ headers: {}, tlsAuthorized: false }, cfg))
})

test('node tls matcher detects authorized socket on common request shapes', (t) => {
  t.true(isNodeTlsClientAuthorized({ headers: {}, socket: { authorized: true } }))
  t.true(isNodeTlsClientAuthorized({ headers: {}, client: { authorized: true } }))
  t.true(isNodeTlsClientAuthorized({ headers: {}, raw: { socket: { authorized: true } } }))
  t.true(isNodeTlsClientAuthorized({ headers: {}, req: { socket: { authorized: true } } }))
  t.false(isNodeTlsClientAuthorized({ headers: {}, socket: { authorized: false } }))
  t.false(isNodeTlsClientAuthorized({ headers: {} }))
})

test('node tls matcher supports peer certificate requirement', (t) => {
  const withPeer = {
    headers: {},
    socket: {
      authorized: true,
      getPeerCertificate: () => ({ subject: { CN: 'replication-admin' } }),
    },
  }
  const withoutPeer = {
    headers: {},
    socket: {
      authorized: true,
      getPeerCertificate: () => ({}),
    },
  }
  t.true(isNodeTlsClientAuthorized(withPeer, { requirePeerCertificate: true }))
  t.false(isNodeTlsClientAuthorized(withoutPeer, { requirePeerCertificate: true }))
})

test('node tls matcher factory composes into auth config', (t) => {
  const requireAdmin = createReplicationAdminAuthorizer<RequestLike>({
    mode: 'mtls',
    mtlsMatcher: createNodeTlsMtlsMatcher({ requirePeerCertificate: true }),
  })
  t.notThrows(() =>
    requireAdmin({
      headers: {},
      socket: {
        authorized: true,
        getPeerCertificate: () => ({ subject: { CN: 'replication-admin' } }),
      },
    }),
  )
  const error = t.throws(() =>
    requireAdmin({
      headers: {},
      socket: {
        authorized: true,
        getPeerCertificate: () => ({}),
      },
    }),
  )
  t.truthy(error)
})

test('forwarded tls matcher validates proxy verify headers', (t) => {
  t.true(
    isForwardedTlsClientAuthorized({
      headers: { 'x-client-verify': 'SUCCESS' },
    }),
  )
  t.false(
    isForwardedTlsClientAuthorized({
      headers: { 'x-client-verify': 'FAILED' },
    }),
  )
  t.false(isForwardedTlsClientAuthorized({ headers: {} }))
})

test('forwarded tls matcher supports peer certificate and custom verify policy', (t) => {
  t.true(
    isForwardedTlsClientAuthorized(
      {
        headers: {
          'x-client-verify': 'SUCCESS',
          'x-forwarded-client-cert': 'CN=replication-admin,O=RayDB',
        },
      },
      { requirePeerCertificate: true },
    ),
  )
  t.false(
    isForwardedTlsClientAuthorized(
      {
        headers: { 'x-client-verify': 'SUCCESS' },
      },
      { requirePeerCertificate: true },
    ),
  )
  t.true(
    isForwardedTlsClientAuthorized(
      {
        headers: { 'x-forwarded-client-cert': 'CN=replication-admin,O=RayDB' },
      },
      { requireVerifyHeader: false, requirePeerCertificate: true },
    ),
  )
})

test('forwarded tls matcher factory composes into auth config', (t) => {
  const requireAdmin = createReplicationAdminAuthorizer({
    mode: 'mtls',
    mtlsMatcher: createForwardedTlsMtlsMatcher({ requirePeerCertificate: true }),
  })
  t.notThrows(() =>
    requireAdmin({
      headers: {
        'x-client-verify': 'SUCCESS',
        'x-forwarded-client-cert': 'CN=replication-admin,O=RayDB',
      },
    }),
  )
  const error = t.throws(() =>
    requireAdmin({
      headers: {
        'x-client-verify': 'FAILED',
        'x-forwarded-client-cert': 'CN=replication-admin,O=RayDB',
      },
    }),
  )
  t.truthy(error)
})

test('replication admin auth helper throws unauthorized and invalid config', (t) => {
  const requireAdmin = createReplicationAdminAuthorizer({
    mode: 'token',
    token: 'abc123',
  })
  const error = t.throws(() => requireAdmin(request({ authorization: 'Bearer wrong' })))
  t.truthy(error)
  t.true(String(error?.message).includes('not satisfied'))

  const invalid = t.throws(() =>
    createReplicationAdminAuthorizer({
      mode: 'token',
      token: '   ',
    }),
  )
  t.truthy(invalid)
  t.true(String(invalid?.message).includes('requires a non-empty token'))
})
