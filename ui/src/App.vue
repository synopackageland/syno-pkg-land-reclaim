<script setup lang="ts">
import {inject,onMounted,onUnmounted} from 'vue'
import {AppRoot,ToastProvider} from '@synopackageland/ui'
import {locale,t} from './i18n'
import {isFullPage} from '@synopackageland/sdk'
import {AuthorizationGate,ExecutorApproval} from '@synopackageland/sdk/vue'
import AppContent from './AppContent.vue'
import ApprovalWait from './components/ApprovalWait.vue'
import {authorizationContext,closeApp,requireAuthorization} from './stores/authorization'
import {onRootCallDenied} from './api'
import {DSM_WINDOW_FRONT} from './dsm/appWindow'
import {SHELL_CONTEXT} from './context'
// In a DSM window, dialogs may leave the window as DSM's own do; a press on one
// does not reach the window, so it brings the window to the front itself.
const toFront=inject(DSM_WINDOW_FRONT,undefined)
// Until the DSM window's executor reads ready, its approval takes the content's place.
// The pages load behind it meanwhile, hidden, so their reads need not wait for its check.
const shell=inject(SHELL_CONTEXT,undefined)
const admin=shell?.user?.isAdmin===true
// On a Portal or standalone page it names the App from the window's launch, as a title bar does.
const approvalContext={reopen:authorizationContext.reopen,fullPage:shell?isFullPage(shell):undefined,displayName:shell?.displayName??authorizationContext.displayName,appIcon:shell?.appIcon}
let stopDenied:(()=>void)|undefined
onMounted(()=>{stopDenied=onRootCallDenied(error=>requireAuthorization(error))})
onUnmounted(()=>{stopDenied?.();closeApp()})
</script>
<template>
  <AppRoot class="app-root" :lang="locale" :dialog-bounds="toFront ? 'viewport' : 'app'" @activate="toFront?.()">
    <ToastProvider>
      <ExecutorApproval preload :admin="admin" :locale="locale" :purpose="t('shell.purpose')" :ctx="approvalContext"><AuthorizationGate :ctx="authorizationContext"><AppContent /></AuthorizationGate></ExecutorApproval>
      <ApprovalWait />
    </ToastProvider>
  </AppRoot>
</template>
<style scoped>.app-root{position:relative;height:100%}</style>
