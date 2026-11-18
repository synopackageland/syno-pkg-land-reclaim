import {ref} from 'vue'
import {createUiApp} from './createUiApp'
import {setBackend} from './api'
import {cgiBackend,createCallTransport,createCgiTransport,type CgiConfig,type CgiFetch,type CgiTransport,type WindowCall} from './api/cgi'
import './styles'
import App from './App.vue'
import {APP_VISIBLE,bindAppReopen,bindAppVisibility} from './usage/appVisibility'
import {DSM_WINDOW_FRONT,type DsmAppWindow} from './dsm/appWindow'
import {configureAuthorization,openApp,reportAuthorizationProgress,type AuthorizationAccess} from './stores/authorization'
import {installEscapeGuard} from './composables/useEscapeGuard'
import {createAuthorizationClient,unavailableAuthorizationClient,type AuthorizationClient} from './api/authorization'
import {synoTokenHeaders} from './dsm/synoToken'
import {createPortalAccess,type PortalAccess,type PortalOptions} from '@synopackageland/sdk'
import {configurePortal} from './portal'
import {locale} from './i18n'
import {SHELL_CONTEXT,type AuthorizationStatus} from './context'
import type {Context,ExecutorSetup,ShellContext} from '@synopackageland/sdk'
import {ExecutorSetupKey} from '@synopackageland/sdk/vue'
import {bindChunkRecovery} from './dsm/chunkRecovery'
import {createDsmInventorySource,dsmInventorySource,inventorySource,setDsmInventorySource} from './dsm/snapshotInventory'
import {createRecycleBinSource,setRecycleBinSource} from './dsm/recycleBins'
export type MountOptions={
 lang?:string
 appWindow?:DsmAppWindow
 authorization?:{client?:AuthorizationClient;access?:AuthorizationAccess;onSessionExpired?:()=>void}
 cgi?:CgiConfig
 /** The package window's ctx.call, in place of a CGI transport built here. */
 call?:WindowCall
 /** Fixed build-generated host runtime; CGI cannot supply tasks or shell. */
 createAuthorization?:(call:(method:string)=>Promise<unknown>,progress?:(event:unknown)=>void)=>AuthorizationAccess
 /** The window's DSM WebAPI (sendWebAPI) in the administrator's session. */
 webApi?:PortalOptions['send']
 /** The window's own Application Portal access, in place of one built here. */
 portal?:PortalAccess
 /** The launch the shell lays out: a DSM window or a standalone (Portal) page. */
 shell?:ShellContext
 /** The DSM window's executor approval, confirmed with the administrator's password. */
 executor?:ExecutorSetup
}
type DsmDesktop={AppLaunch?:(app:string,params:Record<string,string>)=>void}
const launch=(app:string,params:Record<string,string>)=>{(window.SYNO?.SDS as DsmDesktop|undefined)?.AppLaunch?.(app,params)}

/** The helper answers an entry change DSM refused as a document (exit 1,
 * PORTAL_FAILED); the SDK access reads only a rejection as a failed change. */
async function portalCall(transport:CgiTransport,method:string){
 const document=await transport({method,params:{}}) as {exit?:unknown}|null
 if(method!=='portal.status'&&document?.exit!==0)throw {code:'PORTAL_FAILED'}
 return document
}
/** Outside a DSM window DSM's WebAPI is out of reach: the alias reads unavailable. */
const noWebApi:PortalOptions['send']=()=>Promise.reject({code:'portal_unavailable'})
export type MountedApp={unmount:()=>void;reopen:()=>void}
const fetchCgi:CgiFetch=(input,init)=>fetch(input,init)
export function mount(element:Element,options?:MountOptions):MountedApp{
 const inventory=createDsmInventorySource({webApi:options?.webApi})
 setDsmInventorySource(inventory)
 setRecycleBinSource(options?.webApi?createRecycleBinSource({webApi:options.webApi}):null)
 const cgi=options?.cgi?{headerProvider:synoTokenHeaders,...options.cgi}:undefined
 const transport=options?.call?createCallTransport(options.call):cgi?createCgiTransport(fetchCgi,cgi):undefined
 const client=options?.authorization?.client??(transport?createAuthorizationClient(transport):unavailableAuthorizationClient)
 const access=options?.authorization?.access??(transport&&options?.createAuthorization?options.createAuthorization(async method=>{
  if(method==='authorization.status')return {command:'authorization',schema_version:1,...await client.status()}
  return transport({method,params:{}})
 },reportAuthorizationProgress):undefined)
 if(transport)setBackend(cgiBackend(transport))
 // Below DSM 7.2 the root helper owns the entry; only administrators pass
 // the package's entry gate.
 configurePortal(options?.portal??(transport?createPortalAccess({appId:'SYNO.SDS.SynoReclaim.Instance',defaultAlias:'synoreclaim',locale,
  location:window.location,call:method=>portalCall(transport,method),send:options?.webApi??noWebApi,isAdmin:()=>true,launch,entry:true}):undefined))
 configureAuthorization({client,access,onSessionExpired:options?.authorization?.onSessionExpired})
 const app=createUiApp(App)
 const stopChunkRecovery=bindChunkRecovery(app,options?.shell)
 const visible=ref(true)
 const reopen=()=>{void openApp()}
 const stopVisibility=options?.appWindow?bindAppVisibility(options.appWindow,visible):undefined
 const stopReopen=options?.appWindow?bindAppReopen(options.appWindow,reopen):undefined
 app.provide(APP_VISIBLE,visible)
 const appWindow=options?.appWindow
 if(appWindow)app.provide(DSM_WINDOW_FRONT,()=>{appWindow.toFront()})
 if(options?.shell)app.provide(SHELL_CONTEXT,options.shell)
 if(options?.executor)app.provide(ExecutorSetupKey,options.executor)
 app.mount(element)
 const stopEscapeGuard=options?.appWindow?installEscapeGuard(element):undefined
 return {unmount:()=>{stopChunkRecovery();stopEscapeGuard?.();stopReopen?.();stopVisibility?.();app.unmount();configurePortal(undefined);if(inventorySource()===inventory)setDsmInventorySource(dsmInventorySource);setRecycleBinSource(null)},reopen}
}

/** What the synopkgland build's DSM window hands the app it mounts. */
export type PackageWindowContext=ShellContext&Pick<Context,'executor'|'call'|'webApi'>&{
 lang?:string
 appWindow?:DsmAppWindow
 portal?:PortalAccess
}
/** The App answers the approval status; approving is the executor's, with the
 * password the window asks for (the SDK's ExecutorApproval). The Gate's request only reads
 * the executor again, so a lost approval brings that form back. There is no
 * revoke: the administrator disables the package or uninstalls it. */
export function mountPackageApp(element:Element,ctx:PackageWindowContext):MountedApp{
 const executor=ctx.executor
 return mount(element,{lang:ctx.lang,appWindow:ctx.appWindow,call:ctx.call,webApi:ctx.webApi,portal:ctx.portal,shell:ctx,executor,
  createAuthorization:executor?call=>{
   const status=async()=>await call('authorization.status') as AuthorizationStatus
   return {canManage:()=>true,status,request:async()=>{await executor.status();return status()},revoke:status}
  }:undefined})
}
window.DsmPackageApps={...window.DsmPackageApps,SynoReclaim:{mount:mountPackageApp}}
