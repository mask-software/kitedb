import { render } from "@opentui/solid";
import { App } from "./app.tsx";

// The renderer is destroyed on q/Esc, Ctrl+C and exit signals; by the time onDestroy runs it has
// restored the terminal and disposed the app, so exiting here skips no cleanup.
await render(() => <App />, { onDestroy: () => process.exit(0) });
