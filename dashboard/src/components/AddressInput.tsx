import { NAMED_SITE_KIND } from '@shared/nsite-address';

import { PubkeyLabel } from '@/components/PubkeyLabel';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import type { ParsedAddress } from '@/hooks/useParsedAddress';

interface AddressInputProps {
  value: string;
  onChange(value: string): void;
  /** Result of `useParsedAddress(value)`, owned by the form. */
  parsed: ParsedAddress;
  label?: string;
  id?: string;
}

/**
 * Address field accepting every format an nsite might be pasted in — npub,
 * nprofile, naddr, a `nostr:` URI, an `a` tag, an nsite subdomain label, an
 * nsite gateway or `nsite://` URL, or the address of a web page that names
 * its nsite — and saying whose site it turned out to be.
 */
export function AddressInput({ value, onChange, parsed, label = 'nsite', id = 'address' }: AddressInputProps) {
  return (
    <div className="space-y-2">
      <Label htmlFor={id}>{label}</Label>
      <Input
        id={id}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder="npub1…, naddr1…, nsite://…, or a site’s URL"
        autoComplete="off"
        spellCheck={false}
        aria-invalid={parsed.error ? true : undefined}
      />

      {parsed.checking && (
        <p className="text-xs text-muted-foreground" aria-live="polite">
          Asking {parsed.checking} which nsite it is…
        </p>
      )}

      {parsed.error && <p className="text-xs text-destructive">{parsed.error}</p>}

      {parsed.address && (
        <p className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
          <PubkeyLabel pubkey={parsed.address.pubkey} className="font-medium text-foreground" />
          <span aria-hidden>·</span>
          <span className="truncate">
            {parsed.address.kind === NAMED_SITE_KIND ? parsed.address.identifier : 'Root site'}
          </span>
        </p>
      )}
    </div>
  );
}
