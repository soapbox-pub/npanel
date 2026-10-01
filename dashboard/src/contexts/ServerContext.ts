import { createContext } from 'react';

import type { Server } from '@/lib/server';

export interface ServerContextType extends Server {
  /** Forget the chosen server and ask again. Only offered when standalone. */
  disconnect: () => void;
}

export const ServerContext = createContext<ServerContextType | undefined>(undefined);
