import { useState } from 'react';

import { AddressInput } from '@/components/AddressInput';
import { useParsedAddress } from '@/hooks/useParsedAddress';
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
import { useCreateHost } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';

/** Add a hostname → nsite mapping, the DNS-editor moment of the dashboard. */
export function AddHostDialog() {
  const [open, setOpen] = useState(false);
  const [hostname, setHostname] = useState('');
  const [address, setAddress] = useState('');

  const parsed = useParsedAddress(address);
  const createHost = useCreateHost();
  const { toast } = useToast();

  const submit = async () => {
    try {
      await createHost.mutateAsync({ hostname, address });
      toast({ title: 'Hostname added', description: `${hostname.trim().toLowerCase()} is now configured.` });
      setOpen(false);
      setHostname('');
      setAddress('');
    } catch (error) {
      toast({
        title: "Couldn't add that hostname",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button>Add hostname</Button>
      </DialogTrigger>

      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add a hostname</DialogTitle>
          <DialogDescription>
            Point the hostname's A or AAAA record at this server, then map it to an nsite here.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="hostname">Hostname</Label>
            <Input
              id="hostname"
              value={hostname}
              onChange={(event) => setHostname(event.target.value)}
              placeholder="docs.example.com"
              autoComplete="off"
              spellCheck={false}
            />
          </div>

          <AddressInput value={address} onChange={setAddress} parsed={parsed} />
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => setOpen(false)}>
            Cancel
          </Button>
          <Button onClick={submit} disabled={!hostname.trim() || !parsed.address || createHost.isPending}>
            {createHost.isPending ? 'Adding…' : 'Add hostname'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
