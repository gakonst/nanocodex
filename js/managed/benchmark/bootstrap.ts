// Establish the isolated service name before binding the broker back to it.
import {WorkerEntrypoint} from 'cloudflare:workers';
export class ManagedAgentOwnership extends WorkerEntrypoint {}
export default {fetch:()=>new Response('Benchmark deployment initializing',{status:503})};
