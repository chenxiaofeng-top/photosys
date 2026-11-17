<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { onBeforeUnmount, onMounted, ref } from "vue";

const props = defineProps<{ scanId: number; fileId: number }>();
const target = ref<HTMLElement>();
const image = ref("");
const error = ref("");
let observer: IntersectionObserver | undefined;
let mounted = false;

onMounted(() => {
  mounted = true;
  observer = new IntersectionObserver(([entry]) => {
    if (!entry?.isIntersecting || !observer) return;
    observer.disconnect();
    observer = undefined;
    void load();
  });
  if (target.value) observer.observe(target.value);
});

onBeforeUnmount(() => {
  mounted = false;
  observer?.disconnect();
  observer = undefined;
});

async function load() {
  try {
    const preview = await invoke<string>("library_preview", { scanId: props.scanId, fileId: props.fileId, detail: false });
    if (mounted) image.value = preview;
  } catch (cause) {
    if (mounted) error.value = String(cause);
  }
}
</script>

<template>
  <span ref="target" class="thumbnail">
    <img v-if="image" :src="image" alt="" />
    <span v-else-if="error" class="preview-error" :title="error">预览失败</span>
    <span v-else>照片</span>
  </span>
</template>

<style scoped>
.thumbnail {
  display: grid;
  width: 100%;
  height: 100%;
  place-items: center;
}

img {
  width: 100%;
  height: 100%;
  min-height: 0;
  object-fit: contain;
}

.preview-error {
  color: #9c2626;
}
</style>
