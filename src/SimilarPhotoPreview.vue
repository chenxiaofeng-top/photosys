<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { ref, watch } from "vue";

const props = defineProps<{
  scanId: number;
  fileId: number;
  name: string;
  detail?: boolean;
}>();

const frame = ref<HTMLDivElement>();
const source = ref("");
const error = ref("");

watch(
  () => [frame.value, props.scanId, props.fileId, props.detail] as const,
  ([element, scanId, fileId, detail], _, onCleanup) => {
    source.value = "";
    error.value = "";
    if (!element) return;
    let current = true;
    async function load() {
      try {
        const image = await invoke<string>("similar_preview", { scanId, fileId, detail: detail ?? false });
        if (current) source.value = image;
      } catch (cause) {
        if (current) error.value = String(cause);
      }
    }

    if (detail) {
      void load();
      onCleanup(() => { current = false; });
      return;
    }

    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        observer.disconnect();
        void load();
      }
    });
    observer.observe(element);
    onCleanup(() => {
      current = false;
      observer.disconnect();
    });
  },
  { flush: "post" },
);
</script>

<template>
  <div ref="frame" class="photo-preview" :class="{ detail }">
    <img v-if="source" :src="source" :alt="name" />
    <p v-else-if="error" class="preview-error" role="status">{{ error }}</p>
    <span v-else class="preview-placeholder">加载预览…</span>
  </div>
</template>

<style scoped>
.photo-preview {
  box-sizing: border-box;
  width: 100%;
  aspect-ratio: 4 / 3;
  background: var(--preview);
  overflow: hidden;
  display: grid;
  place-items: center;
  border-radius: 0.5rem;
}

.photo-preview.detail {
  aspect-ratio: 1;
}

.photo-preview img {
  width: 100%;
  height: 100%;
  min-height: 0;
  object-fit: contain;
}

.preview-error,
.preview-placeholder {
  margin: 0;
  padding: 0.35rem;
  font-size: 0.75rem;
  text-align: center;
  overflow-wrap: anywhere;
}

.preview-error {
  color: var(--danger);
}
</style>
