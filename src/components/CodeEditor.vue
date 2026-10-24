<script setup lang="ts">
import { computed, ref, watch } from 'vue'

const props = defineProps<{ modelValue: string; language?: 'json' | 'text'; placeholder?: string }>()
const emit = defineEmits<{ 'update:modelValue': [value: string] }>()
const editor = ref<HTMLTextAreaElement | null>(null)
const highlighted = computed(() => props.language === 'json' ? highlightJson(props.modelValue) : escapeHtml(props.modelValue))

function escapeHtml(value: string) {
  return value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;').replaceAll("'", '&#39;')
}
function highlightJson(value: string) {
  const pattern = /("(?:\\.|[^"\\])*")(?=\s*:)|("(?:\\.|[^"\\])*")|\b(true|false|null)\b|-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/g
  let html = ''
  let cursor = 0
  for (const match of value.matchAll(pattern)) {
    const index = match.index ?? 0
    html += escapeHtml(value.slice(cursor, index))
    const token = match[0]
    const kind = match[1] ? 'json-key' : match[2] ? 'json-string' : match[3] ? 'json-literal' : 'json-number'
    html += `<span class="${kind}">${escapeHtml(token)}</span>`
    cursor = index + token.length
  }
  return html + escapeHtml(value.slice(cursor))
}
function syncScroll() {
  const pre = editor.value?.previousElementSibling as HTMLElement | null
  if (pre && editor.value) { pre.scrollTop = editor.value.scrollTop; pre.scrollLeft = editor.value.scrollLeft }
}
watch(() => props.modelValue, () => requestAnimationFrame(syncScroll))
</script>

<template>
  <div class="code-editor">
    <pre aria-hidden="true" class="code-highlight" v-html="highlighted + (modelValue.endsWith('\n') ? ' ' : '')"></pre>
    <textarea ref="editor" spellcheck="false" autocapitalize="off" autocomplete="off" autocorrect="off"
      :value="modelValue" :placeholder="placeholder" aria-label="Request Body 編輯器"
      @input="emit('update:modelValue', ($event.target as HTMLTextAreaElement).value)" @scroll="syncScroll"></textarea>
  </div>
</template>
