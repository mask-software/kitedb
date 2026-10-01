import { defineConfig } from "vitest/config";

// Standalone config so tests don't boot the app's Nitro / TanStack Start plugins.
export default defineConfig({
	test: {
		include: ["tests/**/*.test.ts"],
		environment: "node",
	},
});
