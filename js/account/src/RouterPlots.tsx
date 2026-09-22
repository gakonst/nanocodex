const colors: Record<string,string> = {cloudflare:"#d9a05b",openrouter:"#5b9b91",vercel:"#8896c5",workers_ai:"#b194b8",chatgpt:"#6ba8ba"};
const color = (name:string) => colors[name] ?? "#839995";
const shortMs = (n:number) => n>=1000 ? `${(n/1000).toFixed(2)}s` : `${Math.round(n)}ms`;
export type LatencyRow = {label:string;provider:string;p50:number|null;p95?:number|null;baseline?:number;count:number;failures:number};
/** A zero-based shared axis, no interpolation or fabricated missing measurements. */
export function LatencyPlot({rows,max,label,compare=false}:{rows:LatencyRow[];max?:number;label:string;compare?:boolean}) {
 const ceiling=max??Math.max(1000,...rows.flatMap(r=>[r.p50??0,r.p95??0,r.baseline??0]))*1.08;
 const width=540,left=100,right=60,track=width-left-right,step=compare?62:55,top=22,height=top+rows.length*step+30;
 const x=(n:number)=>left+n/ceiling*track;
 return <svg className="router-latency-plot" viewBox={`0 0 ${width} ${height}`} role="img" aria-label={label}>
   <title>{label}</title>
   {[0,.25,.5,.75,1].map(t=><g key={t}><line x1={x(ceiling*t)} x2={x(ceiling*t)} y1={8} y2={height-26} className="router-plot-grid"/><text x={x(ceiling*t)} y={height-8} textAnchor="middle" className="router-plot-axis">{t===0?"0":shortMs(ceiling*t)}</text></g>)}
   {rows.map((r,i)=>{const y=top+i*step;return <g key={`${r.label}:${i}`}>
     <text x={left-10} y={y+12} textAnchor="end" className="router-plot-label">{r.label}</text>
     {compare&&r.baseline!==undefined&&<><rect x={left} y={y-2} width={x(r.baseline)-left} height={10} rx={3} fill={color(r.provider)} opacity={.22}/><text x={x(r.baseline)+6} y={y+7} className="router-plot-axis">{shortMs(r.baseline)}</text></>}
     {r.p50===null?<text x={left+6} y={y+15} className="router-plot-axis">No TTFT samples</text>:<>
       <rect x={left} y={y+(compare?12:0)} width={Math.max(1,x(r.p50)-left)} height={compare?14:20} rx={3} fill={color(r.provider)}/>
       <text x={x(r.p50)+6} y={y+(compare?24:14)} className="router-plot-value">{shortMs(r.p50)}</text>
       {!compare&&r.p95!==null&&r.p95!==undefined&&r.p95>r.p50&&<><line x1={x(r.p50)} x2={x(r.p95)} y1={y+25} y2={y+25} stroke={color(r.provider)} strokeWidth={2}/><line x1={x(r.p95)} x2={x(r.p95)} y1={y+21} y2={y+29} stroke={color(r.provider)} strokeWidth={2}/></>}
     </>}
     {!compare&&<text x={left} y={y+42} className={`router-plot-axis ${r.failures?"router-plot-failure":""}`}>{r.count} samples · {r.failures} failed{r.count<3?" · sparse":""}</text>}
   </g>;})}
 </svg>;
}
export type OutcomeSegment = {label:string;count:number;color:string};
export function OutcomePlot({segments,label}:{segments:OutcomeSegment[];label:string}) {
 const total=segments.reduce((n,s)=>n+s.count,0);
 return <div className="router-outcome" role="img" aria-label={`${label}: ${segments.map(s=>`${s.count} ${s.label}`).join(', ')}`}>
   <div className="router-outcome-track">{total?segments.filter(s=>s.count>0).map(s=><div key={s.label} title={`${s.count} ${s.label}`} style={{width:`${s.count/total*100}%`,background:s.color}}>{s.count/total>.12?s.count:null}</div>):<span>No observations yet</span>}</div>
   <div className="router-outcome-legend">{segments.map(s=><div key={s.label}><i style={{background:s.color}}/><strong>{s.count}</strong><span>{s.label}</span></div>)}</div>
 </div>;
}
