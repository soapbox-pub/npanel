import { Check, Copy, Plus } from 'lucide-react';
import { useState } from 'react';

import { PermissionPicker } from '@/components/bunker/PermissionPicker';
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
import { QRCodeCanvas } from '@/components/ui/qrcode';
import { useMintConnection } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import { EMPTY_DRAFT, type PermissionDraft, draftToSpecs } from '@/lib/permissions';

/**
 * Authorize a client to sign through this bunker.
 *
 * Ends in one of two places: a `bunker://` URI to hand over, shown once
 * because its secret is single-use, or nothing to show at all — when the
 * client published its own invitation, npanel has already answered it.
 */
export function MintConnectionDialog({ slug }: { slug: string }) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState('');
  const [invitation, setInvitation] = useState('');
  const [permissions, setPermissions] = useState<PermissionDraft>(EMPTY_DRAFT);
  const [minted, setMinted] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const mint = useMintConnection();
  const { toast } = useToast();

  const specs = draftToSpecs(permissions);
  const uri = invitation.trim();
  const answering = uri.startsWith('nostrconnect://');

  const reset = () => {
    setName('');
    setInvitation('');
    setPermissions(EMPTY_DRAFT);
    setMinted(null);
  };

  const close = (next: boolean) => {
    setOpen(next);
    if (!next) reset();
  };

  const submit = async () => {
    try {
      const result = await mint.mutateAsync({
        slug,
        name: name.trim(),
        permissions: specs,
        uri: uri || undefined,
      });

      if (result.uri) {
        setMinted(result.uri);
      } else {
        toast({ title: 'Connection minted', description: `${name.trim()} is connected.` });
        close(false);
      }
    } catch (error) {
      toast({
        title: "Couldn't mint that connection",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  const copy = async () => {
    if (!minted) return;
    await navigator.clipboard.writeText(minted);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogTrigger asChild>
        <Button>
          <Plus className="mr-2 h-4 w-4" />
          Mint connection
        </Button>
      </DialogTrigger>

      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-lg">
        {minted ? (
          <>
            <DialogHeader>
              <DialogTitle>Hand this over now</DialogTitle>
              <DialogDescription>
                This URI is shown once. Its secret is single-use and npanel keeps no copy — if it's lost, revoke
                the connection and mint another.
              </DialogDescription>
            </DialogHeader>

            <div className="flex flex-col items-center gap-4 rounded-lg border bg-muted/30 p-6">
              <QRCodeCanvas value={minted} size={200} className="rounded bg-white p-2" />
              <code className="w-full break-all rounded bg-background p-3 font-mono text-xs">{minted}</code>
              <Button variant="outline" size="sm" onClick={copy}>
                {copied ? <Check className="mr-2 h-4 w-4" /> : <Copy className="mr-2 h-4 w-4" />}
                {copied ? 'Copied' : 'Copy URI'}
              </Button>
            </div>

            <DialogFooter>
              <Button onClick={() => close(false)}>Done</Button>
            </DialogFooter>
          </>
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>Mint a connection</DialogTitle>
              <DialogDescription>
                One client's authorization to sign through this bunker. It gets a key of its own, so revoking it
                later affects nothing else.
              </DialogDescription>
            </DialogHeader>

            <div className="space-y-4">
              <div className="space-y-1.5">
                <Label htmlFor="connection-name">Name</Label>
                <Input
                  id="connection-name"
                  value={name}
                  onChange={(event) => setName(event.target.value)}
                  placeholder="CI deploy bot"
                />
                <p className="text-xs text-muted-foreground">
                  How you'll recognise it in the list. Say what holds it, not what it does.
                </p>
              </div>

              <div className="space-y-1.5">
                <Label htmlFor="connection-invitation">Client invitation, if it gave you one</Label>
                <Input
                  id="connection-invitation"
                  value={invitation}
                  onChange={(event) => setInvitation(event.target.value)}
                  placeholder="nostrconnect://…"
                  className="font-mono text-xs"
                />
                <p className="text-xs text-muted-foreground">
                  {answering
                    ? 'npanel will answer this client directly.'
                    : "Leave this empty and npanel mints a bunker:// URI for the client to use instead."}
                </p>
              </div>

              <PermissionPicker value={permissions} onChange={setPermissions} idPrefix="mint" />
            </div>

            <DialogFooter>
              <Button onClick={submit} disabled={!name.trim() || !specs.length || mint.isPending}>
                {mint.isPending ? 'Minting…' : 'Mint connection'}
              </Button>
            </DialogFooter>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
