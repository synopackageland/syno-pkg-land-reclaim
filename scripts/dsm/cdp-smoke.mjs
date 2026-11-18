#!/usr/bin/env node
// End-to-end DSM UI smoke driver for one explicitly authorized volume scan.
// It uses the logged-in browser tab and never handles credentials or tokens.

import fs from 'node:fs/promises'
import path from 'node:path'

const args = process.argv.slice(2)
const has = name => args.includes(name)
const option = (name, fallback) => {
  const index = args.indexOf(name)
  return index >= 0 ? args[index + 1] : fallback
}
const dsmUrl = (option('--dsm-url', process.env.DSM_E2E_URL || '') || '').replace(/\/+$/, '')
if (!/^https:\/\/[^/]+$/.test(dsmUrl)) throw new Error('Pass --dsm-url https://<host>:<port> or set DSM_E2E_URL')
const port = Number(option('--port', '9444'))
const output = path.resolve(option('--output', 'target/dsm-smoke/screenshots'))
const locales = option('--locales', 'enu,cht').split(',').filter(Boolean)
const startScan = has('--start-volume-scan')
const resumeScan = has('--resume-after-scan')
if (startScan === resumeScan) throw new Error('Pass exactly one of --start-volume-scan or --resume-after-scan')
if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('Invalid CDP port')
if (locales.length !== 2 || locales[0] !== 'enu' || locales[1] !== 'cht') {
  throw new Error('Locales must be enu,cht in that order')
}
await fs.mkdir(output, { recursive: true })
const launchMarker = path.join(output, 'scan-launch.json')
let savedLaunch = null
const previousAudits = []
if (startScan) {
  try {
    await fs.access(launchMarker)
    throw new Error('A scan-launch marker already exists; this run will not launch another scan')
  } catch (error) {
    if (error?.code !== 'ENOENT') throw error
  }
} else {
  savedLaunch = JSON.parse(await fs.readFile(launchMarker, 'utf8'))
  if (savedLaunch.intent !== 'one UI scan'
    || savedLaunch.target !== '/volume1'
    || savedLaunch.mode !== 'gentle'
    || savedLaunch.request?.targets?.length !== 1
    || savedLaunch.request?.targets?.[0] !== '/volume1'
    || savedLaunch.request?.mode !== 'gentle'
    || !savedLaunch.response?.job_id) {
    throw new Error('The existing scan-launch marker does not authorize resuming this exact Gentle /volume1 scan')
  }
  for (const filename of ['smoke-audit.json']) {
    try {
      previousAudits.push(JSON.parse(await fs.readFile(path.join(output, filename), 'utf8')))
    } catch (error) {
      if (error?.code !== 'ENOENT') throw error
    }
  }
  if (!previousAudits.length) {
    for (const filename of ['smoke-audit-prelaunch.json', 'smoke-audit-prelaunch-2.json']) {
      try {
        previousAudits.push(JSON.parse(await fs.readFile(path.join(output, filename), 'utf8')))
      } catch (error) {
        if (error?.code !== 'ENOENT') throw error
      }
    }
  }
}

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms))
const base = 'http://127.0.0.1:' + port
const tabs = await (await fetch(base + '/json/list')).json()
const tab = tabs.find(item => item.type === 'page' && item.url.startsWith(dsmUrl + '/'))
if (!tab) throw new Error('Open a logged-in DSM tab at ' + dsmUrl + ' in the CDP browser first')
const ws = new WebSocket(tab.webSocketDebuggerUrl)
await new Promise((resolve, reject) => {
  ws.addEventListener('open', resolve, { once: true })
  ws.addEventListener('error', reject, { once: true })
})

let nextId = 0
const pending = new Map()
const cdpErrors = []
const safeText = value => String(value ?? '')
  .replace(/SynoToken=[^&\s]+/gi, 'SynoToken=[redacted]')
  .replace(/(password|passwd|serial(?:number)?)\s*[=:]\s*[^&\s]+/gi, '$1=[redacted]')
  .slice(0, 300)
ws.addEventListener('message', event => {
  const message = JSON.parse(event.data)
  if (message.id && pending.has(message.id)) {
    const entry = pending.get(message.id)
    pending.delete(message.id)
    if (message.error) entry.reject(new Error(entry.method + ': ' + message.error.message))
    else entry.resolve(message.result)
    return
  }
  if (message.method === 'Runtime.consoleAPICalled' && message.params.type === 'error') {
    const first = message.params.args?.find(item => item.type === 'string')
    cdpErrors.push({ kind: 'console', text: first ? safeText(first.value) : 'console.error' })
  }
  if (message.method === 'Runtime.exceptionThrown') {
    cdpErrors.push({ kind: 'exception', text: safeText(message.params.exceptionDetails?.text ?? 'uncaught exception') })
  }
})
const send = (method, params = {}) => new Promise((resolve, reject) => {
  const id = ++nextId
  pending.set(id, { resolve, reject, method })
  ws.send(JSON.stringify({ id, method, params }))
})
const evaluate = async (expression, { awaitPromise = true } = {}) => {
  let response
  try {
    response = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise })
  } catch (error) {
    throw new Error('Runtime.evaluate: ' + safeText(error?.message ?? error))
  }
  if (response.exceptionDetails) {
    throw new Error(safeText(response.result?.description ?? response.exceptionDetails.text ?? 'page evaluation failed'))
  }
  return response.result?.value
}
const waitFor = async (expression, timeout = 12000) => {
  const started = Date.now()
  while (Date.now() - started < timeout) {
    const value = await evaluate(expression)
    if (value) return value
    await sleep(150)
  }
  throw new Error('Timed out waiting for DSM UI state')
}
const waitForApiQuiet = async (quietMs = 1500, timeout = 30000) => {
  const started = Date.now()
  let quietSince = null
  while (Date.now() - started < timeout) {
    const state = await evaluate('(()=>{const a=window.__synoreclaimAudit;return {started:a?.requestsStarted??0,inFlight:a?.inFlight??0}})()')
    if (state.started > 0 && state.inFlight === 0) {
      quietSince ??= Date.now()
      if (Date.now() - quietSince >= quietMs) return
    } else {
      quietSince = null
    }
    await sleep(100)
  }
  throw new Error('Timed out waiting for DSM API requests to settle')
}
const capture = async name => {
  const shot = await send('Page.captureScreenshot', { format: 'png', fromSurface: true, captureBeyondViewport: false })
  await fs.writeFile(path.join(output, name), Buffer.from(shot.data, 'base64'))
}
const shotPrefix = locale => locale === 'cht' ? 'zh-tw-' : 'en-'
const activeAppText = 'document.querySelector(".x-window.active-win")?.innerText||""'
const readyApp = 'Boolean(document.querySelector(".x-window.active-win .toolbar-title h1"))'
const allApiRequests = []
const allBusyResponses = []
const allRuntimeErrors = []
const result = {
  screenshots: [],
  pageVisits: [],
  scan: null,
  preStartJobCheck: null,
  volumeMounts: [],
  systemPartitionRouteExposed: [],
  apiRequests: [],
  webapiFailures: [],
  busyResponses: [],
  domainResponses: [],
  runtimeErrors: [],
  cdpErrors,
  review: [],
  routeFallbacks: [],
  logout: null,
}
const previousScan = previousAudits.find(audit => audit.scan?.response?.job_id === savedLaunch?.response?.job_id)?.scan
if (resumeScan) {
  result.scan = {
    target: savedLaunch.target,
    mode: savedLaunch.mode,
    launchedVia: 'New Scan dialog primary button',
    request: savedLaunch.request,
    response: savedLaunch.response,
    started_at: previousScan?.started_at ?? savedLaunch.created_at,
    marker: path.basename(launchMarker),
    resumed_at: new Date().toISOString(),
  }
}

const installAuditHook = async () => evaluate([
  '(()=>{',
  'if(window.__synoreclaimAudit?.hooked)return true;',
  'const api=window.SYNO?.API;',
  'if(!api?.Request)return false;',
  'const audit=window.__synoreclaimAudit={hooked:true,apiRequests:[],busyResponses:[],runtimeErrors:[],latestActiveJobs:[],jobsSeen:false,volumeMounts:[],scanStart:null,scanRequest:null,requestsStarted:0,inFlight:0};',
  'const activeStates=["queued","running","pausing","cancelling","paused"];',
  'const safeJobs=jobs=>(Array.isArray(jobs)?jobs:[]).filter(j=>activeStates.includes(j.state)).map(j=>({job_id:typeof j.job_id==="string"?j.job_id:null,kind:typeof j.kind==="string"?j.kind:null,state:typeof j.state==="string"?j.state:null,profile:typeof j.profile==="string"?j.profile:null}));',
  'const original=api.Request;',
  'api.Request=new Proxy(original,{apply(target,thisArg,args){',
  'const options=args[0];',
  'if(!options?.api?.startsWith("SYNO.SynoReclaim.")||typeof options.callback!=="function")return Reflect.apply(target,thisArg,args);',
  'const callback=options.callback;',
  'let completed=false;audit.requestsStarted+=1;audit.inFlight+=1;',
  'const wrapped={...options,callback:function(...responseArgs){',
  'if(!completed){completed=true;audit.inFlight=Math.max(0,audit.inFlight-1)}',
  'const [ok,response]=responseArgs;',
  'const raw= response?.errors;',
  'const detail=raw&&!Array.isArray(raw)?{reason:typeof raw.reason==="string"?raw.reason.slice(0,40):null,exit:typeof raw.exit==="number"?raw.exit:null,stderr_code:typeof raw.stderr_code==="string"?raw.stderr_code.slice(0,40):null,document_completion:typeof raw.document?.completion==="string"?raw.document.completion:null,document_error_codes:Array.isArray(raw.document?.errors)?raw.document.errors.map(e=>e?.code).filter(x=>typeof x==="string").slice(0,20):[]}:null;',
  'const errorCodes=Array.isArray(raw)?raw.map(e=>e?.code).filter(x=>typeof x==="string").slice(0,20):[];',
  'const row={captured_at:new Date().toISOString(),api:options.api,method:options.method,ok:Boolean(ok),code:typeof response?.code==="number"?response.code:null,completion:typeof response?.completion==="string"?response.completion:null,exit:typeof response?.exit==="number"?response.exit:null,errorCodes:errorCodes,errorDetail:detail};',
  'audit.apiRequests.push(row);',
  'if(options.api==="SYNO.SynoReclaim.Volumes"&&options.method==="list"&&ok&&Array.isArray(response?.volumes))audit.volumeMounts=response.volumes.map(v=>({mount:v?.mount?.path??null,metadata_access:Boolean(v?.metadata_access),share_count:Array.isArray(v?.shares)?v.shares.length:null}));',
  'if(options.api==="SYNO.SynoReclaim.Jobs"&&options.method==="list"&&ok){audit.latestActiveJobs=safeJobs(response?.jobs);audit.jobsSeen=true;}',
  'if(options.api==="SYNO.SynoReclaim.Scan"&&options.method==="start"){const p=options.params?.params||{};audit.scanRequest={targets:Array.isArray(p.targets)?p.targets.map(t=>typeof t==="string"?t:t?.path??null):[],mode:typeof p.mode==="string"?p.mode:null,snapshots:typeof p.snapshots==="boolean"?p.snapshots:null};audit.scanStart={ok:Boolean(ok),code:row.code,completion:row.completion,job_id:typeof response?.job_id==="string"?response.job_id:null,errorDetail:detail};}',
  'if(row.code===4002){let origin="undetermined";if(detail?.reason==="busy"&&(detail.exit===-1||detail.exit===null)&&detail.document_completion===null&&!detail.stderr_code)origin="dsm_shim_slot_busy";else if(detail?.reason==="busy"&&detail.exit===4&&detail.document_completion==="busy")origin="cli_exit_4_busy";const strip=document.querySelector(".job-summary")?.innerText||"";const title=document.querySelector(".x-window.active-win .toolbar-title h1")?.innerText||"";audit.busyResponses.push({...row,origin,activeJobs:[...audit.latestActiveJobs],jobStripText:String(strip).replace(/SynoToken=[^&\\s]+/gi,"SynoToken=[redacted]").slice(0,300),pageTitle:String(title).slice(0,100)});}',
  'if(!ok||(row.code!==null&&row.code!==0)||errorCodes.length)audit.runtimeErrors.push({type:"webapi",api:row.api,method:row.method,ok:row.ok,code:row.code,completion:row.completion,errorCodes:errorCodes});',
  'return Reflect.apply(callback,this,responseArgs);',
  '}};',
  'try{return Reflect.apply(target,thisArg,[wrapped,...args.slice(1)])}catch(error){if(!completed){completed=true;audit.inFlight=Math.max(0,audit.inFlight-1)}throw error}',
  '}});',
  'window.addEventListener("error",event=>{if(String(event.filename||"").includes("SynoReclaim"))audit.runtimeErrors.push({type:"error",message:String(event.message||"").slice(0,300)});});',
  'window.addEventListener("unhandledrejection",event=>{const message=String(event.reason?.message||event.reason||"");if(message.includes("SynoReclaim"))audit.runtimeErrors.push({type:"unhandledrejection",message:message.slice(0,300)});});',
  'return true;',
  '})()',
].join(''))

const collectAudit = async () => {
  const audit = await evaluate('window.__synoreclaimAudit||{apiRequests:[],busyResponses:[],runtimeErrors:[],volumeMounts:[],latestActiveJobs:[]}')
  allApiRequests.push(...(audit.apiRequests || []))
  allBusyResponses.push(...(audit.busyResponses || []))
  allRuntimeErrors.push(...(audit.runtimeErrors || []).filter(item => item.type !== 'webapi'))
  if (audit.volumeMounts?.length) result.volumeMounts = audit.volumeMounts
  await evaluate('(()=>{const a=window.__synoreclaimAudit;if(a){a.apiRequests.length=0;a.busyResponses.length=0;a.runtimeErrors.length=0}})()')
}

const requestDsm = async (api, method, params = {}) => {
  const expression = [
    'new Promise(resolve=>{let ended=false;const finish=value=>{if(ended)return;ended=true;clearTimeout(timer);resolve(value)};',
    'const timer=setTimeout(()=>finish({timeout:true}),15000);',
    'window.SYNO.API.Request({api:' + JSON.stringify(api) + ',method:' + JSON.stringify(method) + ',version:1,params:{params:' + JSON.stringify(params) + '},callback:(ok,r)=>{',
    'const raw=r?.errors;const detail=raw&&!Array.isArray(raw)?{reason:raw.reason??null,exit:typeof raw.exit==="number"?raw.exit:null,stderr_code:raw.stderr_code??null,document_completion:raw.document?.completion??null,document_error_codes:Array.isArray(raw.document?.errors)?raw.document.errors.map(e=>e?.code).filter(x=>typeof x==="string"):[]}:null;',
    'const out={captured_at:new Date().toISOString(),ok:Boolean(ok),code:typeof r?.code==="number"?r.code:null,completion:typeof r?.completion==="string"?r.completion:null,job_id:typeof r?.job_id==="string"?r.job_id:null,state:typeof r?.state==="string"?r.state:null,command:typeof r?.command==="string"?r.command:null,errorDetail:detail,errorCodes:Array.isArray(raw)?raw.map(e=>e?.code).filter(x=>typeof x==="string"):[]};',
    'if(' + JSON.stringify(api) + '==="SYNO.SynoReclaim.Jobs"&&' + JSON.stringify(method) + '==="list")out.jobs=(r?.jobs||[]).map(j=>({job_id:j?.job_id??null,kind:j?.kind??null,state:j?.state??null,profile:j?.profile??null,result_completion:j?.result?.completion??null,result_exit_code:typeof j?.result?.exit_code==="number"?j.result.exit_code:null}));',
    'if(' + JSON.stringify(api) + '==="SYNO.SynoReclaim.Volumes"&&' + JSON.stringify(method) + '==="list")out.volumes=(r?.volumes||[]).map(v=>({mount:v?.mount?.path??null,metadata_access:Boolean(v?.metadata_access),share_count:Array.isArray(v?.shares)?v.shares.length:null}));',
    'if(' + JSON.stringify(api) + '==="SYNO.SynoReclaim.Job"&&' + JSON.stringify(method) + '==="result")out.scanSummary={command:r?.command??null,job_id:r?.job_id??null,generated_at:r?.generated_at??null,completion:r?.completion??null,warningCodes:(r?.warnings||[]).map(w=>w?.code).filter(x=>typeof x==="string"),errorCodes:(r?.errors||[]).map(e=>e?.code).filter(x=>typeof x==="string"),resources:r?.resources?{duty_percent:r.resources.scheduling?.duty_percent??null,requested_io_class:r.resources.scheduling?.requested_io_class??null,applied_io_class:r.resources.scheduling?.applied_io_class??null,effectiveness_unknown:r.resources.scheduling?.effectiveness_unknown??null}:null,filesystems:(r?.filesystems||[]).map(f=>({mount:f?.mount?.path??null,completion:f?.completion??null,consistency:f?.consistency??null,selection_stable:typeof f?.selection_stable==="boolean"?f.selection_stable:null,scan_started:f?.scan_started??null,scan_finished:f?.scan_finished??null,warningCodes:(f?.warnings||[]).map(w=>w?.code).filter(x=>typeof x==="string"),errorCodes:(f?.errors||[]).map(e=>e?.code).filter(x=>typeof x==="string"),coverage:f?.coverage?{snapshots_not_materialized:f.coverage.snapshots_not_materialized??null,unresolved_extents:f.coverage.unresolved_extents??null,reference_accounting:f.coverage.reference_accounting??null}:null,stats:f?.stats?{entries:f.stats.entries??null,subvolumes_discovered:f.stats.subvolumes_discovered??null,subvolumes_scanned:f.stats.subvolumes_scanned??null,subvolumes_reused:f.stats.subvolumes_reused??null,elapsed_ms:f.stats.elapsed_ms??null}:null,report_bytes:f?.report?.bytes??null}))};',
    'finish(out);}});})',
  ].join('')
  return evaluate(expression)
}

const visibleText = async () => evaluate(activeAppText)
const appButton = async labels => evaluate(
  '(()=>{const w=document.querySelector(".x-window.active-win");const ls=' + JSON.stringify(labels) +
  ';const b=[...(w?.querySelectorAll("button,[role=button]")||[])].find(e=>ls.includes(e.innerText.trim().split("\\n")[0])||ls.includes(e.getAttribute("aria-label")));b?.click();return Boolean(b)})()',
)
const closeNewScan = async () => evaluate(
  '(()=>{const d=[...document.querySelectorAll("[role=dialog]")].find(x=>/new scan|掃描/.test(x.innerText.toLowerCase()));if(!d)return false;const b=d.querySelector("button[aria-label]");b?.click();return Boolean(b)})()',
)
const openNewScan = async () => {
  const existing = await evaluate('Boolean([...document.querySelectorAll("[role=dialog]")].some(x=>/new scan|掃描/i.test(x.innerText)))')
  if (existing) return
  await waitFor('Boolean([...document.querySelectorAll(".x-window.active-win button")].some(e=>/new scan|新增掃描|新的掃描/i.test(e.innerText.trim())))')
  const clicked = await appButton(['New scan', '新增掃描', '新的掃描'])
  if (!clicked) throw new Error('New Scan toolbar button was not found')
  await waitFor('Boolean([...document.querySelectorAll("[role=dialog]")].some(x=>/new scan|掃描/i.test(x.innerText)))')
}
const setLocale = async locale => {
  await waitFor('typeof window._S==="function"')
  if (locale === 'cht') {
    await evaluate('(()=>{if(window.__synoreclaimLocaleOverride)return window._S("lang");const base=window._S;window._S=function(key,...args){if(key==="lang")return "cht";return Reflect.apply(base,this,[key,...args])};window.__synoreclaimLocaleOverride=true;return window._S("lang")})()')
  }
  result.localeMethod = result.localeMethod || {}
  result.localeMethod[locale] = locale === 'cht' ? 'temporary CDP override of DSM _S("lang") to cht' : 'DSM session language'
}
const openApp = async () => {
  if (await evaluate(readyApp)) {
    await waitForApiQuiet()
    return
  }
  await waitFor('Boolean(document.querySelector(".launch-icon.app-item-icon"))', 20000)
  const clicked = await evaluate('(()=>{const e=[...document.querySelectorAll(".launch-icon.app-item-icon")].find(x=>x.innerText.replace(/\\s/g,"").includes("SynoReclaim"));e?.click();return Boolean(e)})()')
  if (!clicked) throw new Error('Syno Reclaim DSM desktop icon was not found')
  await waitFor(readyApp, 20000)
  await waitForApiQuiet()
}
const navigateLocale = async locale => {
  await send('Page.navigate', { url: dsmUrl + '/?lang=' + locale })
  await waitFor('document.readyState==="complete"', 20000)
  await sleep(1600)
  await waitFor('Boolean(document.querySelector(".launch-icon.app-item-icon"))', 20000)
  await setLocale(locale)
  await send('Emulation.setDeviceMetricsOverride', { width: 1600, height: 900, screenWidth: 1600, screenHeight: 900, deviceScaleFactor: 1, mobile: false })
  if (!await installAuditHook()) throw new Error('DSM WebAPI shim is unavailable')
  await openApp()
}
const navigateToStoredPage = async (locale, page) => {
  await collectAudit()
  await evaluate('sessionStorage.setItem("synoreclaim.page",JSON.stringify(' + JSON.stringify(page) + '))')
  await navigateLocale(locale)
  const title = locale === 'cht' ? '掃描中' : 'Scanning'
  await waitFor('document.querySelector(".x-window.active-win .toolbar-title h1")?.innerText===' + JSON.stringify(title), 20000)
  await waitForApiQuiet()
  result.routeFallbacks.push({ locale, page, mechanism: 'sessionStorage synoreclaim.page followed by DSM reload and app relaunch' })
}
const takePage = async (locale, name, labels = null) => {
  if (labels) {
    const clicked = await appButton(labels)
    if (!clicked && name === 'scanning') {
      await navigateToStoredPage(locale, name)
    } else if (!clicked) {
      throw new Error('Page control not found for ' + name)
    }
    await waitFor('document.querySelector(".x-window.active-win .toolbar-title h1")?.innerText===' + JSON.stringify(labels[0]), 20000)
  }
  await waitForApiQuiet()
  await sleep(250)
  const filename = shotPrefix(locale) + name + '.png'
  await capture(filename)
  result.screenshots.push(filename)
  const title = await evaluate('document.querySelector(".x-window.active-win .toolbar-title h1")?.innerText||document.querySelector(".x-window.active-win h1")?.innerText||""')
  result.pageVisits.push({ locale, page: name, title })
  if (name === 'overview') {
    const shown = await evaluate('/system partition|root file system|system volume|系統分割區/i.test(document.querySelector(".x-window.active-win")?.innerText||"")')
    result.systemPartitionRouteExposed.push({ locale, exposed: Boolean(shown) })
    if (shown) {
      const route = await appButton(['System partition', '系統分割區'])
      if (route) {
        await waitForApiQuiet()
        await sleep(250)
        const systemShot = shotPrefix(locale) + 'system-partition.png'
        await capture(systemShot)
        result.screenshots.push(systemShot)
        result.pageVisits.push({ locale, page: 'system-partition', title: await evaluate('document.querySelector(".x-window.active-win h1")?.innerText||""') })
      }
    }
  }
  return title
}

const captureNewScan = async locale => {
  await openNewScan()
  await waitForApiQuiet()
  let filename = shotPrefix(locale) + 'new-scan.png'
  await capture(filename)
  result.screenshots.push(filename)
  const firstState = await evaluate('(()=>{const d=[...document.querySelectorAll("[role=dialog]")].find(x=>/new scan|掃描/i.test(x.innerText));const groups=[...(d?.querySelectorAll(".choice-grid")||[])];return {scope:[...(groups[0]?.querySelectorAll("[role=radio]")||[])].map(e=>e.getAttribute("aria-checked")==="true"),mode:[...(groups[1]?.querySelectorAll("[role=radio]")||[])].map(e=>e.getAttribute("aria-checked")==="true"),selectedVolume:d?.querySelector(".volume-card.selected")?.innerText.trim()||""}})()')
  const toggled = await evaluate('(()=>{const d=[...document.querySelectorAll("[role=dialog]")].find(x=>/new scan|掃描/i.test(x.innerText));const b=d?.querySelectorAll(".choice-grid")[0]?.querySelectorAll("[role=radio]")[1];b?.click();return Boolean(b)})()')
  if (!toggled) throw new Error('Selected shared-folders option is missing')
  await sleep(250)
  const shareCount = await evaluate('(()=>{const d=[...document.querySelectorAll("[role=dialog]")].find(x=>/new scan|掃描/i.test(x.innerText));return d?.querySelectorAll(".share-list label").length??0})()')
  filename = shotPrefix(locale) + 'new-scan-shares.png'
  await capture(filename)
  result.screenshots.push(filename)
  await evaluate('(()=>{const d=[...document.querySelectorAll("[role=dialog]")].find(x=>/new scan|掃描/i.test(x.innerText));d?.querySelectorAll(".choice-grid")[0]?.querySelectorAll("[role=radio]")[0]?.click()})()')
  await sleep(200)
  const finalState = await evaluate('(()=>{const d=[...document.querySelectorAll("[role=dialog]")].find(x=>/new scan|掃描/i.test(x.innerText));const groups=[...(d?.querySelectorAll(".choice-grid")||[])];return {scope:[...(groups[0]?.querySelectorAll("[role=radio]")||[])].map(e=>e.getAttribute("aria-checked")==="true"),mode:[...(groups[1]?.querySelectorAll("[role=radio]")||[])].map(e=>e.getAttribute("aria-checked")==="true")}})()')
  result.newScanSetup = result.newScanSetup || []
  result.newScanSetup.push({ locale, initial: firstState, final: finalState, sharedFolderOptions: shareCount })
}
const closeDialogSafely = async locale => {
  await send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Escape', code: 'Escape' })
  await send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Escape', code: 'Escape' })
  await sleep(250)
  const stillOpen = await evaluate('Boolean(document.querySelector("[role=dialog] .review-dialog"))')
  if (!stillOpen) return true
  return evaluate('(()=>{const d=document.querySelector("[role=dialog]");const labels=' + JSON.stringify(locale === 'cht' ? ['關閉', '取消'] : ['Close', 'Cancel']) + ';const b=[...(d?.querySelectorAll("button")||[])].find(e=>labels.includes(e.innerText.trim()));b?.click();return Boolean(b)})()')
}

const startScanFromUi = async locale => {
  let volumes = await evaluate('window.__synoreclaimAudit?.volumeMounts||[]')
  let volumeSource = 'captured DSM UI Volumes.list'
  if (!volumes.length) {
    volumeSource = 'read-only DSM Volumes.list preflight'
    for (let attempt = 1; attempt <= 6; attempt++) {
      const response = await requestDsm('SYNO.SynoReclaim.Volumes', 'list', {})
      if (response.ok) { volumes = response.volumes || []; break }
      if (response.code !== 4002) break
      await sleep(1200)
    }
  }
  result.volumeMounts = volumes || []
  if (volumes?.length !== 1 || volumes[0]?.mount !== '/volume1' || !volumes[0]?.metadata_access) {
    throw new Error('Expected only /volume1 to be available for this authorized whole-volume scan')
  }
  let jobsSnapshot = await evaluate('({seen:Boolean(window.__synoreclaimAudit?.jobsSeen),jobs:window.__synoreclaimAudit?.latestActiveJobs||[]})')
  let jobSource = 'captured DSM UI Jobs.list'
  let jobs = { ok: jobsSnapshot.seen, jobs: jobsSnapshot.jobs }
  if (!jobsSnapshot.seen) {
    jobSource = 'read-only DSM Jobs.list preflight'
    for (let attempt = 1; attempt <= 6; attempt++) {
      jobs = await requestDsm('SYNO.SynoReclaim.Jobs', 'list', {})
      if (jobs.ok) break
      if (jobs.code !== 4002) break
      await sleep(1200)
    }
  }
  const active = (jobs.jobs || []).filter(job => ['queued', 'running', 'pausing', 'cancelling', 'paused'].includes(job.state))
  result.preStartJobCheck = { ok: jobs.ok, source: jobSource, activeJobs: active }
  if (!jobs.ok || active.length) throw new Error('A job is active or the job check failed; the scan was not started')
  const state = await evaluate('(()=>{const d=[...document.querySelectorAll("[role=dialog]")].find(x=>/new scan|掃描/i.test(x.innerText));const groups=[...(d?.querySelectorAll(".choice-grid")||[])];const volume=d?.querySelector(".volume-card.selected")?.innerText.trim()||"";return {scope:[...(groups[0]?.querySelectorAll("[role=radio]")||[])].map(e=>e.getAttribute("aria-checked")==="true"),mode:[...(groups[1]?.querySelectorAll("[role=radio]")||[])].map(e=>e.getAttribute("aria-checked")==="true"),volume,button:d?.querySelector(".dialog-footer button")?.innerText.trim()||"",disabled:d?.querySelector(".dialog-footer button")?.disabled??true}})()')
  if (!state.scope?.[0] || state.scope?.[1] || !state.mode?.[1] || state.mode?.[0] || state.disabled) {
    throw new Error('New Scan dialog is not set to whole-volume Gentle')
  }
  const marker = { intent: 'one UI scan', target: '/volume1', mode: 'gentle', locale, created_at: new Date().toISOString(), button_label: safeText(state.button) }
  await fs.writeFile(launchMarker, JSON.stringify(marker, null, 2) + '\n', { flag: 'wx', mode: 0o600 })
  const clicked = await evaluate('(()=>{const d=[...document.querySelectorAll("[role=dialog]")].find(x=>/new scan|掃描/i.test(x.innerText));const b=d?.querySelector(".dialog-footer button");if(!b||b.disabled)return false;b.click();return true})()')
  if (!clicked) throw new Error('Whole-volume scan button could not be activated')
  await waitFor('Boolean(window.__synoreclaimAudit?.scanStart)', 20000)
  const start = await evaluate('({request:window.__synoreclaimAudit?.scanRequest,response:window.__synoreclaimAudit?.scanStart})')
  result.scan = { target: '/volume1', mode: 'gentle', launchedVia: 'New Scan dialog primary button', request: start.request, response: start.response, started_at: new Date().toISOString(), marker: path.basename(launchMarker) }
  await fs.writeFile(launchMarker, JSON.stringify({ ...marker, request: start.request, response: start.response }, null, 2) + '\n', { mode: 0o600 })
  if (!start.response?.ok || start.request?.targets?.length !== 1 || start.request?.targets?.[0] !== '/volume1' || start.request?.mode !== 'gentle') {
    throw new Error('UI scan request did not match the sole /volume1 Gentle scan')
  }
  if (!start.response.job_id) throw new Error('UI scan response did not include a job id')
  await waitFor('document.querySelector(".x-window.active-win .toolbar-title h1")?.innerText==="Scanning"||document.querySelector(".x-window.active-win .toolbar-title h1")?.innerText==="掃描中"', 15000)
  const filename = shotPrefix(locale) + 'scanning-active.png'
  await capture(filename)
  result.screenshots.push(filename)
  result.pageVisits.push({ locale, page: 'scanning-active', title: await evaluate('document.querySelector(".x-window.active-win .toolbar-title h1")?.innerText||""') })
  return start.response.job_id
}
const waitForScanCompletion = async jobId => {
  const started = Date.now()
  const timeout = 90 * 60 * 1000
  let seenActive = false
  let lastNotice = 0
  let terminal = null
  while (Date.now() - started < timeout) {
    const response = await requestDsm('SYNO.SynoReclaim.Jobs', 'list', {})
    const job = response.jobs?.find(item => item.job_id === jobId)
    if (job && ['queued', 'running', 'pausing', 'cancelling'].includes(job.state)) seenActive = true
    if (job && ['completed', 'partial', 'failed', 'cancelled', 'interrupted'].includes(job.state)) {
      terminal = job
      break
    }
    if (!job && seenActive) {
      const finalResult = await requestDsm('SYNO.SynoReclaim.Job', 'result', { job_id: jobId })
      if (finalResult.completion && finalResult.completion !== 'paused') {
        terminal = { job_id: jobId, state: finalResult.completion, result: finalResult }
        break
      }
    }
    if (Date.now() - lastNotice >= 60000) {
      console.log('Waiting for the single /volume1 scan to finish; no further scan will be launched.')
      lastNotice = Date.now()
    }
    await sleep(3500)
  }
  if (!terminal) throw new Error('Timed out waiting for the single scan; it was not cancelled or restarted')
  let finalResult = terminal.result ?? null
  if (!finalResult) finalResult = await requestDsm('SYNO.SynoReclaim.Job', 'result', { job_id: jobId })
  result.scan.finished_at = new Date().toISOString()
  result.scan.final_job_state = terminal.state
  result.scan.result = { ok: finalResult.ok, completion: finalResult.completion, errorCodes: finalResult.errorCodes || [], summary: finalResult.scanSummary ?? null }
  const filename = 'en-scanning-complete.png'
  await capture(filename)
  result.screenshots.push(filename)
}

const resumeCompletedScan = async () => {
  const jobId = savedLaunch.response.job_id
  let jobs = null
  for (let attempt = 1; attempt <= 6; attempt++) {
    jobs = await requestDsm('SYNO.SynoReclaim.Jobs', 'list', {})
    if (jobs.ok) break
    if (jobs.code !== 4002) break
    await sleep(1200)
  }
  if (!jobs?.ok) throw new Error('Could not verify the saved scan job; no scan was started')
  const activeJobs = (jobs.jobs || []).filter(job => ['queued', 'running', 'pausing', 'cancelling', 'paused'].includes(job.state))
  const ownJob = jobs.jobs?.find(job => job.job_id === jobId) ?? null
  result.resumePreflight = { captured_at: jobs.captured_at, jobId, savedJobState: ownJob?.state ?? null, activeJobs }
  if (activeJobs.length) throw new Error('An active DSM job is present; resume stopped without starting a scan')

  let finalResult = null
  for (let attempt = 1; attempt <= 6; attempt++) {
    finalResult = await requestDsm('SYNO.SynoReclaim.Job', 'result', { job_id: jobId })
    if (finalResult.ok) break
    if (finalResult.code !== 4002) break
    await sleep(1200)
  }
  if (!finalResult?.ok || finalResult.command !== 'scan' || finalResult.job_id !== jobId || !finalResult.scanSummary) {
    throw new Error('The saved scan result could not be verified; no scan was started')
  }
  result.scan.finished_at = finalResult.scanSummary.filesystems?.[0]?.scan_finished ?? finalResult.scanSummary.generated_at ?? null
  result.scan.final_job_state = ownJob?.state ?? finalResult.completion
  result.scan.result = {
    ok: finalResult.ok,
    completion: finalResult.completion,
    errorCodes: finalResult.scanSummary.errorCodes,
    summary: finalResult.scanSummary,
  }
  result.resumePreflight.resultCapturedAt = finalResult.captured_at
}

const selectEligibleExploreRow = async () => evaluate('(()=>{const rows=[...document.querySelectorAll(".x-window.active-win tbody tr")];const controls=rows.map((row,index)=>({row,index,wrap:row.querySelector(".basket-select")})).filter(x=>x.wrap);const controlOf=x=>x.wrap.matches("input[type=checkbox],button,[role=checkbox]")?x.wrap:x.wrap.querySelector("input[type=checkbox],button,[role=checkbox]");const checked=controls.filter(x=>{const e=controlOf(x);return e&&(e.checked===true||e.getAttribute("aria-checked")==="true")});if(checked.length===1)return {selected:true,existing:true,rowIndex:checked[0].index,rowCount:rows.length,selectionCount:1};if(checked.length>1)return {selected:false,existing:true,rowCount:rows.length,selectionCount:checked.length};const enabled=controls.filter(x=>{const e=controlOf(x);return e&&!e.disabled&&e.getAttribute("aria-disabled")!=="true"});const file=enabled.find(x=>x.row.querySelector(".explore-entry-name")?.closest("button")?.querySelector("svg")?.classList.contains("lucide-file"));const candidate=file??enabled[0];if(candidate){controlOf(candidate).click();return {selected:true,existing:false,rowIndex:candidate.index,rowCount:rows.length,selectionCount:1,preferredFile:Boolean(file)}}return {selected:false,rowCount:rows.length,selectionCount:0}})()')
const openReview = async locale => {
  await waitFor('document.querySelector(".x-window.active-win .toolbar-title h1")?.innerText===' + JSON.stringify(locale === 'cht' ? '探索' : 'Explore'), 20000)
  await waitForApiQuiet()
  const selection = await selectEligibleExploreRow()
  if (!selection.selected) {
    result.review.push({ locale, available: false, selection })
    return
  }
  if (selection.selected) await sleep(400)
  const clicked = await evaluate('(()=>{const w=document.querySelector(".x-window.active-win");const b=[...(w?.querySelectorAll("button,[role=button]")||[])].find(e=>/review|檢視|檢查|審查/i.test((e.innerText||"")+" "+(e.getAttribute("aria-label")||"")));b?.click();return Boolean(b)})()')
  if (!clicked) {
    result.review.push({ locale, available: false, selection })
    return
  }
  await waitFor('Boolean(document.querySelector("[role=dialog] .review-dialog"))', 20000)
  await waitForApiQuiet()
  await sleep(250)
  const readReviewDialog = async () => evaluate('(()=>{const content=document.querySelector("[role=dialog] .review-dialog");const dialog=content?.closest("[role=dialog]");const titleElement=dialog?.querySelector("[data-dialog-title],h1,h2,[role=heading]");const title=titleElement?.getAttribute("data-dialog-title")||titleElement?.innerText||"";const recheck=[...(dialog?.querySelectorAll("button,[role=button]")||[])].find(e=>/recheck selected items|重新檢查選取項目/i.test(e.innerText||""));const errorText=content&&!content.querySelector(".review-dialog__selection")?(content.innerText||"").replace(/\\/volume1\\/\\S+/g,"/volume1/[selected path]").slice(0,500):"";return {visible:Boolean(content),hasConfirmControls:Boolean(content?.querySelector(".review-dialog__selection")),title,recheckAvailable:Boolean(recheck),errorText,confirmPressed:false}})()')
  const captureReview = async suffix => {
    const filename = shotPrefix(locale) + 'review-' + suffix + '.png'
    await capture(filename)
    result.screenshots.push(filename)
    return filename
  }
  const initialDialog = await readReviewDialog()
  const initialScreenshot = await captureReview(initialDialog.hasConfirmControls ? 'initial-confirmation' : 'refused')
  let recheck = { attempted: false, screenshot: null }
  let dialog = initialDialog
  if (initialDialog.recheckAvailable) {
    const clicked = await evaluate('(()=>{const d=document.querySelector("[role=dialog] .review-dialog")?.closest("[role=dialog]");const b=[...(d?.querySelectorAll("button,[role=button]")||[])].find(e=>/recheck selected items|重新檢查選取項目/i.test(e.innerText||""));b?.click();return Boolean(b)})()')
    if (clicked) {
      recheck.attempted = true
      await waitForApiQuiet()
      await sleep(250)
      dialog = await readReviewDialog()
      recheck.screenshot = await captureReview(dialog.hasConfirmControls ? 'confirmation' : 'after-recheck')
    }
  }
  const visitPage = dialog.hasConfirmControls ? 'review-confirmation' : 'review-refused'
  result.pageVisits.push({ locale, page: visitPage, title: dialog.title })
  result.review.push({ locale, opened: true, available: dialog.hasConfirmControls, selection, initialDialog, initialScreenshot, recheck, dialog })
  await closeDialogSafely(locale)
}

const pageLabels = locale => locale === 'cht'
  ? [['overview', ['總覽']], ['scanning', ['掃描中']], ['explore', ['探索']], ['snapshots', ['快照']], ['recycle-bin', ['資源回收筒']], ['history', ['掃描紀錄']], ['settings', ['設定']]]
  : [['overview', ['Overview']], ['scanning', ['Scanning']], ['explore', ['Explore']], ['snapshots', ['Snapshot']], ['recycle-bin', ['Recycle Bin']], ['history', ['History']], ['settings', ['Settings']]]

const logoutDSM = async () => {
  const itemSelector = JSON.stringify('[syno-id="menu-user-menu-menu-item-4"],[role=menuitem],.v-menu-item')
  const clicked = await evaluate('(()=>{const visible=e=>e&&e.getBoundingClientRect().width>0&&e.getBoundingClientRect().height>0;const item=[...document.querySelectorAll(' + itemSelector + ')].find(e=>visible(e)&&(/sign\\s*out|log\\s*out|登出/i.test(e.innerText||"")||e.getAttribute("syno-id")==="menu-user-menu-menu-item-4"));if(item)return true;const tray=document.querySelector("#sds-taskbar-tray-item-user");tray?.click();return Boolean(tray)})()')
  await sleep(400)
  const logoutClicked = await evaluate('(()=>{const visible=e=>e&&e.getBoundingClientRect().width>0&&e.getBoundingClientRect().height>0;const b=[...document.querySelectorAll(' + itemSelector + ')].find(e=>visible(e)&&(/sign\\s*out|log\\s*out|登出/i.test((e.innerText||"")+" "+(e.getAttribute("aria-label")||""))||e.getAttribute("syno-id")==="menu-user-menu-menu-item-4"));b?.click();return Boolean(b)})()')
  if (logoutClicked) {
    let signedOut = false
    try { signedOut = Boolean(await waitFor('/signin|login|登入/i.test(document.body?.innerText||"")', 10000)) } catch {}
    result.logout = { accountMenuClicked: clicked, logoutButtonClicked: true, signedOut: Boolean(signedOut) }
    return Boolean(signedOut)
  }
  result.logout = { accountMenuClicked: clicked, logoutButtonClicked: false, signedOut: false }
  return false
}

await send('Page.enable')
await send('Runtime.enable')
await send('Log.enable')
await send('Network.enable')
await send('Emulation.setDeviceMetricsOverride', { width: 1600, height: 900, screenWidth: 1600, screenHeight: 900, deviceScaleFactor: 1, mobile: false })
let thrown = null
try {
  for (const locale of locales) {
    if (locale !== locales[0]) await collectAudit()
    await navigateLocale(locale)
    await openNewScan()
    await captureNewScan(locale)
    await collectAudit()
    if (locale === locales[0]) {
      if (startScan) {
        const jobId = await startScanFromUi(locale)
        await collectAudit()
        await waitForScanCompletion(jobId)
      } else {
        await resumeCompletedScan()
      }
      await collectAudit()
    } else {
      await closeNewScan()
    }
    if (locale === locales[0]) await closeNewScan()
    for (const [name, labels] of pageLabels(locale)) {
      await takePage(locale, name, labels)
    }
    const exploreLabel = locale === 'cht' ? ['探索'] : ['Explore']
    await appButton(exploreLabel)
    await waitFor('document.querySelector(".x-window.active-win .toolbar-title h1")?.innerText===' + JSON.stringify(locale === 'cht' ? '探索' : 'Explore'), 20000)
    await waitForApiQuiet()
    await sleep(250)
    await openReview(locale)
    await collectAudit()
    if (locale === locales[0]) await collectAudit()
  }
} catch (error) {
  thrown = error
} finally {
  try {
    await collectAudit()
  } catch {}
  try {
    const loggedOut = await logoutDSM()
    if (!loggedOut) console.error('DSM logout control was not confirmed; inspect the DSM UI before ending the browser session.')
  } catch (error) {
    result.logout = { error: safeText(error?.message ?? error) }
  }
  const oldApiRequests = previousAudits.flatMap(audit => audit.apiRequests || []).map(row => ({
    ...row,
    captured_at: row.captured_at ?? null,
    capture_timestamp_status: row.captured_at ? 'captured' : 'not recorded in original audit',
  }))
  const classifyBusy = row => {
    if (row.origin && row.origin !== 'undetermined') return row.origin
    if (row.errorDetail?.reason === 'busy'
      && (row.errorDetail.exit === null || row.errorDetail.exit === -1)
      && row.errorDetail.document_completion === null
      && !row.errorDetail.stderr_code) return 'dsm_shim_slot_busy'
    if (row.errorDetail?.reason === 'busy'
      && row.errorDetail.exit === 4
      && row.errorDetail.document_completion === 'busy') return 'cli_exit_4_busy'
    return 'undetermined'
  }
  const oldBusyResponses = previousAudits.flatMap(audit => audit.busyResponses || []).map(source => {
    const row = { ...source, origin: classifyBusy(source) }
    return {
      ...row,
      captured_at: row.captured_at ?? null,
      capture_timestamp_status: row.captured_at ? 'captured' : 'not recorded in original audit',
      observation: row.origin === 'dsm_shim_slot_busy'
      ? 'DSM WebAPI shim rejected before CLI execution; inspect activeJobs and jobStripText for current work.'
      : row.origin === 'cli_exit_4_busy'
        ? 'CLI returned completion busy with exit 4; inspect errorDetail.document_completion for the holder result.'
        : 'Response body does not identify the busy origin; preserved without assigning a cause.',
    }
  })
  const oldRuntimeErrors = previousAudits.flatMap(audit => audit.runtimeErrors || [])
  const oldCdpErrors = previousAudits.flatMap(audit => audit.cdpErrors || [])
  const oldScreenshots = previousAudits.flatMap(audit => audit.screenshots || [])
  const oldPageVisits = previousAudits.flatMap(audit => audit.pageVisits || [])
  const mergedApiRequests = [...oldApiRequests, ...allApiRequests]
  result.screenshots = [...new Set([...oldScreenshots, ...result.screenshots])]
  const pagesByKey = new Map()
  for (const visit of [...oldPageVisits, ...result.pageVisits]) pagesByKey.set(visit.locale + ':' + visit.page, visit)
  result.pageVisits = [...pagesByKey.values()]
  result.apiRequests = mergedApiRequests
  result.busyResponses = [...oldBusyResponses, ...allBusyResponses.map(row => ({
    ...row,
    captured_at: row.captured_at ?? null,
    capture_timestamp_status: row.captured_at ? 'captured' : 'not recorded in original audit',
    observation: row.origin === 'dsm_shim_slot_busy'
      ? 'DSM WebAPI shim rejected before CLI execution; inspect activeJobs and jobStripText for current work.'
      : row.origin === 'cli_exit_4_busy'
        ? 'CLI returned completion busy with exit 4; inspect errorDetail.document_completion for the holder result.'
        : 'Response body does not identify the busy origin; preserved without assigning a cause.',
  }))]
  result.webapiFailures = mergedApiRequests.filter(row => !row.ok || (row.code !== null && row.code !== 0))
  const domainCounts = new Map()
  for (const row of mergedApiRequests) {
    if (!row.errorCodes?.length && row.completion !== 'failed') continue
    const key = row.api + ' ' + row.method + ': ' + (row.errorCodes || []).join(', ')
    domainCounts.set(key, (domainCounts.get(key) ?? 0) + 1)
  }
  result.domainResponses = [...domainCounts].map(([method, count]) => ({ method, count }))
  result.runtimeErrors = [...oldRuntimeErrors, ...allRuntimeErrors]
  result.cdpErrors = [...oldCdpErrors, ...cdpErrors]
  await fs.writeFile(path.join(output, 'smoke-audit.json'), JSON.stringify(result, null, 2) + '\n', { mode: 0o600 })
  await fs.writeFile(path.join(output, 'busy-response-ledger.json'), JSON.stringify({
    generated_at: new Date().toISOString(),
    count: result.busyResponses.length,
    entries: result.busyResponses,
  }, null, 2) + '\n', { mode: 0o600 })
  ws.close()
}
if (thrown) {
  console.error(safeText(thrown?.message ?? thrown))
  process.exitCode = 1
} else {
  console.log(JSON.stringify(result, null, 2))
}
