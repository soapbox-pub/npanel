import type { FilterOperator } from '@shared/analytics-query';
import { Check, ListFilter, X } from 'lucide-react';
import { useState } from 'react';

import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from '@/components/ui/command';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { useDebounce } from '@/hooks/useDebounce';
import { useSuggestions } from '@/hooks/useAnalytics';
import { suggestionParams } from '@/lib/analytics/query';
import { FILTER_GROUPS, FILTER_LABELS, LABELLED_FILTERS } from '@/lib/analytics/reports';
import {
  type DashboardState,
  type FilterKey,
  apiFilters,
  findFilter,
  withFilter,
  withoutFilter,
} from '@/lib/analytics/state';

const OPERATOR_LABELS: Record<FilterOperator, string> = {
  is: 'is',
  is_not: 'is not',
  contains: 'contains',
  contains_not: 'does not contain',
};

interface FiltersProps {
  hostname: string;
  state: DashboardState;
  onChange(state: DashboardState): void;
}

/** The Filter button, the pills for what's filtered, and the dialog both open. */
export function Filters({ hostname, state, onChange }: FiltersProps) {
  const [editing, setEditing] = useState<FilterKey | null>(null);

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="outline" className="gap-2">
            <ListFilter className="size-4" />
            Filter
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="w-56">
          {FILTER_GROUPS.map((group, index) => (
            <div key={group.title}>
              {index > 0 && <DropdownMenuSeparator />}
              <DropdownMenuLabel className="text-xs text-muted-foreground">{group.title}</DropdownMenuLabel>
              {group.keys.map((key) => (
                <DropdownMenuItem key={key} onSelect={() => setEditing(key)}>
                  {FILTER_LABELS[key]}
                </DropdownMenuItem>
              ))}
            </div>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>

      {editing && (
        <FilterDialog
          key={editing}
          hostname={hostname}
          filterKey={editing}
          state={state}
          onClose={() => setEditing(null)}
          onChange={(next) => {
            setEditing(null);
            onChange(next);
          }}
        />
      )}

      {state.filters.map((filter) => (
        <Badge key={filter.key} variant="secondary" className="h-9 gap-1 rounded-md px-3 text-sm font-normal">
          <button type="button" className="text-left" onClick={() => setEditing(filter.key)}>
            {FILTER_LABELS[filter.key]} {OPERATOR_LABELS[filter.operator]}{' '}
            <span className="font-medium">
              {filter.clauses.map((clause) => state.labels[clause] ?? clause).join(' or ')}
            </span>
          </button>
          <button
            type="button"
            aria-label={`Remove the ${FILTER_LABELS[filter.key]} filter`}
            className="ml-1 rounded-sm opacity-60 hover:opacity-100"
            onClick={() => onChange(withoutFilter(state, filter.key))}
          >
            <X className="size-3.5" />
          </button>
        </Badge>
      ))}
    </>
  );
}

/**
 * One filter, being set: an operator and any number of values, offered from
 * what the site has actually seen.
 */
function FilterDialog({
  hostname,
  filterKey,
  state,
  onChange,
  onClose,
}: FiltersProps & { filterKey: FilterKey; onClose(): void }) {
  const existing = findFilter(state, filterKey);
  const [operator, setOperator] = useState<FilterOperator>(existing?.operator ?? 'is');
  const [clauses, setClauses] = useState<string[]>(existing?.clauses ?? []);
  const [labels, setLabels] = useState<Record<string, string>>({});
  const [search, setSearch] = useState('');
  const query = useDebounce(search, 250);

  const substring = operator === 'contains' || operator === 'contains_not';
  // The other filters narrow the suggestions; this one's own would only hide
  // the values it could still be changed to.
  const others = apiFilters(state.filters.filter((filter) => filter.key !== filterKey));
  const suggestions = useSuggestions(hostname, filterKey, suggestionParams(state, others, query), !substring);

  const toggle = (value: string, label: string) => {
    setClauses((current) => (current.includes(value) ? current.filter((c) => c !== value) : [...current, value]));
    if (LABELLED_FILTERS.includes(filterKey)) setLabels((current) => ({ ...current, [value]: label }));
  };

  const apply = () => {
    const values = substring && search.trim() && !clauses.length ? [search.trim()] : clauses;
    onChange(
      values.length
        ? withFilter(state, { operator, key: filterKey, clauses: values }, labels)
        : withoutFilter(state, filterKey),
    );
  };

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Filter by {FILTER_LABELS[filterKey].toLowerCase()}</DialogTitle>
          <DialogDescription>
            {substring ? 'Type what the value should contain.' : 'Pick one or more values this site has seen.'}
          </DialogDescription>
        </DialogHeader>

        <Select value={operator} onValueChange={(value) => setOperator(value as FilterOperator)}>
          <SelectTrigger className="w-44">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {(Object.keys(OPERATOR_LABELS) as FilterOperator[]).map((op) => (
              <SelectItem key={op} value={op}>
                {OPERATOR_LABELS[op]}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>

        {clauses.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {clauses.map((clause) => (
              <Badge key={clause} variant="secondary" className="gap-1">
                {labels[clause] ?? state.labels[clause] ?? clause}
                <button
                  type="button"
                  aria-label={`Remove ${clause}`}
                  onClick={() => setClauses((current) => current.filter((c) => c !== clause))}
                >
                  <X className="size-3" />
                </button>
              </Badge>
            ))}
          </div>
        )}

        <Command shouldFilter={false} className="rounded-md border">
          <CommandInput
            placeholder={substring ? 'Contains…' : 'Search…'}
            value={search}
            onValueChange={setSearch}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && substring && search.trim()) {
                event.preventDefault();
                toggle(search.trim(), search.trim());
                setSearch('');
              }
            }}
          />
          {!substring && (
            <CommandList>
              <CommandEmpty>{suggestions.isFetching ? 'Loading…' : 'Nothing matches.'}</CommandEmpty>
              <CommandGroup>
                {(suggestions.data ?? []).map((option) => (
                  <CommandItem key={option.value} value={option.value} onSelect={() => toggle(option.value, option.label)}>
                    <Check className={clauses.includes(option.value) ? 'size-4' : 'size-4 opacity-0'} />
                    {option.label}
                  </CommandItem>
                ))}
              </CommandGroup>
            </CommandList>
          )}
        </Command>

        <DialogFooter className="gap-2 sm:justify-between">
          {existing ? (
            <Button variant="ghost" onClick={() => onChange(withoutFilter(state, filterKey))}>
              Remove filter
            </Button>
          ) : (
            <span />
          )}
          <Button onClick={apply} disabled={!clauses.length && !(substring && search.trim())}>
            Apply filter
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
