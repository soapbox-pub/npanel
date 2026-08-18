import { randomBytes } from "node:crypto";
import { BunkerURI, NConnectSigner, NSecSigner, type NostrConnectRequest, type NostrEvent } from "@nostrify/nostrify";
import type { Kysely } from "kysely";
import { generateSecretKey, getPublicKey } from "nostr-tools";
import type { Bunker, BunkerConnection, Database } from "../db/schema.ts";
import { type Logger, errFields } from "../log.ts";
import type { NostrService } from "../nostr.ts";
import { RateLimiter } from "../rate-limit.ts";
import type { SettingsService } from "../settings.ts";
import type { BunkerApi, BunkerTestResult, MintedConnection, PairingStatus } from "./api.ts";
import { type Permission, allows, formatPermission, isGrantableMethod, parsePermissions } from "./permissions.ts";
import { NOSTR_CONNECT_KIND, decodeRequest, decodeResponse, detectEncryption, signResponse } from "./rpc.ts";
import type { Secrets } from "./secrets.ts";
import { formatBunkerUri, formatNostrConnectUri, parseNostrConnectUri } from "./uri.ts";

/** How long npanel waits for a signer to answer a pairing invitation. */
const PAIRING_TIMEOUT = 5 * 60 * 1000;

/**
 * How many of a client's own relays one connection may pull in.
 *
 * A `nostrconnect://` URI can name any number, and every one is a socket the
 * gateway holds for as long as the connection lives.
 */
const MAX_CONNECTION_RELAYS = 4;

/** How often a connection's "last used" is written, in seconds. */
const TOUCH_INTERVAL = 60;

/** Longest a failed relay subscription waits before trying again. */
const MAX_SUBSCRIPTION_BACKOFF = 60_000;

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

interface Subscription {
  /** The pubkey set this filter covers, so an unchanged one is left alone. */
  key: string;
  controller: AbortController;
}

/** A connection and the bunker it signs through, which are always read together. */
interface Session {
  connection: BunkerConnection;
  bunker: Bunker;
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
  private subscriptions = new Map<string, Subscription>();
  private seen = new SeenSet();
  private limiter = new RateLimiter();
  private inFlight = 0;
  private idle: (() => void)[] = [];

  constructor(deps: BunkerServiceDeps) {
    this.db = deps.db;
    this.nostr = deps.nostr;
    this.settings = deps.settings;
    this.secrets = deps.secrets;
    this.log = deps.log;
    this.dashboardHost = deps.dashboardHost;
  }

  async start(): Promise<void> {
    if (this.running) return;
    this.running = true;

    this.unsubscribeSettings = this.settings.onChange((keys) => {
      // A signer bakes its timeout in at construction, so the setting only
      // takes effect once the cached ones are thrown away.
      if (keys.includes("bunker_request_timeout")) {
        this.signers.clear();
      }
    });

    await this.refresh();

    this.log.info("bunker_started", { subscriptions: this.subscriptions.size });
  }

  async stop(): Promise<void> {
    this.running = false;

    this.unsubscribeSettings?.();
    this.unsubscribeSettings = undefined;

    for (const bunkerId of [...this.pairings.keys()]) {
      await this.cancelPairing(bunkerId);
    }

    for (const subscription of this.subscriptions.values()) {
      subscription.controller.abort();
    }
    this.subscriptions.clear();

    this.signers.clear();
    await this.nostr.close();
  }

  async reload(): Promise<void> {
    await this.settings.reload();
    await this.refresh();
  }

  /** Resolves once every request in flight has been answered. For tests. */
  async whenIdle(): Promise<void> {
    if (this.inFlight === 0) return;
    await new Promise<void>((resolve) => this.idle.push(resolve));
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

  async mintConnection(input: {
    bunkerId: number;
    name: string;
    permissions: string[];
    createdBy: string;
    uri?: string;
  }): Promise<MintedConnection> {
    const bunker = await this.loadBunker(input.bunkerId);

    const name = input.name.trim();
    if (!name) {
      throw new Error("A connection needs a name, so people can tell it from the others.");
    }

    // Throws with something worth reading if a spec is malformed, before
    // anything is written or any key is generated.
    const permissions = parsePermissions(input.permissions);

    const signerKey = generateSecretKey();
    const signerPubkey = getPublicKey(signerKey);

    const invitation = input.uri === undefined ? undefined : parseNostrConnectUri(input.uri);
    const relays = invitation ? invitation.relays.slice(0, MAX_CONNECTION_RELAYS) : this.gatewayRelays();

    if (!relays.length) {
      throw new Error("No relays are configured for the gateway, so there is nowhere to answer requests.");
    }
    this.assertRelayBudget(relays);

    const now = Math.floor(Date.now() / 1000);
    const inserted = await this.db
      .insertInto("bunker_connections")
      .values({
        bunker_id: bunker.id,
        name,
        signer_secret: this.secrets.encrypt(Buffer.from(signerKey).toString("hex")),
        signer_pubkey: signerPubkey,
        // A client that published an invitation has already identified itself,
        // so there is nothing left to claim and no secret to hand out.
        client_pubkey: invitation?.clientPubkey ?? null,
        secret: invitation ? null : randomBytes(8).toString("hex"),
        relays: JSON.stringify(relays),
        permissions: JSON.stringify(permissions.map(formatPermission)),
        created_by: input.createdBy,
        created_at: now,
        last_used_at: null,
        last_error: null,
      })
      .returning(["id", "secret"])
      .executeTakeFirstOrThrow();

    // Subscribe before answering. The client's next request follows its
    // invitation immediately, and kind 24133 is ephemeral: anything that
    // arrives before the filter is registered is simply gone.
    await this.refresh();

    this.log.info("bunker_connection_minted", {
      bunker: bunker.slug,
      connection: name,
      signer_pubkey: signerPubkey,
      mode: invitation ? "nostrconnect" : "bunker_uri",
      permissions: permissions.length,
    });

    if (!invitation) {
      return {
        signer_pubkey: signerPubkey,
        uri: formatBunkerUri({ signerPubkey, relays, secret: inserted.secret ?? "" }),
      };
    }

    try {
      const response = { id: "connect", result: invitation.secret };
      await this.nostr.publish(signResponse(signerKey, invitation.clientPubkey, response, "nip44"), { relays });
    } catch (error) {
      // The client never learned this connection exists, so leaving the row
      // behind would only litter the dashboard with one that can never be used.
      await this.db.deleteFrom("bunker_connections").where("id", "=", inserted.id).execute();
      await this.refresh();
      throw new Error(`Couldn't answer the client on its relays: ${message(error)}`);
    }

    return { signer_pubkey: signerPubkey, uri: null };
  }

  /**
   * Bring relay subscriptions in line with the connections that exist.
   *
   * One subscription per relay, carrying every connection pubkey reachable
   * there, rather than one per connection: a hundred connections on one relay
   * is one socket and one filter.
   *
   * Subscriptions whose pubkey set hasn't changed are left strictly alone.
   * Rebuilding one loses every request that arrives in the gap, and no relay
   * will replay a kind 24133 — it is ephemeral by design.
   */
  private async refresh(): Promise<void> {
    if (!this.running) return;

    const rows = await this.db
      .selectFrom("bunker_connections")
      .innerJoin("bunkers", "bunkers.id", "bunker_connections.bunker_id")
      .select(["bunker_connections.signer_pubkey as signer_pubkey", "bunker_connections.relays as relays"])
      .where("bunkers.enabled", "=", 1)
      .execute();

    const wanted = new Map<string, Set<string>>();
    for (const row of rows) {
      for (const relay of parseRelays(row.relays)) {
        const pubkeys = wanted.get(relay) ?? new Set<string>();
        pubkeys.add(row.signer_pubkey);
        wanted.set(relay, pubkeys);
      }
    }

    for (const [relay, subscription] of this.subscriptions) {
      if (wanted.has(relay)) continue;
      subscription.controller.abort();
      this.subscriptions.delete(relay);
    }

    for (const [relay, pubkeys] of wanted) {
      const listed = [...pubkeys].sort();
      const key = listed.join(",");

      const existing = this.subscriptions.get(relay);
      if (existing?.key === key) continue;
      existing?.controller.abort();

      const controller = new AbortController();
      this.subscriptions.set(relay, { key, controller });
      void this.runSubscription(relay, listed, controller.signal);
    }
  }

  private async runSubscription(relay: string, pubkeys: string[], signal: AbortSignal): Promise<void> {
    let backoff = 1000;

    while (!signal.aborted) {
      // `since` is now, every time: a request from before this subscription
      // existed has been waiting longer than any client would, and replaying
      // one would sign against a decision somebody made minutes ago.
      const filter = { kinds: [NOSTR_CONNECT_KIND], "#p": pubkeys, since: Math.floor(Date.now() / 1000) };

      try {
        for await (const msg of this.nostr.req([filter], { signal, relays: [relay] })) {
          if (msg[0] !== "EVENT") continue;
          // Relays legitimately deliver the same event on several connections,
          // and a duplicate here would be a second signature.
          if (this.seen.add(msg[2].id)) continue;
          // Not awaited: a request waiting on somebody's phone must not hold
          // up every other client on this relay.
          this.track(this.handleEvent(msg[2]));
        }
      } catch (error) {
        if (signal.aborted) return;
        this.log.warn("bunker_subscription_failed", { relay, ...errFields(error) });
      }

      if (signal.aborted) return;
      await sleep(backoff, signal);
      backoff = Math.min(backoff * 2, MAX_SUBSCRIPTION_BACKOFF);
    }
  }

  /** Refuse to hold more sockets than an admin said this gateway may. */
  private assertRelayBudget(relays: string[]): void {
    const max = this.settings.get("bunker_max_relays");
    const total = new Set([...this.subscriptions.keys(), ...relays]);

    if (total.size > max) {
      throw new Error(
        `That would need ${total.size} relays and this gateway is limited to ${max}. Raise bunker_max_relays in Settings, or ask the client to connect on the gateway's own relays.`,
      );
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

  /**
   * Answer one request from a client.
   *
   * Every path through here ends in a response. A client that gets silence
   * waits until it gives up, having learned nothing, which is worse than any
   * error string this could send instead.
   */
  private async handleEvent(event: NostrEvent): Promise<void> {
    const target = event.tags.find((tag) => tag[0] === "p")?.[1];
    if (!target) return;

    const session = await this.loadSession(target);
    if (!session) return;

    let signerKey: Uint8Array;
    try {
      signerKey = this.connectionKey(session.connection);
    } catch (error) {
      this.log.error("bunker_connection_unreadable", {
        connection: session.connection.name,
        ...errFields(error),
      });
      return;
    }

    const request = decodeRequest(signerKey, event);
    // Anyone can address an event to a pubkey that appears in a URI, so one
    // that won't decrypt or parse is noise, not a failure.
    if (!request) return;

    // Captured before anything runs: `switch_relays` moves the connection, and
    // this answer still has to reach the client where it is listening now.
    const relays = parseRelays(session.connection.relays);
    const encryption = detectEncryption(event.content);

    let response: { id: string; result: string; error?: string };
    try {
      response = { id: request.id, result: await this.execute(session, request, event.pubkey) };
    } catch (error) {
      response = { id: request.id, result: "", error: message(error) };
    }

    try {
      await this.nostr.publish(signResponse(signerKey, event.pubkey, response, encryption), { relays });
    } catch (error) {
      this.log.warn("bunker_response_failed", {
        connection: session.connection.name,
        method: request.method,
        ...errFields(error),
      });
    }
  }

  private async execute(session: Session, request: NostrConnectRequest, clientPubkey: string): Promise<string> {
    const { connection, bunker } = session;

    if (bunker.enabled !== 1) {
      throw new Error("This bunker is switched off.");
    }

    if (
      !this.limiter.take(connection.signer_pubkey, {
        capacity: this.settings.get("bunker_rate_limit"),
        interval: 60_000,
      })
    ) {
      throw new Error("Too many requests. Slow down and try again shortly.");
    }

    if (request.method === "connect") {
      return await this.connect(connection, request, clientPubkey);
    }

    // Answered rather than ignored, so a client whose pairing went away fails
    // immediately instead of waiting out its timeout.
    if (!connection.client_pubkey || connection.client_pubkey !== clientPubkey) {
      throw new Error("Not connected. Send a connect request first.");
    }

    switch (request.method) {
      case "ping":
        return "pong";

      case "get_public_key": {
        // Cached from pairing, so the commonest call costs no upstream hop.
        if (!bunker.user_pubkey) {
          throw new Error("This bunker isn't connected to a signer yet.");
        }
        return bunker.user_pubkey;
      }

      case "switch_relays":
        return await this.switchRelays(connection);

      case "logout": {
        await this.db.deleteFrom("bunker_connections").where("id", "=", connection.id).execute();
        await this.refresh();

        this.log.info("bunker_connection_logout", {
          bunker: bunker.slug,
          connection: connection.name,
          signer_pubkey: connection.signer_pubkey,
        });

        return "ack";
      }

      default:
        return await this.forward(session, request);
    }
  }

  /**
   * Bind a client to a connection.
   *
   * Three cases, and they are easy to conflate: a first pairing, the same
   * client reconnecting, and somebody replaying a URI that was already used.
   */
  private async connect(
    connection: BunkerConnection,
    request: NostrConnectRequest,
    clientPubkey: string,
  ): Promise<string> {
    if (connection.client_pubkey === clientPubkey) {
      // A reconnect. Acknowledge without consuming anything.
      return "ack";
    }
    if (connection.client_pubkey) {
      throw new Error("This connection is already in use by another client.");
    }

    const offered = request.params[1] ?? "";
    if (!connection.secret || offered !== connection.secret) {
      throw new Error(offered ? "That secret isn't valid." : "This connection needs the secret from its bunker URI.");
    }

    // The token is single-use, as NIP-46 requires, but the row keeps working:
    // only the claim is spent, so a replayed URI can't pair a second client.
    await this.db
      .updateTable("bunker_connections")
      .set({ client_pubkey: clientPubkey, secret: null, last_used_at: Math.floor(Date.now() / 1000) })
      .where("id", "=", connection.id)
      .execute();

    this.log.info("bunker_connection_paired", {
      connection: connection.name,
      signer_pubkey: connection.signer_pubkey,
      client_pubkey: clientPubkey,
    });

    return "ack";
  }

  /** Offer the gateway's own relays, so clients drift home from foreign ones. */
  private async switchRelays(connection: BunkerConnection): Promise<string> {
    const relays = this.gatewayRelays();
    const current = parseRelays(connection.relays);

    if (!relays.length || same(current, relays)) {
      // NIP-46 spells "nothing to change" as a null result.
      return "null";
    }

    await this.db
      .updateTable("bunker_connections")
      .set({ relays: JSON.stringify(relays) })
      .where("id", "=", connection.id)
      .execute();

    // Listening on the new relays before the client is told to move, or its
    // next request lands where nothing is subscribed.
    await this.refresh();

    return JSON.stringify(relays);
  }

  /** Check a request against the connection's grant, then ask the signer. */
  private async forward(session: Session, request: NostrConnectRequest): Promise<string> {
    const { connection, bunker } = session;

    if (!isGrantableMethod(request.method)) {
      throw new Error(`Unsupported method: ${request.method}`);
    }

    let template: SignTemplate | undefined;
    if (request.method === "sign_event") {
      template = parseTemplate(request.params[0]);
    }

    if (!allows(this.grantOf(connection), request.method, template?.kind)) {
      const attempted = template ? `${request.method}:${template.kind}` : request.method;
      await this.deny(session, attempted);
      throw new Error(`Not allowed: ${attempted}. Ask whoever runs this npanel to grant it.`);
    }

    const signer = this.signerFor(bunker);

    try {
      const result = await dispatchUpstream(signer, request, template);
      await this.touch(connection);
      return result;
    } catch (error) {
      // Not recorded against the bunker: one refused signature is a decision,
      // not a broken upstream. Reachability is what `testBunker` is for.
      this.log.warn("bunker_forward_failed", {
        bunker: bunker.slug,
        connection: connection.name,
        method: request.method,
        ...errFields(error),
      });
      throw error;
    }
  }

  /** A connection's grant, failing closed if the column can't be read. */
  private grantOf(connection: BunkerConnection): Permission[] {
    try {
      const specs: unknown = JSON.parse(connection.permissions);
      if (!Array.isArray(specs)) return [];
      return parsePermissions(specs.filter((spec): spec is string => typeof spec === "string"));
    } catch (error) {
      this.log.error("bunker_permissions_unreadable", { connection: connection.name, ...errFields(error) });
      return [];
    }
  }

  private async deny(session: Session, attempted: string): Promise<void> {
    this.log.warn("bunker_request_denied", {
      bunker: session.bunker.slug,
      connection: session.connection.name,
      signer_pubkey: session.connection.signer_pubkey,
      attempted,
    });

    // Kept on the row as well as in the log, so staff can see what a client
    // wanted without going near journalctl.
    await this.db
      .updateTable("bunker_connections")
      .set({ last_error: `Refused ${attempted}` })
      .where("id", "=", session.connection.id)
      .execute();
  }

  /** Record that a connection is alive, without a write per signature. */
  private async touch(connection: BunkerConnection): Promise<void> {
    const now = Math.floor(Date.now() / 1000);
    const recent = connection.last_used_at !== null && now - connection.last_used_at < TOUCH_INTERVAL;
    if (recent && !connection.last_error) return;

    await this.db
      .updateTable("bunker_connections")
      .set({ last_used_at: now, last_error: null })
      .where("id", "=", connection.id)
      .execute();
  }

  /** The keypair a connection answers on. */
  private connectionKey(connection: BunkerConnection): Uint8Array {
    return Uint8Array.from(Buffer.from(this.secrets.decrypt(connection.signer_secret), "hex"));
  }

  private async loadSession(signerPubkey: string): Promise<Session | undefined> {
    const connection = await this.db
      .selectFrom("bunker_connections")
      .selectAll()
      .where("signer_pubkey", "=", signerPubkey)
      .executeTakeFirst();
    if (!connection) return undefined;

    const bunker = await this.db
      .selectFrom("bunkers")
      .selectAll()
      .where("id", "=", connection.bunker_id)
      .executeTakeFirst();
    if (!bunker) return undefined;

    return { connection, bunker };
  }

  private track(work: Promise<void>): void {
    this.inFlight++;

    void work
      .catch((error: unknown) => {
        this.log.error("bunker_request_failed", errFields(error));
      })
      .finally(() => {
        this.inFlight--;
        if (this.inFlight > 0) return;
        for (const resolve of this.idle.splice(0)) resolve();
      });
  }

  private async loadBunker(id: number): Promise<Bunker> {
    const bunker = await this.db.selectFrom("bunkers").selectAll().where("id", "=", id).executeTakeFirst();
    if (!bunker) throw new Error("Bunker not found.");
    return bunker;
  }
}

/** The four fields NIP-46 says a `sign_event` request carries. */
interface SignTemplate {
  kind: number;
  content: string;
  tags: string[][];
  created_at: number;
}

/**
 * Read the event a client wants signed.
 *
 * The kind decides whether this is allowed at all, so a template that doesn't
 * have one can't be checked and must not be forwarded.
 */
function parseTemplate(param: string | undefined): SignTemplate {
  let value: unknown;
  try {
    value = JSON.parse(param ?? "");
  } catch {
    throw new Error("That sign_event request didn't carry an event.");
  }

  if (typeof value !== "object" || value === null) {
    throw new Error("That sign_event request didn't carry an event.");
  }

  const template = value as Partial<SignTemplate>;
  if (typeof template.kind !== "number") {
    throw new Error("That event has no kind, so there is no way to tell whether it's allowed.");
  }

  return {
    kind: template.kind,
    content: typeof template.content === "string" ? template.content : "",
    tags: Array.isArray(template.tags) ? template.tags : [],
    created_at: typeof template.created_at === "number" ? template.created_at : Math.floor(Date.now() / 1000),
  };
}

/** Pass one already-authorized request to the signer that holds the key. */
async function dispatchUpstream(
  signer: NConnectSigner,
  request: NostrConnectRequest,
  template: SignTemplate | undefined,
): Promise<string> {
  const [first, second] = request.params;

  switch (request.method) {
    case "sign_event":
      // Checked before this was called; the type doesn't know that.
      if (!template) throw new Error("That sign_event request didn't carry an event.");
      return JSON.stringify(await signer.signEvent(template));
    case "nip04_encrypt":
      return await signer.nip04.encrypt(first, second);
    case "nip04_decrypt":
      return await signer.nip04.decrypt(first, second);
    case "nip44_encrypt":
      return await signer.nip44.encrypt(first, second);
    case "nip44_decrypt":
      return await signer.nip44.decrypt(first, second);
    default:
      throw new Error(`Unsupported method: ${request.method}`);
  }
}

/**
 * Event ids already handled, bounded.
 *
 * A relay may deliver the same ephemeral event on more than one connection,
 * and handling one twice means signing twice.
 */
class SeenSet {
  private ids = new Set<string>();

  constructor(private readonly max = 512) {}

  /** Record an id, returning whether it had been seen before. */
  add(id: string): boolean {
    if (this.ids.has(id)) return true;

    this.ids.add(id);
    if (this.ids.size > this.max) {
      // Sets iterate in insertion order, so this drops the oldest.
      const oldest = this.ids.values().next().value;
      if (oldest !== undefined) this.ids.delete(oldest);
    }

    return false;
  }
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    timer.unref?.();
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        resolve();
      },
      { once: true },
    );
  });
}

function same(a: string[], b: string[]): boolean {
  return a.length === b.length && [...a].sort().join(",") === [...b].sort().join(",");
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
