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

/**
 * Hex pubkey for whatever names one — an npub, an nprofile, or hex itself —
 * and `undefined` when the value names nobody.
 */
export function decodePubkey(value: string): string | undefined {
  const identifier = value.trim().replace(/^(?:web\+)?nostr:/, '');
  if (!identifier) return undefined;
  if (/^[0-9a-f]{64}$/i.test(identifier)) return identifier.toLowerCase();

  try {
    const decoded = nip19.decode(identifier);
    if (decoded.type === 'npub') return decoded.data;
    if (decoded.type === 'nprofile') return decoded.data.pubkey;
  } catch {
    // Not a NIP-19 identifier at all.
  }

  return undefined;
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

const RELATIVE = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });

const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ['year', 365 * 86_400],
  ['month', 30 * 86_400],
  ['week', 7 * 86_400],
  ['day', 86_400],
  ['hour', 3_600],
  ['minute', 60],
];

/** "3 hours ago", for a Unix timestamp; the exact time belongs in a tooltip. */
export function timeAgo(seconds: number, now = Date.now() / 1000): string {
  const elapsed = seconds - now;
  for (const [unit, size] of UNITS) {
    if (Math.abs(elapsed) >= size) return RELATIVE.format(Math.round(elapsed / size), unit);
  }
  return 'just now';
}

/** The full date and time of a Unix timestamp, in the reader's locale. */
export function fullDate(seconds: number): string {
  return new Date(seconds * 1000).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
}

/** Visitor counts the way Plausible abbreviates them: 1.2k, 34k, 5.6M. */
export function compactNumber(value: number): string {
  return new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 }).format(value);
}
