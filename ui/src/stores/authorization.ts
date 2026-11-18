// Product facade over the canonical Gate. It stores readback only; the Gate
// owns confirmation, reopen sequencing and cancellation.
import {reactive} from 'vue'
import {ApiError} from '@/api'
import {sdkRefusal} from '@/api/errors'
import {locale} from '@/i18n'
import type {AuthorizationClient,AuthorizationState,AuthorizationReason} from '@/api/authorization'
import type {Context,AuthorizationStatus,GateContext} from '@/context'
export type AuthorizationAccess=NonNullable<Context['authorization']>
/** A root task report from the DSM window while approval waits on it (CLI 0.7.1 options.progress). */
export interface AuthorizationProgress {op:string;state:'queued'|'running';elapsedMs:number}
export const authorization=reactive({state:'checking' as 'checking'|AuthorizationState,reason:null as AuthorizationReason|null,error:null as ApiError|null,progress:null as AuthorizationProgress|null})
let client:AuthorizationClient|undefined
let access:AuthorizationAccess|undefined
let onSessionExpired:(()=>void)|undefined
const listeners=new Set<()=>void>()
export function configureAuthorization(deps:{client?:AuthorizationClient;access?:AuthorizationAccess;onSessionExpired?:()=>void}){client=deps.client;access=deps.access;onSessionExpired=deps.onSessionExpired}
export function resetAuthorization(){gate=undefined;client=undefined;access=undefined;onSessionExpired=undefined;listeners.clear();Object.assign(authorization,{state:'checking',reason:null,error:null,progress:null})}
export function reportAuthorizationProgress(event:unknown){
 const e=event as Partial<AuthorizationProgress>|null
 if(!e||typeof e.op!=='string'||(e.state!=='queued'&&e.state!=='running')||typeof e.elapsedMs!=='number'||!Number.isFinite(e.elapsedMs)||e.elapsedMs<0)return
 authorization.progress={op:e.op,state:e.state,elapsedMs:e.elapsedMs}
}
export function openApp():Promise<void>{authorization.state='checking';for(const listener of [...listeners])listener();return Promise.resolve()}
export function closeApp(){authorization.state='checking'}
export function requireAuthorization(error?:ApiError){authorization.error=error||null;void openApp()}
let gate:(()=>Promise<boolean>)|undefined
/** Lets code outside the Gate's component tree reach its requireAuthorization (bound by AppContent). */
export function bindAuthorizationGate(require:()=>Promise<boolean>){gate=require;return()=>{if(gate===require)gate=undefined}}
/**
 * Await before root work: true once authorized; false when the user dismissed
 * the Gate's dialog, and the work must not start. Without a bound Gate (a page
 * mounted on its own in a test) there is nothing to ask, so it resolves true.
 */
export function authorizeRootWork():Promise<boolean>{return gate?gate():Promise.resolve(true)}
async function read(method:'status'|'request'|'revoke'):Promise<AuthorizationStatus>{
 authorization.state='checking'
 try{
  let value:AuthorizationStatus
  if(access)value=await access[method]()
  else if(method==='status'&&client){const status=await client.status();value={command:'authorization',schema_version:1,...status}}
  else throw new ApiError('backend_unavailable')
  authorization.state=value.state;authorization.reason=value.reason as AuthorizationReason|null
  if(value.state==='session_expired')onSessionExpired?.()
  return value
 }catch(error){
  authorization.state='unavailable'
  // The Gate names these refusals and checks again on its own.
  const refusal=sdkRefusal(error)
  if(refusal){authorization.error=error as ApiError;throw refusal}
  if(error instanceof ApiError&&error.code==='session_timeout'){authorization.state='session_expired';onSessionExpired?.()}
  if(error instanceof ApiError&&error.code==='no_permission')authorization.state='not_admin'
  authorization.error=error instanceof ApiError?error:null
  if(method!=='status')throw error
  return {command:'authorization',schema_version:1,state:authorization.state,reason:authorization.state==='unavailable'?'service_unavailable':authorization.state}
 }finally{authorization.progress=null}
}
export const authorizationContext:GateContext={
 displayName:'Syno Reclaim',get locale(){return locale},
 reopen(fn){listeners.add(fn);return()=>{listeners.delete(fn)}},
 authorization:{canManage:()=>access?.canManage?.()||false,status:()=>read('status'),request:()=>read('request'),revoke:()=>read('revoke')},
}
