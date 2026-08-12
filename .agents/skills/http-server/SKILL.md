---
name: http-server
description: Add an HTTP server to the project using Hono with CORS, config integration, and the Bun default export pattern
---

## Overview

This skill adds an HTTP server to the project using [Hono](https://hono.dev),
a fast and lightweight web framework that works across runtimes.

## Steps

### 1. Install Hono

```bash
bun add hono
```

### 2. Create `src/server.ts`

Create the server entry point with CORS enabled and the project's `Config`
class wired in as middleware:

```ts
import process from "node:process";
import { Hono } from "hono";
import { cors } from "hono/cors";

import { Config } from "./config.ts";

const config = new Config({
  get(key) {
    return process.env[key];
  },
});

const app = new Hono<{ Variables: { config: Config } }>()
  .use(cors())
  .use(async (c, next) => {
    c.set("config", config);
    await next();
  });

export default app;
```

Key details:

- **CORS** is enabled by default via `hono/cors`.
- **Config** is set on every request via middleware, accessible with
  `c.get("config")` in any route handler.
- **`export default app`** makes this work with `bun run` out of the box.
  Bun detects the default export's `fetch` method and serves it automatically.
- Uses `node:process` for environment access (not `Bun.env`) per project
  conventions.

### 3. Add scripts to `package.json`

Add `dev` and `start` scripts:

```json
{
  "scripts": {
    "dev": "bun run --watch src/server.ts",
    "start": "bun run src/server.ts"
  }
}
```

- `dev` watches for file changes and auto-restarts.
- Bun auto-loads `.env` files, so no extra flags are needed.

### 4. Add server config properties

Add `port` and `publicUrl` getters to the `Config` class in `src/config.ts`:

```ts
get port(): number {
  const value = this.env.get("PORT");
  if (!value) {
    return 3000;
  }
  const port = Number.parseInt(value, 10);
  if (Number.isNaN(port) || port < 1 || port > 65535) {
    throw new Error("PORT must be a valid port number (1-65535).");
  }
  return port;
}

get publicUrl(): string | undefined {
  return this.env.get("PUBLIC_URL");
}
```

Add the corresponding env vars to `.env.example`:

```
PORT="3000"
PUBLIC_URL="https://example.com"
```

### 5. Add routes

Add routes to `src/server.ts` above the default export:

```ts
app.get("/", (c) => {
  return c.json({ message: "Hello from Gleestack!" });
});
```

Access config in any route handler via `c.get("config")`.

### 6. Verify

```bash
bun run dev
curl http://localhost:3000/
```

## Notes

- Hono supports all the runtimes this project targets (Node.js, Bun, Deno).
- For additional Hono middleware, see https://hono.dev/docs/middleware/builtin.
