/**
 * The slice of Bun's server API a WebSocket fixture needs, declared for the
 * same reason `src/test/serve.ts` declares its own.
 */
declare const Bun: {
  serve(options: {
    port: number;
    hostname: string;
    fetch(request: Request, server: { upgrade(request: Request): boolean }): Response | undefined;
    websocket: {
      message(socket: { send(data: string): void }, message: string | Uint8Array): void;
    };
  }): {
    readonly port: number;
    stop(closeActiveConnections?: boolean): Promise<void>;
  };
};

export interface EmptyRelay {
  /** A `ws://` URL the relays setting accepts. */
  url: string;
  close(): Promise<void>;
}

/**
 * A relay that has no events and says so at once.
 *
 * For tests that run the real relay pool, where a `fakeNostr` can't be passed
 * in — the deploy thread builds its own. Every `REQ` is answered with an
 * `EOSE`, so a query finishes immediately rather than waiting out its timeout,
 * and nothing is ever asked of a relay on the internet.
 */
export function startEmptyRelay(): EmptyRelay {
  const server = Bun.serve({
    port: 0,
    hostname: "127.0.0.1",
    fetch(request, bun) {
      return bun.upgrade(request) ? undefined : new Response("A relay speaks WebSocket.", { status: 426 });
    },
    websocket: {
      message(socket, message) {
        const frame: unknown = JSON.parse(typeof message === "string" ? message : new TextDecoder().decode(message));
        if (Array.isArray(frame) && frame[0] === "REQ") socket.send(JSON.stringify(["EOSE", frame[1]]));
      },
    },
  });

  return {
    url: `ws://127.0.0.1:${server.port}`,
    close: () => server.stop(true),
  };
}
