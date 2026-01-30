# RayDB Browser Demo

This demo runs the WASI build in the browser using Vite.

## Steps

1) Build the WASM bundle:

```bash
cd ray-rs
npm run build:wasm
```

2) Install demo deps:

```bash
cd ray-rs/examples/browser
npm install
```

3) Start the dev server:

```bash
npm run dev
```

Open the URL Vite prints (usually http://localhost:5173) and click "Run demo".

## Notes

- The demo uses the WASI memfs in the browser.
- After each run, it saves the `.raydb` file to OPFS when available, falling back to IndexedDB.
- Cross-origin isolation headers are enabled in `vite.config.ts` to allow SharedArrayBuffer.
