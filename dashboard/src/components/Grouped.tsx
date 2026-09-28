import type { ReactNode } from 'react';

import { Switch } from '@/components/ui/switch';
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
      <div className="overflow-hidden rounded-xl bg-card shadow-[0_0_0_0.5px_rgb(0_0_0/0.05),0_1px_2px_rgb(0_0_0/0.04)] dark:shadow-[inset_0_0_0_0.5px_rgb(255_255_255/0.08)] [&>*:first-child>[data-row]]:border-t-0">
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
  icon,
  children,
  htmlFor,
  className,
}: {
  label: ReactNode;
  description?: ReactNode;
  /** A {@link RowIcon}, drawn in front of the label as Settings does. */
  icon?: ReactNode;
  children?: ReactNode;
  /** The control the label names, so clicking the label reaches it. */
  htmlFor?: string;
  className?: string;
}) {
  const Label = htmlFor ? 'label' : 'div';

  return (
    <div className="flex items-center gap-3 pl-4">
      {icon}
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
  icon?: ReactNode;
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

const ICON_COLORS = {
  blue: 'bg-[#0a84ff]',
  green: 'bg-[#30d158]',
  orange: 'bg-[#ff9f0a]',
  red: 'bg-[#ff453a]',
  purple: 'bg-[#bf5af2]',
  indigo: 'bg-[#5e5ce6]',
  gray: 'bg-[#8e8e93]',
  teal: 'bg-[#40c8e0]',
} as const;

/** The little coloured tile in front of a Settings row. */
export function RowIcon({ color, children }: { color: keyof typeof ICON_COLORS; children: ReactNode }) {
  return (
    <span
      aria-hidden
      className={cn(
        'flex size-7 shrink-0 items-center justify-center rounded-[7px] text-white [&_svg]:size-4',
        ICON_COLORS[color],
      )}
    >
      {children}
    </span>
  );
}
