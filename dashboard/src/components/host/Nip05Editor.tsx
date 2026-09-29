import { useState } from 'react';
import { z } from 'zod';

import { Row, Section } from '@/components/Grouped';
import { PubkeyLabel } from '@/components/PubkeyLabel';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Skeleton } from '@/components/ui/skeleton';
import {
  useDeleteNip05Name,
  useFiles,
  useNip05Names,
  useSetNip05Name,
} from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import { NIP05_PATH, parseNip05Name } from '@shared/nip05';
import { parsePubkey } from '@shared/pubkey';
import { parseRelayUrl } from '@shared/relay-url';

/** What a deployed `nostr.json` has to look like before it is worth importing. */
const documentSchema = z.object({
  names: z.record(z.string(), z.string()),
  relays: z.record(z.string(), z.array(z.string())).optional(),
});

interface Draft {
  name: string;
  pubkey: string;
  relays: string;
}

const EMPTY: Draft = { name: '', pubkey: '', relays: '' };

/**
 * The NIP-05 names this site answers for.
 *
 * npanel serves whatever is here at `/.well-known/nostr.json`, in place of any
 * `nostr.json` the site itself deploys. A request naming no name gets the
 * whole list. Nothing here is admin-only: a name is a setting of the site, so
 * anyone who can manage the site sets it.
 */
export function Nip05Editor({ hostname }: { hostname: string }) {
  const names = useNip05Names(hostname);
  const files = useFiles(hostname);
  const setName = useSetNip05Name();
  const deleteName = useDeleteNip05Name();
  const { toast } = useToast();

  const [draft, setDraft] = useState<Draft>(EMPTY);
  // The name being replaced, which is locked while it is: changing it would be
  // a different identifier, not an edit of this one.
  const [editing, setEditing] = useState<string | undefined>();
  const [importing, setImporting] = useState(false);

  const parsed = parseDraft(draft);

  if (names.isLoading) return <Skeleton className="h-40 w-full rounded-xl" />;

  if (names.isError) {
    return (
      <p className="py-12 text-center text-muted-foreground">
        {names.error instanceof Error ? names.error.message : "Couldn't load the names."}
      </p>
    );
  }

  const entries = names.data ?? [];
  const deployed = files.data?.files.some((file) => file.path === NIP05_PATH) ?? false;

  const reset = () => {
    setDraft(EMPTY);
    setEditing(undefined);
  };

  const save = async () => {
    if ('error' in parsed) return;

    try {
      await setName.mutateAsync({ hostname, ...parsed });
      reset();
      toast({ title: `${parsed.name}@${hostname} saved` });
    } catch (error) {
      toast({
        title: "Couldn't save that name",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  const remove = async (name: string) => {
    try {
      await deleteName.mutateAsync({ hostname, name });
      if (editing === name) reset();
      toast({ title: `${name}@${hostname} removed` });
    } catch (error) {
      toast({
        title: "Couldn't remove that name",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  /**
   * Copy the names out of the site's deployed `nostr.json`.
   *
   * Fetched from the live site rather than from npanel's copy of the blob,
   * which needs the hostname to already resolve here — and which is safe to do
   * only because this is offered when the database is empty, so the file is
   * what comes back rather than an echo of what npanel already serves.
   */
  const importDeployed = async () => {
    setImporting(true);
    try {
      const response = await fetch(`https://${hostname}${NIP05_PATH}`);
      if (!response.ok) {
        throw new Error(`${hostname} answered ${response.status} for ${NIP05_PATH}.`);
      }

      const document = documentSchema.parse(await response.json());
      let imported = 0;
      let skipped = 0;

      for (const [name, pubkey] of Object.entries(document.names)) {
        let entry: { name: string; pubkey: string; relays: string[] };
        try {
          entry = {
            name: parseNip05Name(name),
            pubkey: parsePubkey(pubkey),
            // A relay the file lists but npanel can't parse is dropped rather
            // than failing the name: the name is the part that matters.
            relays: keepValid(document.relays?.[pubkey] ?? []),
          };
        } catch {
          skipped += 1;
          continue;
        }

        await setName.mutateAsync({ hostname, ...entry });
        imported += 1;
      }

      toast({
        title: `Imported ${imported} ${imported === 1 ? 'name' : 'names'}`,
        description: skipped ? `${skipped} could not be read and were left out.` : undefined,
      });
    } catch (error) {
      toast({
        title: "Couldn't import that file",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    } finally {
      setImporting(false);
    }
  };

  return (
    <div className="space-y-8">
      <Section
        footer={
          <>
            Served at <span className="font-mono">{NIP05_PATH}</span>
            {entries.length ? ", in place of any the site deploys." : '. Until a name is added, the site\'s own file is served.'}{' '}
            Use <code className="font-mono">_</code> for <span className="font-mono">{hostname}</span> itself.
          </>
        }
        action={
          !entries.length &&
          deployed && (
            <Button variant="link" disabled={importing} onClick={importDeployed}>
              {importing ? 'Importing…' : 'Import from the site'}
            </Button>
          )
        }
      >
        {entries.length ? (
          entries.map((entry) => (
            <Row
              key={entry.name}
              label={
                <span className="font-mono text-[13px]">
                  {entry.name === '_' ? hostname : `${entry.name}@${hostname}`}
                </span>
              }
              description={
                <span className="flex min-w-0 flex-wrap items-center gap-x-2">
                  <PubkeyLabel pubkey={entry.pubkey} compact />
                  {entry.relays.length > 0 && (
                    <span className="truncate font-mono">
                      {entry.relays.length} {entry.relays.length === 1 ? 'relay' : 'relays'}
                    </span>
                  )}
                </span>
              }
            >
              <div className="flex items-center gap-1">
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => {
                    setDraft({ name: entry.name, pubkey: entry.pubkey, relays: entry.relays.join(', ') });
                    setEditing(entry.name);
                  }}
                >
                  Edit
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  className="text-destructive hover:bg-destructive/10"
                  disabled={deleteName.isPending}
                  onClick={() => remove(entry.name)}
                >
                  Remove
                </Button>
              </div>
            </Row>
          ))
        ) : (
          <p className="px-4 py-8 text-center text-sm text-muted-foreground">No names yet.</p>
        )}
      </Section>

      <Section title={editing ? `Edit ${editing}@${hostname}` : 'Add a name'}>
        <form
          className="grid grid-cols-1 gap-3 p-4 sm:grid-cols-[minmax(0,1fr)_minmax(0,2fr)]"
          onSubmit={(event) => {
            event.preventDefault();
            void save();
          }}
        >
          <div className="space-y-1.5">
            <Label htmlFor="nip05-name">Name</Label>
            <Input
              id="nip05-name"
              value={draft.name}
              disabled={!!editing}
              placeholder="bob"
              onChange={(event) => setDraft({ ...draft, name: event.target.value })}
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="nip05-pubkey">Pubkey</Label>
            <Input
              id="nip05-pubkey"
              value={draft.pubkey}
              placeholder="npub1…"
              onChange={(event) => setDraft({ ...draft, pubkey: event.target.value })}
            />
          </div>
          <div className="space-y-1.5 sm:col-span-2">
            <Label htmlFor="nip05-relays">
              Relays <span className="font-normal text-muted-foreground">(optional)</span>
            </Label>
            <Input
              id="nip05-relays"
              value={draft.relays}
              placeholder="wss://relay.example.com"
              onChange={(event) => setDraft({ ...draft, relays: event.target.value })}
            />
          </div>

          <div className="flex flex-wrap items-center gap-3 sm:col-span-2">
            <Button type="submit" size="sm" disabled={'error' in parsed || setName.isPending}>
              {editing ? 'Save' : 'Add'}
            </Button>
            {editing && (
              <Button type="button" variant="ghost" size="sm" onClick={reset}>
                Cancel
              </Button>
            )}
            {/* Only once something has been typed, so an untouched form isn't
                scolding anyone for not having filled it in yet. */}
            {'error' in parsed && !isBlank(draft) && <span className="text-xs text-destructive">{parsed.error}</span>}
          </div>
        </form>
      </Section>
    </div>
  );
}

/**
 * The draft as the server would read it, or the first complaint it would make.
 *
 * The same parsers the API validates with, so the button goes live exactly when
 * a request would be accepted instead of a round trip later.
 */
function parseDraft(draft: Draft): { name: string; pubkey: string; relays: string[] } | { error: string } {
  try {
    return {
      name: parseNip05Name(draft.name),
      pubkey: parsePubkey(draft.pubkey),
      relays: splitRelays(draft.relays).map(parseRelayUrl),
    };
  } catch (error) {
    return { error: error instanceof Error ? error.message : String(error) };
  }
}

/** Relays as typed: separated by commas, spaces, or both. */
function splitRelays(input: string): string[] {
  return input
    .split(/[\s,]+/)
    .map((value) => value.trim())
    .filter(Boolean);
}

function keepValid(relays: string[]): string[] {
  return relays.flatMap((relay) => {
    try {
      return [parseRelayUrl(relay)];
    } catch {
      return [];
    }
  });
}

function isBlank(draft: Draft): boolean {
  return !draft.name.trim() && !draft.pubkey.trim() && !draft.relays.trim();
}
