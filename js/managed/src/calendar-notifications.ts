import { createHash } from "node:crypto";
const hash = (value: string) => createHash("sha256").update(value).digest("hex");
type RecordValue = Record<string, unknown>;
const record = (v: unknown): RecordValue => v && typeof v === "object" && !Array.isArray(v) ? v as RecordValue : {};
/** Normalize provider data once; persisted JSON is the immutable retry payload.
 * Attachment references are bounded metadata; their contents are never fetched. */
export function calendarEventSnapshot(value: unknown, prior?: string): RecordValue {
  const raw = record(value), truncated: string[] = [], unavailable: string[] = [];
  const old = prior ? record(JSON.parse(prior)) : {};
  const cancelled = raw.status === "cancelled";
  if (cancelled) {
    if (Array.isArray(old.truncated)) truncated.push(...old.truncated.filter((v):v is string=>typeof v === "string"));
    if (Array.isArray(old.unavailable)) unavailable.push(...old.unavailable.filter((v):v is string=>typeof v === "string"));
  }
  const text = (v:unknown, field:string, limit=2048): string | null => {
    if (typeof v !== "string") return null;
    const bytes=new TextEncoder().encode(v);
    if (bytes.byteLength > limit) truncated.push(field);
    return bytes.byteLength > limit ? new TextDecoder().decode(bytes.slice(0,limit)).replace(/\uFFFD$/u, "") : v;
  };
  const field = (name:string) => raw[name] ?? (cancelled ? old[name === "summary" ? "title" : name] : undefined);
  const person = (v:unknown) => {const p=record(v); return {email:text(p.email,"person.email",320),displayName:text(p.displayName,"person.displayName",512),responseStatus:text(p.responseStatus,"person.responseStatus",64),self:p.self===true,optional:p.optional===true};};
  const time = (v:unknown) => {const t=record(v); return { ...(typeof t.date === "string" ? {date:text(t.date,"date",32)} : {}),...(typeof t.dateTime === "string" ? {dateTime:text(t.dateTime,"dateTime",64)} : {}),...(typeof t.timeZone === "string" ? {timeZone:text(t.timeZone,"timeZone",128)} : {})};};
  const attendees=field("attendees");
  if (Array.isArray(attendees) && attendees.length>100) truncated.push("attendees");
  if (raw.attendeesOmitted === true) truncated.push("attendees_provider_omitted");
  for (const name of ["summary","description","start","end","organizer","location"]) if (field(name) === undefined) {const key=name === "summary" ? "title" : name;if(!unavailable.includes(key)) unavailable.push(key);}
  const entries=record(raw.conferenceData).entryPoints;
  const attachments=field("attachments");
  if(Array.isArray(attachments)&&attachments.length>5) truncated.push("attachments");
  if(Array.isArray(entries)&&entries.length>10) truncated.push("conferenceLinks");
  const result = {id:text(raw.id,"id",1024),status:cancelled ? "cancelled" : text(raw.status,"status",64) ?? "confirmed",
    title:text(field("summary"),"title",2048),description:text(field("description"),"description",16000),
    start:time(field("start")),end:time(field("end")),allDay:typeof record(field("start")).date === "string",
    attendees:Array.isArray(attendees)?attendees.slice(0,100).map(person):[],organizer:person(field("organizer")),
    location:text(field("location"),"location",2048),htmlLink:text(field("htmlLink"),"htmlLink"),
    conferenceLinks:Array.isArray(entries)?entries.slice(0,10).map(v=>{const p=record(v);return {type:text(p.entryPointType,"conference.type",64),uri:text(p.uri,"conference.uri")};}):cancelled && Array.isArray(old.conferenceLinks)?old.conferenceLinks:[],
    attachments:Array.isArray(attachments)?attachments.slice(0,5).map(v=>{const a=record(v);return {title:text(a.title,"attachment.title",512),fileUrl:text(a.fileUrl,"attachment.fileUrl",2048),mimeType:text(a.mimeType,"attachment.mimeType",128)};}):[],
    hangoutLink:text(field("hangoutLink"),"hangoutLink"),recurringEventId:text(field("recurringEventId"),"recurringEventId",1024),
    originalStartTime:time(field("originalStartTime")),contentStatus:truncated.length?"truncated":unavailable.length?"partial":"complete",truncated,unavailable};
  // Enforce a byte budget even for multi-byte descriptions and attendee names.
  while(new TextEncoder().encode(JSON.stringify(result)).byteLength>20000) {
    if(!truncated.includes("serialized_byte_limit")) truncated.push("serialized_byte_limit");
    result.contentStatus="truncated";
    if(result.attendees.length) result.attendees.pop();
    else if(result.description) result.description=result.description.slice(0,Math.floor(result.description.length/2));
    else if(result.conferenceLinks.length) result.conferenceLinks.pop();
    else if(result.attachments.length) result.attachments.pop();
    else {
      // JSON escaping can expand control characters sixfold. Reduce scalar
      // metadata too, keeping event identity and explicit truncation markers.
      const keys=["title","location","htmlLink","hangoutLink","recurringEventId"] as const;
      const key=keys.filter(k=>Boolean(result[k])).sort((a,b)=>JSON.stringify(result[b]).length-JSON.stringify(result[a]).length)[0];
      if(key) result[key]=result[key]!.slice(0,Math.floor(result[key]!.length/2)) || null;
      else { result.organizer={email:null,displayName:null,responseStatus:null,self:false,optional:false}; }
    }
  }
  return result;
}
export async function stageCalendarNotifications(db:D1Database, source:{id:string;connection_id:string;calendar_id:string;notifications_initialized:number;sync_token:string|null}, events:unknown[], fence:()=>Promise<void>) {
  for(const event of events) {
    await fence();
    const id=record(event).id;
    if(typeof id!=="string" || !id || id.length>1024) throw new Error("calendar_push_invalid_event");
    const prior=await db.prepare("SELECT version,snapshot FROM calendar_notification_snapshots WHERE source_id=? AND event_id=?").bind(source.id,id).first<{version:string;snapshot:string}>();
    const snapshot=JSON.stringify(calendarEventSnapshot(event,prior?.snapshot));
    const version=hash(snapshot);
    if(prior?.version===version) continue;
    const statements=[db.prepare(`INSERT INTO calendar_notification_snapshots(source_id,event_id,version,snapshot) VALUES(?,?,?,?) ON CONFLICT(source_id,event_id) DO UPDATE SET version=excluded.version,snapshot=excluded.snapshot`).bind(source.id,id,version,snapshot)];
    if(source.notifications_initialized && (prior || source.sync_token)) {
      // A transition gets its own durable ID, including A -> B -> A edits.
      const notificationId=crypto.randomUUID();
      const input=JSON.stringify({provider:"google_calendar",type:"calendar.event.changed",source:{connectionId:source.connection_id,calendarId:source.calendar_id,eventId:id},event:JSON.parse(snapshot),untrusted:true});
      statements.push(db.prepare("INSERT INTO calendar_notification_outbox(id,source_id,input,created_at) VALUES(?,?,?,?)").bind(notificationId,source.id,input,Date.now()));
    }
    await fence();
    await db.batch(statements);
    await fence();
  }
}
export async function drainCalendarNotifications(db:D1Database, sourceId:string, authorize:()=>Promise<void>, deliver:(id:string,input:string)=>Promise<"accepted"|"duplicate"|"busy">):Promise<boolean> {
  const rows=(await db.prepare("SELECT id,input FROM calendar_notification_outbox WHERE source_id=? ORDER BY created_at,id LIMIT 10").bind(sourceId).all<{id:string;input:string}>()).results;
  for(const row of rows) {
    await authorize();
    if(await deliver(row.id,row.input)==="busy") return false;
    await authorize();
    await db.prepare("DELETE FROM calendar_notification_outbox WHERE id=? AND source_id=?").bind(row.id,sourceId).run();
  }
  return rows.length<10;
}
