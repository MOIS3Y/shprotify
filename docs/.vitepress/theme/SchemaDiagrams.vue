<script setup lang="ts">
import { computed, ref } from 'vue'
import { useData, withBase } from 'vitepress'

const { lang } = useData()
const ru = computed(() => lang.value.startsWith('ru'))
const selected = ref('catalog')
const groups = computed(() => [
  { id: 'catalog', name: ru.value ? 'Каталог' : 'Catalog' },
  { id: 'personal', name: ru.value ? 'Личные данные' : 'Personal data' },
  { id: 'discovery', name: 'Discovery' },
  { id: 'processing', name: ru.value ? 'Фоновые процессы' : 'Background work' },
])
const image = computed(() => withBase(`/diagrams/${selected.value}.svg`))
</script>

<template>
  <div class="schema-viewer">
    <div class="schema-select" role="group" :aria-label="ru ? 'Раздел схемы' : 'Schema section'">
      <button v-for="group in groups" :key="group.id" type="button"
        :aria-pressed="selected === group.id" @click="selected = group.id">{{ group.name }}</button>
    </div>
    <a class="schema-canvas" :href="image" target="_blank" rel="noopener"
      :aria-label="ru ? 'Открыть диаграмму в полном размере' : 'Open full-size diagram'">
      <img :src="image" :alt="`${ru ? 'Связи таблиц' : 'Table relationships'}: ${selected}`" />
    </a>
    <p class="schema-caption">{{ ru
      ? 'Диаграмма показывает связи и основные поля таблиц. Нажмите, чтобы открыть полный размер. Все поля и ограничения есть в DBML.'
      : 'Actual foreign keys, PK/FK columns and selected fields. Open a diagram at full size to zoom. Download DBML for every field and constraint.' }}</p>
  </div>
</template>
