import DefaultTheme from 'vitepress/theme'
import type { Theme } from 'vitepress'

import './style.css'
import ApiReference from './ApiReference.vue'
import SchemaDiagrams from './SchemaDiagrams.vue'

export default {
  extends: DefaultTheme,
  enhanceApp({ app }) {
    app.component('ApiReference', ApiReference)
    app.component('SchemaDiagrams', SchemaDiagrams)
  },
} satisfies Theme
