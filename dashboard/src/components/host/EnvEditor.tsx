import { type ReactNode, useState } from 'react';

import { Section } from '@/components/Grouped';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Skeleton } from '@/components/ui/skeleton';
import { useDeleteEnvVariable, useEnvVariables, useSetEnvVariable } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import { ENV_PATH, MAX_VARIABLES, parseEnvName, parseEnvValue } from '@shared/env';

interface Draft {
  name: string;
  value: string;
}

const EMPTY: Draft = { name: '', value: '' };

/**
 * The variables this site's pages read as `window.ENV`, laid out the way
 * Heroku lays out config vars: a name and a value to a row, each editable in
 * place, and an empty row at the foot for the next one.
 *
 * Nothing here is admin-only: a variable is a setting of the site, so anyone
 * who can manage the site sets it.
 */
export function EnvEditor({ hostname }: { hostname: string }) {
  const variables = useEnvVariables(hostname);
  const setVariable = useSetEnvVariable();
  const deleteVariable = useDeleteEnvVariable();
  const { toast } = useToast();

  const [draft, setDraft] = useState<Draft>(EMPTY);
  // The variable whose value is being changed. Its name is locked while it
  // is: a new name is a different variable, not an edit of this one.
  const [editing, setEditing] = useState<Draft | undefined>();

  if (variables.isLoading) return <Skeleton className="h-40 w-full rounded-xl" />;

  if (variables.isError) {
    return (
      <p className="py-12 text-center text-muted-foreground">
        {variables.error instanceof Error ? variables.error.message : "Couldn't load the variables."}
      </p>
    );
  }

  const entries = variables.data ?? [];
  const full = entries.length >= MAX_VARIABLES;

  const parsed = parseDraft(draft);
  // The add row would quietly replace a variable that already exists, so it
  // points at the row that does instead.
  const taken = !('error' in parsed) && entries.some((entry) => entry.name === parsed.name);
  const problem = 'error' in parsed ? parsed.error : taken ? `${parsed.name} is already set. Edit it above.` : undefined;

  const editingValue = editing ? parseDraft(editing) : undefined;

  const save = async (variable: Draft, done: () => void) => {
    try {
      await setVariable.mutateAsync({ hostname, ...variable });
      done();
      toast({ title: `${variable.name} saved` });
    } catch (error) {
      toast({
        title: "Couldn't save that variable",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  const remove = async (name: string) => {
    try {
      await deleteVariable.mutateAsync({ hostname, name });
      if (editing?.name === name) setEditing(undefined);
      toast({ title: `${name} removed` });
    } catch (error) {
      toast({
        title: "Couldn't remove that variable",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <div className="space-y-8">
      <Section
        title="Variables"
        footer={
          <>
            Every page loads these as <code className="font-mono">window.ENV</code> from{' '}
            <span className="font-mono">{ENV_PATH}</span>, before any of the site's own scripts run — so a site
            built to read <code className="font-mono">window.ENV.API_URL</code> can be pointed somewhere else without a
            new deploy. They are public: every visitor can read them, so keep secrets out.
          </>
        }
      >
        {entries.map((entry) =>
          editing?.name === entry.name ? (
            <VariableRow key={entry.name}>
              <form
                className="contents"
                onSubmit={(event) => {
                  event.preventDefault();
                  if (editingValue && !('error' in editingValue)) {
                    void save(editingValue, () => setEditing(undefined));
                  }
                }}
              >
                <Input value={entry.name} readOnly disabled className="font-mono" aria-label="Name" />
                <Input
                  autoFocus
                  value={editing.value}
                  className="font-mono"
                  aria-label={`Value of ${entry.name}`}
                  onChange={(event) => setEditing({ name: entry.name, value: event.target.value })}
                  onKeyDown={(event) => {
                    if (event.key === 'Escape') setEditing(undefined);
                  }}
                />
                <div className="flex items-center gap-1">
                  <Button
                    type="submit"
                    size="sm"
                    disabled={!editingValue || 'error' in editingValue || setVariable.isPending}
                  >
                    Save
                  </Button>
                  <Button type="button" variant="ghost" size="sm" onClick={() => setEditing(undefined)}>
                    Cancel
                  </Button>
                </div>
                {editingValue && 'error' in editingValue && (
                  <span className="text-xs text-destructive sm:col-span-3">{editingValue.error}</span>
                )}
              </form>
            </VariableRow>
          ) : (
            <VariableRow key={entry.name}>
              <Input value={entry.name} readOnly className="font-mono" aria-label="Name" />
              <Input value={entry.value} readOnly className="font-mono" aria-label={`Value of ${entry.name}`} />
              <div className="flex items-center gap-1">
                <Button variant="ghost" size="sm" onClick={() => setEditing({ name: entry.name, value: entry.value })}>
                  Edit
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  className="text-destructive hover:bg-destructive/10"
                  disabled={deleteVariable.isPending}
                  onClick={() => remove(entry.name)}
                >
                  Remove
                </Button>
              </div>
            </VariableRow>
          ),
        )}

        <VariableRow>
          {full ? (
            <p className="py-1 text-sm text-muted-foreground sm:col-span-3">
              A site can have at most {MAX_VARIABLES} variables. Remove one to add another.
            </p>
          ) : (
            <form
              className="contents"
              onSubmit={(event) => {
                event.preventDefault();
                if (!('error' in parsed) && !taken) void save(parsed, () => setDraft(EMPTY));
              }}
            >
              <Input
                value={draft.name}
                placeholder="API_URL"
                className="font-mono"
                aria-label="New variable's name"
                autoCapitalize="characters"
                autoComplete="off"
                spellCheck={false}
                onChange={(event) => setDraft({ ...draft, name: event.target.value })}
              />
              <Input
                value={draft.value}
                placeholder="https://api.example.com"
                className="font-mono"
                aria-label="New variable's value"
                autoComplete="off"
                spellCheck={false}
                onChange={(event) => setDraft({ ...draft, value: event.target.value })}
              />
              <Button type="submit" size="sm" disabled={!!problem || setVariable.isPending}>
                Add
              </Button>
              {/* Only once something has been typed, so an untouched row isn't
                  scolding anyone for not having filled it in yet. */}
              {problem && (!!draft.name.trim() || !!draft.value) && (
                <span className="text-xs text-destructive sm:col-span-3">{problem}</span>
              )}
            </form>
          )}
        </VariableRow>
      </Section>
    </div>
  );
}

/** One row of the grid: a name, a value, and what can be done with them. */
function VariableRow({ children }: { children: ReactNode }) {
  return (
    <div className="pl-4">
      <div
        data-row
        className="grid grid-cols-1 items-center gap-2 border-t py-2.5 pr-4 sm:grid-cols-[minmax(0,2fr)_minmax(0,3fr)_auto]"
      >
        {children}
      </div>
    </div>
  );
}

/**
 * The draft as the server would read it, or the first complaint it would make —
 * the same rules the API validates with, so the button goes live exactly when a
 * request would be accepted.
 */
function parseDraft(draft: Draft): Draft | { error: string } {
  try {
    return { name: parseEnvName(draft.name), value: parseEnvValue(draft.value) };
  } catch (error) {
    return { error: error instanceof Error ? error.message : String(error) };
  }
}
