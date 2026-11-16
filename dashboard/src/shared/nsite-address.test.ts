import { nip19 } from 'nostr-tools';
import { describe, expect, it } from 'vitest';

import { encodePubkeyB36, type NsiteAddress, parseNsiteAddress, webPageUrl } from './nsite-address';

// The same cases as `src/nsite_address.rs`, so the two parsers can't drift apart.
const PUBKEY = '3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d';
const root: NsiteAddress = { pubkey: PUBKEY, kind: 15128, identifier: '' };
const docs: NsiteAddress = { pubkey: PUBKEY, kind: 35128, identifier: 'docs' };

describe('parseNsiteAddress', () => {
  it('parses nsite:// and nostr: URIs', () => {
    const npub = nip19.npubEncode(PUBKEY);
    const label = `${encodePubkeyB36(PUBKEY)}docs`;
    const naddr = nip19.naddrEncode({ pubkey: PUBKEY, kind: 35128, identifier: 'docs', relays: ['wss://relay.example.com'] });
    const rootNaddr = nip19.naddrEncode({ pubkey: PUBKEY, kind: 15128, identifier: '' });

    expect(parseNsiteAddress(`nsite://${npub}/`)).toEqual(root);
    expect(parseNsiteAddress(`nsite://${npub}`)).toEqual(root);
    expect(parseNsiteAddress(`nsite://${label}/a/b?c=d#e`)).toEqual(docs);
    expect(parseNsiteAddress(`nsite://${naddr}/app.js?v=1`)).toEqual(docs);
    expect(parseNsiteAddress(`NSITE://${naddr.toUpperCase()}/`)).toEqual(docs);
    expect(parseNsiteAddress(`nsite://${rootNaddr}/`)).toEqual(root);

    expect(parseNsiteAddress(`nostr:${naddr}`)).toEqual(docs);
    expect(parseNsiteAddress(`NOSTR:${nip19.nprofileEncode({ pubkey: PUBKEY })}`)).toEqual(root);
    expect(parseNsiteAddress(`web+nostr:${npub}`)).toEqual(root);
  });

  it('tells web pages apart', () => {
    expect(webPageUrl(' https://example.com/a ')?.href).toBe('https://example.com/a');
    expect(webPageUrl('http://example.com')).toBeDefined();
    expect(webPageUrl('nsite://example')).toBeUndefined();
    expect(webPageUrl('example.com')).toBeUndefined();
    expect(webPageUrl('https://')).toBeUndefined();
    // A page off any gateway is no site by its name alone.
    expect(() => parseNsiteAddress('https://example.com/')).toThrow(/^Unrecognized address/);
  });
});
