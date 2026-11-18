<script setup lang="ts">
// Mounted at once inside the non-blocking Gate. Every business call runs as
// root, so nothing loads until the Gate reads back ready; a dismissed dialog
// leaves a way to authorize again. Unmount stops consumers.
import {computed,inject,nextTick,onMounted,onUnmounted,ref,watchPostEffect} from 'vue'
import {Button,EmptyState,LoadingArea} from '@synopackageland/ui'
import {useAuthorization,useExecutorPreload} from '@synopackageland/sdk/vue'
import {ShieldAlert} from '@lucide/vue'
import {authorizeRootWork,bindAuthorizationGate} from './stores/authorization'
import {t} from './i18n'
import {ApiError,onRootCallDenied} from './api'
import {sdkRefusal} from './api/errors'
import AppShell from './shell/AppShell.vue'
import StateView from './components/states/StateView.vue'
import UnsupportedVolumeList from './components/states/UnsupportedVolumeList.vue'
import {blockingState} from './components/states/states'
import {loadSession,session} from './stores/session'
import {loadVolumes,unsupportedVolumes,volumeList,volumes} from './stores/volumes'
import {runningJobs,refreshJobs,startJobPolling,volumesBehindJobs} from './stores/jobs'
import {APP_VISIBLE} from './usage/appVisibility'
import {startUsageSampling} from './usage/useUsageChange'
import {FIRST_SAMPLE_DELAY} from './usage/usageChange'
import {openNewScan} from './stores/nav'
const appVisible=inject(APP_VISIBLE,ref(true))
const {requireAuthorization,text}=useAuthorization()
const unbindGate=bindAuthorizationGate(requireAuthorization)
// A page's call refused by a stopped package is the Gate's to say too: the
// pages give way to loading and load again once the package runs.
const stopRefused=onRootCallDenied(error=>{if(!loading.value&&sdkRefusal(error))void start()})
const loading=ref(true)
// The Gate has read ready: the shell may show its chrome while the first reads load.
const authorized=ref(false)
const declined=ref(false)
const gate=computed(()=>blockingState(session.status,volumes.document))
// A refused root call while loading leaves the pages nothing to show.
const failure=computed(()=>session.status==='error'?session.error??new ApiError('unknown'):volumes.error)
// Preloaded pages are hidden behind the executor's check, and the dialog
// would leave them: the first scan's dialog waits until they show. Preload
// also ends just before discarded pages unmount; after the render they are gone.
const preloading=useExecutorPreload()
const firstScan=ref(false)
watchPostEffect(()=>{if(firstScan.value&&!preloading.value){firstScan.value=false;openNewScan()}})
let disposed=false
let stopPolling:(()=>void)|undefined
let stopUsage:(()=>void)|undefined
async function start(){
 declined.value=false
 loading.value=true
 authorized.value=false
 if(!await authorizeRootWork()){if(!disposed){declined.value=true;loading.value=false};return}
 if(disposed)return
 authorized.value=true
 // The session, jobs and volumes are independent reads: one round trip, not three.
 // The jobs leave the volumes to this check, which reads them again only when a
 // scan ended after they were read, not for every finished scan in the history.
 await Promise.all([loadSession(),refreshJobs({reloadVolumes:false}),loadVolumes()])
 if(disposed)return
 // A stopped package is the Gate's to say; the work goes on once it runs.
 if(sdkRefusal(session.error))return start()
 if(session.status!=='ready'){loading.value=false;return}
 if(sdkRefusal(volumes.error))return start()
 if(!volumes.error&&volumesBehindJobs()){
  await loadVolumes()
  if(disposed)return
  if(sdkRefusal(volumes.error))return start()
 }
 stopPolling?.()
 stopPolling=startJobPolling({loadNow:false})
 stopUsage?.()
 // The sample reads the volumes and history again: it waits for the first page's reads to finish.
 stopUsage=startUsageSampling(appVisible,{firstDelayMs:FIRST_SAMPLE_DELAY})
 loading.value=false
 if(!runningJobs.value.length&&volumeList.value.length&&volumeList.value.every(v=>v.reports===0))firstScan.value=true
}
// Children mount before the Gate starts its background check; asking first
// would open its dialog on "checking" instead of waiting for the answer.
onMounted(async()=>{await nextTick();if(!disposed)await start()})
onUnmounted(()=>{disposed=true;unbindGate();stopRefused();stopPolling?.();stopUsage?.()})
</script>
<template>
  <LoadingArea v-if="loading && !authorized" :text="t('common.loading')" />
  <!-- One shell from the Gate's ready until the pages show: its chrome stands while the reads load. -->
  <AppShell v-else-if="loading || !(declined || failure || gate)" :loading="loading" />
  <EmptyState v-else-if="declined" :title="text('state.required')" :description="text('purpose')" :icon="ShieldAlert">
    <template #actions><Button variant="primary" @click="start">{{ text('authorize') }}</Button></template>
  </EmptyState>
  <StateView v-else-if="failure" kind="error" :error="failure">
    <Button @click="start">{{ t('common.retry') }}</Button>
  </StateView>
  <StateView v-else-if="gate==='no_supported_volume'" kind="no_supported_volume"><UnsupportedVolumeList :mounts="unsupportedVolumes" /></StateView>
  <StateView v-else-if="gate" :kind="gate" />
</template>
