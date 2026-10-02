import { Plus } from 'lucide-react';
import { lazy, Suspense, useState } from 'react';

import type { AddHostDraft } from '@/components/AddHostForm';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog';
import { Skeleton } from '@/components/ui/skeleton';

// The form checks domains against the Public Suffix List, the largest thing
// on the page it sits on, so it is fetched only when someone goes to add a site.
const AddHostForm = lazy(() => import('@/components/AddHostForm'));

const EMPTY_DRAFT: AddHostDraft = { hostname: '', address: '', hidden: false, redirectWww: true };

/** Map a domain to an nsite. */
export function AddHostDialog() {
  const [open, setOpen] = useState(false);
  // Held here rather than in the form, so a draft survives the dialog closing.
  const [draft, setDraft] = useState<AddHostDraft>(EMPTY_DRAFT);

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button className="rounded-full">
          <Plus aria-hidden />
          Add Site
        </Button>
      </DialogTrigger>

      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add Site</DialogTitle>
          <DialogDescription>Serve an nsite from a domain you control.</DialogDescription>
        </DialogHeader>

        <Suspense
          fallback={
            <div className="space-y-5">
              <Skeleton className="h-16" />
              <Skeleton className="h-16" />
              <Skeleton className="h-6" />
            </div>
          }
        >
          <AddHostForm
            draft={draft}
            onChange={setDraft}
            onAdded={() => {
              setOpen(false);
              setDraft(EMPTY_DRAFT);
            }}
            onCancel={() => setOpen(false)}
          />
        </Suspense>
      </DialogContent>
    </Dialog>
  );
}
