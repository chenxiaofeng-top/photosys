<script lang="ts">
export type Rotation = 0 | 90 | 180 | 270;
</script>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

const props = defineProps<{ src: string; alt: string; rotation: Rotation; detail?: boolean }>();
const emit = defineEmits<{ error: [] }>();
const frame = ref<HTMLElement>();
const width = ref(0);
const height = ref(0);
const naturalWidth = ref(0);
const naturalHeight = ref(0);
let observer: ResizeObserver | undefined;
let resizeFrame: number | undefined;

watch(() => props.src, () => {
  naturalWidth.value = 0;
  naturalHeight.value = 0;
});

const swapped = computed(() => props.rotation === 90 || props.rotation === 270);
const rotatedWidth = computed(() => swapped.value ? naturalHeight.value : naturalWidth.value);
const rotatedHeight = computed(() => swapped.value ? naturalWidth.value : naturalHeight.value);
const aspectRatio = computed(() => `${rotatedWidth.value || 1} / ${rotatedHeight.value || 1}`);
const imageStyle = computed(() => {
  const scale = rotatedWidth.value && rotatedHeight.value
    ? Math.min(width.value / rotatedWidth.value, height.value / rotatedHeight.value) : 0;
  return {
    width: `${naturalWidth.value * scale}px`,
    height: `${naturalHeight.value * scale}px`,
    transform: `translate(-50%, -50%) rotate(${props.rotation}deg)`,
    visibility: scale ? "visible" as const : "hidden" as const,
  };
});

function loaded(event: Event) {
  const image = event.currentTarget as HTMLImageElement;
  naturalWidth.value = image.naturalWidth;
  naturalHeight.value = image.naturalHeight;
}

onMounted(() => {
  observer = new ResizeObserver(([entry]) => {
    if (!entry) return;
    if (resizeFrame !== undefined) cancelAnimationFrame(resizeFrame);
    // Apply reactive styles in the next frame, outside the observer delivery
    // cycle, so dialog and scrollbar resizing cannot trigger a resize loop.
    resizeFrame = requestAnimationFrame(() => {
      resizeFrame = undefined;
      width.value = entry.contentRect.width;
      height.value = entry.contentRect.height;
    });
  });
  if (frame.value) observer.observe(frame.value);
});
onBeforeUnmount(() => {
  observer?.disconnect();
  if (resizeFrame !== undefined) cancelAnimationFrame(resizeFrame);
});
</script>

<template>
  <span ref="frame" class="rotatable-image" :class="{ detail }" :style="detail ? { aspectRatio } : undefined">
    <img :key="src" :src="src" :alt="alt" :style="imageStyle" @load="loaded" @error="emit('error')" />
  </span>
</template>

<style scoped>
.rotatable-image {
  position: relative;
  display: block;
  width: 100%;
  height: 100%;
  min-width: 0;
  min-height: 0;
  overflow: hidden;
}

.rotatable-image.detail {
  height: auto;
  max-height: 55vh;
  background: var(--library-preview);
}

img {
  position: absolute;
  left: 50%;
  top: 50%;
  max-width: none;
  max-height: none;
  transform-origin: center;
}
</style>
