import { ChevronRight } from 'lucide-react';
import type { ReactNode } from 'react';
import { Link } from 'react-router-dom';

import { Switch } from '@/components/ui/switch';
import { type TileColor, TILE_COLORS } from '@/lib/tile-colors';
import { cn } from '@/lib/utils';

/**
 * A grouped inset list, as System Settings and iOS Settings draw one: a
 * heading above, a rounded panel of rows split by inset hairlines, and a
 * footnote below.
 */
export function Section({
  title,
  footer,
  action,
  children,
  className,
}: {
  title?: ReactNode;
  footer?: ReactNode;
  /** Something to the right of the heading, like an Add button. */
  action?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={cn('space-y-2', className)}>
      {(title || action) && (
        <div className="flex min-h-7 items-end justify-between gap-3 px-1">
          {title && <h2 className="text-[13px] font-semibold text-foreground/80">{title}</h2>}
          {action}
        </div>
      )}
      <div className="overflow-hidden rounded-xl bg-card surface [&>*:first-child>[data-row]]:border-t-0">
        {children}
      </div>
      {footer && <div className="px-1 text-xs leading-relaxed text-muted-foreground">{footer}</div>}
    </section>
  );
}

/**
 * One row of a {@link Section}: a label and its note on the left, whatever
 * controls it on the right. The hairline above starts where the text does.
 */
export function Row({
  label,
  description,
  children,
  htmlFor,
  className,
}: {
  label: ReactNode;
  description?: ReactNode;
  children?: ReactNode;
  /** The control the label names, so clicking the label reaches it. */
  htmlFor?: string;
  className?: string;
}) {
  const Label = htmlFor ? 'label' : 'div';

  return (
    <div className="pl-4">
      <div
        data-row
        className={cn(
          'flex min-h-12 min-w-0 flex-1 flex-wrap items-center justify-between gap-x-4 gap-y-2 border-t py-2.5 pr-4',
          className,
        )}
      >
        <Label htmlFor={htmlFor} className="min-w-0 flex-1 basis-48">
          <span className="block text-sm">{label}</span>
          {description && <span className="mt-0.5 block text-xs text-muted-foreground">{description}</span>}
        </Label>
        {children}
      </div>
    </div>
  );
}

/** A row that is one switch. */
export function ToggleRow({
  id,
  checked,
  onCheckedChange,
  disabled,
  ...row
}: {
  id: string;
  label: ReactNode;
  description?: ReactNode;
  checked: boolean;
  disabled?: boolean;
  onCheckedChange(checked: boolean): void;
}) {
  return (
    <Row {...row} htmlFor={id}>
      <Switch id={id} checked={checked} disabled={disabled} onCheckedChange={onCheckedChange} />
    </Row>
  );
}

/** A row that is a full-width field, its label above it. */
export function FieldRow({ label, htmlFor, children }: { label: ReactNode; htmlFor: string; children: ReactNode }) {
  return (
    <div className="pl-4">
      <div data-row className="space-y-1.5 border-t py-3 pr-4">
        <label htmlFor={htmlFor} className="block text-sm">
          {label}
        </label>
        {children}
      </div>
    </div>
  );
}

/** The little coloured tile a section of settings is marked with, as Settings draws one. */
export function RowIcon({ color, children, className }: { color: TileColor; children: ReactNode; className?: string }) {
  return (
    <span
      aria-hidden
      className={cn(
        'flex size-7 shrink-0 items-center justify-center rounded-[7px] text-white [&_svg]:size-4',
        TILE_COLORS[color],
        className,
      )}
    >
      {children}
    </span>
  );
}

/**
 * A row that opens a page of its own: its section's icon, its name, a word or
 * two of what's inside, and the chevron that says it goes somewhere.
 */
export function LinkRow({
  to,
  icon,
  label,
  value,
}: {
  to: string;
  icon: ReactNode;
  label: ReactNode;
  value?: ReactNode;
}) {
  return (
    <Link
      to={to}
      className="group flex items-center gap-3 pl-4 transition-colors hover:bg-foreground/[0.03] focus-visible:bg-foreground/[0.05] focus-visible:outline-hidden active:bg-foreground/[0.06]"
    >
      {icon}
      <span data-row className="flex min-h-12 min-w-0 flex-1 items-center gap-3 border-t py-2.5 pr-3">
        <span className="min-w-0 flex-1 truncate text-sm">{label}</span>
        {value !== undefined && value !== null && value !== '' && (
          <span className="min-w-0 max-w-[50%] truncate text-sm text-muted-foreground">{value}</span>
        )}
        <ChevronRight className="size-4 shrink-0 text-muted-foreground/60" aria-hidden />
      </span>
    </Link>
  );
}
