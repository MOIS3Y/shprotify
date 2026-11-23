<script setup lang="ts">
import { computed, ref } from 'vue'
import { useData } from 'vitepress'
import { data as groups } from '../api.data'

const { lang } = useData()
const ru = computed(() => lang.value.startsWith('ru'))
const query = ref('')
const filtered = computed(() => groups.map((group) => ({
  ...group,
  operations: group.operations.filter((operation) =>
    `${group.name} ${operation.method} ${operation.path} ${operation.summary}`
      .toLowerCase().includes(query.value.trim().toLowerCase())),
})).filter((group) => group.operations.length))
const permission = computed(() => ru.value ? {
  public: 'Публичный по умолчанию', authenticated: 'Аккаунт', admin: 'Администратор',
} : {
  public: 'Public by default', authenticated: 'Account', admin: 'Administrator',
})
</script>

<template>
  <div class="api-reference">
    <label class="api-filter-label" for="api-filter">{{ ru ? 'Найти операцию' : 'Find an operation' }}</label>
    <input id="api-filter" v-model="query" type="search" class="api-filter"
      :placeholder="ru ? 'Путь, метод или название' : 'Path, method or name'" />
    <p v-if="!filtered.length" role="status">{{ ru ? 'Нет подходящих операций.' : 'No matching operations.' }}</p>
    <section v-for="group in filtered" :key="group.name" class="api-group">
      <h2>{{ group.name }}</h2>
      <p class="api-group-description">{{ group.description }}</p>
      <details v-for="operation in group.operations" :id="operation.id" :key="operation.id" class="api-operation">
        <summary>
          <span :class="['http-method', operation.method.toLowerCase()]">{{ operation.method }}</span>
          <code>{{ operation.path }}</code>
          <span class="operation-expand" aria-hidden="true">+</span>
        </summary>
        <div class="operation-detail">
          <p>{{ operation.summary }}</p>
          <p><strong>{{ ru ? 'Доступ' : 'Access' }}:</strong> {{ permission[operation.permission] }}</p>
          <p class="operation-id">operationId: <code>{{ operation.id }}</code></p>
        </div>
      </details>
    </section>
  </div>
</template>
