-- CPU-only receiver benchmark, deliberately no game/screen/input/model claim.
local NS={} assert(loadfile('addon/Transport.lua'))('Nanocodex',NS)
local T=NS.Transport
local frame=T.Encode(42,1,0,string.rep('x',96),true)
local symbols={'F21'}
for i=1,#frame do local n=frame:byte(i) symbols[#symbols+1]='F'..(13+math.floor(n/64)) symbols[#symbols+1]='F'..(13+math.floor(n/8)%8) symbols[#symbols+1]='F'..(13+n%8) end
symbols[#symbols+1]='F22'
local received=0
local parser=T.NewKeys(function(s) assert(T.Decode(s)) received=received+1 end,function() return 0 end)
local start=os.clock()
for _=1,1000 do for _,key in ipairs(symbols) do parser:Key(key) end end
local elapsed=os.clock()-start
assert(received==1000)
print(string.format('CPU only: 1000 max frames, 335000 key symbols, %.4fs, %.1f KiB/s payload parsing',elapsed,96000/1024/elapsed))
local link=T.New(42) assert(link:Send(string.rep('x',96))) link:Packet()
start=os.clock()
for _=1,1000000 do link:Packet() end
print(string.format('CPU only: 1M unchanged cached packet reads %.4fs',os.clock()-start))
