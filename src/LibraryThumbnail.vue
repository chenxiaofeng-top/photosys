<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { onBeforeUnmount, onMounted, ref } from "vue";
import { queueLibraryThumbnail } from "./libraryThumbnailQueue";

const props = defineProps<{ scanId: number; fileId: number; video: boolean }>();
const target = ref<HTMLElement>();
const image = ref("");
const error = ref("");
let observer: IntersectionObserver | undefined;
let cancelQueued: (() => void) | undefined;
let mounted = false;

onMounted(() => {
  mounted = true;
  observer = new IntersectionObserver((entries) => {
    if (!observer) return;
    if (entries[entries.length - 1]?.isIntersecting) {
      cancelQueued ??= queueLibraryThumbnail(load);
    } else {
      cancelQueued?.();
      cancelQueued = undefined;
    }
  });
  if (target.value) observer.observe(target.value);
});

onBeforeUnmount(() => {
  mounted = false;
  observer?.disconnect();
  observer = undefined;
  cancelQueued?.();
});

async function load() {
  observer?.disconnect();
  observer = undefined;
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
    <span v-else>{{ video ? "▶ 视频" : "照片" }}</span>
    <span v-if="video && image" class="video-label" aria-hidden="true">▶ 视频</span>
  </span>
</template>

<style scoped>
.thumbnail {
  position: relative;
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

.video-label {
  position: absolute;
  right: 0.5rem;
  bottom: 0.5rem;
  padding: 0.2rem 0.4rem;
  border-radius: 0.25rem;
  background: #162922;
  color: #fff;
  font-size: 0.75rem;
}
</style>
