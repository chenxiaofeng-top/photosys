<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, shallowRef, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { personName, type Person, type SimilarFaceCriteria, type SimilarFaceResults } from "./people";

const props = defineProps<{
  scanId: number;
  person: Person;
  revision: number;
  defaultThreshold: number;
  writable: boolean;
  busy: boolean;
}>();
const emit = defineEmits<{ changed: []; working: [busy: boolean]; preview: [fileId: number] }>();
const threshold = ref<number | string>(Number(props.defaultThreshold.toPrecision(7)));
const result = shallowRef<SimilarFaceResults>();
const selected = ref<number[]>([]);
const error = ref("");
const message = ref("");
const loading = ref(false);
const saving = ref(false);
const hasAvatar = computed(() => props.person.avatarFaceId != null);
const valid = computed(() => typeof threshold.value === "number" && threshold.value >= 0 && threshold.value < 1);
const faces = computed(() => result.value?.faces);
const selectedFaces = computed(() => faces.value?.items.filter(face => selected.value.includes(face.faceId)) ?? []);
const selectedFiles = computed(() => new Set(selectedFaces.value.map(face => face.fileId)));
let request = 0;
let mounted = true;

function clearResults() {
  request++;
  selected.value = [];
  result.value = undefined;
  loading.value = false;
}

watch(threshold, () => { clearResults(); error.value = ""; message.value = ""; }, { flush: "sync" });
watch(() => props.revision, () => {
  const hadResults = !!result.value || loading.value;
  clearResults();
  message.value = hadResults && !saving.value ? "人物数据已更新，请重新筛选。" : "";
});
onBeforeUnmount(() => {
  mounted = false;
  request++;
  emit("working", false);
});

function criteria(): SimilarFaceCriteria | undefined {
  const { id, avatarFaceId } = props.person;
  if (avatarFaceId == null || !valid.value) return;
  return { targetPersonId: id, anchorFaceId: avatarFaceId, threshold: Number(threshold.value) };
}

async function search(page = 0) {
  const filters = criteria();
  if (props.busy || saving.value || !filters) return;
  clearResults();
  const token = request;
  loading.value = true;
  error.value = "";
  message.value = "";
  try {
    const value = await invoke<SimilarFaceResults>("people_similar_faces", { scanId: props.scanId, criteria: filters, page });
    if (token === request) result.value = value;
  } catch (cause) {
    if (token === request) error.value = String(cause);
  } finally {
    if (token === request) loading.value = false;
  }
}

async function confirm() {
  const filters = criteria();
  const anchorToken = result.value?.anchorToken;
  if (!props.writable || props.busy || loading.value || saving.value || !filters || !anchorToken || !selectedFaces.value.length) return;
  const choices = selectedFaces.value.map(({ faceId, token }) => ({ faceId, token }));
  saving.value = true;
  emit("working", true);
  error.value = "";
  message.value = "";
  try {
    await invoke("people_confirm_similar_faces", { scanId: props.scanId, criteria: filters, anchorToken, selected: choices });
    if (!mounted) return;
    clearResults();
    emit("changed");
    await nextTick();
    if (mounted) message.value = `已归入 ${choices.length} 张人脸，可撤销。`;
  } catch (cause) {
    if (mounted) {
      clearResults();
      error.value = String(cause);
    }
  } finally {
    if (mounted) {
      saving.value = false;
      emit("working", false);
    }
  }
}
</script>

<template>
  <section class="similarity-panel" aria-label="相似人脸" :aria-busy="loading || saving">
    <div class="similarity-reference">
      <img v-if="person.thumbnail" :src="person.thumbnail" alt="" width="56" height="56" />
      <strong>比对头像 · {{ personName(person) }}</strong>
    </div>
    <p v-if="!hasAvatar" class="muted">此人物暂无可比对的头像。请更新人物索引后再试。</p>
    <form v-else class="similarity-actions" @submit.prevent="search()">
      <label>相似度门槛
        <input v-model.number="threshold" type="number" min="0" step="any" required :disabled="busy || saving" />
      </label>
      <button type="submit" :disabled="busy || loading || saving || !valid">筛选相似人脸</button>
    </form>
    <p v-if="hasAvatar && !valid" class="error" role="status">相似度门槛须大于或等于 0 并小于 1。</p>
    <p v-if="loading" role="status">正在查找相似人脸…</p>
    <p v-if="message" role="status">{{ message }}</p>
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <p v-if="faces && !faces.total" class="muted" role="status">没有达到门槛的候选人脸。</p>
    <template v-if="faces?.total">
      <div class="similarity-actions">
        <span role="status">共 {{ faces.total }} 张候选<span v-if="writable"> · 已选 {{ selected.length }} 张</span></span>
        <button v-if="writable" type="button" :disabled="busy || saving || !selected.length" @click="confirm">
          {{ saving ? '正在归组…' : '确认所选为同一人' }}
        </button>
        <span v-else class="muted">此相册只读。</span>
      </div>
      <ul class="similar-faces">
        <li v-for="face in faces.items" :key="face.faceId" :class="{ checked: selected.includes(face.faceId) }">
          <label class="candidate-label">
            <img :src="face.thumbnail" alt="" width="64" height="64" loading="lazy" />
            <input v-if="writable" v-model="selected" type="checkbox" :value="face.faceId"
              :disabled="busy || saving || (!selected.includes(face.faceId) && selectedFiles.has(face.fileId))"
              :aria-label="`选择人脸 ${face.faceId}`" />
          </label>
          <span>模型相似度 {{ face.score.toFixed(3) }}</span>
          <span class="muted">当前归属 · {{ face.sourcePersonId == null ? '待整理' : personName({ id: face.sourcePersonId, name: face.sourcePersonName ?? '' }) }}</span>
          <button type="button" class="secondary" @click="emit('preview', face.fileId)">查看原照片</button>
        </li>
      </ul>
      <nav v-if="faces.total > 24" class="similarity-actions" aria-label="相似人脸分页">
        <button type="button" class="secondary" :disabled="busy || saving || faces.page === 0" @click="search(faces.page - 1)">上一页</button>
        <span>{{ faces.page + 1 }} / {{ Math.ceil(faces.total / 24) }}</span>
        <button type="button" class="secondary" :disabled="busy || saving || (faces.page + 1) * 24 >= faces.total" @click="search(faces.page + 1)">下一页</button>
      </nav>
    </template>
  </section>
</template>

<style scoped>
.similarity-panel { margin: 1rem 0; padding: 1rem; border: 1px solid var(--border); border-radius: .75rem; background: var(--surface); }
.similarity-reference, .similarity-actions, .similarity-actions label { display: flex; flex-wrap: wrap; align-items: center; gap: .625rem; }
.similarity-reference { overflow-wrap: anywhere; }
.similarity-reference img { border-radius: 50%; object-fit: cover; }
.similarity-actions { margin-top: .75rem; }
.similarity-actions input { width: 5rem; min-height: 40px; box-sizing: border-box; padding: .25rem .5rem; border: 1px solid var(--control-border); border-radius: .5rem; color: var(--text); background: var(--surface); font: inherit; }
.muted { color: var(--muted); font-size: .875rem; }
.similar-faces { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 155px), 1fr)); gap: .625rem; padding: 0; list-style: none; }
.similar-faces li { display: grid; justify-items: start; align-content: start; gap: .5rem; padding: .625rem; border: 1px solid var(--border); border-radius: .625rem; overflow-wrap: anywhere; }
.similar-faces .checked { border-color: var(--accent); background: var(--accent-soft); }
.candidate-label { display: flex; align-items: flex-start; gap: .5rem; cursor: pointer; }
.candidate-label img { border-radius: .375rem; object-fit: cover; }
.candidate-label input { width: 1.125rem; height: 1.125rem; accent-color: var(--accent); }
</style>
