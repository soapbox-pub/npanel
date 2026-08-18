/**
 * What the main thread may ask the bunker worker to do.
 *
 * Kept apart from the implementation so {@link file://./host.ts} can speak this
 * interface without importing a service that would drag a database connection
 * and a relay pool onto the thread that serves sites.
 */

export interface PairingStatus {
  /** `idle` when nothing is in flight, which is also what a restart looks like. */
  state: "idle" | "waiting" | "connected" | "failed";
  /** The `nostrconnect://` URI to show while waiting, if this flow has one. */
  uri: string | null;
  error: string | null;
}

export interface MintedConnection {
  /** The pubkey clients address, and how the API names this connection. */
  signer_pubkey: string;
  /**
   * The `bunker://` URI to show once, or null when the client supplied its own
   * invitation and has already been answered.
   */
  uri: string | null;
}

export interface BunkerTestResult {
  ok: boolean;
  user_pubkey: string | null;
  error: string | null;
}

export interface BunkerApi {
  /** Re-read state shared with the main thread, which wrote it. */
  reload(): Promise<void>;
  /**
   * Begin connecting a bunker to the signer that holds its key. With a
   * `bunker://` URI npanel consumes it; without one npanel mints a
   * `nostrconnect://` URI for the signer to consume instead.
   */
  startPairing(input: { bunkerId: number; uri?: string }): Promise<PairingStatus>;
  pairingStatus(bunkerId: number): Promise<PairingStatus>;
  cancelPairing(bunkerId: number): Promise<void>;
  /** Ask the upstream signer who it is, recording whether it answered. */
  testBunker(bunkerId: number): Promise<BunkerTestResult>;
  /**
   * Authorize a client to sign through a bunker.
   *
   * With a `nostrconnect://` URI npanel answers the client's invitation;
   * without one it mints a `bunker://` URI for the client to consume. Only the
   * gateway can do this: it holds the key that seals the connection's own
   * keypair.
   */
  mintConnection(input: {
    bunkerId: number;
    name: string;
    permissions: string[];
    createdBy: string;
    uri?: string;
  }): Promise<MintedConnection>;
  /** Close relays and the database connection, before the thread goes away. */
  stop(): Promise<void>;
}

/**
 * What the API needs from the gateway.
 *
 * An interface rather than the worker handle itself, so route tests can stand
 * in a plain object and never spawn a thread.
 */
export interface BunkerGateway extends BunkerApi {
  /** Whether this deployment can hold credentials at all. */
  readonly available: boolean;
}

/** One call, as it crosses the thread boundary. */
export type BunkerCall = {
  [K in keyof BunkerApi]: { method: K; args: Parameters<BunkerApi[K]> };
}[keyof BunkerApi];
