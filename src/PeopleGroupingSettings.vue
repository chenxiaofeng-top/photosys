<script setup lang="ts">
import { computed, onBeforeUnmount, ref, shallowRef, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { personName, type GroupingParameters, type GroupingSettings, type PendingGroupPreview } from "./people";

const props = defineProps<{
  scanId: number; settings: GroupingSettings; pending: number; revision: number;
  writable: boolean; busy: boolean;
}>();
const emit = defineEmits<{ changed: []; working: [working: boolean]; preview: [fileId: number] }>();

const threshold = ref<number | string>(0);
const margin = ref<number | string>(0);
const saved = shallowRef(props.settings.parameters);
const preview = shallowRef<PendingGroupPreview>();
const working = ref<"save" | "preview" | "apply">();
const error = ref("");
const message = ref("");
let context = 0;

const blocked = computed(() => props.busy || !!working.value);
const dirty = computed(() => threshold.value !== saved.value.threshold || margin.value !== saved.value.margin);
const valid = computed(() => [threshold.value, margin.value].every(value =>
  typeof value === "number" && value >= 0 && value < 1,
));
const matching = computed(() => (preview.value?.visibleMatches ?? 0) + (preview.value?.ignoredMatches ?? 0));

function fill(parameters: GroupingParameters) {
  threshold.value = parameters.threshold;
  margin.value = parameters.margin;
}
watch([() => props.scanId, () => props.settings.featureVersion], () => {
  context++;
  saved.value = props.settings.parameters;
  fill(saved.value);
  preview.value = undefined;
  error.value = "";
  message.value = "";
}, { immediate: true });
watch(() => props.settings.parameters, parameters => {
  if (parameters.threshold === saved.value.threshold && parameters.margin === saved.value.margin) return;
  saved.value = parameters;
  fill(parameters);
});
watch([threshold, margin, () => props.revision, () => props.busy], () => { preview.value = undefined; });
watch(working, value => emit("working", !!value), { flush: "sync" });
onBeforeUnmount(() => { context++; emit("working", false); });

async function run(action: "save" | "preview" | "apply") {
  if (blocked.value || (action !== "preview" && !props.writable)) return;
  if (action === "save" ? !valid.value || !dirty.value : dirty.value || !props.pending) return;
  const token = preview.value?.token;
  if (action === "apply" && (!token || !matching.value)) return;
  const request = context;
  const revision = props.revision;
  const scanId = props.scanId;
  working.value = action;
  preview.value = undefined;
  error.value = "";
  message.value = "";
  try {
    if (action === "save") {
      const parameters = { threshold: Number(threshold.value), margin: Number(margin.value) };
      await invoke("people_save_grouping", { scanId, featureVersion: props.settings.featureVersion, parameters });
      if (request !== context) return;
      saved.value = parameters;
      message.value = "已保存分组参数。";
      emit("changed");
    } else {
      const result = await invoke<PendingGroupPreview>("people_regroup_pending", {
        scanId,
        previewToken: action === "apply" ? token : null,
      });
      if (request !== context) return;
      if (action === "preview") {
        if (revision === props.revision) preview.value = result;
      } else {
        message.value = `已补分 ${result.visibleMatches + result.ignoredMatches} 张人脸，可使用撤销恢复。`;
        emit("changed");
      }
    }
  } catch (cause) {
    if (request === context) error.value = String(cause);
  } finally {
    working.value = undefined;
  }
}
</script>

<template>
  <details class="grouping-settings">
    <summary>分组设置</summary>
    <form class="grouping-actions" @submit.prevent="run('save')">
      <label>最低相似度
        <input v-model.number="threshold" type="number" min="0" step="any" required :disabled="blocked || !writable" />
      </label>
      <label>候选分差
        <input v-model.number="margin" type="number" min="0" step="any" required :disabled="blocked || !writable" />
      </label>
      <button type="submit" :disabled="blocked || !writable || !dirty || !valid">保存参数</button>
      <button type="button" class="secondary" :disabled="blocked || !writable" @click="fill(settings.defaults)">恢复默认值</button>
    </form>
    <p v-if="!valid" class="error" role="status">分组参数须大于或等于 0 并小于 1。</p>
    <p class="muted">降低最低相似度会扩大匹配范围。降低候选分差会允许更接近的候选分数。修改后可先预览待整理人脸的补分结果。</p>
    <div class="grouping-actions">
      <button type="button" class="secondary" :disabled="blocked || dirty || !pending" @click="run('preview')">{{ working === 'preview' ? '正在预览…' : '预览补分' }}</button>
      <span v-if="dirty" class="muted">保存参数后可预览。</span>
      <button v-if="matching" type="button" :disabled="blocked || !writable || dirty" @click="run('apply')">确认补分 {{ matching }} 张人脸</button>
      <span v-if="working === 'apply'" role="status">正在补分…</span>
    </div>
    <p v-if="preview" role="status">
      {{ matching ? `可归入普通人物 ${preview.visibleMatches} 张，归入已忽略人物 ${preview.ignoredMatches} 张。` : '没有可以补分到已有分组的人脸。' }}
    </p>
    <ul v-if="preview?.samples.length" class="grouping-samples" aria-label="补分示例">
      <li v-for="sample in preview.samples" :key="sample.faceId">
        <img :src="sample.thumbnail" alt="" width="56" height="56" />
        <span>{{ personName({ id: sample.personId, name: sample.personName }) }}{{ sample.personIgnored ? ' · 已忽略' : '' }}</span>
        <button type="button" class="secondary" @click="emit('preview', sample.fileId)">查看原照片</button>
      </li>
    </ul>
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
.grouping-samples { display: flex; flex-wrap: wrap; gap: .75rem; padding: 0; list-style: none; }
.grouping-samples li { display: grid; justify-items: start; gap: .375rem; max-width: 12rem; padding: .625rem; border: 1px solid var(--border); border-radius: .5rem; overflow-wrap: anywhere; }
.grouping-samples img { border-radius: .5rem; object-fit: cover; }
</style>
