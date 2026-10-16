import { useSeoMeta } from '@unhead/react';
import { Plus, Signpost } from 'lucide-react';
import { useState } from 'react';

import { DnsRecords } from '@/components/DnsRecords';
import { LinkRow, RowIcon, Section } from '@/components/Grouped';
import { PageHeader } from '@/components/PageHeader';
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
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import { useCreateRedirect, useRedirects } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type { Redirect } from '@/lib/api-types';
import { redirectSummary } from '@/lib/redirect-summary';

/**
 * Hostnames that send every visit to another address: a row each, which
 * opens the redirect's own page. Admin only.
 */
export function RedirectsPage() {
  useSeoMeta({ title: 'Redirects — npanel' });

  const redirects = useRedirects();
  const list = redirects.data ?? [];

  return (
    <div className="space-y-8">
      <PageHeader title="Redirects" actions={list.length ? <AddRedirectDialog /> : undefined} />

      {redirects.isLoading ? (
        <Skeleton className="h-48 w-full rounded-xl" />
      ) : redirects.isError ? (
        <p className="rounded-xl bg-destructive/10 p-4 text-sm text-destructive">
          {redirects.error instanceof Error ? redirects.error.message : "Couldn't load redirects. Reload the page to try again."}
        </p>
      ) : list.length ? (
        <Section>
          {list.map((redirect) => (
            <RedirectRow key={redirect.hostname} redirect={redirect} />
          ))}
        </Section>
      ) : (
        <Empty />
      )}
    </div>
  );
}

function RedirectRow({ redirect }: { redirect: Redirect }) {
  return (
    <LinkRow
      to={`/redirects/${encodeURIComponent(redirect.hostname)}`}
      icon={
        <RowIcon color={redirect.enabled ? 'indigo' : 'gray'}>
          <Signpost />
        </RowIcon>
      }
      label={redirect.hostname}
      value={redirect.enabled ? redirectSummary(redirect) : 'Off'}
    />
  );
}

function Empty() {
  return (
    <div className="flex flex-col items-center gap-4 rounded-2xl border-2 border-dashed px-8 py-16 text-center">
      <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary">
        <Signpost className="size-7" aria-hidden />
      </span>
      <div className="space-y-1">
        <p className="text-lg font-semibold">No redirects yet</p>
        <p className="max-w-sm text-sm text-muted-foreground">
          Send everyone who visits a domain to another address: an old name, a misspelling, or a site that moved.
        </p>
      </div>
      <AddRedirectDialog />
    </div>
  );
}

interface Draft {
  hostname: string;
  target: string;
  keepPath: boolean;
  permanent: boolean;
}

const EMPTY_DRAFT: Draft = { hostname: '', target: '', keepPath: true, permanent: true };

function AddRedirectDialog() {
  const [open, setOpen] = useState(false);
  // Held outside the form's own lifetime, so a draft survives the dialog closing.
  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);
  const set = (change: Partial<Draft>) => setDraft({ ...draft, ...change });
  const createRedirect = useCreateRedirect();
  const { toast } = useToast();

  const domain = draft.hostname.trim().toLowerCase();

  const submit = async () => {
    try {
      await createRedirect.mutateAsync({
        hostname: draft.hostname,
        target: draft.target,
        keep_path: draft.keepPath,
        permanent: draft.permanent,
      });
      toast({ title: `${domain} added` });
      setOpen(false);
      setDraft(EMPTY_DRAFT);
    } catch (error) {
      toast({
        title: "Couldn't add that redirect",
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
          Add Redirect
        </Button>
      </DialogTrigger>

      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add Redirect</DialogTitle>
          <DialogDescription>Send every visit to a domain you control to another address.</DialogDescription>
        </DialogHeader>

        <form
          className="space-y-5"
          onSubmit={(event) => {
            event.preventDefault();
            void submit();
          }}
        >
          <div className="space-y-2">
            <Label htmlFor="redirect-hostname">From</Label>
            <Input
              id="redirect-hostname"
              value={draft.hostname}
              onChange={(event) => set({ hostname: event.target.value })}
              placeholder="old.example.com"
              autoComplete="off"
              spellCheck={false}
            />
            <DnsRecords hostname={draft.hostname} />
          </div>

          <div className="space-y-2">
            <Label htmlFor="redirect-target">To</Label>
            <Input
              id="redirect-target"
              value={draft.target}
              onChange={(event) => set({ target: event.target.value })}
              placeholder="https://example.com"
              autoComplete="off"
              spellCheck={false}
              inputMode="url"
            />
          </div>

          <div className="space-y-3">
            <label htmlFor="redirect-keep-path" className="flex items-center justify-between gap-4 text-sm">
              <span>Keep the path, so /about goes to /about there</span>
              <Switch
                id="redirect-keep-path"
                checked={draft.keepPath}
                onCheckedChange={(keepPath) => set({ keepPath })}
              />
            </label>
            <label htmlFor="redirect-permanent" className="flex items-center justify-between gap-4 text-sm">
              <span>Permanent, so search engines move to the new address</span>
              <Switch
                id="redirect-permanent"
                checked={draft.permanent}
                onCheckedChange={(permanent) => set({ permanent })}
              />
            </label>
          </div>

          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={!domain || !draft.target.trim() || createRedirect.isPending}>
              {createRedirect.isPending ? 'Adding…' : 'Add Redirect'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export default RedirectsPage;
