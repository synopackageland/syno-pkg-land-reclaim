import {authorization,authorizationContext,configureAuthorization,reportAuthorizationProgress,resetAuthorization} from './authorization'
import type {AuthorizationStatus} from '@/context'

const ready:AuthorizationStatus={command:'authorization',schema_version:1,state:'ready',reason:null}
beforeEach(()=>resetAuthorization())

it('keeps the latest root task progress the window reports',()=>{
 reportAuthorizationProgress({op:'bootstrap',state:'queued',elapsedMs:1000})
 reportAuthorizationProgress({op:'bootstrap',state:'running',elapsedMs:31000})
 expect(authorization.progress).toEqual({op:'bootstrap',state:'running',elapsedMs:31000})
})

it.each([null,'running',{op:'bootstrap',state:'done',elapsedMs:1},{op:'bootstrap',state:'running',elapsedMs:-1},{op:'bootstrap',state:'running',elapsedMs:Number.NaN},{op:1,state:'running',elapsedMs:1}])('ignores a malformed report %#',event=>{
 reportAuthorizationProgress(event)
 expect(authorization.progress).toBeNull()
})

it('drops progress once the approval settles, whatever the outcome',async()=>{
 let finish!:(value:AuthorizationStatus)=>void
 configureAuthorization({access:{status:async()=>ready,request:()=>new Promise(resolve=>{finish=resolve}),revoke:async()=>{throw Error('denied')}}})
 const pending=authorizationContext.authorization!.request()
 reportAuthorizationProgress({op:'bootstrap',state:'running',elapsedMs:45000})
 expect(authorization.progress).not.toBeNull()
 finish(ready);await pending
 expect(authorization.progress).toBeNull()
 const failed=authorizationContext.authorization!.revoke()
 reportAuthorizationProgress({op:'bootstrap',state:'running',elapsedMs:45000})
 await expect(failed).rejects.toThrow('denied')
 expect(authorization.progress).toBeNull()
})
