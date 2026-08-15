import { formatNsiteAddress, NAMED_SITE_KIND } from '@shared/nsite-address';

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
 * nprofile, naddr, an `a` tag, an nsite subdomain label, or a full nsite URL —
 * showing the single canonical form that will be stored.
 */
export function AddressInput({ value, onChange, parsed, label = 'nsite', id = 'address' }: AddressInputProps) {
  return (
    <div className="space-y-2">
      <Label htmlFor={id}>{label}</Label>
      <Input
        id={id}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder="npub1…, naddr1…, or https://npub1….nsite.lol"
        autoComplete="off"
        spellCheck={false}
      />

      {parsed.error && <p className="text-sm text-destructive">{parsed.error}</p>}

      {parsed.address && (
        <div className="rounded-md border bg-muted/40 p-3 text-sm space-y-1">
          <div className="flex items-center gap-2">
            <span className="text-muted-foreground">Author</span>
            <PubkeyLabel pubkey={parsed.address.pubkey} />
          </div>
          <div className="flex items-center gap-2">
            <span className="text-muted-foreground">nsite</span>
            <span>
              {parsed.address.kind === NAMED_SITE_KIND
                ? `Named site "${parsed.address.identifier}"`
                : 'Root site'}
            </span>
          </div>
          <div className="text-xs font-mono text-muted-foreground break-all">
            {formatNsiteAddress(parsed.address)}
          </div>
        </div>
      )}
    </div>
  );
}
