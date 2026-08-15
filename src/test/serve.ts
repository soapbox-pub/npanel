/**
 * The slice of Bun's server API the tests use.
 *
 * Mirrors the declaration in `src/tls-check.ts`, and exists for the same
 * reason: `node:http` is banned (see AGENTS.md), and `@types/bun` would put
 * every Bun global in scope when `tsc` refusing them is what keeps the rest of
 * the source portable.
 */
declare const Bun: {
  serve(options: {
    port: number;
    hostname: string;
    fetch(request: Request): Response | Promise<Response>;
  }): {
    readonly port: number;
    stop(closeActiveConnections?: boolean): Promise<void>;
  };
};

export interface TestServer {
  /** Base URL with no trailing slash, so `${server.url}/${path}` reads plainly. */
  url: string;
  close(): Promise<void>;
}

/**
 * A fixture server on an ephemeral loopback port.
 *
 * Ephemeral because tests run concurrently and a fixed port would collide.
 * Handlers answer with a `Response`, which is what the code under test sees
 * anyway — npanel fetches blobs with `fetch`.
 */
export function serve(handler: (request: Request) => Response | Promise<Response>): TestServer {
  const server = Bun.serve({ port: 0, hostname: "127.0.0.1", fetch: handler });

  return {
    url: `http://127.0.0.1:${server.port}`,
    // Closing active connections too: some tests deliberately leave a request
    // unanswered, and a graceful stop would wait for it forever.
    close: () => server.stop(true),
  };
}
