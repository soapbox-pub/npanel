import { Plus } from 'lucide-react';
import { useState } from 'react';
import { useNavigate } from 'react-router-dom';

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
import { useCreateBunker } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';

/**
 * Add a bunker.
 *
 * Only the name is asked for here. Connecting it to the signer that holds the
 * key is the interesting part and happens on its own page, where there is room
 * for a QR code and something to say about what to approve.
 */
export function AddBunkerDialog() {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState('');

  const createBunker = useCreateBunker();
  const navigate = useNavigate();
  const { toast } = useToast();

  const submit = async () => {
    try {
      const { bunker } = await createBunker.mutateAsync({ name: name.trim() });
      setOpen(false);
      setName('');
      navigate(`/bunkers/${bunker.slug}`);
    } catch (error) {
      toast({
        title: "Couldn't add that bunker",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button>
          <Plus className="mr-2 h-4 w-4" />
          Add bunker
        </Button>
      </DialogTrigger>

      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add a bunker</DialogTitle>
          <DialogDescription>
            An identity npanel can sign for. The key stays in the signer app that holds it — npanel asks that app
            to sign, and never sees it.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-1.5">
          <Label htmlFor="bunker-name">Name</Label>
          <Input
            id="bunker-name"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="Press team"
            onKeyDown={(event) => {
              if (event.key === 'Enter' && name.trim()) void submit();
            }}
          />
          <p className="text-xs text-muted-foreground">
            Whose identity this is. You'll connect it to a signer next.
          </p>
        </div>

        <DialogFooter>
          <Button onClick={submit} disabled={!name.trim() || createBunker.isPending}>
            {createBunker.isPending ? 'Adding…' : 'Add bunker'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
