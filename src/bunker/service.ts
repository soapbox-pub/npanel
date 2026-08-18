import { randomBytes } from "node:crypto";
import { BunkerURI, NConnectSigner, NSecSigner } from "@nostrify/nostrify";
import type { Kysely } from "kysely";
import { generateSecretKey, getPublicKey } from "nostr-tools";
import type { Bunker, Database } from "../db/schema.ts";
import { type Logger, errFields } from "../log.ts";
import type { NostrService } from "../nostr.ts";
import type { SettingsService } from "../settings.ts";
import type { BunkerApi, BunkerTestResult, PairingStatus } from "./api.ts";
import { NOSTR_CONNECT_KIND, decodeResponse } from "./rpc.ts";
import type { Secrets } from "./secrets.ts";
import { formatNostrConnectUri } from "./uri.ts";

/** How long npanel waits for a signer to answer a pairing invitation. */
const PAIRING_TIMEOUT = 5 * 60 * 1000;

/**
 * What npanel asks the upstream signer to let it do.
 *
 * Everything, deliberately. npanel is a gateway, not a client: what any given
 * downstream connection may do is decided here, per connection, and asking the
 * upstream for a narrower set would only mean a grant staff made in the
 * dashboard failing later on the phone, where nobody is looking.
 */
const UPSTREAM_PERMS = ["sign_event", "nip04_encrypt", "nip04_decrypt", "nip44_encrypt", "nip44_decrypt"];

export interface BunkerServiceDeps {
  db: Kysely<Database>;
  nostr: NostrService;
  settings: SettingsService;
  secrets: Secrets;
  log: Logger;
  /** Named in pairing invitations, so a signer shows staff who is asking. */
  dashboardHost: string;
}

interface Pairing {
  state: PairingStatus["state"];
  uri: string | null;
  error: string | null;
  /**
   * What the signer must echo back to prove it was the one that answered.
   *
   * Deliberately not part of {@link PairingStatus}, which crosses a thread
   * boundary and ends up in a dashboard.
   */
  secret: string;
  controller: AbortController;
  timer: ReturnType<typeof setTimeout>;
}

interface CachedSigner {
  signer: NConnectSigner;
  /** The row revision this was built from, so a re-pair rebuilds it. */
  revision: number;
}

/**
 * The bunker gateway.
 *
 * Runs in its own thread (see {@link file://./worker.ts}), holds its own
 * database connection and relay pool, and is written so that nothing it does
 * can reach the request path that serves sites. Everything here is ordinary
 * async code with no threading in it, which is also what makes it testable.
 */
export class BunkerService implements BunkerApi {
  private readonly db: Kysely<Database>;
  private readonly nostr: NostrService;
  private readonly settings: SettingsService;
  private readonly secrets: Secrets;
  private readonly log: Logger;
  private readonly dashboardHost: string;

  private running = false;
  private unsubscribeSettings: (() => void) | undefined;
  private pairings = new Map<number, Pairing>();
  private signers = new Map<number, CachedSigner>();

  constructor(deps: BunkerServiceDeps) {
    this.db = deps.db;
    this.nostr = deps.nostr;
    this.settings = deps.settings;
    this.secrets = deps.secrets;
    this.log = deps.log;
    this.dashboardHost = deps.dashboardHost;
  }

  start(): void {
    if (this.running) return;
    this.running = true;

    this.unsubscribeSettings = this.settings.onChange((keys) => {
      // A signer bakes its timeout in at construction, so the setting only
      // takes effect once the cached ones are thrown away.
      if (keys.includes("bunker_request_timeout")) {
        this.signers.clear();
      }
    });

    this.log.info("bunker_started", {});
  }

  async stop(): Promise<void> {
    this.running = false;

    this.unsubscribeSettings?.();
    this.unsubscribeSettings = undefined;

    for (const bunkerId of [...this.pairings.keys()]) {
      await this.cancelPairing(bunkerId);
    }

    this.signers.clear();
    await this.nostr.close();
  }

  async reload(): Promise<void> {
    await this.settings.reload();
  }

  async startPairing(input: { bunkerId: number; uri?: string }): Promise<PairingStatus> {
    const bunker = await this.loadBunker(input.bunkerId);

    // A second attempt supersedes the first: the URI shown by an abandoned one
    // would still be live, and two listeners racing for the same response is a
    // coin flip over which keypair gets stored.
    await this.cancelPairing(bunker.id);

    const clientKey = generateSecretKey();
    const controller = new AbortController();
    const pairing: Pairing = {
      state: "waiting",
      uri: null,
      error: null,
      secret: randomBytes(8).toString("hex"),
      controller,
      timer: setTimeout(() => {
        this.failPairing(bunker.id, "No signer answered. Start again when you're ready to approve it.");
      }, PAIRING_TIMEOUT),
    };
    pairing.timer.unref?.();
    this.pairings.set(bunker.id, pairing);

    if (input.uri === undefined) {
      // The invitation has to exist before this returns, since showing it is
      // the whole point of the call. Only the waiting is deferred.
      pairing.uri = this.buildInvitation(clientKey, pairing.secret);
    }

    void this.runPairing(bunker, clientKey, input.uri, pairing);

    return snapshot(pairing);
  }

  async pairingStatus(bunkerId: number): Promise<PairingStatus> {
    const pairing = this.pairings.get(bunkerId);
    return pairing ? snapshot(pairing) : { state: "idle", uri: null, error: null };
  }

  async cancelPairing(bunkerId: number): Promise<void> {
    const pairing = this.pairings.get(bunkerId);
    if (!pairing) return;

    clearTimeout(pairing.timer);
    pairing.controller.abort();
    this.pairings.delete(bunkerId);
  }

  async testBunker(bunkerId: number): Promise<BunkerTestResult> {
    const bunker = await this.loadBunker(bunkerId);

    try {
      const signer = this.signerFor(bunker);
      const userPubkey = await signer.getPublicKey();
      await this.markReachable(bunker, userPubkey);

      return { ok: true, user_pubkey: userPubkey, error: null };
    } catch (error) {
      const reason = message(error);
      await this.markUnreachable(bunker, reason);
      this.log.warn("bunker_unreachable", { bunker: bunker.slug, err_msg: reason });

      return { ok: false, user_pubkey: bunker.user_pubkey, error: reason };
    }
  }

  /** Relays the gateway uses when it has no better instruction. */
  private gatewayRelays(): string[] {
    const configured = this.settings.get("bunker_relays");
    return configured.length ? configured : this.settings.get("relays");
  }

  private buildInvitation(clientKey: Uint8Array, secret: string): string {
    return formatNostrConnectUri({
      clientPubkey: getPublicKey(clientKey),
      relays: this.gatewayRelays(),
      secret,
      perms: UPSTREAM_PERMS,
      name: "npanel",
      url: `https://${this.dashboardHost}`,
    });
  }

  private async runPairing(
    bunker: Bunker,
    clientKey: Uint8Array,
    uri: string | undefined,
    pairing: Pairing,
  ): Promise<void> {
    try {
      const upstream =
        uri === undefined ? await this.awaitSigner(clientKey, pairing) : await this.callSigner(clientKey, uri);

      const signer = this.buildSigner(clientKey, upstream.pubkey, upstream.relays);
      const userPubkey = await signer.getPublicKey();

      await this.saveUpstream(bunker, clientKey, upstream.pubkey, upstream.relays, userPubkey);

      if (this.pairings.get(bunker.id) !== pairing) return;
      clearTimeout(pairing.timer);
      pairing.state = "connected";
      pairing.error = null;

      this.log.info("bunker_paired", {
        bunker: bunker.slug,
        user_pubkey: userPubkey,
        upstream_pubkey: upstream.pubkey,
        relays: upstream.relays.length,
      });
    } catch (error) {
      if (pairing.controller.signal.aborted) return;
      this.log.warn("bunker_pairing_failed", { bunker: bunker.slug, ...errFields(error) });
      this.failPairing(bunker.id, message(error));
    }
  }

  /** Wait for a signer to accept an invitation npanel published. */
  private async awaitSigner(clientKey: Uint8Array, pairing: Pairing): Promise<{ pubkey: string; relays: string[] }> {
    const clientPubkey = getPublicKey(clientKey);
    const relays = this.gatewayRelays();

    if (!relays.length) {
      throw new Error("No relays are configured, so there is nowhere to pair.");
    }

    const filter = { kinds: [NOSTR_CONNECT_KIND], "#p": [clientPubkey] };

    for await (const msg of this.nostr.req([filter], { signal: pairing.controller.signal, relays })) {
      if (msg[0] !== "EVENT") continue;

      const response = decodeResponse(clientKey, msg[2]);
      // Anyone may address an event to a pubkey that is briefly public, so
      // only the one carrying back our own secret settles this.
      if (!response || (response.result !== pairing.secret && response.result !== "ack")) continue;

      return { pubkey: msg[2].pubkey, relays };
    }

    throw new Error("Stopped listening before a signer answered.");
  }

  /** Consume a `bunker://` URI the signer produced. */
  private async callSigner(clientKey: Uint8Array, uri: string): Promise<{ pubkey: string; relays: string[] }> {
    let parsed: BunkerURI;
    try {
      parsed = new BunkerURI(uri.trim());
    } catch {
      throw new Error("That isn't a bunker:// URI.");
    }

    if (!parsed.relays.length) {
      throw new Error("That bunker:// URI names no relays, so there is nowhere to reach the signer.");
    }

    const signer = this.buildSigner(clientKey, parsed.pubkey, parsed.relays);

    try {
      await signer.connect(parsed.secret);
    } catch (error) {
      // A bunker:// secret is spent by the first connect that reaches the
      // signer, so a failure here often can't be retried with the same URI.
      throw new Error(`The signer refused the connection: ${message(error)}. Generate a fresh URI and try again.`);
    }

    return { pubkey: parsed.pubkey, relays: parsed.relays };
  }

  private buildSigner(clientKey: Uint8Array, upstreamPubkey: string, relays: string[]): NConnectSigner {
    return new NConnectSigner({
      relay: this.nostr.group(relays),
      pubkey: upstreamPubkey,
      signer: new NSecSigner(clientKey),
      timeout: this.settings.get("bunker_request_timeout") * 1000,
    });
  }

  /** The signer for a paired bunker, decrypting its credential on first use. */
  private signerFor(bunker: Bunker): NConnectSigner {
    const cached = this.signers.get(bunker.id);
    if (cached && cached.revision === bunker.updated_at) return cached.signer;

    if (!bunker.upstream_pubkey || !bunker.upstream_secret) {
      throw new Error("This bunker isn't connected to a signer yet.");
    }
    if (bunker.key_fingerprint && bunker.key_fingerprint !== this.secrets.fingerprint) {
      throw new Error("This bunker's credential was encrypted with a different SECRET_KEY. Pair it again.");
    }

    const clientKey = Uint8Array.from(Buffer.from(this.secrets.decrypt(bunker.upstream_secret), "hex"));
    const signer = this.buildSigner(clientKey, bunker.upstream_pubkey, parseRelays(bunker.upstream_relays));

    this.signers.set(bunker.id, { signer, revision: bunker.updated_at });

    return signer;
  }

  private async saveUpstream(
    bunker: Bunker,
    clientKey: Uint8Array,
    upstreamPubkey: string,
    relays: string[],
    userPubkey: string,
  ): Promise<void> {
    const now = Math.floor(Date.now() / 1000);

    await this.db
      .updateTable("bunkers")
      .set({
        user_pubkey: userPubkey,
        upstream_pubkey: upstreamPubkey,
        upstream_secret: this.secrets.encrypt(Buffer.from(clientKey).toString("hex")),
        upstream_relays: JSON.stringify(relays),
        key_fingerprint: this.secrets.fingerprint,
        status: "connected",
        error: null,
        last_ok_at: now,
        updated_at: now,
      })
      .where("id", "=", bunker.id)
      .execute();

    // Built from the row, so it has to be rebuilt from the new one.
    this.signers.delete(bunker.id);
  }

  private async markReachable(bunker: Bunker, userPubkey: string): Promise<void> {
    if (bunker.user_pubkey && bunker.user_pubkey !== userPubkey) {
      // The upstream now signs as someone else. Recording it beats serving
      // connections that quietly changed whose name they speak in.
      this.log.warn("bunker_identity_changed", {
        bunker: bunker.slug,
        was: bunker.user_pubkey,
        now: userPubkey,
      });
    }

    await this.db
      .updateTable("bunkers")
      .set({ user_pubkey: userPubkey, status: "connected", error: null, last_ok_at: Math.floor(Date.now() / 1000) })
      .where("id", "=", bunker.id)
      .execute();
  }

  private async markUnreachable(bunker: Bunker, reason: string): Promise<void> {
    // A bunker nobody has paired stays `pending`: it isn't broken, it's unbuilt.
    const status = bunker.upstream_pubkey ? "error" : "pending";

    await this.db.updateTable("bunkers").set({ status, error: reason }).where("id", "=", bunker.id).execute();
  }

  private failPairing(bunkerId: number, reason: string): void {
    const pairing = this.pairings.get(bunkerId);
    if (!pairing) return;

    clearTimeout(pairing.timer);
    pairing.controller.abort();
    pairing.state = "failed";
    pairing.error = reason;
    pairing.uri = null;
  }

  private async loadBunker(id: number): Promise<Bunker> {
    const bunker = await this.db.selectFrom("bunkers").selectAll().where("id", "=", id).executeTakeFirst();
    if (!bunker) throw new Error("Bunker not found.");
    return bunker;
  }
}

function snapshot(pairing: Pairing): PairingStatus {
  return { state: pairing.state, uri: pairing.uri, error: pairing.error };
}

function parseRelays(json: string): string[] {
  try {
    const value: unknown = JSON.parse(json);
    return Array.isArray(value) ? value.filter((url): url is string => typeof url === "string") : [];
  } catch {
    return [];
  }
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
