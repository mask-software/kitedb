/**
 * Docker lifecycle management for Memgraph benchmark
 */

import { spawn } from "node:child_process";
import { dirname } from "node:path";
import neo4j from "neo4j-driver";

const COMPOSE_FILE = new URL("./docker-compose.yml", import.meta.url).pathname;
const COMPOSE_DIR = dirname(COMPOSE_FILE);
const MEMGRAPH_URI = "bolt://localhost:7687";
const STARTUP_TIMEOUT_MS = 60_000;
const POLL_INTERVAL_MS = 1_000;

/**
 * Check if Docker is available on the system
 */
export async function isDockerAvailable(): Promise<boolean> {
  return new Promise((resolve) => {
    const proc = spawn("docker", ["info"], {
      stdio: "ignore",
    });
    proc.on("close", (code) => resolve(code === 0));
    proc.on("error", () => resolve(false));
  });
}

/**
 * Check if port 7687 is already in use
 */
export async function isPortInUse(port: number = 7687): Promise<boolean> {
  return new Promise((resolve) => {
    const proc = spawn("lsof", ["-i", `:${port}`], {
      stdio: "ignore",
    });
    proc.on("close", (code) => resolve(code === 0));
    proc.on("error", () => resolve(false));
  });
}

/**
 * Run a docker compose command
 */
function runDockerCompose(
  args: string[],
): Promise<{ code: number; stdout: string; stderr: string }> {
  return new Promise((resolve, reject) => {
    const proc = spawn("docker", ["compose", "-f", COMPOSE_FILE, ...args], {
      cwd: COMPOSE_DIR,
      stdio: ["ignore", "pipe", "pipe"],
    });

    let stdout = "";
    let stderr = "";

    proc.stdout.on("data", (data) => {
      stdout += data.toString();
    });
    proc.stderr.on("data", (data) => {
      stderr += data.toString();
    });

    proc.on("close", (code) => {
      resolve({ code: code ?? 1, stdout, stderr });
    });
    proc.on("error", reject);
  });
}

/**
 * Check if Memgraph is ready to accept connections
 */
export async function isMemgraphReady(): Promise<boolean> {
  const driver = neo4j.driver(MEMGRAPH_URI);
  try {
    const session = driver.session();
    await session.run("RETURN 1");
    await session.close();
    await driver.close();
    return true;
  } catch {
    await driver.close();
    return false;
  }
}

/**
 * Wait for Memgraph to be ready with timeout
 */
async function waitForMemgraph(timeoutMs: number = STARTUP_TIMEOUT_MS): Promise<void> {
  const startTime = Date.now();

  while (Date.now() - startTime < timeoutMs) {
    if (await isMemgraphReady()) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));
  }

  throw new Error(`Memgraph failed to start within ${timeoutMs / 1000}s`);
}

/**
 * Start Memgraph container and wait for it to be ready
 */
export async function startMemgraph(): Promise<void> {
  // Check Docker is available
  if (!(await isDockerAvailable())) {
    throw new Error(
      "Docker is required but not available.\n" +
        "Install Docker from https://docker.com",
    );
  }

  // Check if port is already in use
  if (await isPortInUse(7687)) {
    // Check if it's our container or something else
    if (await isMemgraphReady()) {
      console.log("  Memgraph already running, reusing existing instance");
      return;
    }
    throw new Error(
      "Port 7687 is in use by another process.\n" +
        "Stop the existing process or use --skip-docker if Memgraph is already running.",
    );
  }

  console.log("  Starting Memgraph container...");

  // Pull image first (in case it's not cached)
  const pullResult = await runDockerCompose(["pull", "--quiet"]);
  if (pullResult.code !== 0) {
    console.log("  Warning: Could not pull latest image, using cached version");
  }

  // Start the container (don't use --wait, we'll poll ourselves)
  const upResult = await runDockerCompose(["up", "-d"]);
  if (upResult.code !== 0) {
    throw new Error(`Failed to start Memgraph:\n${upResult.stderr}`);
  }

  // Wait for Memgraph to be ready
  console.log("  Waiting for Memgraph to be ready...");
  await waitForMemgraph();
  console.log("  Memgraph is ready");
}

/**
 * Stop and remove Memgraph container
 */
export async function stopMemgraph(): Promise<void> {
  console.log("  Stopping Memgraph container...");
  const result = await runDockerCompose(["down", "-v"]);
  if (result.code !== 0) {
    console.error(`Warning: Failed to stop Memgraph cleanly: ${result.stderr}`);
  } else {
    console.log("  Memgraph stopped");
  }
}

/**
 * Get Memgraph connection URI
 */
export function getMemgraphUri(): string {
  return MEMGRAPH_URI;
}
