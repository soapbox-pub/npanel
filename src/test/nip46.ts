import type { NRelay, NostrEvent, NostrFilter } from "@nostrify/nostrify";
import { finalizeEvent, generateSecretKey, getPublicKey } from "nostr-tools";
import { NOSTR_CONNECT_KIND, decryptFrom, encryptTo } from "../bunker/rpc.ts";

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

  const subscribers = new Set<Channel<NostrEvent>>();
  const requests: FakeSigner["requests"] = [];
  const failures = new Map<string, string>();

  function publish(to: string, id: string, result: string, error?: string): void {
    const payload = JSON.stringify(error === undefined ? { id, result } : { id, result, error });
    const event = finalizeEvent(
      {
        kind: NOSTR_CONNECT_KIND,
        created_at: Math.floor(Date.now() / 1000),
        tags: [["p", to]],
        content: encryptTo(signerKey, to, payload, "nip44"),
      },
      signerKey,
    );

    for (const subscriber of subscribers) subscriber.push(event);
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
      async *req(filters: NostrFilter[], reqOpts?: { signal?: AbortSignal }) {
        const channel = new Channel<NostrEvent>();
        subscribers.add(channel);

        try {
          while (true) {
            const event = await channel.next(reqOpts?.signal);
            if (!event) return;
            if (filters.some((filter) => matches(filter, event))) {
              yield ["EVENT", "sub", event] as const;
            }
          }
        } finally {
          subscribers.delete(channel);
        }
      },

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
