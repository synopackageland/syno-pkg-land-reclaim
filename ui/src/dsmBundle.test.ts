import { flushPromises } from '@vue/test-utils'
import { setBackend } from './api'
import { fixtureBackend } from './api/fixtures'
import { allowAuthorizationForTests, elapseGateRetries, fakeGateRetryTimers } from './authorizationFixture'
import type { Method } from './api/methods'
import { mount } from './dsmBundle'
import { inventorySource } from './dsm/snapshotInventory'
import type { AuthorizationClient } from './api/authorization'
import type { DsmAppWindow } from './dsm/appWindow'
import { portal } from './portal'
import { portalMessages, type Context } from '@synopackageland/sdk'
import type { Mock } from 'vitest'
import { locale } from './i18n'

const businessFixture = fixtureBackend({ scenario: 'normal' })
async function fixtureCgiData(body: string, authorization: unknown) {
  const request = JSON.parse(body) as { method: string; params: object }
  if (request.method === 'authorization.status') return authorization
  const method = (['session.get', 'jobs.list', 'volumes.list', 'overview.get', 'cache.status'] as Method[])
    .find(method => method === request.method)
  if (!method) throw new Error(`Unexpected CGI call: ${request.method}`)
  return businessFixture(method, request.params as never)
}

/** The DSM window's event surface, so a test can fire show/restore again. */
function fakeAppWindow() {
  const handlers = new Map<string, Set<() => void>>()
  const appWindow: DsmAppWindow = {
    toFront: vi.fn(),
    on: (event, callback) => {
      const set = handlers.get(event) ?? new Set<() => void>()
      set.add(callback)
      handlers.set(event, set)
    },
    un: (event, callback) => {
      handlers.get(event)?.delete(callback)
    },
  }
  return { appWindow, fire: (event: string) => { for (const callback of [...(handlers.get(event) ?? [])]) callback() } }
}

describe('DSM bundle', () => {
  // DSM's page declares no device-width viewport; a phone would lay a full
  // page out ~980px wide and never reach mobile mode. The SDK shell sets one.
  // As the file's first mount it pays the lazy pages' first import: ~0.5 s
  // alone, past 5 s on a gate host that also runs the Rust builds, and an
  // unfinished run leaves its app mounted under the tests that follow.
  it('gives a full-page launch a device-width viewport and restores it on unmount', async () => {
    setBackend(fixtureBackend({ scenario: 'normal' }))
    allowAuthorizationForTests()
    const viewport = () => document.head.querySelector<HTMLMetaElement>('meta[name="viewport"]')
    const element = document.createElement('div')
    document.body.append(element)
    const authorization = { client: { status: async () => ({ state: 'ready', reason: null }) } as AuthorizationClient }
    const shown = async () => {
      for (let i = 0; i < 50 && !element.querySelector('main'); i++) {
        await flushPromises()
        await vi.dynamicImportSettled()
      }
    }

    const desktop = mount(element, { lang: 'enu', authorization, shell: { fullPage: false } })
    await shown()
    expect(viewport()).toBeNull()
    desktop.unmount()

    const page = mount(element, { lang: 'enu', authorization, shell: { fullPage: true } })
    await shown()
    expect(viewport()?.content).toBe('width=device-width, initial-scale=1, viewport-fit=cover, interactive-widget=resizes-content')
    page.unmount()
    expect(viewport()).toBeNull()
    element.remove()
  }, 30_000)

  it('mounts the app, which the DSM window unmounts on close', async () => {
    setBackend(fixtureBackend({ scenario: 'normal' }))
    allowAuthorizationForTests()
    const element = document.createElement('div')
    document.body.append(element)

    const app = mount(element, { lang: 'enu' })
    await flushPromises()
    expect(element.querySelector('[data-ui-app]')).not.toBeNull()

    app.unmount()
    expect(element.innerHTML).toBe('')
    element.remove()
  })

  it('checks authorization again on every window open, including a reused instance', async () => {
    let checks = 0
    const client: AuthorizationClient = { status: async () => { checks += 1; return { state: 'ready', reason: null } } }
    const { appWindow, fire } = fakeAppWindow()
    const element = document.createElement('div')
    document.body.append(element)

    const app = mount(element, { lang: 'enu', appWindow, authorization: { client } })
    await flushPromises()
    expect(checks).toBe(1)

    fire('show')
    await flushPromises()
    expect(checks).toBe(2)

    app.reopen()
    await flushPromises()
    expect(checks).toBe(3)

    app.unmount()
    fire('show')
    await flushPromises()
    expect(checks).toBe(3)
    element.remove()
  })

  // DSM's own dialogs leave their window; a press on a floating panel does
  // not reach the window, so the app brings the window to the front itself.
  it('lets dialogs leave the DSM window and fronts the window when one is pressed', async () => {
    setBackend(fixtureBackend({ scenario: 'normal' }))
    allowAuthorizationForTests()
    const { appWindow } = fakeAppWindow()
    const element = document.createElement('div')
    document.body.append(element)

    const app = mount(element, { lang: 'enu', appWindow })
    await flushPromises()
    const host = document.querySelector('[data-ui-floating]')
    expect(host).not.toBeNull()

    host!.dispatchEvent(new Event('pointerdown'))
    expect(appWindow.toFront).toHaveBeenCalledOnce()

    app.unmount()
    expect(document.querySelector('[data-ui-floating]')).toBeNull()
    element.remove()
  })

  it('keeps dialogs inside the app outside a DSM window', async () => {
    setBackend(fixtureBackend({ scenario: 'normal' }))
    allowAuthorizationForTests()
    const element = document.createElement('div')
    document.body.append(element)

    const app = mount(element, { lang: 'enu' })
    await flushPromises()
    expect(element.querySelector('[data-ui-app]')).not.toBeNull()
    expect(document.querySelector('[data-ui-floating]')).toBeNull()

    app.unmount()
    element.remove()
  })

  it('reports unavailable, not a fake ready, when no CGI client is wired yet', async () => {
    const element = document.createElement('div')
    document.body.append(element)

    fakeGateRetryTimers()
    const app = mount(element, { lang: 'enu' })
    try {
      await elapseGateRetries()
      await flushPromises()
      expect(element.textContent).toContain('Authorization could not be checked')
    } finally {
      vi.useRealTimers()
      app.unmount()
      element.remove()
    }
  })
})

describe('DSM bundle native provider wiring (BINDING.md)', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  // An expired DSM session is a session condition; it must never turn into a
  // grant prompt.
  it('sends an SDK session_required answer to the login flow', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => ({
      ok: true,
      status: 200,
      text: async () => JSON.stringify({ success: false, error: { code: 'session_required', message: 'sign in to DSM again' } }),
    })))
    const element = document.createElement('div')
    document.body.append(element)
    let expired = 0

    const app = mount(element, {
      lang: 'enu',
      authorization: { onSessionExpired: () => { expired += 1 } },
      cgi: { url: 'cgi/api.cgi', headerProvider: () => ({}) },
    })
    await flushPromises()
    expect(expired).toBe(1)
    expect(element.textContent).toContain('Sign in to DSM again')
    expect(element.textContent).not.toContain('Authorize')
    app.unmount()
    element.remove()
  })

  it('attaches the provided headers and enters the app', async () => {
    setBackend(fixtureBackend({ scenario: 'normal' }))
    const sent: Record<string, string>[] = []
    const fetch = vi.fn(async (_input: string, init: { headers: Record<string, string>; body: string }) => {
      sent.push(init.headers)
      return {
        ok: true,
        status: 200,
        text: async () => JSON.stringify({ success: true, data: await fixtureCgiData(init.body, { command: 'authorization', schema_version: 1, state: 'ready', reason: null }) }),
      }
    })
    vi.stubGlobal('fetch', fetch)
    const element = document.createElement('div')
    document.body.append(element)

    const app = mount(element, {
      lang: 'enu',
      cgi: { url: 'cgi/api.cgi', headerProvider: () => ({ 'X-SYNO-TOKEN': 'token-1' }) },
    })
    // Follow the session/jobs/volumes lazy imports the ready flow starts.
    for (let i = 0; i < 50 && !element.querySelector('nav'); i += 1) {
      await flushPromises()
      await vi.dynamicImportSettled()
    }
    expect(element.querySelector('nav')).not.toBeNull()
    expect(fetch.mock.calls.length).toBeGreaterThan(1)
    expect(sent[0]['X-SYNO-TOKEN']).toBe('token-1')
    app.unmount()
    element.remove()
  })

  it("sends DSM's SynoToken by default", async () => {
    setBackend(fixtureBackend({ scenario: 'normal' }))
    vi.stubGlobal('SYNO', { SDS: { Session: { SynoToken: 'dsm-token' } } })
    const sent: Record<string, string>[] = []
    const fetch = vi.fn(async (_input: string, init: { headers: Record<string, string>; body: string }) => {
      sent.push(init.headers)
      return {
        ok: true,
        status: 200,
        text: async () => JSON.stringify({ success: true, data: await fixtureCgiData(init.body, { command: 'authorization', schema_version: 1, state: 'ready', reason: null }) }),
      }
    })
    vi.stubGlobal('fetch', fetch)
    const element = document.createElement('div')
    document.body.append(element)

    const app = mount(element, { lang: 'enu', cgi: { url: 'cgi/api.cgi' } })
    for (let i = 0; i < 50 && !element.querySelector('nav'); i += 1) {
      await flushPromises()
      await vi.dynamicImportSettled()
    }
    expect(element.querySelector('nav')).not.toBeNull()
    expect(sent[0]).toEqual({ 'X-SYNO-TOKEN': 'dsm-token', 'Content-Type': 'application/json' })
    vi.unstubAllGlobals()
    app.unmount()
    element.remove()
  })

  // The root helper answers the entry; the window's own DSM WebAPI the alias.
  it('installs the Application Portal access over the CGI and the window WebAPI', async () => {
    setBackend(fixtureBackend({ scenario: 'normal' }))
    allowAuthorizationForTests()
    const methods: string[] = []
    vi.stubGlobal('fetch', vi.fn(async (_input: string, init: { body: string }) => {
      methods.push((JSON.parse(init.body) as { method: string }).method)
      return { ok: true, status: 200, text: async () => JSON.stringify({ success: true, data: { state: 'inactive', entry: 'registered', exit: 0 } }) }
    }))
    const webApi = vi.fn(async (api: string) => api === 'SYNO.API.Info'
      ? { 'SYNO.Core.AppPortal': { maxVersion: 2 } }
      : { id: 'SYNO.SDS.SynoReclaim.Instance', alias: 'reclaim' })
    const element = document.createElement('div')
    document.body.append(element)

    const app = mount(element, { lang: 'enu', cgi: { url: 'cgi/api.cgi', headerProvider: () => ({}) }, webApi })
    expect(portal.value!.messages.title).toBe('Application Portal')
    const status = await portal.value!.status()
    expect(methods).toContain('portal.status')
    expect(status).toMatchObject({ state: 'active', alias: 'reclaim', entry: 'registered', defaultAlias: 'synoreclaim' })
    vi.unstubAllGlobals()
    app.unmount()
    expect(portal.value).toBeUndefined()
    element.remove()
  })

  // The helper answers a change DSM refused as a document (exit 1); the SDK
  // access reads only a rejection as a failed change.
  it('rejects PORTAL_FAILED when the helper answers a refused entry change', async () => {
    setBackend(fixtureBackend({ scenario: 'normal' }))
    allowAuthorizationForTests()
    vi.stubGlobal('fetch', vi.fn(async () => ({
      ok: true, status: 200,
      text: async () => JSON.stringify({ success: true, data: { state: 'inactive', entry: 'unregistered', error: 'PORTAL_FAILED', exit: 1 } }),
    })))
    const element = document.createElement('div')
    const app = mount(element, { lang: 'enu', cgi: { url: 'cgi/api.cgi', headerProvider: () => ({}) }, webApi: vi.fn() })
    await expect(portal.value!.register!()).rejects.toEqual({ code: 'PORTAL_FAILED' })
    await expect(portal.value!.unregister!()).rejects.toEqual({ code: 'PORTAL_FAILED' })
    vi.unstubAllGlobals()
    app.unmount()
  })

  it('reads the portal as unavailable when the window has no DSM WebAPI', async () => {
    setBackend(fixtureBackend({ scenario: 'normal' }))
    allowAuthorizationForTests()
    vi.stubGlobal('fetch', vi.fn(async () => ({
      ok: true, status: 200,
      text: async () => JSON.stringify({ success: true, data: { state: 'inactive', entry: 'registered', exit: 0 } }),
    })))
    const element = document.createElement('div')
    const app = mount(element, { lang: 'enu', cgi: { url: 'cgi/api.cgi', headerProvider: () => ({}) } })
    expect((await portal.value!.status()).state).toBe('unavailable')
    vi.unstubAllGlobals()
    app.unmount()
  })

  it('shows the SDK panel text in the window language', () => {
    setBackend(fixtureBackend({ scenario: 'normal' }))
    allowAuthorizationForTests()
    const element = document.createElement('div')
    const app = mount(element, { lang: 'enu', cgi: { url: 'cgi/api.cgi', headerProvider: () => ({}) } })
    expect(portal.value!.messages).toEqual(portalMessages(locale))
    app.unmount()
  })

  it('installs no Application Portal access without the package CGI', () => {
    setBackend(fixtureBackend({ scenario: 'normal' }))
    allowAuthorizationForTests()
    const element = document.createElement('div')
    const app = mount(element, { lang: 'enu' })
    expect(portal.value).toBeUndefined()
    app.unmount()
  })

})

it('shows the approval wait when the window reports a root task past 30 s',async()=>{
 let report!:(event:unknown)=>void
 vi.stubGlobal('fetch',vi.fn(async()=>({ok:true,status:200,text:async()=>JSON.stringify({success:true,data:{command:'authorization',schema_version:1,state:'authorization_required',reason:'grant_missing'}})})))
 const element=document.createElement('div');document.body.append(element)
 const app=mount(element,{cgi:{url:'/webman/3rdparty/SynoReclaim/cgi/api.cgi',headerProvider:async()=>({})},createAuthorization:(call,progress)=>{report=progress!;return {status:async()=>await call('authorization.status') as import('./context').AuthorizationStatus,request:()=>new Promise(()=>{}),revoke:async()=>{throw Error('unreachable')}}}})
 try{
  await flushPromises()
  report({op:'bootstrap',state:'running',elapsedMs:31000})
  await flushPromises()
  expect(element.textContent).toContain('Still waiting for DSM')
 }finally{app.unmount();element.remove();vi.unstubAllGlobals()}
})

// The synopkgland build's DSM window loads app/app.js and mounts the app it
// registers with its own ctx: the package CGI, the executor approval and the
// portal entry come from there.
describe('the package window mount', () => {
 const ready={command:'authorization',schema_version:1,state:'ready',reason:null}
 const required={command:'authorization',schema_version:1,state:'authorization_required',reason:'grant_missing'}
 /** The window's ctx.call: an answer for every method, or the fixture's answer to each request. */
 const callOf=(answer?:unknown)=>vi.fn(async(method:string,params?:Record<string,unknown>)=>answer??fixtureCgiData(JSON.stringify({method,params:params??{}}),ready)) as unknown as Context['call'] & Mock
 it('registers the app for the generated DSM window',()=>{
  expect(window.DsmPackageApps?.SynoReclaim?.mount).toBeTypeOf('function')
 })
 it('approves the executor in the window with the password the administrator types',async()=>{
  const call=callOf(required)
  const authorize=vi.fn(async()=>({state:'ready' as const}))
  const executor={status:vi.fn(async()=>({state:'needs-admin-approval' as const})),authorize,control:vi.fn()}
  const element=document.createElement('div');document.body.append(element)
  const app=window.DsmPackageApps!.SynoReclaim!.mount(element,{lang:'enu',call,fullPage:false,user:{name:'admin',isAdmin:true},executor})
  try{
   await flushPromises()
   expect(document.querySelector('[role="dialog"]')).toBeNull()
   const password=element.querySelector<HTMLInputElement>('input[type="password"]')!
   password.value='secret';password.dispatchEvent(new Event('input'))
   await flushPromises()
   element.querySelector('form')!.dispatchEvent(new Event('submit'))
   await flushPromises()
   expect(authorize).toHaveBeenCalledWith('secret')
  }finally{app.unmount();element.remove()}
 })
 // A Portal or standalone page has no title bar, so the approval names the App with the window's displayName and appIcon.
 it.each([
  [true,'Syno Reclaim','webman/3rdparty/SynoReclaim/images/icon_64.png'],
  [false,undefined,undefined],
 ])('names the app above the approval on a full page (fullPage %s)',async(fullPage,name,icon)=>{
  const call=callOf(required)
  const executor={status:vi.fn(async()=>({state:'needs-admin-approval' as const})),authorize:vi.fn(),control:vi.fn()}
  const element=document.createElement('div');document.body.append(element)
  const app=window.DsmPackageApps!.SynoReclaim!.mount(element,{lang:'enu',call,fullPage,displayName:'Syno Reclaim',appIcon:'webman/3rdparty/SynoReclaim/images/icon_64.png',user:{name:'admin',isAdmin:true},executor})
  try{
   await flushPromises()
   const brand=element.querySelector('.executor-brand')
   expect(brand?.textContent).toBe(name)
   expect(brand?.querySelector('img')?.getAttribute('src')).toBe(icon)
  }finally{app.unmount();element.remove()}
 })
 it('shows the app once the executor reads ready, and the form again when it no longer does',async()=>{
  const call=callOf()
  type Status={state:'ready'|'blocked';retry?:boolean}
  // Every view that subscribes hears each result, as the window's access does.
  let current:Status={state:'ready'}
  const listeners:Array<(status:Status)=>void>=[]
  const publish=(status:Status)=>{current=status;listeners.forEach(listener=>listener(status))}
  const executor={status:vi.fn(async()=>current),authorize:vi.fn(),control:vi.fn(),subscribe:(listener:(status:Status)=>void)=>{listeners.push(listener)}}
  const element=document.createElement('div');document.body.append(element)
  const app=window.DsmPackageApps!.SynoReclaim!.mount(element,{lang:'enu',call,fullPage:false,user:{name:'admin',isAdmin:true},executor})
  try{
   for(let i=0;i<50&&!element.querySelector('main');i++){await flushPromises();await vi.dynamicImportSettled()}
   expect(element.querySelector('main')).not.toBeNull()
   expect(element.querySelector('input[type="password"]')).toBeNull()
   publish({state:'blocked',retry:true})
   await flushPromises()
   expect(element.querySelector('main')).toBeNull()
   expect(element.querySelector('input[type="password"]')).not.toBeNull()
  }finally{app.unmount();element.remove()}
 })
 it("answers the Gate's Authorize by reading the executor again, never without a password",async()=>{
  const call=callOf(required)
  const executor={status:vi.fn(async()=>({state:'ready' as const})),authorize:vi.fn(),control:vi.fn()}
  const element=document.createElement('div');document.body.append(element)
  const app=window.DsmPackageApps!.SynoReclaim!.mount(element,{lang:'enu',call,fullPage:false,user:{name:'admin',isAdmin:true},executor})
  try{
   await flushPromises()
   const reads=executor.status.mock.calls.length
   const button=[...document.querySelectorAll('[role="dialog"] button')].find(b=>b.textContent?.includes('Authorize')) as HTMLButtonElement
   button.click()
   await flushPromises()
   expect(executor.status.mock.calls.length).toBe(reads+1)
   expect(executor.authorize).not.toHaveBeenCalled()
  }finally{app.unmount();element.remove()}
 })
 it('tells a non-administrator that an administrator must approve, with no password field',async()=>{
  const call=callOf({command:'authorization',schema_version:1,state:'not_admin',reason:'not_admin'})
  const executor={status:vi.fn(async()=>({state:'needs-admin-approval' as const})),authorize:vi.fn(),control:vi.fn()}
  const element=document.createElement('div');document.body.append(element)
  const app=window.DsmPackageApps!.SynoReclaim!.mount(element,{lang:'enu',call,fullPage:false,user:{name:'user',isAdmin:false},executor})
  try{
   await flushPromises()
   expect(element.textContent).toContain('This app needs administrator approval')
   expect(element.textContent).toContain('Ask an administrator to open this app and approve it once')
   expect(element.querySelector('input[type="password"]')).toBeNull()
   expect(element.querySelector('main')).toBeNull()
  }finally{app.unmount();element.remove()}
 })
 // The window's call posts to the page-relative CGI (an Application Portal
 // alias on 80/443 answers /webman/... with 403) and repeats a call the SDK
 // refused while the App is stopped or starting; the App adds no retry.
 it("calls the CGI through the window's call, never its own fetch",async()=>{
  const fetch=vi.fn()
  vi.stubGlobal('fetch',fetch)
  const call=callOf()
  const element=document.createElement('div')
  const app=window.DsmPackageApps!.SynoReclaim!.mount(element,{lang:'enu',fullPage:false,call})
  try{
   await flushPromises()
   expect(call).toHaveBeenCalledWith('authorization.status',{},expect.anything())
   expect(fetch).not.toHaveBeenCalled()
  }finally{app.unmount();vi.unstubAllGlobals()}
 })
 it("uses the window's WebAPI for snapshot inventory and releases it on unmount",async()=>{
  const call=callOf(ready)
  const webApi=vi.fn(async()=>({luns:[],snapshots:[]}))
  const element=document.createElement('div')
  const app=window.DsmPackageApps!.SynoReclaim!.mount(element,{lang:'enu',call,fullPage:false,webApi})
  try{
   expect(await inventorySource().inventory()).not.toBeNull()
   expect(webApi).toHaveBeenCalledWith('SYNO.Core.ISCSI.LUN','list',1,{})
  }finally{app.unmount()}
  expect(await inventorySource().inventory()).toBeNull()
 })
 it("uses the window's portal access",async()=>{
  const call=callOf(ready)
  const access={status:vi.fn()} as unknown as import('@synopackageland/sdk').PortalAccess
  const element=document.createElement('div')
  const app=window.DsmPackageApps!.SynoReclaim!.mount(element,{lang:'enu',call,fullPage:false,portal:access})
  expect(portal.value).toBe(access)
  app.unmount()
  expect(portal.value).toBeUndefined()
 })
 it("hands the window's launch to the shell",async()=>{
  const call=callOf()
  const element=document.createElement('div');document.body.append(element)
  const app=window.DsmPackageApps!.SynoReclaim!.mount(element,{lang:'enu',call,fullPage:true,displayName:'Syno Reclaim',appIcon:'webman/3rdparty/SynoReclaim/images/icon_64.png'})
  try{
   for(let i=0;i<50&&!element.querySelector('main');i++){await flushPromises();await vi.dynamicImportSettled()}
   expect(element.querySelector('[data-ui-layout-mode]')).not.toBeNull()
   expect(document.head.querySelector<HTMLMetaElement>('meta[name="viewport"]')?.content).toBe('width=device-width, initial-scale=1, viewport-fit=cover, interactive-widget=resizes-content')
  }finally{app.unmount();element.remove()}
 })
})
