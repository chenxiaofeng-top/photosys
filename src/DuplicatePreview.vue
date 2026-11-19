<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";
const props = defineProps<{ scanId: number; fileId: number; name: string; detail?: boolean }>();
const frame = ref<HTMLElement>();
const source = ref("");
const error = ref("");
watch(() => [frame.value, props.scanId, props.fileId, props.detail] as const, ([element, scanId, fileId, detail], _, cleanup) => {
  source.value = "";
  error.value = "";
  if (!element) return;
  let current = true;
  let observer: IntersectionObserver | undefined;
  async function load() {
    try {
      const result = await invoke<string>("duplicate_preview", { scanId, fileId, detail: detail ?? false });
      if (current) source.value = result;
    } catch (cause) { if (current) error.value = String(cause); }
  }
  if (detail) void load();
  else {
    observer = new IntersectionObserver(entries => {
      if (entries.some(entry => entry.isIntersecting)) { observer?.disconnect(); void load(); }
    });
    observer.observe(element);
  }
  cleanup(() => { current = false; observer?.disconnect(); });
}, { flush: "post" });
</script>
<template>
  <div ref="frame" class="duplicate-preview" :class="{ detail }">
    <img v-if="source" :src="source" :alt="name" />
    <p v-else role="status">{{ error || '加载预览…' }}</p>
  </div>
</template>
<style scoped>
.duplicate-preview { width: 6rem; aspect-ratio: 4 / 3; border-radius: .5rem; background: var(--preview); display: grid; place-items: center; overflow: hidden; }
.duplicate-preview.detail { width: 100%; aspect-ratio: 4 / 3; }
img { width: 100%; height: 100%; object-fit: contain; }
p { padding: .4rem; font-size: .75rem; overflow-wrap: anywhere; margin: 0; }
</style>
