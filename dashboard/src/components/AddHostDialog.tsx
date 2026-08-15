import { useState } from 'react';

import { AddressInput } from '@/components/AddressInput';
import { DnsRecords } from '@/components/DnsRecords';
import { useParsedAddress } from '@/hooks/useParsedAddress';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
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
import { useCreateHost } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';

/** Add a domain → nsite mapping, the DNS-editor moment of the dashboard. */
export function AddHostDialog() {
  const [open, setOpen] = useState(false);
  const [hostname, setHostname] = useState('');
  const [address, setAddress] = useState('');
  const [hidden, setHidden] = useState(false);

  const parsed = useParsedAddress(address);
  const createHost = useCreateHost();
  const { toast } = useToast();

  const submit = async () => {
    try {
      await createHost.mutateAsync({ hostname, address, hidden });
      toast({ title: 'Site added', description: `${hostname.trim().toLowerCase()} is now configured.` });
      setOpen(false);
      setHostname('');
      setAddress('');
      setHidden(false);
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
        <Button>Add site</Button>
      </DialogTrigger>

      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add a site</DialogTitle>
          <DialogDescription>
            Point the domain at this server with the DNS records below, then map it to an nsite here.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
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
            <DnsRecords hostname={hostname} />
          </div>

          <AddressInput value={address} onChange={setAddress} parsed={parsed} />

          {/* Offered here rather than only after the fact, so a site that was
              never meant to be listed is never listed. */}
          <div className="flex items-start gap-3">
            <Checkbox
              id="hidden"
              checked={hidden}
              onCheckedChange={(checked) => setHidden(checked === true)}
              className="mt-0.5"
            />
            <Label htmlFor="hidden" className="font-normal text-sm text-muted-foreground">
              Unlisted — keep this site off the public list and away from staff without access.
            </Label>
          </div>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            Cancel
          </Button>
          <Button onClick={submit} disabled={!hostname.trim() || !parsed.address || createHost.isPending}>
            {createHost.isPending ? 'Adding…' : 'Add site'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
