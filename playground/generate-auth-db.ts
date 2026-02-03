/**
 * Generate Auth Test Database (Single-File Format)
 *
 * Creates a simple auth system database for testing the playground's load functionality.
 * Run with: bun run playground/generate-auth-db.ts
 */

import { join } from "node:path";
import { Database, PropType } from "@kitedb/core";
import type { JsPropValue } from "@kitedb/core";

// Helper functions to create PropValue objects
function str(value: string): JsPropValue {
	return { propType: PropType.String, stringValue: value };
}

function int(value: bigint): JsPropValue {
	return { propType: PropType.Int, intValue: Number(value) };
}

function bool(value: boolean): JsPropValue {
	return { propType: PropType.Bool, boolValue: value };
}

// ============================================================================
// Generate the database
// ============================================================================

async function generateAuthDb() {
	const dbPath = join(import.meta.dirname, "auth.kitedb");

	console.log(`Creating auth database at: ${dbPath}`);

	const db = Database.open(dbPath);

	// Start transaction
	db.begin();

	// Define labels
	const userLabel = db.defineLabel("user");
	const roleLabel = db.defineLabel("role");
	const permLabel = db.defineLabel("permission");
	const sessionLabel = db.defineLabel("session");
	const auditLabel = db.defineLabel("audit");

	// Define edge types
	const hasRoleEtype = db.defineEtype("has_role");
	const grantsEtype = db.defineEtype("grants");
	const hasSessionEtype = db.defineEtype("has_session");
	const performedEtype = db.defineEtype("performed");
	const inheritsEtype = db.defineEtype("inherits");

	// Define property keys
	const nameProp = db.definePropkey("name");
	const emailProp = db.definePropkey("email");
	const usernameProp = db.definePropkey("username");
	const createdAtProp = db.definePropkey("createdAt");
	const isActiveProp = db.definePropkey("isActive");
	const descriptionProp = db.definePropkey("description");
	const priorityProp = db.definePropkey("priority");
	const resourceProp = db.definePropkey("resource");
	const actionProp = db.definePropkey("action");
	const tokenProp = db.definePropkey("token");
	const expiresAtProp = db.definePropkey("expiresAt");
	const ipAddressProp = db.definePropkey("ipAddress");
	const timestampProp = db.definePropkey("timestamp");
	const detailsProp = db.definePropkey("details");

	// Create permissions
	const permissions = [
		{
			key: "perm:read_users",
			name: "read_users",
			resource: "users",
			action: "read",
		},
		{
			key: "perm:write_users",
			name: "write_users",
			resource: "users",
			action: "write",
		},
		{
			key: "perm:delete_users",
			name: "delete_users",
			resource: "users",
			action: "delete",
		},
		{
			key: "perm:read_roles",
			name: "read_roles",
			resource: "roles",
			action: "read",
		},
		{
			key: "perm:write_roles",
			name: "write_roles",
			resource: "roles",
			action: "write",
		},
		{
			key: "perm:read_audit",
			name: "read_audit",
			resource: "audit",
			action: "read",
		},
		{ key: "perm:admin_all", name: "admin_all", resource: "*", action: "*" },
	];

	const permIds = new Map<string, number>();
	for (const perm of permissions) {
		const nodeId = db.createNode(perm.key);
		db.addNodeLabel(nodeId, permLabel);
		db.setNodeProp(nodeId, nameProp, str(perm.name));
		db.setNodeProp(nodeId, resourceProp, str(perm.resource));
		db.setNodeProp(nodeId, actionProp, str(perm.action));
		permIds.set(perm.name, nodeId);
		console.log(`  Created permission: ${perm.name}`);
	}

	// Create roles
	const roles = [
		{
			key: "role:admin",
			name: "admin",
			description: "Full system access",
			priority: 100n,
		},
		{
			key: "role:moderator",
			name: "moderator",
			description: "User management",
			priority: 50n,
		},
		{
			key: "role:user",
			name: "user",
			description: "Basic user access",
			priority: 10n,
		},
		{
			key: "role:guest",
			name: "guest",
			description: "Read-only access",
			priority: 1n,
		},
	];

	const roleIds = new Map<string, number>();
	for (const role of roles) {
		const nodeId = db.createNode(role.key);
		db.addNodeLabel(nodeId, roleLabel);
		db.setNodeProp(nodeId, nameProp, str(role.name));
		db.setNodeProp(nodeId, descriptionProp, str(role.description));
		db.setNodeProp(nodeId, priorityProp, int(role.priority));
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
			db.addEdge(roleId, grantsEtype, permId);
		}
	}

	// Role inheritance (moderator inherits from user, user inherits from guest)
	const modId = roleIds.get("moderator")!;
	const userId = roleIds.get("user")!;
	const guestId = roleIds.get("guest")!;
	db.addEdge(modId, inheritsEtype, userId);
	db.addEdge(userId, inheritsEtype, guestId);

	// Create users
	const now = Date.now();
	const users = [
		{
			key: "user:alice",
			username: "alice",
			email: "alice@example.com",
			createdAt: BigInt(now - 86400000 * 30),
			isActive: true,
		},
		{
			key: "user:bob",
			username: "bob",
			email: "bob@example.com",
			createdAt: BigInt(now - 86400000 * 20),
			isActive: true,
		},
		{
			key: "user:charlie",
			username: "charlie",
			email: "charlie@example.com",
			createdAt: BigInt(now - 86400000 * 10),
			isActive: true,
		},
		{
			key: "user:diana",
			username: "diana",
			email: "diana@example.com",
			createdAt: BigInt(now - 86400000 * 5),
			isActive: false,
		},
		{
			key: "user:eve",
			username: "eve",
			email: "eve@example.com",
			createdAt: BigInt(now - 86400000 * 2),
			isActive: true,
		},
	];

	const userIds = new Map<string, number>();
	for (const user of users) {
		const nodeId = db.createNode(user.key);
		db.addNodeLabel(nodeId, userLabel);
		db.setNodeProp(nodeId, usernameProp, str(user.username));
		db.setNodeProp(nodeId, emailProp, str(user.email));
		db.setNodeProp(nodeId, createdAtProp, int(user.createdAt));
		db.setNodeProp(nodeId, isActiveProp, bool(user.isActive));
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
		db.addEdge(uId, hasRoleEtype, rId);
	}

	// Create some sessions for active users
	const sessions = [
		{
			key: "session:sess_alice_1",
			token: "tok_abc123",
			expiresAt: BigInt(now + 3600000),
			ipAddress: "192.168.1.100",
		},
		{
			key: "session:sess_bob_1",
			token: "tok_def456",
			expiresAt: BigInt(now + 3600000),
			ipAddress: "192.168.1.101",
		},
		{
			key: "session:sess_charlie_1",
			token: "tok_ghi789",
			expiresAt: BigInt(now + 1800000),
			ipAddress: "10.0.0.50",
		},
	];

	const sessionIds = new Map<string, number>();
	for (const session of sessions) {
		const nodeId = db.createNode(session.key);
		db.addNodeLabel(nodeId, sessionLabel);
		db.setNodeProp(nodeId, tokenProp, str(session.token));
		db.setNodeProp(nodeId, expiresAtProp, int(session.expiresAt));
		db.setNodeProp(nodeId, ipAddressProp, str(session.ipAddress));
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
		db.addEdge(uId, hasSessionEtype, sId);
	}

	// Create audit logs
	const auditLogs = [
		{
			key: "audit:1",
			action: "user_login",
			timestamp: BigInt(now - 3600000),
			details: "alice logged in from 192.168.1.100",
		},
		{
			key: "audit:2",
			action: "user_created",
			timestamp: BigInt(now - 86400000 * 2),
			details: "eve was created by alice",
		},
		{
			key: "audit:3",
			action: "role_assigned",
			timestamp: BigInt(now - 86400000 * 2),
			details: "guest role assigned to eve",
		},
		{
			key: "audit:4",
			action: "user_login",
			timestamp: BigInt(now - 7200000),
			details: "bob logged in from 192.168.1.101",
		},
		{
			key: "audit:5",
			action: "user_deactivated",
			timestamp: BigInt(now - 86400000),
			details: "diana was deactivated by bob",
		},
	];

	const auditIds = new Map<string, number>();
	for (const audit of auditLogs) {
		const nodeId = db.createNode(audit.key);
		db.addNodeLabel(nodeId, auditLabel);
		db.setNodeProp(nodeId, actionProp, str(audit.action));
		db.setNodeProp(nodeId, timestampProp, int(audit.timestamp));
		db.setNodeProp(nodeId, detailsProp, str(audit.details));
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
		db.addEdge(uId, performedEtype, aId);
	}

	// Commit transaction
	db.commit();

	// Get stats before optimization
	const dbStats = db.stats();
	console.log("\nDatabase stats:");
	console.log(
		`  Nodes: ${Number(dbStats.snapshotNodes) + dbStats.deltaNodesCreated}`,
	);
	console.log(
		`  Edges: ${Number(dbStats.snapshotEdges) + dbStats.deltaEdgesAdded}`,
	);

	// Optimize (compact delta into snapshot) and vacuum (shrink file)
	console.log("\nOptimizing and vacuuming database...");
	db.optimizeSingleFile();
	db.vacuumSingleFile();

	db.close();

	console.log(`\nDatabase saved to: ${dbPath}`);
	console.log("You can now upload this file to the playground!");
}

generateAuthDb().catch(console.error);
