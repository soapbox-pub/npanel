import { Check, Copy, Smartphone } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { QRCodeCanvas } from '@/components/ui/qrcode';
import { Skeleton } from '@/components/ui/skeleton';
import { useBunkerPairing, useCancelPairing, useStartPairing } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';

/**
 * Connecting a bunker to the signer that really holds its key.
 *
 * Two directions, because signers differ: npanel can mint an invitation for a
 * phone to scan, or swallow a `bunker://` URI the phone produced. The first is
 * the better experience and so is the one on screen; the second is a line of
 * text away for signers that only emit those.
 */
export function PairingCard({ slug, paired }: { slug: string; paired: boolean }) {
  const [active, setActive] = useState(false);
  const [uri, setUri] = useState('');
  const [copied, setCopied] = useState(false);

  const start = useStartPairing();
  const cancel = useCancelPairing();
  const { toast } = useToast();

  const [state, setState] = useState<'idle' | 'waiting' | 'connected' | 'failed'>('idle');

  // Polling stops the moment the answer arrives, and the outcome is shown here
  // rather than announced in a toast that can be missed while looking at a
  // phone. `active` only ever changes because somebody clicked something.
  const pairing = useBunkerPairing(slug, active && state === 'waiting');
  const invitation = pairing.data?.uri ?? null;

  if (active && pairing.data && pairing.data.state !== state) {
    setState(pairing.data.state);
  }

  const begin = async (bunkerUri?: string) => {
    try {
      await start.mutateAsync({ slug, uri: bunkerUri });
      setActive(true);
      setState('waiting');
      setUri('');
    } catch (error) {
      toast({
        title: "Couldn't start pairing",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  const stop = async () => {
    setActive(false);
    setState('idle');
    await cancel.mutateAsync(slug).catch(() => {});
  };

  const copy = async () => {
    if (!invitation) return;
    await navigator.clipboard.writeText(invitation);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-base">{paired ? 'Reconnect this bunker' : 'Connect a signer'}</CardTitle>
        <CardDescription>
          {paired
            ? 'Point this bunker at a different signer. Connections already minted from it keep working.'
            : 'npanel never holds the key. Connect the app that does — Amber, or another NIP-46 signer — and npanel signs by asking it.'}
        </CardDescription>
      </CardHeader>

      <CardContent className="space-y-6">
        {active && state === 'connected' ? (
          <div className="flex flex-wrap items-center gap-3">
            <Check className="h-5 w-5 text-primary" />
            <p className="flex-1 text-sm">A signer answered. npanel can sign through this bunker now.</p>
            <Button variant="outline" size="sm" onClick={() => setActive(false)}>
              Done
            </Button>
          </div>
        ) : active && state === 'failed' ? (
          <div className="space-y-3">
            <p className="text-sm text-destructive">{pairing.data?.error ?? 'No signer answered.'}</p>
            <Button variant="outline" size="sm" onClick={() => setActive(false)}>
              Try again
            </Button>
          </div>
        ) : active && state === 'waiting' && invitation ? (
          <div className="space-y-4">
            <div className="flex flex-col items-center gap-4 rounded-lg border bg-muted/30 p-6">
              <QRCodeCanvas value={invitation} size={220} className="rounded bg-white p-2" />
              <p className="text-center text-sm text-muted-foreground">
                Scan this with your signer, then approve the request there.
              </p>
              <div className="flex flex-wrap justify-center gap-2">
                <Button asChild size="sm">
                  <a href={invitation}>
                    <Smartphone className="mr-2 h-4 w-4" />
                    Open in a signer on this device
                  </a>
                </Button>
                <Button variant="outline" size="sm" onClick={copy}>
                  {copied ? <Check className="mr-2 h-4 w-4" /> : <Copy className="mr-2 h-4 w-4" />}
                  {copied ? 'Copied' : 'Copy invitation'}
                </Button>
              </div>
            </div>

            <p className="text-sm text-muted-foreground">
              Approve npanel for everything it asks for. It decides what each connection you mint is allowed to
              do; a signer that asks you to confirm every request will leave those connections timing out.
            </p>

            <div className="flex items-center gap-3">
              <Skeleton className="h-2 w-2 rounded-full" />
              <span className="text-sm text-muted-foreground">Waiting for a signer to answer…</span>
              <Button variant="ghost" size="sm" onClick={stop}>
                Cancel
              </Button>
            </div>
          </div>
        ) : (
          <div className="space-y-6">
            <Button onClick={() => begin()} disabled={start.isPending}>
              {start.isPending ? 'Starting…' : paired ? 'Connect a different signer' : 'Connect a signer'}
            </Button>

            <div className="space-y-2 border-t pt-4">
              <Label htmlFor="bunker-uri">Or paste a bunker:// URI</Label>
              <div className="flex flex-wrap gap-2">
                <Input
                  id="bunker-uri"
                  value={uri}
                  onChange={(event) => setUri(event.target.value)}
                  placeholder="bunker://…"
                  className="min-w-64 flex-1 font-mono text-xs"
                />
                <Button
                  variant="outline"
                  onClick={() => begin(uri.trim())}
                  disabled={!uri.trim().startsWith('bunker://') || start.isPending}
                >
                  Use it
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">
                For signers that hand you a URI instead of scanning one. Its secret is single-use, so generate a
                fresh one if this doesn't work first time.
              </p>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
