<script setup lang="ts">
import {AppRoot} from '@synopackageland/ui'
import {AuthorizationGate as Gate} from '@synopackageland/sdk/vue'
import {locale} from '@/i18n'
import type {AuthorizationStatus,GateContext} from '@/context'
const params=new URLSearchParams(window.location.search)
const status:AuthorizationStatus={command:'authorization',schema_version:1,state:(params.get('state')||'authorization_required') as AuthorizationStatus['state'],reason:params.get('reason')||'grant_missing'}
const ctx:GateContext={displayName:'Syno Reclaim',locale,reopen:()=>()=>{},authorization:{canManage:()=>false,status:async()=>status,request:async()=>status,revoke:async()=>status}}
</script>
<template><AppRoot style="height:100%" :lang="locale"><div style="width:1100px;height:720px"><Gate :ctx="ctx" /></div></AppRoot></template>
