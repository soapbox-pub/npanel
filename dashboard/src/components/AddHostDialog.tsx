import { Plus } from 'lucide-react';
import { useState } from 'react';

import { AddressInput } from '@/components/AddressInput';
import { DnsRecords } from '@/components/DnsRecords';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';
import { useCreateHost } from '@/hooks/useNpanel';
import { useParsedAddress } from '@/hooks/useParsedAddress';
import { useToast } from '@/hooks/useToast';
import { isRootDomain } from '@shared/hostname';

/** Map a domain to an nsite. */
export function AddHostDialog() {
  const [open, setOpen] = useState(false);
  const [hostname, setHostname] = useState('');
  const [address, setAddress] = useState('');
  const [hidden, setHidden] = useState(false);
  const [redirectWww, setRedirectWww] = useState(true);

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
      setOpen(false);
      setHostname('');
      setAddress('');
      setHidden(false);
      setRedirectWww(true);
    } catch (error) {
      toast({
        title: "Couldn't add that site",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button className="rounded-full">
          <Plus aria-hidden />
          Add Site
        </Button>
      </DialogTrigger>

      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add Site</DialogTitle>
          <DialogDescription>Serve an nsite from a domain you control.</DialogDescription>
        </DialogHeader>

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
              onChange={(event) => setHostname(event.target.value)}
              placeholder="docs.example.com"
              autoComplete="off"
              spellCheck={false}
            />
            <DnsRecords hostname={hostname} www={www && redirectWww} />
          </div>

          <AddressInput value={address} onChange={setAddress} parsed={parsed} />

          <div className="space-y-3">
            {www && (
              <label htmlFor="redirect-www" className="flex items-center justify-between gap-4 text-sm">
                <span>
                  Redirect <span className="font-medium">www.{domain}</span>
                </span>
                <Switch id="redirect-www" checked={redirectWww} onCheckedChange={setRedirectWww} />
              </label>
            )}
            {/* Offered here rather than only after the fact, so a site that was
                never meant to be listed is never listed. */}
            <label htmlFor="listed" className="flex items-center justify-between gap-4 text-sm">
              <span>Show on the public site list</span>
              <Switch id="listed" checked={!hidden} onCheckedChange={(listed) => setHidden(!listed)} />
            </label>
          </div>

          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={!domain || !parsed.address || createHost.isPending}>
              {createHost.isPending ? 'Adding…' : 'Add Site'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
