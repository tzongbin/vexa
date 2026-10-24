<script setup lang="ts">
import { computed, ref } from 'vue'

const props = defineProps<{ name?: string; value: unknown; depth?: number; comma?: boolean }>()
const expanded = ref((props.depth ?? 0) < 2)
const entries = computed(() => Array.isArray(props.value)
  ? props.value.map((value, index) => [String(index), value] as const)
  : props.value && typeof props.value === 'object' ? Object.entries(props.value as Record<string, unknown>) : [])
const isContainer = computed(() => props.value !== null && typeof props.value === 'object')
const scalarType = computed(() => props.value === null ? 'null' : typeof props.value)
const scalarText = computed(() => typeof props.value === 'string' ? JSON.stringify(props.value) : String(props.value))
</script>

<template>
  <div class="json-node" :style="{ '--depth': depth ?? 0 }">
    <div class="json-line">
      <button v-if="isContainer" class="json-fold" :aria-label="expanded ? '收合節點' : '展開節點'" @click="expanded = !expanded">{{ expanded ? '⌄' : '›' }}</button>
      <span v-else class="json-fold-spacer"></span>
      <span v-if="name !== undefined" class="json-key">{{ JSON.stringify(name) }}<span class="json-punctuation">: </span></span>
      <template v-if="isContainer">
        <span class="json-punctuation">{{ Array.isArray(value) ? '[' : '{' }}</span><span v-if="!expanded" class="json-collapsed"> {{ entries.length }} {{ entries.length === 1 ? 'item' : 'items' }} </span><span v-if="!expanded" class="json-punctuation">{{ Array.isArray(value) ? ']' : '}' }}</span>
      </template>
      <span v-else :class="`json-${scalarType}`">{{ scalarText }}<span class="json-punctuation">{{ comma ? ',' : '' }}</span></span>
    </div>
    <template v-if="isContainer && expanded">
      <JsonNode v-for="([key, child], index) in entries" :key="`${key}-${index}`" :name="Array.isArray(value) ? undefined : key" :value="child" :depth="(depth ?? 0) + 1" :comma="index < entries.length - 1" />
      <div class="json-line"><span class="json-fold-spacer"></span><span class="json-punctuation">{{ Array.isArray(value) ? ']' : '}' }}{{ comma ? ',' : '' }}</span></div>
    </template>
  </div>
</template>

<script lang="ts">
export default { name: 'JsonNode' }
</script>
