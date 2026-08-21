import { EventEmitter } from "node:events";
import { readFile } from "node:fs/promises";
import { type Socket, connect } from "node:net";

/**
 * How long a command waits for its reply.
 *
 * Generous, because `ADD_ONION` does key generation and registration before it
 * answers. It exists so that a Tor which stops replying — rather than closing
 * the connection — is still noticed and reconnected to, instead of leaving the
 * reconciler waiting forever on a promise nothing will settle.
 */
const COMMAND_TIMEOUT = 30_000;

/** How long to wait for the control socket itself to come up. */
const CONNECT_TIMEOUT = 10_000;

/** The virtual port an onion answers on. Browsers ask for `http://<address>/`. */
const ONION_PORT = 80;

/** One complete reply: its status code, and the text of each line. */
interface Reply {
  code: number;
  /** Line contents with the status code and separator stripped. */
  lines: string[];
}

/** An onion service's stable identity. */
export interface OnionKey {
  /** The address, with its `.onion` suffix. */
  address: string;
  /** `ED25519-V3:<base64>`, verbatim as Tor spells it in both directions. */
  secretKey: string;
}

export interface TorControlOptions {
  /** `tcp://host:port` or `unix:/path/to/socket`. */
  url: string;
  /** Used only if Tor's control port asks for a password. */
  password: string;
}

/**
 * A connection to Tor's control port.
 *
 * The protocol is line-based and strictly ordered — one reply per command, in
 * order — so commands are serialized through a queue and a single pending
 * slot. The one wrinkle is that asynchronous event lines (any 6xx code) can
 * arrive between a command and its reply, so replies are assembled first and
 * routed by status code second. npanel subscribes to no events, but Tor is
 * free to send them and a reader that assumed otherwise would pair the wrong
 * reply with the wrong command.
 *
 * Emits `close` when the connection ends, for any reason. Callers reconnect;
 * this class never does.
 */
export class TorControl extends EventEmitter {
  private buffer = "";
  private lines: string[] = [];
  private dataBlock: string[] | undefined;
  private pending:
    | { resolve(reply: Reply): void; reject(error: Error): void; timer: ReturnType<typeof setTimeout> }
    | undefined;
  private queue: Promise<unknown> = Promise.resolve();
  private done = false;

  private constructor(private readonly socket: Socket) {
    super();

    socket.setEncoding("utf8");
    socket.on("data", (chunk: string) => this.receive(chunk));
    socket.on("error", (error: Error) => this.shutdown(error));
    socket.on("close", () => this.shutdown(new Error("The Tor control connection closed.")));
  }

  /** Connect and authenticate, or throw explaining which of the two failed. */
  static async connect(options: TorControlOptions): Promise<TorControl> {
    const socket = await openSocket(options.url);
    const control = new TorControl(socket);

    try {
      await control.authenticate(options.password);
    } catch (error) {
      control.close();
      throw error;
    }

    return control;
  }

  /**
   * Register an onion service forwarding to `targetPort` on loopback.
   *
   * Pass `existing` to republish a service npanel already has the key for,
   * which is what happens on every reconnect; omit it to have Tor generate a
   * new one. Services are deliberately not created with `Flags=Detach`: an
   * onion outliving this connection would keep answering at an address whose
   * server is gone, which is worse than the address being briefly unreachable.
   */
  async addOnion(targetPort: number, existing?: OnionKey): Promise<OnionKey> {
    const spec = existing ? existing.secretKey : "NEW:ED25519-V3";

    let reply: Reply;
    try {
      reply = await this.command(`ADD_ONION ${spec} Port=${ONION_PORT},127.0.0.1:${targetPort}`);
    } catch (error) {
      // Tor still has the service from a connection it has not finished tearing
      // down. The state we wanted is the state it is in, so take it.
      if (existing && error instanceof TorControlError && /collision/i.test(error.message)) {
        return existing;
      }
      throw error;
    }

    const serviceId = field(reply, "ServiceID");
    if (!serviceId) {
      throw new Error("Tor accepted the onion service but did not say what its address is.");
    }

    const secretKey = field(reply, "PrivateKey") ?? existing?.secretKey;
    if (!secretKey) {
      throw new Error("Tor created an onion service but did not return its key, so the address could not be kept.");
    }

    return { address: `${serviceId}.onion`, secretKey };
  }

  /** Unregister an onion service. Its key is unaffected. */
  async delOnion(address: string): Promise<void> {
    await this.command(`DEL_ONION ${address.replace(/\.onion$/, "")}`);
  }

  close(): void {
    this.socket.destroy();
    this.shutdown(new Error("The Tor control connection was closed by npanel."));
  }

  private async authenticate(password: string): Promise<void> {
    const info = await this.command("PROTOCOLINFO 1");

    const auth = info.lines.find((line) => line.startsWith("AUTH ")) ?? "";
    const methods = (auth.match(/METHODS=([^\s]+)/)?.[1] ?? "").split(",");
    const cookieFile = auth.match(/COOKIEFILE=("(?:[^"\\]|\\.)*")/)?.[1];

    if (password && methods.includes("HASHEDPASSWORD")) {
      await this.command(`AUTHENTICATE ${quote(password)}`);
      return;
    }

    if (cookieFile && (methods.includes("COOKIE") || methods.includes("SAFECOOKIE"))) {
      // Plain COOKIE rather than SAFECOOKIE's challenge-response: Tor offers
      // both whenever cookie authentication is on, and the difference only
      // matters to something that can read the socket, which on loopback or a
      // unix socket can read the cookie file too.
      const path = unquote(cookieFile);
      let cookie: Uint8Array;
      try {
        cookie = await readFile(path);
      } catch (error) {
        throw new Error(
          `Could not read Tor's authentication cookie at ${path}: ${(error as Error).message}. The user npanel runs as has to be in the group that owns it — on Debian and Ubuntu that is debian-tor.`,
        );
      }
      await this.command(`AUTHENTICATE ${toHex(cookie)}`);
      return;
    }

    if (methods.includes("NULL")) {
      await this.command("AUTHENTICATE");
      return;
    }

    if (methods.includes("HASHEDPASSWORD")) {
      throw new Error(
        "Tor's control port wants a password. Set the Tor control password under Settings to the one its HashedControlPassword was made from.",
      );
    }

    throw new Error(`npanel cannot authenticate to Tor, which offers ${methods.join(", ") || "no method"}.`);
  }

  /** Send one command once every command before it has been answered. */
  private command(command: string): Promise<Reply> {
    const run = this.queue.then(
      () => this.send(command),
      () => this.send(command),
    );

    // Swallowed here so that one failed command does not reject the next one's
    // turn in the queue; `run` still carries the failure to the caller.
    this.queue = run.then(
      () => undefined,
      () => undefined,
    );

    return run;
  }

  private send(command: string): Promise<Reply> {
    return new Promise<Reply>((resolve, reject) => {
      if (this.done) {
        reject(new Error("The Tor control connection is closed."));
        return;
      }

      const timer = setTimeout(() => {
        // Nothing can be assumed about a connection that owes a reply, so drop
        // it rather than sending a second command into the same silence.
        this.socket.destroy();
        this.shutdown(new Error(`Tor did not answer ${command.split(" ")[0]} within ${COMMAND_TIMEOUT / 1000}s.`));
      }, COMMAND_TIMEOUT);

      this.pending = { resolve, reject, timer };
      this.socket.write(`${command}\r\n`);
    });
  }

  private receive(chunk: string): void {
    this.buffer += chunk;

    let index = this.buffer.indexOf("\r\n");
    while (index !== -1) {
      const line = this.buffer.slice(0, index);
      this.buffer = this.buffer.slice(index + 2);
      this.handleLine(line);
      index = this.buffer.indexOf("\r\n");
    }
  }

  private handleLine(line: string): void {
    if (this.dataBlock) {
      if (line === ".") {
        this.lines.push(this.dataBlock.join("\n"));
        this.dataBlock = undefined;
      } else {
        // A line beginning with a dot is sent with the dot doubled.
        this.dataBlock.push(line.startsWith("..") ? line.slice(1) : line);
      }
      return;
    }

    const code = Number(line.slice(0, 3));
    const separator = line[3];
    const rest = line.slice(4);

    if (!Number.isInteger(code) || (separator !== "-" && separator !== "+" && separator !== " ")) {
      this.socket.destroy();
      this.shutdown(new Error(`Unreadable reply from the Tor control port: ${line}`));
      return;
    }

    if (separator === "+") {
      this.lines.push(rest);
      this.dataBlock = [];
      return;
    }

    if (separator === "-") {
      this.lines.push(rest);
      return;
    }

    const reply: Reply = { code, lines: [...this.lines, rest] };
    this.lines = [];

    // 6xx is an asynchronous event, which owes nothing to any command and may
    // arrive between one and its reply.
    if (code >= 600) {
      this.emit("tor_event", reply);
      return;
    }

    const pending = this.pending;
    if (!pending) return;
    this.pending = undefined;
    clearTimeout(pending.timer);

    if (code === 250 || code === 251) {
      pending.resolve(reply);
    } else {
      pending.reject(new TorControlError(code, reply.lines.join(" ")));
    }
  }

  private shutdown(error: Error): void {
    if (this.done) return;
    this.done = true;

    const pending = this.pending;
    if (pending) {
      this.pending = undefined;
      clearTimeout(pending.timer);
      pending.reject(error);
    }

    this.emit("close", error);
  }
}

/** A non-250 reply, kept with its code so callers can tell them apart. */
export class TorControlError extends Error {
  constructor(
    readonly code: number,
    readonly detail: string,
  ) {
    super(`Tor refused the command (${code}): ${detail}`);
    this.name = "TorControlError";
  }
}

function openSocket(url: string): Promise<Socket> {
  return new Promise((resolve, reject) => {
    const socket = url.startsWith("unix:") ? connect(url.slice("unix:".length)) : connect(tcpTarget(url));

    const timer = setTimeout(() => {
      socket.destroy();
      reject(new Error(`Tor's control port at ${url} did not accept a connection within ${CONNECT_TIMEOUT / 1000}s.`));
    }, CONNECT_TIMEOUT);

    socket.once("connect", () => {
      clearTimeout(timer);
      socket.setNoDelay(true);
      socket.removeAllListeners("error");
      resolve(socket);
    });

    socket.once("error", (error: Error) => {
      clearTimeout(timer);
      reject(new Error(`Could not reach Tor's control port at ${url}: ${error.message}`));
    });
  });
}

function tcpTarget(url: string): { host: string; port: number } {
  const parsed = new URL(url);
  return { host: parsed.hostname.replace(/^\[|]$/g, ""), port: Number(parsed.port) };
}

/** The value of a `Key=Value` line, if the reply carries one. */
function field(reply: Reply, key: string): string | undefined {
  const prefix = `${key}=`;
  return reply.lines.find((line) => line.startsWith(prefix))?.slice(prefix.length);
}

/** Tor's QuotedString: wrapped in quotes, with quotes and backslashes escaped. */
function quote(value: string): string {
  return `"${value.replace(/(["\\])/g, "\\$1")}"`;
}

function unquote(value: string): string {
  return value.slice(1, -1).replace(/\\(.)/g, "$1");
}

function toHex(bytes: Uint8Array): string {
  let hex = "";
  for (const byte of bytes) hex += byte.toString(16).padStart(2, "0");
  return hex;
}
