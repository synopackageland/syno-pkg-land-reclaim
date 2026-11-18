import {mount,flushPromises} from '@vue/test-utils'
import {setBackend} from './api'
import {fixtureBackend} from './api/fixtures'
import {configureAuthorization,resetAuthorization} from './stores/authorization'
import App from './App.vue'
import {nav} from './stores/nav'
import {session} from './stores/session'

beforeEach(()=>{nav.newScanOpen=false;session.status='loading';resetAuthorization();setBackend(fixtureBackend({scenario:'normal'}))})
it('renders the app inside the shared gate once authorization reads back ready',async()=>{
 configureAuthorization({client:{status:async()=>({state:'ready',reason:null})}})
 const wrapper=mount(App,{attachTo:document.body})
 for(let i=0;i<10;i++){await flushPromises();await vi.dynamicImportSettled()}
 expect(wrapper.find('.authorization-content nav').exists()).toBe(true)
 wrapper.unmount()
})
it('opens without an authorization screen and holds root data while status is unresolved',async()=>{
 let resolve!:(value:{state:'ready';reason:null})=>void
 configureAuthorization({client:{status:()=>new Promise(r=>{resolve=r})}})
 const wrapper=mount(App,{attachTo:document.body})
 await flushPromises()
 expect(document.querySelector('.authorization-view')).toBeNull()
 expect(document.body.textContent).not.toContain('Checking authorization')
 expect(wrapper.text()).toContain('Loading')
 expect(wrapper.find('nav').exists()).toBe(false)
 resolve({state:'ready',reason:null})
 await flushPromises()
 wrapper.unmount()
})
it('renders the SDK AuthorizationGate, not a copied gate',async()=>{
 const {AuthorizationGate}=await import('@synopackageland/sdk/vue')
 configureAuthorization({client:{status:async()=>({state:'ready',reason:null})}})
 const wrapper=mount(App,{attachTo:document.body})
 await flushPromises()
 expect(wrapper.findComponent(AuthorizationGate).exists()).toBe(true)
 wrapper.unmount()
})
