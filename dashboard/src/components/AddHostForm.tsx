import { AddressInput } from '@/components/AddressInput';
import { DnsRecords } from '@/components/DnsRecords';
import { Button } from '@/components/ui/button';
import { DialogFooter } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';
import { useCreateHost } from '@/hooks/useNpanel';
import { useParsedAddress } from '@/hooks/useParsedAddress';
import { useToast } from '@/hooks/useToast';
import { isRootDomain } from '@shared/hostname';

export interface AddHostDraft {
  hostname: string;
  address: string;
  hidden: boolean;
  redirectWww: boolean;
}

interface AddHostFormProps {
  draft: AddHostDraft;
  onChange: (draft: AddHostDraft) => void;
  onAdded: () => void;
  onCancel: () => void;
}

/** The body of the Add Site dialog. */
export function AddHostForm({ draft, onChange, onAdded, onCancel }: AddHostFormProps) {
  const { hostname, address, hidden, redirectWww } = draft;
  const set = (change: Partial<AddHostDraft>) => onChange({ ...draft, ...change });

  const parsed = useParsedAddress(address);
  const createHost = useCreateHost();
  const { toast } = useToast();

  const domain = hostname.trim().toLowerCase();
  // Only a root domain owns its `www`, so that is the only place the offer means anything.
  const www = isRootDomain(domain);

  const submit = async () => {
    try {
      await createHost.mutateAsync({ hostname, address, hidden, redirect_www: redirectWww });
      toast({ title: `${domain} added` });
      onAdded();
    } catch (error) {
      toast({
        title: "Couldn't add that site",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <form
      className="space-y-5"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <div className="space-y-2">
        <Label htmlFor="hostname">Domain</Label>
        <Input
          id="hostname"
          value={hostname}
          onChange={(event) => set({ hostname: event.target.value })}
          placeholder="docs.example.com"
          autoComplete="off"
          spellCheck={false}
        />
        <DnsRecords hostname={hostname} www={www && redirectWww} />
      </div>

      <AddressInput value={address} onChange={(address) => set({ address })} parsed={parsed} />

      <div className="space-y-3">
        {www && (
          <label htmlFor="redirect-www" className="flex items-center justify-between gap-4 text-sm">
            <span>
              Redirect <span className="font-medium">www.{domain}</span>
            </span>
            <Switch id="redirect-www" checked={redirectWww} onCheckedChange={(redirectWww) => set({ redirectWww })} />
          </label>
        )}
        {/* Offered here rather than only after the fact, so a site that was
            never meant to be listed is never listed. */}
        <label htmlFor="listed" className="flex items-center justify-between gap-4 text-sm">
          <span>Show on the public site list</span>
          <Switch id="listed" checked={!hidden} onCheckedChange={(listed) => set({ hidden: !listed })} />
        </label>
      </div>

      <DialogFooter>
        <Button type="button" variant="outline" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit" disabled={!domain || !parsed.address || createHost.isPending}>
          {createHost.isPending ? 'Adding…' : 'Add Site'}
        </Button>
      </DialogFooter>
    </form>
  );
}

export default AddHostForm;
