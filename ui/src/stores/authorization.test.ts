// @vitest-environment jsdom
import {afterEach,expect,test,vi} from 'vitest'
import {createApp,h,nextTick} from 'vue'
import {AuthorizationGate as Gate,AuthorizationSettings} from '@synopackageland/sdk/vue'
import type {AuthorizationStatus,GateContext} from '../context'
import {elapseGateRetries,fakeGateRetryTimers} from '../authorizationFixture'
const status=(state:AuthorizationStatus['state'],reason:string|null=null):AuthorizationStatus=>({command:'authorization',schema_version:1,state,reason})
async function settle(){for(let n=0;n<10;n++){await Promise.resolve();await nextTick()}}
// The Gate's dialog is portalled to the document, not rendered inside the Gate's root.
const page=()=>document.body.textContent||''
const buttons=()=>[...document.querySelectorAll('button')]
const button=(label:string)=>buttons().find(b=>b.textContent===label)
function fixture(value:AuthorizationStatus){
 let reopen=()=>{}
 const authorization={canManage:()=>true,status:vi.fn(async()=>value),request:vi.fn(async()=>status('ready')),revoke:vi.fn(async()=>status('authorization_required','grant_missing'))}
 const dispose=vi.fn()
 const ctx={locale:'en',authorization,reopen:(fn:()=>void)=>{reopen=fn;return dispose}} as unknown as GateContext
 const root=document.createElement('div');document.body.append(root)
 const onReady=vi.fn()
 const app=createApp({render:()=>h(Gate,{ctx,onReady},()=>[h('main','Product'),h(AuthorizationSettings)])});app.mount(root)
 return {authorization,root,dispose,onReady,reopen:()=>reopen(),stop(){app.unmount();root.remove()}}
}
afterEach(()=>{vi.useRealTimers();document.body.innerHTML=''})
test.each(['helper_untrusted','cli_untrusted'])('untrusted %s offers reinstall instructions and no password action',async(reason)=>{
 const f=fixture(status('authorization_required',reason));await settle()
 expect(page()).toContain('Reinstall');expect(button('Authorize')).toBeUndefined();expect(f.authorization.request).not.toHaveBeenCalled();f.stop()
})
test.each(['not_admin','session_expired'] as const)('%s never offers password authorization',async(state)=>{
 const f=fixture(status(state));await settle();expect(button('Authorize')).toBeUndefined();expect(button('Try again')).toBeTruthy();f.stop()
})
test('unavailable is checked again in the background before it offers only a retry',async()=>{
 fakeGateRetryTimers()
 const f=fixture(status('unavailable'));await settle()
 expect(button('Try again')).toBeUndefined()
 await elapseGateRetries();await settle()
 expect(f.authorization.status).toHaveBeenCalledTimes(3);expect(button('Authorize')).toBeUndefined();expect(button('Try again')).toBeTruthy();f.stop()
})
test('shows the product at once and asks only once the background check needs a password',async()=>{
 const f=fixture(status('authorization_required','grant_missing'))
 expect(f.root.textContent).toContain('Product');expect(document.querySelector('.authorization-view')).toBeNull()
 await settle();expect(button('Authorize')).toBeTruthy();f.stop()
})
test('missing grant is approved through the host flow and the dialog closes on ready readback',async()=>{
 const f=fixture(status('authorization_required','grant_missing'));await settle()
 button('Authorize')!.click();await settle()
 expect(f.authorization.request).toHaveBeenCalledOnce();expect(document.querySelector('[data-authorization-state]')).toBeNull();f.stop();expect(f.dispose).toHaveBeenCalledOnce()
})
test('refreshes product data only after a fresh ready readback',async()=>{
 const f=fixture(status('authorization_required','grant_missing'));await settle()
 expect(f.onReady).not.toHaveBeenCalled()
 button('Authorize')!.click();await settle()
 expect(f.onReady).toHaveBeenCalledOnce();f.stop()
})
test('reopen checks again in the background and unmount discards its reply',async()=>{
 const f=fixture(status('ready'));await settle()
 let finish!:(value:AuthorizationStatus)=>void
 f.authorization.status.mockImplementation(()=>new Promise(resolve=>{finish=resolve}))
 f.reopen();await nextTick()
 expect(f.authorization.status).toHaveBeenCalledTimes(2);expect(f.root.textContent).toContain('Product');expect(document.querySelector('[data-authorization-state]')).toBeNull()
 f.stop();finish(status('ready'));await settle();expect(f.root.childNodes.length).toBe(0)
})
test('revoking from the settings asks for confirmation and queues a reopen that arrives while busy',async()=>{
 const f=fixture(status('ready'));await settle()
 let finish!:(value:AuthorizationStatus)=>void
 f.authorization.revoke.mockImplementation(()=>new Promise(resolve=>{finish=resolve}))
 const revoke=button('Revoke authorization')!;revoke.click();await settle()
 const confirm=buttons().find(b=>b.textContent==='Revoke authorization'&&b!==revoke)!
 confirm.click();await settle();expect(f.authorization.revoke).toHaveBeenCalledOnce()
 f.reopen();finish(status('authorization_required','grant_missing'));await settle()
 expect(f.authorization.status).toHaveBeenCalledTimes(2);f.stop()
})
