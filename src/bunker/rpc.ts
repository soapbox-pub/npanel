import { NSchema, type NostrConnectRequest, type NostrConnectResponse, type NostrEvent } from "@nostrify/nostrify";
import { finalizeEvent, nip04, nip44 } from "nostr-tools";

/** NIP-46's request and response envelope. */
export const NOSTR_CONNECT_KIND = 24133;

export type Encryption = "nip04" | "nip44";

/**
 * Which scheme a payload used.
 *
 * NIP-44 is the standard and NIP-04 is what older clients still send. npanel
 * accepts both and answers in whichever it was asked, because dropping NIP-04
 * fails in the least debuggable way available: a decrypt error on this side
 * and a silent timeout on the other.
 */
export function detectEncryption(content: string): Encryption {
  return content.includes("?iv=") ? "nip04" : "nip44";
}

export function encryptTo(secretKey: Uint8Array, pubkey: string, plaintext: string, encryption: Encryption): string {
  if (encryption === "nip04") {
    return nip04.encrypt(secretKey, pubkey, plaintext);
  }
  return nip44.v2.encrypt(plaintext, nip44.v2.utils.getConversationKey(secretKey, pubkey));
}

export function decryptFrom(secretKey: Uint8Array, pubkey: string, content: string): string {
  if (detectEncryption(content) === "nip04") {
    return nip04.decrypt(secretKey, pubkey, content);
  }
  return nip44.v2.decrypt(content, nip44.v2.utils.getConversationKey(secretKey, pubkey));
}

/**
 * Read a response addressed to `secretKey`, or undefined if this event isn't
 * one.
 *
 * Anyone may `p`-tag a pubkey, so an event that fails to decrypt or doesn't
 * parse is noise rather than an error: the only sane thing is to ignore it and
 * keep listening.
 */
export function decodeResponse(secretKey: Uint8Array, event: NostrEvent): NostrConnectResponse | undefined {
  try {
    const plaintext = decryptFrom(secretKey, event.pubkey, event.content);
    return NSchema.connectResponse().parse(JSON.parse(plaintext));
  } catch {
    return undefined;
  }
}

/** Read a request addressed to `secretKey`, or undefined if this isn't one. */
export function decodeRequest(secretKey: Uint8Array, event: NostrEvent): NostrConnectRequest | undefined {
  try {
    const plaintext = decryptFrom(secretKey, event.pubkey, event.content);
    return NSchema.connectRequest().parse(JSON.parse(plaintext));
  } catch {
    return undefined;
  }
}

/** Build the signed, encrypted event carrying one response back to a client. */
export function signResponse(
  secretKey: Uint8Array,
  clientPubkey: string,
  response: NostrConnectResponse,
  encryption: Encryption,
): NostrEvent {
  return finalizeEvent(
    {
      kind: NOSTR_CONNECT_KIND,
      created_at: Math.floor(Date.now() / 1000),
      tags: [["p", clientPubkey]],
      content: encryptTo(secretKey, clientPubkey, JSON.stringify(response), encryption),
    },
    secretKey,
  );
}
