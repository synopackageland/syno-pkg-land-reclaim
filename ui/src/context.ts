import type {InjectionKey} from 'vue'
import type {Context as SdkContext,ShellContext} from '@synopackageland/sdk'
/** Minimal canonical authorization context, supplied by the native-layout adapter. */
export interface AuthorizationStatus {command:'authorization';schema_version:1;state:'ready'|'authorization_required'|'not_admin'|'session_expired'|'unavailable';reason:string|null}
export interface Context {
 displayName?:string
 locale:string
 reopen(fn:()=>void):()=>void
 authorization?:{canManage?():boolean;status():Promise<AuthorizationStatus>;request():Promise<AuthorizationStatus>;revoke():Promise<AuthorizationStatus>}
}
/** What the SDK AuthorizationGate reads from its ctx. */
export type GateContext=Pick<SdkContext,'locale'|'displayName'|'reopen'|'authorization'>
/** The launch the shell lays out: the DSM window's ctx, or the dev page's. */
export const SHELL_CONTEXT:InjectionKey<ShellContext>=Symbol('shellContext')
