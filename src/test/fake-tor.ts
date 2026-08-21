import { type AddressInfo, type Socket, createServer } from "node:net";

/** A control port that answers from a script, so tests need no Tor. */
export interface FakeTor {
  port: number;
  /** Every command line the client sent, in order. */
  commands: string[];
  /** Drop the connection, as a Tor being restarted would. */
  drop(): void;
  close(): Promise<void>;
}

/** A control port answering one line at a time from `respond`. */
export async function startFakeTor(respond: (command: string) => string): Promise<FakeTor> {
  const commands: string[] = [];
  let client: Socket | undefined;

  const server = createServer((socket) => {
    client = socket;
    socket.setEncoding("utf8");
    socket.on("error", () => {});
    socket.on("data", (chunk: string) => {
      for (const command of chunk.split("\r\n").filter(Boolean)) {
        commands.push(command);
        const reply = respond(command);
        if (reply) socket.write(reply);
      }
    });
  });

  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));

  return {
    port: (server.address() as AddressInfo).port,
    commands,
    drop: () => client?.destroy(),
    close: () =>
      new Promise<void>((resolve) => {
        client?.destroy();
        server.close(() => resolve());
      }),
  };
}

/** The `PROTOCOLINFO` reply, offering `auth` as its authentication line. */
export function protocolInfo(auth: string): string {
  return `250-PROTOCOLINFO 1\r\n250-AUTH ${auth}\r\n250-VERSION Tor="0.4.8.12"\r\n250 OK\r\n`;
}

export interface FakeTorDaemon extends FakeTor {
  /** Service ids currently published, without the `.onion` suffix. */
  onions: Set<string>;
}

/**
 * A control port that keeps books, registering and unregistering services so a
 * test can ask what Tor would be publishing.
 *
 * Addresses are one letter repeated to the 56 characters a real v3 address has,
 * which is enough to satisfy every caller and far easier to read in a failure.
 */
export async function startFakeTorDaemon(): Promise<FakeTorDaemon> {
  const onions = new Set<string>();
  const keys = new Map<string, string>();
  let minted = 0;

  const tor = await startFakeTor((command) => {
    if (command === "PROTOCOLINFO 1") return protocolInfo("METHODS=NULL");
    if (command === "AUTHENTICATE") return "250 OK\r\n";

    if (command.startsWith("ADD_ONION ")) {
      const spec = command.split(" ")[1] ?? "";

      if (spec === "NEW:ED25519-V3") {
        const address = "abcdefghijklmnopqrstuvwxyz"[minted % 26].repeat(56);
        const key = `ED25519-V3:key${minted}`;
        minted += 1;
        keys.set(key, address);
        onions.add(address);
        return `250-ServiceID=${address}\r\n250-PrivateKey=${key}\r\n250 OK\r\n`;
      }

      const address = keys.get(spec);
      if (!address) return "512 Invalid key\r\n";
      if (onions.has(address)) return "550 Onion address collision\r\n";
      onions.add(address);
      return `250-ServiceID=${address}\r\n250 OK\r\n`;
    }

    if (command.startsWith("DEL_ONION ")) {
      const address = command.slice("DEL_ONION ".length);
      if (!onions.delete(address)) return "552 Unknown service id\r\n";
      return "250 OK\r\n";
    }

    return "510 Unrecognized command\r\n";
  });

  return { ...tor, onions };
}
