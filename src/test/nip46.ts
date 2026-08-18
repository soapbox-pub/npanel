import type { NRelay, NostrConnectResponse, NostrEvent, NostrFilter, NostrRelayEVENT } from "@nostrify/nostrify";
import { finalizeEvent, generateSecretKey, getPublicKey } from "nostr-tools";
import { type Encryption, NOSTR_CONNECT_KIND, decodeResponse, decryptFrom, encryptTo } from "../bunker/rpc.ts";

/**
 * A relay that exists only in memory.
 *
 * Events published to it are handed to every subscriber whose filter matches,
 * and nothing is stored: which is exactly how a real relay treats the
 * ephemeral kind 24133 this carries.
 */
export class MemoryRelay implements NRelay {
  private subscribers = new Set<{ filters: NostrFilter[]; channel: Channel<NostrEvent> }>();
  /** Everything published, in order, for asserting on what was sent. */
  readonly published: NostrEvent[] = [];

  async event(event: NostrEvent): Promise<void> {
    this.published.push(event);

    for (const subscriber of this.subscribers) {
      if (subscriber.filters.some((filter) => matches(filter, event))) {
        subscriber.channel.push(event);
      }
    }
  }

  async *req(filters: NostrFilter[], opts?: { signal?: AbortSignal }): AsyncGenerator<NostrRelayEVENT> {
    const subscriber = { filters, channel: new Channel<NostrEvent>() };
    this.subscribers.add(subscriber);

    try {
      while (true) {
        const event = await subscriber.channel.next(opts?.signal);
        if (!event) return;
        yield ["EVENT", "sub", event];
      }
    } finally {
      this.subscribers.delete(subscriber);
    }
  }

  async query(): Promise<NostrEvent[]> {
    return [];
  }

  async close(): Promise<void> {}
}

/** A NIP-46 client, for driving the signer side of the gateway. */
export class FakeClient {
  readonly secretKey = generateSecretKey();
  readonly pubkey = getPublicKey(this.secretKey);

  constructor(private readonly relay: MemoryRelay) {}

  /** Send a request and wait for the answer to it. */
  async call(
    signerPubkey: string,
    method: string,
    params: string[] = [],
    encryption: Encryption = "nip44",
  ): Promise<NostrConnectResponse> {
    const id = Math.random().toString(36).slice(2);
    const request = this.request(signerPubkey, method, params, encryption, id);

    const controller = new AbortController();
    const subscription = this.relay
      .req([{ kinds: [NOSTR_CONNECT_KIND], "#p": [this.pubkey] }], {
        signal: controller.signal,
      })
      [Symbol.asyncIterator]();

    // Start the subscription, and let it register, before publishing: this
    // relay stores nothing, so an answer sent first would be sent to nobody.
    let pending = subscription.next();
    await tick();

    await this.relay.event(request);

    try {
      const deadline = Date.now() + 2000;
      while (Date.now() < deadline) {
        const next = await Promise.race([pending, timeout(deadline - Date.now())]);
        if (!next || next.done) break;

        pending = subscription.next();

        const response = decodeResponse(this.secretKey, next.value[2]);
        if (response?.id === id) return response;
      }
    } finally {
      controller.abort();
    }

    throw new Error(`No answer to ${method}`);
  }

  /** The event for a request, for tests that need to send it themselves. */
  request(
    signerPubkey: string,
    method: string,
    params: string[] = [],
    encryption: Encryption = "nip44",
    id = Math.random().toString(36).slice(2),
  ): NostrEvent {
    return finalizeEvent(
      {
        kind: NOSTR_CONNECT_KIND,
        created_at: Math.floor(Date.now() / 1000),
        tags: [["p", signerPubkey]],
        content: encryptTo(this.secretKey, signerPubkey, JSON.stringify({ id, method, params }), encryption),
      },
      this.secretKey,
    );
  }
}

function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function timeout(ms: number): Promise<undefined> {
  return new Promise((resolve) => setTimeout(() => resolve(undefined), Math.max(0, ms)));
}

/**
 * A stand-in for the phone at the far end of a bunker.
 *
 * Speaks real NIP-46 over a relay that only exists in memory: events are
 * encrypted, signed and verified exactly as they would be on the wire, so the
 * only thing these tests fake is the network.
 */
export interface FakeSigner {
  /** The `remote-signer-pubkey` clients address requests to. */
  pubkey: string;
  /** The identity it signs as, which `get_public_key` returns. */
  userPubkey: string;
  relay: NRelay;
  /** Accept an invitation, as a signer consuming a `nostrconnect://` URI does. */
  announce(clientPubkey: string, secret: string): void;
  /** Requests it has been sent, in order, for asserting on what was forwarded. */
  requests: { method: string; params: string[]; from: string }[];
  /** Make the next request of this method fail with `reason`. */
  fail(method: string, reason: string): void;
}

export function fakeSigner(opts: { userKey?: Uint8Array; signerKey?: Uint8Array } = {}): FakeSigner {
  const userKey = opts.userKey ?? generateSecretKey();
  const signerKey = opts.signerKey ?? generateSecretKey();
  const pubkey = getPublicKey(signerKey);

  const hub = new MemoryRelay();
  const requests: FakeSigner["requests"] = [];
  const failures = new Map<string, string>();

  function publish(to: string, id: string, result: string, error?: string): void {
    const payload = JSON.stringify(error === undefined ? { id, result } : { id, result, error });

    void hub.event(
      finalizeEvent(
        {
          kind: NOSTR_CONNECT_KIND,
          created_at: Math.floor(Date.now() / 1000),
          tags: [["p", to]],
          content: encryptTo(signerKey, to, payload, "nip44"),
        },
        signerKey,
      ),
    );
  }

  return {
    pubkey,
    userPubkey: getPublicKey(userKey),
    requests,

    fail(method, reason) {
      failures.set(method, reason);
    },

    announce(clientPubkey, secret) {
      publish(clientPubkey, "invitation", secret);
    },

    relay: {
      req: (filters: NostrFilter[], reqOpts?: { signal?: AbortSignal }) => hub.req(filters, reqOpts),

      async event(request: NostrEvent) {
        const plaintext = decryptFrom(signerKey, request.pubkey, request.content);
        const { id, method, params } = JSON.parse(plaintext) as { id: string; method: string; params: string[] };

        requests.push({ method, params, from: request.pubkey });

        const failure = failures.get(method);
        if (failure !== undefined) {
          failures.delete(method);
          publish(request.pubkey, id, "", failure);
          return;
        }

        switch (method) {
          case "connect":
            publish(request.pubkey, id, params[1] || "ack");
            return;
          case "ping":
            publish(request.pubkey, id, "pong");
            return;
          case "get_public_key":
            publish(request.pubkey, id, getPublicKey(userKey));
            return;
          case "sign_event": {
            const template = JSON.parse(params[0]) as { kind: number; content: string; tags: string[][] };
            publish(request.pubkey, id, JSON.stringify(finalizeEvent({ created_at: 0, ...template }, userKey)));
            return;
          }
          default:
            publish(request.pubkey, id, "", `Unsupported method: ${method}`);
        }
      },

      query: async () => [],
      close: async () => {},
    },
  };
}

/** Enough filter matching for NIP-46 traffic, which only ever uses these. */
function matches(filter: NostrFilter, event: NostrEvent): boolean {
  if (filter.kinds && !filter.kinds.includes(event.kind)) return false;
  if (filter.authors && !filter.authors.includes(event.pubkey)) return false;

  const wanted = filter["#p"];
  if (wanted) {
    const tagged = event.tags.filter((tag) => tag[0] === "p").map((tag) => tag[1]);
    if (!tagged.some((pubkey) => wanted.includes(pubkey))) return false;
  }

  return true;
}

/** A queue an async iterator can wait on, so a test can push events into one. */
class Channel<T> {
  private items: T[] = [];
  private waiting: ((value: T | undefined) => void) | undefined;

  push(item: T): void {
    const waiting = this.waiting;
    if (waiting) {
      this.waiting = undefined;
      waiting(item);
      return;
    }
    this.items.push(item);
  }

  /** The next item, or undefined once `signal` aborts. */
  next(signal?: AbortSignal): Promise<T | undefined> {
    const item = this.items.shift();
    if (item !== undefined) return Promise.resolve(item);
    if (signal?.aborted) return Promise.resolve(undefined);

    return new Promise<T | undefined>((resolve) => {
      this.waiting = resolve;
      signal?.addEventListener(
        "abort",
        () => {
          if (this.waiting !== resolve) return;
          this.waiting = undefined;
          resolve(undefined);
        },
        { once: true },
      );
    });
  }
}

/** Poll until `check` is true, so a test isn't written around a fixed sleep. */
export async function waitFor(check: () => boolean | Promise<boolean>, message: string, timeout = 2000): Promise<void> {
  const deadline = Date.now() + timeout;

  while (Date.now() < deadline) {
    if (await check()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }

  throw new Error(`Timed out waiting for ${message}`);
}
