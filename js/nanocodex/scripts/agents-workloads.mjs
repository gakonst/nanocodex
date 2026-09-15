// Synthetic, deterministic workloads. Never include account data or external tools.
export const instructions = 'Use only the supplied data. Do not call tools. Return the requested JSON without Markdown or explanation.';
const tickets = Array.from({length:120}, (_,i)=>({id:`T${String(i+1).padStart(3,'0')}`,region:['eu','us','apac'][i%3],status:['open','closed','pending','open'][i%4],severity:1+i%5,minutes:7+(i*13)%91}));
const selected=tickets.filter(t=>t.status==='open'&&t.severity>=3);
const extraction={ids:selected.map(t=>t.id),regions:['apac','eu','us'].map(region=>({region,count:selected.filter(t=>t.region===region).length,minutes:selected.filter(t=>t.region===region).reduce((n,t)=>n+t.minutes,0)}))};
const jobs=Array.from({length:18},(_,i)=>({id:String.fromCharCode(65+i),duration:2+(i*7)%13,dependencies:[i-1,i-3,i-5].filter((n,k)=>n>=0&&(i+k)%3!==0).map(n=>String.fromCharCode(65+n))}));
const finish={};const schedule={jobs:jobs.map(j=>{const start=Math.max(0,...j.dependencies.map(id=>finish[id]));finish[j.id]=start+j.duration;return {id:j.id,start,finish:finish[j.id]};}),makespan:0};schedule.makespan=Math.max(...Object.values(finish));
const invoices=Array.from({length:60},(_,i)=>({id:`I${String(i+1).padStart(3,'0')}`,quantity:1+i%9,unit_cents:109+(i*31)%700,discount_cents:(i%4)*17}));
const expanded={invoices:invoices.map(i=>({...i,total_cents:i.quantity*i.unit_cents-i.discount_cents})),grand_total_cents:invoices.reduce((n,i)=>n+i.quantity*i.unit_cents-i.discount_cents,0)};
export const workloads=[
 {id:'extraction',prompt:'Select tickets whose status is open and severity is at least 3. Return {"ids":[matching IDs in input order],"regions":[{"region":...,"count":...,"minutes":...}]} with regions in alphabetical order and totals over matching tickets only.\nTickets:\n'+JSON.stringify(tickets),expected:extraction},
 {id:'schedule',prompt:'Compute an earliest-start schedule with unlimited parallel workers. All jobs are available at time 0 except for their listed dependencies; a job starts exactly when every dependency finishes. Return {"jobs":[{"id":...,"start":...,"finish":...}],"makespan":...} in input order. Times are integer units.\nJobs:\n'+JSON.stringify(jobs),expected:schedule},
 {id:'long_output',prompt:'For every invoice below retain id, quantity, unit_cents and discount_cents, and append total_cents = quantity * unit_cents - discount_cents. The discount applies once per invoice, not per item. Return {"invoices":[all 60 expanded invoices in input order],"grand_total_cents":sum of total_cents}. Do not abbreviate.\nInvoices:\n'+JSON.stringify(invoices),expected:expanded},
];
const canonical=value=>JSON.stringify(Array.isArray(value)?value.map(v=>JSON.parse(canonical(v))):value&&typeof value==='object'?Object.fromEntries(Object.keys(value).sort().map(k=>[k,JSON.parse(canonical(value[k]))])):value);
export function validate(workload,text) {
 try { const parsed=JSON.parse(text);return {correct:canonical(parsed)===canonical(workload.expected),valid_json:true}; }
 catch {return {correct:false,valid_json:false};}
}
