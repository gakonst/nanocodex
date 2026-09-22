'use strict';
// WoW-style local UI feedback. These never send game chat or claim a task completed.
(()=>{
 const make=(id,role)=>{const node=document.createElement('div');node.id=id;node.setAttribute('role',role);document.body.append(node);return node;};
 const errors=make('uiErrors','alert'),raid=make('raidNotice','status'),toasts=make('toastStack','region'),tip=make('wowTooltip','tooltip');
 toasts.setAttribute('aria-label','Companion notifications');tip.hidden=true;
 const timers=new Map(),seen=new Map();
 function transient(node,text,duration){clearTimeout(timers.get(node));node.textContent=text;node.classList.remove('visible');void node.offsetWidth;node.classList.add('visible');timers.set(node,setTimeout(()=>{node.classList.remove('visible');node.textContent='';},duration));}
 function toast(title,message,kind){
  const box=document.createElement('section');box.className='wow-toast '+kind;
  const icon=document.createElement('span');icon.className='wow-toast-icon';icon.setAttribute('aria-hidden','true');
  const content=document.createElement('div'),heading=document.createElement('strong'),body=document.createElement('p'),close=document.createElement('button');
  heading.textContent=title;body.textContent=message;content.append(heading,body);close.textContent='×';close.className='wow-toast-close';close.setAttribute('aria-label','Dismiss notification');close.onclick=()=>box.remove();box.append(icon,content,close);toasts.append(box);
  while(toasts.children.length>3)toasts.firstElementChild.remove();setTimeout(()=>box.remove(),9000);
 }
 window.wowNotify=({message='',kind='info',title,toast:wantToast=false})=>{
  if(!message)return;const key=kind+'\n'+message;const now=Date.now();if(now-(seen.get(key)||0)<10000)return;seen.set(key,now);if(seen.size>64)seen.delete(seen.keys().next().value);
  const short=message.length>140?message.slice(0,137)+'…':message;
  if(kind==='error')transient(errors,short,5000);
  else if(kind==='success')transient(raid,short,4000);
  if(wantToast||kind==='reply')toast(title||(kind==='reply'?'New reply':'Journal updated'),short,kind);
 };
 let owner=null;
 function hideTip(){owner=null;tip.hidden=true;tip.replaceChildren();}
 function showTip(target){
  const detail=target.getAttribute('data-wow-tooltip');if(!detail)return;
  owner=target;tip.replaceChildren();const title=document.createElement('strong');title.textContent=target.getAttribute('aria-label')||target.textContent.trim()||'Nanocodex';const description=document.createElement('p');description.textContent=detail;tip.append(title,description);tip.hidden=false;
  const r=target.getBoundingClientRect();const box=tip.getBoundingClientRect();tip.style.left=Math.max(8,Math.min(r.left,innerWidth-box.width-8))+'px';tip.style.top=Math.max(8,(r.bottom+box.height+12<innerHeight?r.bottom+8:r.top-box.height-8))+'px';
 }
 function targetOf(event){return event.target.closest?.('[data-wow-tooltip]');}
 document.querySelectorAll('button[title],.model[title]').forEach(el=>{el.setAttribute('data-wow-tooltip',el.title);el.removeAttribute('title');});
 document.addEventListener('pointerover',e=>{const target=targetOf(e);if(target&&target!==owner)showTip(target);});
 document.addEventListener('pointerout',e=>{if(owner&&!owner.contains(e.relatedTarget))hideTip();});
 document.addEventListener('focusin',e=>{const target=targetOf(e);if(target)showTip(target);});
 document.addEventListener('focusout',hideTip);document.addEventListener('keydown',e=>{if(e.key==='Escape')hideTip();});window.addEventListener('scroll',hideTip,true);
})();
