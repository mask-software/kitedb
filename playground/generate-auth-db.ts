/**
 * Generate Auth Test Database (Single-File Format)
 *
 * Creates a simple auth system database for testing the playground's load functionality.
 * Run with: bun run playground/generate-auth-db.ts
 */

import {
  openGraphDB,
  closeGraphDB,
  defineLabel,
  definePropkey,
  defineEtype,
  createNode,
  addEdge,
  setNodeProp,
  beginTx,
  commit,
  stats,
  PropValueTag,
  optimizeSingleFile,
  vacuumSingleFile,
} from "../src/index.ts";
import type { PropValue } from "../src/types.ts";
import { join } from "node:path";

// Helper functions to create PropValue objects
function str(value: string): PropValue {
  return { tag: PropValueTag.STRING, value };
}

function int(value: bigint): PropValue {
  return { tag: PropValueTag.I64, value };
}

function bool(value: boolean): PropValue {
  return { tag: PropValueTag.BOOL, value };
}

// ============================================================================
// Generate the database
// ============================================================================

async function generateAuthDb() {
  const dbPath = join(import.meta.dirname, "auth.raydb");
  
  console.log(`Creating auth database at: ${dbPath}`);
  
  const db = await openGraphDB(dbPath);

  // Start transaction
  const tx = beginTx(db);

  // Define labels
  const userLabel = defineLabel(tx, "user");
  const roleLabel = defineLabel(tx, "role");
  const permLabel = defineLabel(tx, "permission");
  const sessionLabel = defineLabel(tx, "session");
  const auditLabel = defineLabel(tx, "audit");

  // Define edge types
  const hasRoleEtype = defineEtype(tx, "has_role");
  const grantsEtype = defineEtype(tx, "grants");
  const hasSessionEtype = defineEtype(tx, "has_session");
  const performedEtype = defineEtype(tx, "performed");
  const inheritsEtype = defineEtype(tx, "inherits");

  // Define property keys
  const nameProp = definePropkey(tx, "name");
  const emailProp = definePropkey(tx, "email");
  const usernameProp = definePropkey(tx, "username");
  const createdAtProp = definePropkey(tx, "createdAt");
  const isActiveProp = definePropkey(tx, "isActive");
  const descriptionProp = definePropkey(tx, "description");
  const priorityProp = definePropkey(tx, "priority");
  const resourceProp = definePropkey(tx, "resource");
  const actionProp = definePropkey(tx, "action");
  const tokenProp = definePropkey(tx, "token");
  const expiresAtProp = definePropkey(tx, "expiresAt");
  const ipAddressProp = definePropkey(tx, "ipAddress");
  const timestampProp = definePropkey(tx, "timestamp");
  const detailsProp = definePropkey(tx, "details");

  // Create permissions
  const permissions = [
    { key: "perm:read_users", name: "read_users", resource: "users", action: "read" },
    { key: "perm:write_users", name: "write_users", resource: "users", action: "write" },
    { key: "perm:delete_users", name: "delete_users", resource: "users", action: "delete" },
    { key: "perm:read_roles", name: "read_roles", resource: "roles", action: "read" },
    { key: "perm:write_roles", name: "write_roles", resource: "roles", action: "write" },
    { key: "perm:read_audit", name: "read_audit", resource: "audit", action: "read" },
    { key: "perm:admin_all", name: "admin_all", resource: "*", action: "*" },
  ];

  const permIds = new Map<string, number>();
  for (const perm of permissions) {
    const nodeId = createNode(tx, { labels: [permLabel], key: perm.key });
    setNodeProp(tx, nodeId, nameProp, str(perm.name));
    setNodeProp(tx, nodeId, resourceProp, str(perm.resource));
    setNodeProp(tx, nodeId, actionProp, str(perm.action));
    permIds.set(perm.name, nodeId);
    console.log(`  Created permission: ${perm.name}`);
  }

  // Create roles
  const roles = [
    { key: "role:admin", name: "admin", description: "Full system access", priority: 100n },
    { key: "role:moderator", name: "moderator", description: "User management", priority: 50n },
    { key: "role:user", name: "user", description: "Basic user access", priority: 10n },
    { key: "role:guest", name: "guest", description: "Read-only access", priority: 1n },
  ];

  const roleIds = new Map<string, number>();
  for (const role of roles) {
    const nodeId = createNode(tx, { labels: [roleLabel], key: role.key });
    setNodeProp(tx, nodeId, nameProp, str(role.name));
    setNodeProp(tx, nodeId, descriptionProp, str(role.description));
    setNodeProp(tx, nodeId, priorityProp, int(role.priority));
    roleIds.set(role.name, nodeId);
    console.log(`  Created role: ${role.name}`);
  }

  // Role -> Permission relationships
  const rolePerms: [string, string[]][] = [
    ["admin", ["admin_all"]],
    ["moderator", ["read_users", "write_users", "read_roles", "read_audit"]],
    ["user", ["read_users"]],
  ];

  for (const [roleName, perms] of rolePerms) {
    const roleId = roleIds.get(roleName)!;
    for (const permName of perms) {
      const permId = permIds.get(permName)!;
      addEdge(tx, roleId, grantsEtype, permId);
    }
  }

  // Role inheritance (moderator inherits from user, user inherits from guest)
  const modId = roleIds.get("moderator")!;
  const userId = roleIds.get("user")!;
  const guestId = roleIds.get("guest")!;
  addEdge(tx, modId, inheritsEtype, userId);
  addEdge(tx, userId, inheritsEtype, guestId);

  // Create users
  const now = Date.now();
  const users = [
    { key: "user:alice", username: "alice", email: "alice@example.com", createdAt: BigInt(now - 86400000 * 30), isActive: true },
    { key: "user:bob", username: "bob", email: "bob@example.com", createdAt: BigInt(now - 86400000 * 20), isActive: true },
    { key: "user:charlie", username: "charlie", email: "charlie@example.com", createdAt: BigInt(now - 86400000 * 10), isActive: true },
    { key: "user:diana", username: "diana", email: "diana@example.com", createdAt: BigInt(now - 86400000 * 5), isActive: false },
    { key: "user:eve", username: "eve", email: "eve@example.com", createdAt: BigInt(now - 86400000 * 2), isActive: true },
  ];

  const userIds = new Map<string, number>();
  for (const user of users) {
    const nodeId = createNode(tx, { labels: [userLabel], key: user.key });
    setNodeProp(tx, nodeId, usernameProp, str(user.username));
    setNodeProp(tx, nodeId, emailProp, str(user.email));
    setNodeProp(tx, nodeId, createdAtProp, int(user.createdAt));
    setNodeProp(tx, nodeId, isActiveProp, bool(user.isActive));
    userIds.set(user.username, nodeId);
    console.log(`  Created user: ${user.username}`);
  }

  // Assign roles to users
  const userRoles: [string, string][] = [
    ["alice", "admin"],
    ["bob", "moderator"],
    ["charlie", "user"],
    ["diana", "user"],
    ["eve", "guest"],
  ];

  for (const [userName, roleName] of userRoles) {
    const uId = userIds.get(userName)!;
    const rId = roleIds.get(roleName)!;
    addEdge(tx, uId, hasRoleEtype, rId);
  }

  // Create some sessions for active users
  const sessions = [
    { key: "session:sess_alice_1", token: "tok_abc123", expiresAt: BigInt(now + 3600000), ipAddress: "192.168.1.100" },
    { key: "session:sess_bob_1", token: "tok_def456", expiresAt: BigInt(now + 3600000), ipAddress: "192.168.1.101" },
    { key: "session:sess_charlie_1", token: "tok_ghi789", expiresAt: BigInt(now + 1800000), ipAddress: "10.0.0.50" },
  ];

  const sessionIds = new Map<string, number>();
  for (const session of sessions) {
    const nodeId = createNode(tx, { labels: [sessionLabel], key: session.key });
    setNodeProp(tx, nodeId, tokenProp, str(session.token));
    setNodeProp(tx, nodeId, expiresAtProp, int(session.expiresAt));
    setNodeProp(tx, nodeId, ipAddressProp, str(session.ipAddress));
    sessionIds.set(session.key, nodeId);
    console.log(`  Created session: ${session.key}`);
  }

  // Link sessions to users
  const userSessions: [string, string][] = [
    ["alice", "session:sess_alice_1"],
    ["bob", "session:sess_bob_1"],
    ["charlie", "session:sess_charlie_1"],
  ];

  for (const [userName, sessionKey] of userSessions) {
    const uId = userIds.get(userName)!;
    const sId = sessionIds.get(sessionKey)!;
    addEdge(tx, uId, hasSessionEtype, sId);
  }

  // Create audit logs
  const auditLogs = [
    { key: "audit:1", action: "user_login", timestamp: BigInt(now - 3600000), details: "alice logged in from 192.168.1.100" },
    { key: "audit:2", action: "user_created", timestamp: BigInt(now - 86400000 * 2), details: "eve was created by alice" },
    { key: "audit:3", action: "role_assigned", timestamp: BigInt(now - 86400000 * 2), details: "guest role assigned to eve" },
    { key: "audit:4", action: "user_login", timestamp: BigInt(now - 7200000), details: "bob logged in from 192.168.1.101" },
    { key: "audit:5", action: "user_deactivated", timestamp: BigInt(now - 86400000), details: "diana was deactivated by bob" },
  ];

  const auditIds = new Map<string, number>();
  for (const audit of auditLogs) {
    const nodeId = createNode(tx, { labels: [auditLabel], key: audit.key });
    setNodeProp(tx, nodeId, actionProp, str(audit.action));
    setNodeProp(tx, nodeId, timestampProp, int(audit.timestamp));
    setNodeProp(tx, nodeId, detailsProp, str(audit.details));
    auditIds.set(audit.key, nodeId);
    console.log(`  Created audit log: ${audit.action}`);
  }

  // Link audit logs to users who performed the actions
  const performedBy: [string, string][] = [
    ["alice", "audit:1"],
    ["alice", "audit:2"],
    ["alice", "audit:3"],
    ["bob", "audit:4"],
    ["bob", "audit:5"],
  ];

  for (const [userName, auditKey] of performedBy) {
    const uId = userIds.get(userName)!;
    const aId = auditIds.get(auditKey)!;
    addEdge(tx, uId, performedEtype, aId);
  }

  // Commit transaction
  await commit(tx);

  // Get stats before optimization
  const dbStats = stats(db);
  console.log("\nDatabase stats:");
  console.log(`  Nodes: ${Number(dbStats.snapshotNodes) + dbStats.deltaNodesCreated}`);
  console.log(`  Edges: ${Number(dbStats.snapshotEdges) + dbStats.deltaEdgesAdded}`);

  // Optimize (compact delta into snapshot) and vacuum (shrink file)
  console.log("\nOptimizing and vacuuming database...");
  await optimizeSingleFile(db);
  await vacuumSingleFile(db);

  await closeGraphDB(db);
  
  console.log(`\nDatabase saved to: ${dbPath}`);
  console.log("You can now upload this file to the playground!");
}

generateAuthDb().catch(console.error);
