import { useSeoMeta } from '@unhead/react';
import { ArrowLeft, RefreshCw } from 'lucide-react';
import { Link, useNavigate, useParams } from 'react-router-dom';

import { AccessEditor } from '@/components/AccessEditor';
import { BunkerStatusBadge } from '@/components/bunker/BunkerStatusBadge';
import { ConnectionList } from '@/components/bunker/ConnectionList';
import { MintConnectionDialog } from '@/components/bunker/MintConnectionDialog';
import { PairingCard } from '@/components/bunker/PairingCard';
import { PubkeyLabel } from '@/components/PubkeyLabel';
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import {
  useBunker,
  useBunkerGrants,
  useDeleteBunker,
  useMe,
  useSetBunkerGrants,
  useTestBunker,
  useUpdateBunker,
} from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';

/** One bunker: who it signs as, what signs through it, and who may manage it. */
export function BunkerDetailPage() {
  const slug = useParams().slug;
  const navigate = useNavigate();
  const { toast } = useToast();

  const bunker = useBunker(slug);
  const me = useMe();
  const updateBunker = useUpdateBunker();
  const deleteBunker = useDeleteBunker();
  const testBunker = useTestBunker();
  const grants = useBunkerGrants(slug, me.data?.role === 'admin');
  const setGrants = useSetBunkerGrants();

  useSeoMeta({ title: bunker.data ? `${bunker.data.name} — npanel` : 'npanel' });

  if (bunker.isLoading) return <Skeleton className="h-64 w-full" />;

  if (bunker.isError || !bunker.data) {
    return (
      <div className="space-y-4">
        <BackLink />
        <p className="text-sm text-destructive">
          {bunker.error instanceof Error ? bunker.error.message : 'Bunker not found.'}
        </p>
      </div>
    );
  }

  const data = bunker.data;

  const run = async (action: () => Promise<unknown>, success: string) => {
    try {
      await action();
      toast({ title: success });
    } catch (error) {
      toast({
        title: 'That did not work',
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  const check = async () => {
    try {
      const { result } = await testBunker.mutateAsync(data.slug);
      toast(
        result.ok
          ? { title: 'The signer answered', description: 'This bunker can sign.' }
          : {
              title: "The signer didn't answer",
              description: result.error ?? 'No reason given.',
              variant: 'destructive',
            },
      );
    } catch (error) {
      toast({
        title: "Couldn't reach the gateway",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <div className="space-y-6">
      <BackLink />

      <div className="flex flex-wrap items-start justify-between gap-4">
        <div className="space-y-2">
          <h1 className="text-2xl font-semibold tracking-tight">{data.name}</h1>
          <div className="flex flex-wrap items-center gap-3">
            <BunkerStatusBadge status={data.status} error={data.error} />
            {data.user_pubkey ? (
              <PubkeyLabel pubkey={data.user_pubkey} />
            ) : (
              <span className="text-sm text-muted-foreground">No signer connected yet</span>
            )}
          </div>
        </div>

        <Button variant="outline" onClick={check} disabled={testBunker.isPending || !data.upstream_pubkey}>
          <RefreshCw className={`mr-2 h-4 w-4 ${testBunker.isPending ? 'animate-spin' : ''}`} />
          Check the signer
        </Button>
      </div>

      {data.status !== 'connected' && <PairingCard slug={data.slug} paired={!!data.upstream_pubkey} />}

      <Tabs defaultValue="connections">
        <TabsList>
          <TabsTrigger value="connections">Connections</TabsTrigger>
          <TabsTrigger value="settings">Settings</TabsTrigger>
          {me.data?.role === 'admin' && <TabsTrigger value="access">Access</TabsTrigger>}
        </TabsList>

        <TabsContent value="connections" className="space-y-4 pt-4">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <p className="text-sm text-muted-foreground">
              Each connection is one client's authorization to sign, with a key of its own.
            </p>
            <MintConnectionDialog slug={data.slug} />
          </div>

          <ConnectionList slug={data.slug} />
        </TabsContent>

        <TabsContent value="settings" className="space-y-4 pt-4">
          <Card>
            <CardHeader>
              <CardTitle className="text-base">Settings</CardTitle>
            </CardHeader>
            <CardContent className="space-y-6">
              <div className="space-y-2">
                <label className="flex items-center gap-3 text-sm">
                  <Switch
                    checked={data.enabled}
                    onCheckedChange={(enabled) =>
                      run(
                        () => updateBunker.mutateAsync({ slug: data.slug, patch: { enabled } }),
                        enabled ? 'Bunker switched on' : 'Bunker switched off',
                      )
                    }
                  />
                  Enabled
                </label>
                <p className="text-sm text-muted-foreground">
                  {data.enabled
                    ? 'Connections may sign through this bunker.'
                    : 'Every request is refused, without revoking anything. Switch it back on to resume.'}
                </p>
              </div>

              {data.status === 'connected' && (
                <div className="space-y-2 border-t pt-6">
                  <p className="text-sm">Signer</p>
                  <p className="text-sm text-muted-foreground">
                    Reachable on {data.upstream_relays.length}{' '}
                    {data.upstream_relays.length === 1 ? 'relay' : 'relays'}. Connect a different one to re-point
                    this bunker; the connections above keep working.
                  </p>
                  <PairingCard slug={data.slug} paired />
                </div>
              )}
            </CardContent>
          </Card>

          <Card className="border-destructive/40">
            <CardHeader>
              <CardTitle className="text-base">Danger zone</CardTitle>
            </CardHeader>
            <CardContent>
              <AlertDialog>
                <AlertDialogTrigger asChild>
                  <Button variant="destructive">Delete this bunker</Button>
                </AlertDialogTrigger>
                <AlertDialogContent>
                  <AlertDialogHeader>
                    <AlertDialogTitle>Delete {data.name}?</AlertDialogTitle>
                    <AlertDialogDescription>
                      Every connection minted from it stops working immediately. The key itself is untouched —
                      it was never here — but the signer's session is forgotten, so re-adding this means pairing
                      again.
                    </AlertDialogDescription>
                  </AlertDialogHeader>
                  <AlertDialogFooter>
                    <AlertDialogCancel>Keep it</AlertDialogCancel>
                    <AlertDialogAction
                      onClick={async () => {
                        await run(() => deleteBunker.mutateAsync(data.slug), 'Bunker deleted');
                        navigate('/bunkers');
                      }}
                    >
                      Delete
                    </AlertDialogAction>
                  </AlertDialogFooter>
                </AlertDialogContent>
              </AlertDialog>
            </CardContent>
          </Card>
        </TabsContent>

        {me.data?.role === 'admin' && (
          <TabsContent value="access" className="pt-4">
            <Card>
              <CardContent className="pt-6">
                <AccessEditor
                  granted={grants.data?.map((grant) => grant.user_pubkey)}
                  loading={grants.isLoading}
                  saving={setGrants.isPending}
                  onSave={(pubkeys) => setGrants.mutateAsync({ slug: data.slug, pubkeys })}
                  scope="bunker"
                />
                <p className="mt-4 text-xs text-muted-foreground">
                  Staff granted access here can mint connections that sign as this identity.
                </p>
              </CardContent>
            </Card>
          </TabsContent>
        )}
      </Tabs>
    </div>
  );
}

function BackLink() {
  return (
    <Link to="/bunkers" className="inline-flex items-center text-sm text-muted-foreground hover:text-foreground">
      <ArrowLeft className="mr-1 h-4 w-4" />
      Bunkers
    </Link>
  );
}

export default BunkerDetailPage;
