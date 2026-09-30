import { useSeoMeta } from '@unhead/react';
import {
  AtSign,
  Braces,
  History,
  KeyRound,
  type LucideIcon,
  Plus,
  Settings,
  SlidersHorizontal,
  Trash2,
  UserCog,
  UserMinus,
  UserPlus,
} from 'lucide-react';
import { Fragment } from 'react';
import { Link } from 'react-router-dom';

import { Section } from '@/components/Grouped';
import { PageHeader } from '@/components/PageHeader';
import { PubkeyLabel } from '@/components/PubkeyLabel';
import { SiteIcon } from '@/components/SiteIcon';
import { Skeleton } from '@/components/ui/skeleton';
import { useAuditLog, useHosts } from '@/hooks/useNpanel';
import { type ActivityKind, type Detail, type Part, describe } from '@/lib/activity';
import type { AuditEntry } from '@/lib/api-types';
import { fullDate } from '@/lib/format';
import { cn } from '@/lib/utils';

/** A glyph and a tint per kind of change, so the eye can scan for what it wants. */
const KINDS: Record<ActivityKind, { icon: LucideIcon; tint: string }> = {
  'site-added': { icon: Plus, tint: 'bg-success/15 text-success' },
  'site-changed': { icon: SlidersHorizontal, tint: 'bg-primary/12 text-primary' },
  'site-removed': { icon: Trash2, tint: 'bg-destructive/12 text-destructive' },
  access: { icon: KeyRound, tint: 'bg-[#bf5af2]/15 text-[#a340d9] dark:text-[#da8fff]' },
  'person-added': { icon: UserPlus, tint: 'bg-success/15 text-success' },
  'person-changed': { icon: UserCog, tint: 'bg-warning/15 text-warning' },
  'person-removed': { icon: UserMinus, tint: 'bg-destructive/12 text-destructive' },
  'name-set': { icon: AtSign, tint: 'bg-[#30b0c7]/15 text-[#1f8fa3] dark:text-[#6ac4dc]' },
  'name-removed': { icon: AtSign, tint: 'bg-destructive/12 text-destructive' },
  'env-set': { icon: Braces, tint: 'bg-[#ff375f]/15 text-[#e0244b] dark:text-[#ff7a93]' },
  'env-removed': { icon: Braces, tint: 'bg-destructive/12 text-destructive' },
  settings: { icon: Settings, tint: 'bg-foreground/[0.07] text-muted-foreground' },
  other: { icon: History, tint: 'bg-foreground/[0.07] text-muted-foreground' },
};

/** Every configuration change and who made it, most recent first, a day at a time. Admin only. */
export function AuditLogPage() {
  useSeoMeta({ title: 'Activity — npanel' });

  const entries = useAuditLog();
  const hosts = useHosts();
  const icons = new Map((hosts.data?.hosts ?? []).map((host) => [host.hostname, host.preview.icon]));

  return (
    <div className="space-y-8">
      <PageHeader title="Activity" />

      {entries.isLoading ? (
        <Skeleton className="h-64 w-full rounded-xl" />
      ) : !entries.data?.length ? (
        <p className="py-16 text-center text-muted-foreground">Nothing has changed yet.</p>
      ) : (
        byDay(entries.data).map(([day, group]) => (
          <Section key={day} title={day}>
            {group.map((entry) => (
              <Entry key={entry.id} entry={entry} icons={icons} />
            ))}
          </Section>
        ))
      )}
    </div>
  );
}

function Entry({ entry, icons }: { entry: AuditEntry; icons: Map<string, string | null> }) {
  const activity = describe(entry);
  const { icon: Icon, tint } = KINDS[activity.kind];
  const date = new Date(entry.created_at * 1000);

  return (
    <div className="flex gap-3 pl-4">
      <span className={cn('mt-3 flex size-7 shrink-0 items-center justify-center rounded-full', tint)} aria-hidden>
        <Icon className="size-3.5" strokeWidth={2.5} />
      </span>

      <div data-row className="flex min-w-0 flex-1 gap-3 border-t py-3 pr-4">
        <div className="min-w-0 flex-1 space-y-2">
          <p className="text-sm leading-6">
            <PubkeyLabel pubkey={entry.actor_pubkey} className="align-middle font-semibold" />{' '}
            {activity.sentence.map((part, index) => (
              // Parts are positional: the sentence is rebuilt whole every render.
              <Fragment key={index}>
                <SentencePart part={part} icons={icons} />{' '}
              </Fragment>
            ))}
          </p>

          {activity.details.length > 0 && (
            <div className="flex flex-wrap gap-1.5">
              {activity.details.map((detail, index) => (
                // Details are positional too, and never reorder.
                <DetailChip key={index} detail={detail} />
              ))}
            </div>
          )}
        </div>

        <time
          dateTime={date.toISOString()}
          title={fullDate(entry.created_at)}
          className="shrink-0 pt-0.5 text-xs text-muted-foreground tabular-nums"
        >
          {date.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })}
        </time>
      </div>
    </div>
  );
}

function SentencePart({ part, icons }: { part: Part; icons: Map<string, string | null> }) {
  if (typeof part === 'string') return <span className="text-muted-foreground">{part}</span>;
  if ('strong' in part) return <span className="font-medium">{part.strong}</span>;
  if ('person' in part) return <PubkeyLabel pubkey={part.person} className="align-middle font-medium" />;

  const icon = <SiteIcon hostname={part.site} src={icons.get(part.site)} className="size-5" />;

  // A site that no longer exists has nowhere to link to.
  return icons.has(part.site) ? (
    <Link
      to={`/hosts/${part.site}`}
      className="inline-flex items-center gap-1.5 align-middle font-medium hover:text-primary"
    >
      {icon}
      {part.site}
    </Link>
  ) : (
    <span className="inline-flex items-center gap-1.5 align-middle font-medium">
      {icon}
      {part.site}
    </span>
  );
}

/** One particular of a change, as a small capsule under the sentence. */
function DetailChip({ detail }: { detail: Detail }) {
  const chip = 'inline-flex max-w-full items-center gap-2 rounded-lg bg-muted px-2.5 py-1 text-xs';
  const label = 'label' in detail && detail.label && <span className="text-muted-foreground">{detail.label}</span>;

  switch (detail.type) {
    case 'toggle':
      return (
        <span className={chip}>
          {label}
          <MiniSwitch on={detail.on} />
        </span>
      );

    case 'change':
      return (
        <span className={cn(chip, 'min-w-0')}>
          {label}
          <span className={cn('truncate text-muted-foreground line-through', detail.mono && 'font-mono')}>
            {detail.from || 'Nothing'}
          </span>
          <span aria-hidden className="text-muted-foreground">
            →
          </span>
          <span className={cn('truncate font-medium', detail.mono && 'font-mono')}>{detail.to}</span>
        </span>
      );

    case 'value':
      return (
        <span className={cn(chip, 'min-w-0')}>
          {label}
          <span className={cn('truncate font-medium', detail.mono && 'font-mono')}>{detail.value}</span>
        </span>
      );

    case 'list':
      return (
        <span className={cn(chip, 'flex-col items-start gap-0.5 font-mono')}>
          {label}
          {detail.added.map((value) => (
            <span key={`+${value}`} className="text-success">
              + {value}
            </span>
          ))}
          {detail.removed.map((value) => (
            <span key={`-${value}`} className="text-destructive line-through">
              − {value}
            </span>
          ))}
        </span>
      );

    case 'people':
      return (
        <span className={cn(chip, 'flex-col items-start gap-1')}>
          {detail.added.map((pubkey) => (
            <span key={`+${pubkey}`} className="flex items-center gap-1.5">
              <span className="font-semibold text-success">+</span>
              <PubkeyLabel pubkey={pubkey} />
            </span>
          ))}
          {detail.removed.map((pubkey) => (
            <span key={`-${pubkey}`} className="flex items-center gap-1.5 text-muted-foreground">
              <span className="font-semibold text-destructive">−</span>
              <PubkeyLabel pubkey={pubkey} />
            </span>
          ))}
        </span>
      );

    case 'person':
      return (
        <span className={chip}>
          {label}
          <PubkeyLabel pubkey={detail.pubkey} className="font-medium" />
        </span>
      );
  }
}

/** A switch the size of a word, showing the position it was left in. */
function MiniSwitch({ on }: { on: boolean }) {
  return (
    <span className="inline-flex items-center gap-1.5 font-medium">
      <span
        aria-hidden
        className={cn(
          'flex h-3.5 w-6 items-center rounded-full p-px transition-colors',
          on ? 'justify-end bg-toggle' : 'justify-start bg-foreground/20',
        )}
      >
        <span className="size-3 rounded-full bg-white shadow-sm" />
      </span>
      {on ? 'On' : 'Off'}
    </span>
  );
}

/** Entries under the day they happened: Today, Yesterday, then dates. */
function byDay(entries: AuditEntry[]): [string, AuditEntry[]][] {
  const groups = new Map<string, AuditEntry[]>();
  for (const entry of entries) {
    const day = dayLabel(new Date(entry.created_at * 1000));
    groups.set(day, [...(groups.get(day) ?? []), entry]);
  }
  return [...groups];
}

function dayLabel(date: Date): string {
  const today = new Date();
  const start = (value: Date) => new Date(value.getFullYear(), value.getMonth(), value.getDate()).getTime();
  const days = Math.round((start(today) - start(date)) / 86_400_000);

  if (days === 0) return 'Today';
  if (days === 1) return 'Yesterday';
  if (days < 7) return date.toLocaleDateString(undefined, { weekday: 'long' });
  return date.toLocaleDateString(undefined, {
    month: 'long',
    day: 'numeric',
    year: date.getFullYear() === today.getFullYear() ? undefined : 'numeric',
  });
}

export default AuditLogPage;
