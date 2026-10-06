import type { ToolContext } from 'nanocodex';
import { NATIVE_FORM_STATE, selectBrowserVaultInput, type BrowserVaultIdentity, type PrivateBrowserCdp } from './browser-vault';
import { privateVaultTakeover, type BrowserVaultTouchState, type BrowserVaultTakeoverResult } from './browser-vault-takeover';

export const BROWSER_VAULT_FIELDS = {
  login: ['username', 'password'], api_key: ['api_key'],
  card: ['card_number', 'expiry_month', 'expiry_year', 'card_expiry', 'cvv', 'billing_zip'],
  address: ['address_line_1', 'address_line_2', 'city', 'state', 'zip', 'country'], phone: ['phone_number'],
} as const;
export type BrowserVaultField = typeof BROWSER_VAULT_FIELDS[keyof typeof BROWSER_VAULT_FIELDS][number];
export type VaultFieldMapping = {ref: string; vault_id: string; field: BrowserVaultField};
export type VaultFieldResolution = {kind: keyof typeof BROWSER_VAULT_FIELDS; values: Record<string,string>};
/** Host-only resolver. The caller authenticates account ownership before any RPC. */
export type BrowserVaultFieldsResolver = (request: {vault_id: string; expected_origin: string; fields: BrowserVaultField[]}, context?: ToolContext) => Promise<VaultFieldResolution>;
export function parseVaultFieldMappings(value: unknown, native = false): VaultFieldMapping[] {
  if (!Array.isArray(value) || !value.length || value.length > 32) throw new Error('Invalid Vault field mappings');
  const refs = new Set<string>();
  for (const item of value) {
    if (!item || typeof item !== 'object' || Array.isArray(item) || Object.keys(item).some(k => !['ref','vault_id','field'].includes(k))
      || typeof item.ref !== 'string' || !(native ? /^[0-9a-f-]{36}$/ : /^e(?:[1-9][0-9]?|1[0-9]{2}|200)$/).test(item.ref) || refs.has(item.ref)
      || typeof item.vault_id !== 'string' || !/^[A-Za-z0-9_-]{22,64}$/.test(item.vault_id)
      || !Object.values(BROWSER_VAULT_FIELDS).some(fields => (fields as readonly unknown[]).includes(item.field))) throw new Error('Invalid Vault field mappings');
    refs.add(item.ref);
  }
  return value;
}

// Runs only in the retained private world's native form binding. Page content is
// never authority: exact refs, field type, document, form and origin stay pinned.
const VALIDATE_VAULT_FIELDS = `function(documentId, origin, fields) {
  const bound = globalThis.__nanocodexNativeForm;
  if (!bound || bound.documentId !== documentId || bound.document !== document || bound.href !== location.href
    || bound.origin !== origin || location.origin !== origin || window.top !== window) return false;
  const fieldState = e => [e.outerHTML, 'value' in e ? e.value : null, 'checked' in e ? e.checked : null, e instanceof HTMLSelectElement ? [...e.options].map(o => o.selected) : null];
  const formState = ${NATIVE_FORM_STATE};
  const selection = bound.selectionId ? globalThis.__nanocodexNativeSelection : null;
  if (bound.selectionId && (!selection || selection.id !== bound.selectionId || !selection.valid())) return false;
  const hints = {card_number:'cc-number',expiry_month:'cc-exp-month',expiry_year:'cc-exp-year',card_expiry:'cc-exp',cvv:'cc-csc',
    billing_zip:'postal-code',address_line_1:['address-line1','street-address'],address_line_2:'address-line2',city:'address-level2',state:'address-level1',zip:'postal-code',country:['country','country-name'],phone_number:'tel'};
  return fields.every(f => {
    const b = bound.entries.find(b => b.ref === f.ref), e = b?.element;
    if (!(e instanceof HTMLInputElement || e instanceof HTMLTextAreaElement || e instanceof HTMLSelectElement) || !e.isConnected || e.disabled || e.matches(':disabled') || e.readOnly
      || e.getRootNode() !== document || e.closest('[hidden],[inert],[aria-hidden="true"]')
      || !e.checkVisibility({checkOpacity:true,checkVisibilityCSS:true})) return false;
    const form = e.form;
    if (form !== b.form || formState(form) !== b.formState) return false;
    if (form) {
      if (!(form instanceof HTMLFormElement) || (form.target && form.target !== '_self')) return false;
      const destination = new URL(form.action, location.href);
      if (destination.origin !== origin || destination.username || destination.password) return false;
    }
    if (JSON.stringify([e.outerHTML,e.value,'checked' in e ? e.checked : null,e instanceof HTMLSelectElement ? [...e.options].map(o => o.selected) : null]) !== b.state) return false;
    const tokens = (e.autocomplete || '').toLowerCase().trim().split(/\\s+/);
    if (tokens[tokens.length - 1] === 'webauthn') tokens.pop();
    const hint = tokens[tokens.length - 1];
    if (hint === 'one-time-code' || hint === 'new-password') return false;
    if (f.field === 'password') return e instanceof HTMLInputElement && e.type === 'password';
    if (f.field === 'api_key') return e instanceof HTMLInputElement && ['text','password'].includes(e.type)
      && !hint.match(/^(cc-|address-|tel|email|username|current-password|new-password|one-time-code)/);
    if (f.field === 'username') return e instanceof HTMLInputElement && ['text','email'].includes(e.type)
      && (!hint || ['off','on','username','email'].includes(hint));
    const allowed = hints[f.field];
    return (e instanceof HTMLSelectElement ? !e.multiple && e.options.length <= 200 : ['text','tel','number','email','textarea'].includes(e.type))
      && (Array.isArray(allowed) ? allowed.includes(hint) : allowed === hint);
  });
}`;

// Convert saved values to native select indices inside the private world. The
// only result is indices; option values and resolved material never leave it.
const NATIVE_VAULT_SELECTS = `function(documentId, fields) {
  const bound = globalThis.__nanocodexNativeForm;
  if (!bound || bound.documentId !== documentId || bound.document !== document || bound.href !== location.href) return null;
  const normalize = value => value.trim().toLocaleLowerCase();
  return fields.map(f => {
    const e = bound.entries.find(b => b.ref === f.ref)?.element;
    if (!(e instanceof HTMLSelectElement)) return null;
    const wanted = new Set([normalize(f.value)]);
    if (f.field === 'country' && /^[a-z]{2}$/i.test(f.value)) {
      try { wanted.add(normalize(new Intl.DisplayNames([document.documentElement.lang || 'en','en'],{type:'region'}).of(f.value.toUpperCase()))); } catch {}
    }
    const matches = [...e.options].flatMap((o,index) => {
      if (o.disabled || o.hidden || o.closest('optgroup[disabled],optgroup[hidden]')) return [];
      const candidates = [normalize(o.value),normalize(o.label)];
      const match = candidates.some(v => wanted.has(v) || ['expiry_month','expiry_year'].includes(f.field) && /^\\d+$/.test(v) && Number(v) === Number(f.value));
      return match ? [index] : [];
    });
    return matches.length === 1 ? String(matches[0]) : false;
  });
}`;

/** Authenticated human reuse. Returns private viewport data ONLY to that human's
 * endpoint. Native refs are single-use; the existing fill path owns stale checks. */
export async function injectNativeVaultFields(options: {
  cdp: PrivateBrowserCdp; identity: BrowserVaultIdentity; touch: BrowserVaultTouchState;
  documentId: string; mappings: VaultFieldMapping[]; resolve: BrowserVaultFieldsResolver;
  remember(values: string[]): void; context?: ToolContext; signal?:AbortSignal;
}): Promise<BrowserVaultTakeoverResult> {
  const {cdp,identity,touch} = options;
  const mappings = parseVaultFieldMappings(options.mappings, true);
  const binding = touch.nativeForm;
  if (!binding || binding.documentId !== options.documentId || binding.origin !== identity.expected_origin) throw new Error('Private Vault form is stale');
  const attached = await cdp.attachTarget(identity.target_id);
  const validate = async () => {
    const tree = await cdp.send('Page.getFrameTree', {}, attached.sessionId);
    const frame = tree?.frameTree?.frame;
    if (!frame || frame.parentId || frame.id !== binding.frameId || frame.loaderId !== binding.loaderId
      || new URL(frame.url).origin !== identity.expected_origin) throw new Error('Private Vault form is stale');
    const result = await cdp.send('Runtime.callFunctionOn', {executionContextId:binding.contextId,returnByValue:true,silent:true,
      functionDeclaration:VALIDATE_VAULT_FIELDS,arguments:[binding.documentId,identity.expected_origin,mappings].map(value=>({value}))},attached.sessionId);
    if (result?.exceptionDetails || result?.result?.value !== true) throw new Error('Private Vault field is incompatible');
  };
  await validate();
  const fields: {ref:string;value:string}[] = [];
  for (const id of new Set(mappings.map(mapping => mapping.vault_id))) {
    const selected = mappings.filter(mapping => mapping.vault_id === id);
    const requested = [...new Set(selected.map(mapping => mapping.field))];
    const material = await options.resolve({vault_id:id,expected_origin:identity.expected_origin,fields:requested},options.context);
    if (!material || !Object.hasOwn(BROWSER_VAULT_FIELDS,material.kind) || !material.values
      || Object.keys(material.values).length !== requested.length || requested.some(field =>
        !(BROWSER_VAULT_FIELDS[material.kind] as readonly string[]).includes(field) || !Object.hasOwn(material.values,field)
        || typeof material.values[field] !== 'string' || material.values[field].length > 4096)) throw new Error('Private Vault item is unavailable');
    for (const mapping of selected) fields.push({ref:mapping.ref,value:material.values[mapping.field]!});
  }
  await validate();
  options.context?.signal.throwIfAborted();
  options.signal?.throwIfAborted();
  const converted = await cdp.send('Runtime.callFunctionOn', {executionContextId:binding.contextId,returnByValue:true,silent:true,
    functionDeclaration:NATIVE_VAULT_SELECTS,arguments:[binding.documentId,fields.map(f=>({...f,field:mappings.find(m=>m.ref===f.ref)!.field}))].map(value=>({value}))},attached.sessionId);
  const indices = converted?.result?.value;
  if (converted?.exceptionDetails || !Array.isArray(indices) || indices.length !== fields.length || indices.some(i=>i !== null && (typeof i !== 'string' || !/^(0|[1-9][0-9]{0,2})$/.test(i)))) throw new Error('Private Vault field is incompatible');
  options.remember(fields.map(field => field.value));
  for (let i=0;i<fields.length;i++) if (indices[i] !== null) fields[i]!.value=indices[i];
  return privateVaultTakeover(cdp,identity,{action:'fill_fields',document_id:options.documentId,fields},touch);
}

/** Model-facing wrapper: only fixed status crosses the private boundary, never
 * private viewports, values, provider errors, selectors or resolved material. */
export async function injectBrowserVaultFields(options: {
  cdp: PrivateBrowserCdp; identity: BrowserVaultIdentity; snapshotId: string; mappings: VaultFieldMapping[];
  resolve: BrowserVaultFieldsResolver; remember(values:string[]):void; context?: ToolContext;
}): Promise<{status:'filled'|'outcome_unknown'}> {
  let dispatched = false;
  try {
    const mappings = parseVaultFieldMappings(options.mappings);
    if (!/^[0-9a-f-]{36}$/.test(options.snapshotId)) throw new Error();
    const selection = await selectBrowserVaultInput(options.cdp,options.identity,options.snapshotId,{fields:mappings.map(({ref})=>({ref}))});
    if (!selection) throw new Error();
    const touch: BrowserVaultTouchState = {nativeSelection:selection};
    const observed = await privateVaultTakeover(options.cdp,options.identity,{action:'observe',native_fields:true,native_field_hints:true,native_field_controls:true},touch);
    if (!observed.native_form || observed.native_form.fields.length !== mappings.length) throw new Error();
    await injectNativeVaultFields({...options,touch,documentId:observed.native_form.document_id,
      mappings:mappings.map((mapping,index)=>({...mapping,ref:observed.native_form!.fields[index]!.ref})),
      remember: values => { options.remember(values); dispatched = true; }});
    return {status:'filled'};
  } catch {
    if (dispatched) return {status:'outcome_unknown'};
    throw new Error('Vault fields could not be filled safely');
  }
}

export const vaultFieldMappingProperties = {
  snapshot_id: {type:'string'},
  operation_id: {type:'string'},
  fields: {type:'array',minItems:1,maxItems:32,items:{type:'object',additionalProperties:false,
    properties:{ref:{type:'string'},vault_id:{type:'string'},field:{type:'string',enum:[...new Set(Object.values(BROWSER_VAULT_FIELDS).flat())]}},
    required:['ref','vault_id','field']}},
};
export const vaultFieldInjectionDescription = "Privately fill selected current snapshot refs using saved Vault login, API key, card, address or phone fields. Supply only safe item IDs and field names, never values. Each field mapping has ref, vault_id and field. Requires compatible native controls in the approved origin. Does not submit. Use one stable operation_id per intended fill and reuse identical arguments after uncertainty. Returns fixed status only. Inspect a fresh private snapshot afterwards. No credential export to shell or model; passkeys and verification codes are not Vault fields.";

export function parseNativeVaultInjection(value:Record<string,unknown>): {documentId:string;mappings:VaultFieldMapping[]} {
  if(Object.keys(value).some(k=>!['action','document_id','fields'].includes(k)) || value.action !== 'fill_vault_fields'
    || typeof value.document_id !== 'string' || !/^[0-9a-f-]{36}$/.test(value.document_id)) throw new Error('Invalid private Vault input');
  return {documentId:value.document_id,mappings:parseVaultFieldMappings(value.fields,true)};
}
