// Independent implementation of the retained player-observation contract.
(async video => {
 const same=player=>document.getElementById('movie_player')===player&&new URL(location.href).searchParams.get('v')===video&&player.getVideoData().video_id===video;
 const player=document.getElementById('movie_player');
 if(!player||['getVideoData','isSubtitlesOn','toggleSubtitles','toggleSubtitlesOn'].some(name=>typeof player[name]!=='function')||!same(player))return null;
 const began=performance.now();let caption;
 const observer=new PerformanceObserver(list=>{for(const entry of list.getEntries()){if(entry.startTime+.5<began)continue;try{const url=new URL(entry.name);if(url.protocol==='https:'&&url.hostname==='www.youtube.com'&&url.pathname==='/api/timedtext'&&url.searchParams.get('v')===video)caption=url;}catch{}}});
 observer.observe({type:'resource',buffered:true});const before=Boolean(player.isSubtitlesOn());
 try{if(before)player.toggleSubtitles();player.toggleSubtitlesOn();const end=Date.now()+4000;while(!caption&&Date.now()<end)await new Promise(resolve=>setTimeout(resolve,150));}
 finally{try{if(same(player)&&Boolean(player.isSubtitlesOn())!==before){if(before)player.toggleSubtitlesOn();else player.toggleSubtitles();}}finally{observer.disconnect();}}
 if(!caption||!same(player))return null;
 caption.searchParams.set('fmt','json3');const controller=new AbortController(),timer=setTimeout(()=>controller.abort(),8000);
 try{const response=await fetch(caption.href,{credentials:'include',signal:controller.signal});if(!response.ok||!response.headers.get('content-type')?.toLowerCase().includes('json'))return null;
  const declared=Number(response.headers.get('content-length'));if(declared>5*1024*1024)return null;
  const reader=response.body.getReader();const decoder=new TextDecoder();let text='',size=0;
  for(;;){const {done,value}=await reader.read();if(done)break;size+=value.length;if(size>5*1024*1024){await reader.cancel();return null;}text+=decoder.decode(value,{stream:true});}text+=decoder.decode();
  if(!same(player))return null;return {videoId:video,language:caption.searchParams.get('lang')??'',captionKind:caption.searchParams.get('kind')??'',transcript:JSON.parse(text)};
 }catch{return null;}finally{clearTimeout(timer);}
})
