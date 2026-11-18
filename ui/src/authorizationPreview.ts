import { createUiApp } from '@/createUiApp'
import '@/styles'
import AuthorizationPreview from './components/states/AuthorizationPreview.vue'

createUiApp(AuthorizationPreview).mount('#authorization-preview')
