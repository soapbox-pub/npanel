import { type ReactNode, useState } from 'react';

import { FieldRow } from '@/components/Grouped';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';

/**
 * A field that saves on its own, offering its Save button only once it has
 * been changed. `below` is shown under it while there is a change to save.
 */
export function EditableField({
  id,
  label,
  value,
  busy,
  onSave,
  normalize = (value) => value.trim(),
  below,
  placeholder,
}: {
  id: string;
  label: string;
  value: string;
  busy: boolean;
  onSave(value: string): Promise<boolean>;
  normalize?(value: string): string;
  below?(draft: string): ReactNode;
  placeholder?: string;
}) {
  const [draft, setDraft] = useState(value);
  const changed = normalize(draft) !== value && normalize(draft) !== '';

  return (
    <FieldRow label={label} htmlFor={id}>
      <form
        className="space-y-3"
        onSubmit={async (event) => {
          event.preventDefault();
          if (!(await onSave(normalize(draft)))) setDraft(value);
        }}
      >
        <div className="flex gap-2">
          <Input
            id={id}
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            placeholder={placeholder}
            autoComplete="off"
            spellCheck={false}
          />
          {changed && (
            <Button type="submit" disabled={busy}>
              Save
            </Button>
          )}
        </div>
        {changed && below?.(normalize(draft))}
      </form>
    </FieldRow>
  );
}
