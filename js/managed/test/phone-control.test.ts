import {expect,it} from 'vitest';
import {phoneControlInput} from '../src/phone-control';
const id='11111111-1111-4111-8111-111111111111';
const request=(body:unknown,action='steer')=>new Request(`https://session.internal/phone/calls/${id}/${action}`,{method:'POST',body:JSON.stringify(body)});
it('gets a parent-scoped list and never accepts parent identity from payload',async()=>{
 expect(await phoneControlInput(new Request('https://session.internal/phone/calls'))).toEqual({operation:'list'});
 expect(await phoneControlInput(request({operation_id:id,instructions:'Ask about Friday'}))).toEqual({operation:'steer',call_id:id,operation_id:id,instructions:'Ask about Friday'});
 for(const value of [{agent_id:id},{call_id:id},{operation:'call'},{to:'+15551234567'}])await expect(phoneControlInput(request(value))).rejects.toThrow();
});
it('bounds body bytes and rejects malformed or irrelevant fields',async()=>{
 await expect(phoneControlInput(request({instructions:'x'.repeat(33000)}))).rejects.toThrow();
 await expect(phoneControlInput(request({instructions:'new authority'},'hangup'))).rejects.toThrow();
 expect(await phoneControlInput(request({},'hangup'))).toEqual({operation:'hangup',call_id:id});
});
