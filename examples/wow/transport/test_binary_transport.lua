local NS={}
assert(loadfile('addon/Transport.lua'))('Nanocodex',NS)
local T=NS.Transport
local now,accepted=0,0
local link=T.New(17,function() accepted=accepted+1 return true end)
local receiver=T.NewKeys(function(s) return link:Receive(s) end,function() return now end)
local function binary(packet)
 local result={'F24'}
 for i=1,#packet do
  for shift=7,0,-1 do result[#result+1]=math.floor(packet:byte(i)/2^shift)%2==1 and 'F23' or 'F19' end
 end
 result[#result+1]='F24' return result
end
local function feed(stream)
 local handled,ok
 for _,key in ipairs(stream) do handled,ok=receiver:Key(key) end
 return ok
end
local packet=T.Encode(17,1,0,string.rep('x',96),true)
local stream=binary(packet)
assert(#stream==890 and feed(stream) and accepted==1)
assert(receiver.buffer==nil and receiver.mode==nil)
assert(feed(stream) and accepted==1) -- duplicate transport delivery
-- CRC and session validation remain in the existing link receiver.
for i=1,#packet do
 local corrupt=packet:sub(1,i-1)..string.char((packet:byte(i)+1)%256)..packet:sub(i+1)
 assert(not feed(binary(corrupt)))
 assert(receiver.buffer==nil)
end
assert(not feed(binary(T.Encode(18,2,0,'wrong session',true))))
assert(accepted==1 and link.rx==1)
-- Empty and partial frames always terminate and reset.
assert(not feed({'F24','F24'}))
for bits=1,7 do
 receiver:Key('F24') for _=1,bits do receiver:Key('F19') end
 local _,ok=receiver:Key('F24') assert(not ok and receiver.buffer==nil)
end
-- A lost or duplicated bit cannot pass framing / CRC.
for _,change in ipairs({'drop','duplicate'}) do
 local altered={}
 for i,key in ipairs(stream) do
  if change~='drop' or i~=7 then altered[#altered+1]=key end
  if change=='duplicate' and i==7 then altered[#altered+1]=key end
 end
 assert(not feed(altered)) receiver:Reset()
end
receiver:Key('F24')
for _=1,888 do receiver:Key('F19') end
assert(#receiver.buffer==111)
receiver:Key('F23') assert(receiver.buffer==nil)
-- Both idle and whole-frame deadlines apply to binary.
receiver:Key('F24') receiver:Key('F19') now=3 receiver:Key('F23')
assert(receiver.buffer==nil)
receiver:Key('F24')
for _=1,31 do now=now+1 receiver:Key('F19') end
assert(receiver.buffer==nil)
-- Octal markers/data may not silently splice into a binary frame.
for _,key in ipairs({'F13','F14','F15','F16','F17','F18','F20','F21','F22'}) do
 receiver:Key('F24') receiver:Key('F19') receiver:Key(key)
 assert(receiver.buffer==nil and receiver.mode==nil)
end
receiver:Key('F21') receiver:Key('F13') receiver:Key('F24')
assert(receiver.buffer==nil) -- F24 aborts octal; it does not switch modes
-- Explicit reset and recovery permit another complete packet in either mode.
receiver:Key('F24') receiver:Key('F19') receiver:Reset()
assert(feed(binary(T.Encode(17,2,0,'binary recovery',true))))
local octal={'F21'} local p=T.Encode(17,3,0,'octal recovery',true)
for i=1,#p do
 local n=p:byte(i)
 octal[#octal+1]='F'..(13+math.floor(n/64))
 octal[#octal+1]='F'..(13+math.floor(n/8)%8)
 octal[#octal+1]='F'..(13+n%8)
end
octal[#octal+1]='F22'
assert(feed(octal) and accepted==3)
-- Read Python-generated frames to verify the complete encoder/parser boundary.
if arg[1] then
 for line in io.lines(arg[1]) do
  local wire={} for key in line:gmatch('%S+') do wire[#wire+1]=key end
  assert(feed(wire))
 end
 assert(link.rx==6)
end
print('Lua binary carrier: all assertions passed')
