import type { BrowserVaultNativeForm } from "./browser-vault-takeover";

export type PrivateVaultEntry = {operation_id:string;kind:string;payload:Record<string,string>};
export type PrivateVaultSave = (entry: PrivateVaultEntry) => Promise<{id:string;kind:string;name:string}>;
export type VaultSaveReceipt = {status:"saved"|"not_saved"|"failed";items?:{id:string;kind:string;name:string}[];retryable?:boolean;reason?:string};
const ROLES = ["username","password","api_key","phone_number","card_number","expiry_month","expiry_year","billing_zip","address_line_1","address_line_2","city","state","zip","country"] as const;
type Role = typeof ROLES[number];
export type PrivateVaultDetails = {username?:string;fields?:Record<string,Role>};
export function validatePrivateVaultSave(value:unknown): asserts value is boolean {
  if (typeof value !== "boolean") throw new Error("Invalid Vault save option");
}
export function validatePrivateVaultDetails(value:unknown): asserts value is PrivateVaultDetails {
  if (!value || typeof value!=="object" || Array.isArray(value)) throw new Error("Invalid Vault save details");
  const v=value as PrivateVaultDetails;
  if (Object.keys(v).some(k=>!["username","fields"].includes(k)) || (v.username!==undefined && (typeof v.username!=="string" || !v.username || v.username.length>512 || /[\u0000-\u001f\u007f]/.test(v.username)))) throw new Error("Invalid Vault save details");
  if(v.fields!==undefined && (!v.fields || typeof v.fields!=="object" || Array.isArray(v.fields) || Object.keys(v.fields).length>32 || Object.entries(v.fields).some(([k,r])=>k.length>128 || !ROLES.includes(r)))) throw new Error("Invalid Vault save details");
}
const TTL=10*60_000, MAX=64;
type State={entries:PrivateVaultEntry[];items:{id:string;kind:string;name:string}[];expires:number;finished:boolean;receipt?:VaultSaveReceipt;running?:Promise<VaultSaveReceipt>;scope:string};
/** Only transient values are retained here. Broker writes encrypt them before storage.
 * The scope is the private browser session + target, never a rotating input request ID. */
export class PrivateVaultSaves {
  private pending=new Map<string,State>();
  private usernames=new Map<string,{value:string;expires:number}>();
  private save?:PrivateVaultSave;
  constructor(save?:PrivateVaultSave) {this.save=save;}
  expire(){const now=Date.now();for(const [k,v] of this.pending)if(v.expires<=now&&!v.running)this.pending.delete(k);for(const [k,v] of this.usernames)if(v.expires<=now)this.usernames.delete(k);}
  stage(request:string,origin:string,form:BrowserVaultNativeForm,values:{ref:string;value:string}[],enabled:boolean,scope=request,details?:PrivateVaultDetails) {
    this.expire();const key=scope+"\0"+origin;
    if(!enabled){this.cancelScope(scope,origin);return;}
    let state=this.pending.get(request);
    if(state?.finished)return;
    if(!state){if(this.pending.size>=MAX){const oldest=this.pending.keys().next().value;if(oldest)this.pending.delete(oldest);}state={entries:[],items:[],expires:Date.now()+TTL,finished:false,scope:key};this.pending.set(request,state);}
    // A newer confirmed form replaces any earlier staged values for this panel.
    state.entries=[];
    const fields:Partial<Record<Role,string>>={};const ambiguous=new Set<Role>();
    const set=(role:Role,value:string)=>{if(fields[role]!==undefined && fields[role]!==value)ambiguous.add(role);else fields[role]=value;};
    const auto:Record<string,Role>={username:"username",email:"username","current-password":"password","new-password":"password",tel:"phone_number","cc-number":"card_number","cc-exp-month":"expiry_month","cc-exp-year":"expiry_year","postal-code":"zip","address-line1":"address_line_1","street-address":"address_line_1","address-line2":"address_line_2","address-level2":"city","address-level1":"state","country-name":"country",country:"country"};
    for(const field of form.fields){
      const value=values.find(v=>v.ref===field.ref)?.value;
      // Explicit classification cannot turn verification codes into durable secrets.
      if(!value || field.autocomplete==="one-time-code" || field.autocomplete==="cc-csc" || /\b(?:otp|cvv|cvc|csc|one[ -]?time|verification code|security code|authenticator code)\b/i.test(field.label))continue;
      const role=details?.fields?.[field.ref] ?? auto[field.autocomplete??""] ?? (field.type==="email"?"username":field.type==="password"?"password":undefined);
      if(role)set(role,value);
      if(field.autocomplete==="cc-exp") {const m=/^(\d{1,2})\s*\/\s*(\d{2}|\d{4})$/.exec(value);if(m){set("expiry_month",m[1]);set("expiry_year",m[2].length===2?"20"+m[2]:m[2]);}}
    }
    for(const role of ambiguous)delete fields[role];
    if(details?.username)fields.username=details.username;
    if(fields.username){if(this.usernames.size>=MAX)this.usernames.delete(this.usernames.keys().next().value!);this.usernames.set(key,{value:fields.username,expires:Date.now()+TTL});}
    const username=fields.username??this.usernames.get(key)?.value;
    const name=new URL(origin).hostname;
    const add=(kind:string,payload:Record<string,string>)=>{const old=state!.entries.find(e=>e.kind===kind);const entry={operation_id:old?.operation_id??crypto.randomUUID(),kind,payload:{name,...payload}};state!.entries=state!.entries.filter(e=>e.kind!==kind);state!.entries.push(entry);};
    if(username && fields.password)add("login",{username,password:fields.password,browser_origin:origin});
    if(fields.api_key)add("api_key",{api_key:fields.api_key});
    if(fields.phone_number)add("phone",{phone_number:fields.phone_number});
    const zip=fields.billing_zip??fields.zip;
    if(fields.card_number&&fields.expiry_month&&fields.expiry_year&&zip)add("card",{card_number:fields.card_number,expiry_month:fields.expiry_month,expiry_year:fields.expiry_year,billing_zip:zip});
    if(fields.address_line_1&&fields.city&&fields.state&&fields.zip&&fields.country)add("address",{address_line_1:fields.address_line_1,...(fields.address_line_2?{address_line_2:fields.address_line_2}:{}),city:fields.city,state:fields.state,zip:fields.zip,country:fields.country});
  }
  cancelScope(scope:string,origin:string){const key=scope+"\0"+origin;this.usernames.delete(key);for(const [id,state] of this.pending)if(state.scope===key)this.pending.delete(id);}
  cancel(request:string){const state=this.pending.get(request);if(state)this.usernames.delete(state.scope);this.pending.delete(request);}
  async finish(request:string):Promise<VaultSaveReceipt|undefined>{
    this.expire();const state=this.pending.get(request);if(!state)return;
    if(state.running)return state.running;
    if(state.receipt)return state.receipt;
    state.finished=true;
    const run=async():Promise<VaultSaveReceipt>=>{
      if(!state.entries.length)return state.receipt={status:state.items.length?"saved":"not_saved",...(state.items.length?{items:[...state.items]}:{reason:"incomplete_reusable_fields"})};
      if(!this.save)return state.receipt={status:"failed",retryable:false};
      while(state.entries.length){try{const entry=state.entries[0];const item=await this.save(entry);state.items.push({id:item.id,kind:item.kind,name:item.name});state.entries.shift();if(entry.kind==="login")this.usernames.delete(state.scope);}catch{return state.receipt={status:"failed",retryable:true,...(state.items.length?{items:[...state.items]}:{})};}}
      return state.receipt={status:"saved",items:[...state.items]};
    };
    state.running=run();try{return await state.running;}finally{state.running=undefined;}
  }
  async retry(request:string){this.expire();const state=this.pending.get(request);if(!state?.finished)throw new Error("No pending Vault save");if(state.receipt?.retryable)state.receipt=undefined;return this.finish(request);}
  clear(){this.pending.clear();this.usernames.clear();}
}
