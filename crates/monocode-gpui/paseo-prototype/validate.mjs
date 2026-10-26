// The checked-in AOT artifact in this checkout is stale. Use the SAME official
// current Zod schema; do not weaken validation or rebuild Paseo's artifacts.
import {WSOutboundMessageSchema} from 'paseo-source/packages/protocol/src/messages.ts';
export function validateWSOutboundMessage(value) {return WSOutboundMessageSchema.safeParse(value);}
