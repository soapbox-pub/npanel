import { Checkbox } from '@/components/ui/checkbox';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { ENCRYPTION_METHODS, type PermissionDraft, methodLabel, parseKinds } from '@/lib/permissions';

interface PermissionPickerProps {
  value: PermissionDraft;
  onChange: (draft: PermissionDraft) => void;
  /** Distinguishes the inputs when two of these are on a page. */
  idPrefix: string;
}

/**
 * What a connection is allowed to do.
 *
 * Everything here is refused unless ticked, and there is no prompt behind it:
 * npanel is a server, so a request outside this list is turned down there and
 * then rather than waiting for someone to approve it.
 */
export function PermissionPicker({ value, onChange, idPrefix }: PermissionPickerProps) {
  const toggleMethod = (method: string) => {
    onChange({
      ...value,
      methods: value.methods.includes(method)
        ? value.methods.filter((item) => item !== method)
        : [...value.methods, method],
    });
  };

  return (
    <div className="space-y-4">
      <div className="space-y-3 rounded-md border p-3">
        <div className="flex items-center gap-3">
          <Checkbox
            id={`${idPrefix}-sign`}
            checked={value.sign}
            onCheckedChange={(checked) => onChange({ ...value, sign: checked === true })}
          />
          <Label htmlFor={`${idPrefix}-sign`} className="cursor-pointer">
            Sign events
          </Label>
        </div>

        {value.sign && (
          <div className="space-y-3 pl-7">
            <div className="flex items-center gap-3">
              <Checkbox
                id={`${idPrefix}-any-kind`}
                checked={value.anyKind}
                onCheckedChange={(checked) => onChange({ ...value, anyKind: checked === true })}
              />
              <Label htmlFor={`${idPrefix}-any-kind`} className="cursor-pointer text-sm font-normal">
                Any kind of event
              </Label>
            </div>

            {!value.anyKind && (
              <div className="space-y-1.5">
                <Label htmlFor={`${idPrefix}-kinds`} className="text-xs text-muted-foreground">
                  Kinds, separated by commas
                </Label>
                <Input
                  id={`${idPrefix}-kinds`}
                  defaultValue={value.kinds.join(', ')}
                  onChange={(event) => onChange({ ...value, kinds: parseKinds(event.target.value) })}
                  placeholder="1, 30023"
                  className="font-mono text-sm"
                />
                <p className="text-xs text-muted-foreground">
                  {value.kinds.length
                    ? `Allows ${value.kinds.length} kind${value.kinds.length === 1 ? '' : 's'}.`
                    : 'No kinds listed, so nothing can be signed.'}
                </p>
              </div>
            )}
          </div>
        )}
      </div>

      <div className="space-y-2 rounded-md border p-3">
        <p className="text-sm">Encrypt and decrypt</p>
        <div className="grid gap-2 sm:grid-cols-2">
          {ENCRYPTION_METHODS.map((method) => (
            <div key={method} className="flex items-center gap-3">
              <Checkbox
                id={`${idPrefix}-${method}`}
                checked={value.methods.includes(method)}
                onCheckedChange={() => toggleMethod(method)}
              />
              <Label htmlFor={`${idPrefix}-${method}`} className="cursor-pointer text-sm font-normal">
                {methodLabel(method)}
              </Label>
            </div>
          ))}
        </div>
      </div>

      <p className="text-xs text-muted-foreground">
        Reading the public key, pinging, and ending the session need no permission — every connection can do
        those.
      </p>
    </div>
  );
}
