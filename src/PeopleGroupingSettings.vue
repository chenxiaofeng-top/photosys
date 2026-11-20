<script setup lang="ts">
import { computed, onBeforeUnmount, ref, shallowRef, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import type { GroupingParameters, GroupingSettings } from "./people";

const props = defineProps<{ scanId: number; settings: GroupingSettings; writable: boolean; busy: boolean }>();
const emit = defineEmits<{ changed: []; working: [working: boolean] }>();
const threshold = ref<number | string>(0);
const margin = ref<number | string>(0);
const saved = shallowRef(props.settings.parameters);
const working = ref(false);
const error = ref("");
const message = ref("");
let context = 0;

const blocked = computed(() => props.busy || working.value || !props.writable);
const dirty = computed(() => threshold.value !== saved.value.threshold || margin.value !== saved.value.margin);
const valid = computed(() => [threshold.value, margin.value].every(value =>
  typeof value === "number" && value >= 0 && value < 1,
));

function fill(parameters: GroupingParameters) {
  threshold.value = parameters.threshold;
  margin.value = parameters.margin;
}

watch([() => props.scanId, () => props.settings.featureVersion], () => {
  context++;
  saved.value = props.settings.parameters;
  fill(saved.value);
  error.value = "";
  message.value = "";
}, { immediate: true });

watch(() => props.settings.parameters, parameters => {
  if (parameters.threshold === saved.value.threshold && parameters.margin === saved.value.margin) return;
  saved.value = parameters;
  fill(parameters);
});
watch(working, value => emit("working", value), { flush: "sync" });
onBeforeUnmount(() => { context++; emit("working", false); });

async function save() {
  if (blocked.value || !valid.value || !dirty.value) return;
  const request = context;
  const parameters = { threshold: Number(threshold.value), margin: Number(margin.value) };
  working.value = true;
  error.value = "";
  message.value = "";
  try {
    await invoke("people_save_grouping", { scanId: props.scanId, featureVersion: props.settings.featureVersion, parameters });
    if (request !== context) return;
    saved.value = parameters;
    message.value = "已保存参数，可在「待整理」查看新分组。";
    emit("changed");
  } catch (cause) {
    if (request === context) error.value = String(cause);
  } finally {
    working.value = false;
  }
}
</script>

<template>
  <details class="grouping-settings">
    <summary>分组设置</summary>
    <p class="muted">这些参数用于「待整理」的自动分组。降低最低相似度会扩大分组范围，降低候选分差会允许更接近的候选组。</p>
    <form class="grouping-actions" @submit.prevent="save">
      <label>最低相似度
        <input v-model.number="threshold" type="number" min="0" step="any" required :disabled="blocked" />
      </label>
      <label>候选分差
        <input v-model.number="margin" type="number" min="0" step="any" required :disabled="blocked" />
      </label>
      <button type="submit" :disabled="blocked || !dirty || !valid">保存参数</button>
      <button type="button" class="secondary" :disabled="blocked" @click="fill(settings.defaults)">恢复默认值</button>
    </form>
    <p v-if="!valid" class="error" role="status">分组参数须大于或等于 0 并小于 1。</p>
    <p v-if="message" role="status">{{ message }}</p>
    <p v-if="error" class="error" role="alert">{{ error }}</p>
  </details>
</template>

<style scoped>
.grouping-settings { padding: .875rem; border: 1px solid var(--border); border-radius: .75rem; background: var(--surface); }
.grouping-actions, .grouping-actions label { display: flex; flex-wrap: wrap; align-items: center; gap: .625rem; }
.grouping-actions { margin-top: .75rem; }
input { width: 5rem; height: 40px; padding: .4rem; border: 1px solid var(--control-border); border-radius: .5rem; background: var(--surface); color: var(--text); font: inherit; }
.muted { color: var(--muted); font-size: .875rem; }
</style>
