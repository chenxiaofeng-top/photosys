<script setup lang="ts">
import { computed } from "vue";
const props = defineProps<{ page: number; total: number; label: string; disabled?: boolean }>();
const emit = defineEmits<{ change: [page: number] }>();
const count = computed(() => Math.max(1, Math.ceil(props.total / 25)));
</script>
<template>
  <nav v-if="total > 25" class="actions" :aria-label="label">
    <button type="button" class="secondary" :disabled="disabled || page === 0" @click="emit('change', page - 1)">上一页</button>
    <span role="status">第 {{ page + 1 }} / {{ count }} 页 · 共 {{ total }} 项</span>
    <button type="button" class="secondary" :disabled="disabled || page + 1 >= count" @click="emit('change', page + 1)">下一页</button>
  </nav>
</template>
