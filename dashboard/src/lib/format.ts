import { nip19 } from 'nostr-tools';

/** Middle-truncate a long identifier so it fits in a table cell. */
export function shorten(value: string): string {
  return value.length <= 20 ? value : `${value.slice(0, 10)}…${value.slice(-6)}`;
}

/** npub for a hex pubkey, falling back to the input if it isn't one. */
export function safeNpub(pubkey: string): string {
  try {
    return nip19.npubEncode(pubkey);
  } catch {
    return pubkey;
  }
}

export function formatBytes(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = 0;

  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }

  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}
