<script setup lang="ts">
import type { KeyValue } from '../types'

const props = defineProps<{ modelValue: KeyValue[]; keyLabel?: string; valueLabel?: string; emptyText?: string }>()
const emit = defineEmits<{ 'update:modelValue': [value: KeyValue[]] }>()

function patch(index: number, key: keyof KeyValue, value: string | boolean) {
  emit('update:modelValue', props.modelValue.map((row, rowIndex) => rowIndex === index ? { ...row, [key]: value } : row))
}
function add() {
  emit('update:modelValue', [...props.modelValue, { key: '', value: '', enabled: true }])
}
function remove(index: number) {
  emit('update:modelValue', props.modelValue.filter((_, rowIndex) => rowIndex !== index))
}
</script>

<template>
  <div class="pair-editor">
    <div v-if="modelValue.length" class="pair-heading"><span></span><span>{{ keyLabel ?? 'Key' }}</span><span>{{ valueLabel ?? 'Value' }}</span><span></span></div>
    <div v-for="(row, index) in modelValue" :key="index" class="pair-row">
      <input aria-label="啟用欄位" type="checkbox" :checked="row.enabled" @change="patch(index, 'enabled', ($event.target as HTMLInputElement).checked)" />
      <input :aria-label="keyLabel ?? 'Key'" :placeholder="keyLabel ?? 'Key'" :value="row.key" @input="patch(index, 'key', ($event.target as HTMLInputElement).value)" />
      <input :aria-label="valueLabel ?? 'Value'" :placeholder="valueLabel ?? 'Value'" :value="row.value" @input="patch(index, 'value', ($event.target as HTMLInputElement).value)" />
      <button class="icon-button remove-button" aria-label="刪除欄位" title="刪除" @click="remove(index)">×</button>
    </div>
    <p v-if="!modelValue.length" class="empty-inline">{{ emptyText ?? '尚無欄位' }}</p>
    <button class="text-button add-row" @click="add"><span>＋</span> 新增欄位</button>
  </div>
</template>
