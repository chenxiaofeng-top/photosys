<script setup lang="ts">
import { computed, ref, shallowRef, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { personName, type Face, type FaceSuggestions, type Person, type Page } from "./people";
const props = defineProps<{ scanId: number; scope?: "pending" | "person" | "ignored" | "photo"; personId?: number; fileId?: number; revision?: number; writable?: boolean; busy?: boolean }>();
const emit = defineEmits<{ changed: []; preview: [fileId: number] }>();
const results = shallowRef<Page<Face>>();
const page = ref(0);
const loading = ref(false);
const saving = ref(false);
const error = ref("");
const selected = ref<number[]>([]);
const target = ref("new");
const newName = ref("");
const search = ref("");
const targets = shallowRef<Page<Person>>();
const scope = computed(() => props.scope ?? (props.fileId !== undefined ? "photo" : props.personId !== undefined ? "person" : "pending"));
const selectionDisabled = computed(() => props.busy || saving.value || loading.value || (props.writable === false && scope.value !== "pending"));
const disabled = computed(() => selectionDisabled.value || props.writable === false);
const selectedFaces = computed(() => results.value?.items.filter(f => selected.value.includes(f.id)) ?? []);
const canAssign = computed(() => selectedFaces.value.length > 0 && selectedFaces.value.every(f => !f.needsReview));
const suggestions = shallowRef<FaceSuggestions>();
const loadingSuggestions = ref(false);
const suggestionError = ref("");
let suggestionRequest = 0;
const suggestionsByFace = computed(() => new Map(suggestions.value?.faces.map(face => [face.faceId, face.people])));
const duplicatePhoto = computed(() => new Set(selectedFaces.value.map(face => face.fileId)).size !== selectedFaces.value.length);
const commonSuggestions = computed(() => {
  const first = selectedFaces.value[0];
  if (!first || duplicatePhoto.value) return [];
  return (suggestionsByFace.value.get(first.id) ?? []).filter(person =>
    selectedFaces.value.every(face => suggestionsByFace.value.get(face.id)?.some(candidate => candidate.id === person.id)),
  );
});
watch(selected, () => {
  suggestionRequest++;
  suggestions.value = undefined;
  suggestionError.value = "";
  loadingSuggestions.value = false;
}, { flush: "sync" });
watch([() => props.scanId, scope, () => props.personId, () => props.fileId, () => props.revision], () => { page.value = 0; selected.value = []; });
watch([() => props.scanId, scope, () => props.personId, () => props.fileId, () => props.revision, page], async (_, __, cleanup) => {
  let current = true; cleanup(() => { current = false; });
  loading.value = true; error.value = ""; selected.value = []; results.value = undefined;
  try {
    const value = await invoke<Page<Face>>("people_faces", { scanId: props.scanId, scope: scope.value, personId: props.personId, fileId: props.fileId, page: page.value });
    if (current) results.value = value;
  } catch (cause) { if (current) error.value = String(cause); }
  finally { if (current) loading.value = false; }
}, { immediate: true });
watch([() => props.scanId, search, () => props.revision], (_, __, cleanup) => {
  let current = true;
  target.value = "new";
  const timer = setTimeout(async () => {
    try { const value = await invoke<Page<Person>>("people_list", { scanId: props.scanId, query: search.value, page: 0 }); if (current) targets.value = value; }
    catch (cause) { if (current) error.value = String(cause); }
  }, 200);
  cleanup(() => { current = false; clearTimeout(timer); });
}, { immediate: true });
async function loadSuggestions() {
  if (scope.value !== "pending" || !selectedFaces.value.some(face => !face.needsReview)) return;
  const request = ++suggestionRequest;
  suggestions.value = undefined;
  suggestionError.value = "";
  loadingSuggestions.value = true;
  try {
    const value = await invoke<FaceSuggestions>("people_face_suggestions", { scanId: props.scanId, faceIds: [...selected.value] });
    if (request === suggestionRequest) suggestions.value = value;
  } catch (cause) {
    if (request === suggestionRequest) suggestionError.value = String(cause);
  } finally {
    if (request === suggestionRequest) loadingSuggestions.value = false;
  }
}
async function edit(kind: "assign" | "ignore" | "restore", suggestedPerson?: number) {
  if (disabled.value || !selected.value.length || (kind === "assign" && !canAssign.value)) return;
  const suggestionVersion = suggestedPerson === undefined ? undefined : suggestions.value?.featureVersion;
  if (suggestedPerson !== undefined && !suggestionVersion) return;
  const scanId = props.scanId; saving.value = true; error.value = "";
  try {
    const personId = suggestedPerson ?? (target.value === "new" ? null : Number(target.value));
    await invoke("people_edit", {
      scanId,
      suggestionVersion,
      edit: { kind, faceIds: selected.value, ...(kind === "assign" ? { personId, name: newName.value } : {}) },
    });
    if (scanId === props.scanId) { selected.value = []; newName.value = ""; emit("changed"); }
  } catch (cause) {
    if (scanId === props.scanId) {
      error.value = String(cause);
      if (suggestionVersion) void loadSuggestions();
    }
  }
  finally { saving.value = false; }
}
function selectAll() { selected.value = results.value?.items.map(f => f.id) ?? []; }
</script>
<template>
  <section class="face-editor" aria-label="整理人脸" :aria-busy="loading || saving">
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <p v-if="loading" role="status">正在读取人脸…</p>
    <p v-else-if="!results?.total" class="muted">{{ scope === 'photo' ? '这张照片尚无可用的人脸标注，请在人物页建立或更新索引。' : '这里暂时没有人脸。' }}</p>
    <template v-if="results?.items.length">
      <div class="face-toolbar">
        <button type="button" class="secondary" :disabled="selectionDisabled" @click="selectAll">选择本页</button>
        <button type="button" class="secondary" :disabled="selectionDisabled || !selected.length" @click="selected = []">取消选择</button>
        <button v-if="scope === 'pending'" type="button" class="secondary"
          :disabled="selectionDisabled || loadingSuggestions || !selectedFaces.some(face => !face.needsReview)"
          @click="loadSuggestions">查看归组建议</button>
        <span class="muted">已选择 {{ selected.length }} 张人脸 · 共 {{ results.total }} 张</span>
      </div>
      <ul class="faces">
        <li v-for="face in results.items" :key="face.id" :class="{ selected: selected.includes(face.id) }">
          <label><input v-model="selected" type="checkbox" :value="face.id" :disabled="selectionDisabled" :aria-label="`选择人脸 ${face.id}`" />
            <img :src="face.thumbnail" :alt="face.personName || '未命名人脸'" width="112" height="112" loading="lazy" />
          </label>
          <p>{{ face.personName || (face.personId ? `未命名人物 ${face.personId}` : '待整理') }}<span v-if="face.personId"> · {{ face.manual ? '人工指定' : '自动归入' }}</span><span v-if="face.personIgnored"> · 已忽略人物</span><span v-if="face.ignored"> · 已忽略人脸</span></p>
          <p v-if="face.needsReview" class="review">照片已变化，这是旧标注。请核对当前检出的人脸后重新指定人物，或忽略此标注。</p>
          <template v-if="suggestions && selected.includes(face.id) && !face.needsReview">
            <p v-if="suggestionsByFace.get(face.id)?.length">
              建议人物 · {{ suggestionsByFace.get(face.id)?.map(personName).join('、') }}
            </p>
            <p v-else class="muted">暂无符合条件的建议</p>
          </template>
          <button type="button" class="photo-link secondary" :title="face.fileName" @click="emit('preview', face.fileId)">查看原照片</button>
        </li>
      </ul>
      <p v-if="loadingSuggestions" role="status">正在查找归组建议…</p>
      <p v-if="suggestionError" role="alert" class="error">{{ suggestionError }}</p>
      <div v-if="commonSuggestions.length" class="face-toolbar" aria-label="确认归组建议">
        <button v-for="person in commonSuggestions" :key="person.id" type="button" :disabled="disabled"
          @click="edit('assign', person.id)">{{ selected.length > 1 ? '将所选全部归入' : '归入' }} {{ personName(person) }}</button>
      </div>
      <p v-else-if="suggestions && selected.length > 1" class="muted">
        {{ duplicatePhoto ? '请选择不同照片中的人脸，再批量归入同一人物。' : '所选人脸没有共同的建议人物。' }}
      </p>
      <fieldset :disabled="disabled" class="assign-controls">
        <legend>修改选中人脸的分组</legend>
        <label>查找目标人物 <input v-model="search" placeholder="输入人物名称" maxlength="80" /></label>
        <label>归属人物 <select v-model="target">
          <option value="new">拆成新人物 / 新建人物</option>
          <option v-for="p in targets?.items" :key="p.id" :value="String(p.id)">{{ personName(p) }}</option>
        </select></label>
        <span v-if="targets && targets.total > 24" class="muted">仅显示前 24 项，请输入名称缩小范围。</span>
        <label v-if="target === 'new'">新人物名称 <input v-model="newName" placeholder="可稍后命名" maxlength="80" /></label>
        <button type="button" :disabled="!canAssign" @click="edit('assign')">{{ target === 'new' ? '创建人物并归入' : '移入此人物' }}</button>
        <button v-if="scope !== 'ignored'" type="button" class="secondary" :disabled="!selected.length" @click="edit('ignore')">忽略所选</button>
        <button v-else type="button" class="secondary" :disabled="!selected.length" @click="edit('restore')">恢复所选</button>
      </fieldset>
      <p v-if="writable === false" class="muted">此相册只读，可以查看标注和归组建议。</p>
      <nav v-if="results.total > 24" class="face-toolbar" aria-label="人脸分页">
        <button type="button" class="secondary" :disabled="loading || saving || results.page === 0" @click="page = results!.page - 1">上一页</button>
        <span>{{ results.page + 1 }} / {{ Math.ceil(results.total / 24) }}</span>
        <button type="button" class="secondary" :disabled="loading || saving || (results.page + 1) * 24 >= results.total" @click="page = results!.page + 1">下一页</button>
      </nav>
    </template>
  </section>
</template>
<style scoped>
.face-editor { margin: 1rem 0; }
.face-toolbar, .assign-controls { display: flex; align-items: center; flex-wrap: wrap; gap: .75rem; margin: 1rem 0; }
.assign-controls { border: 1px solid var(--border); border-radius: .75rem; padding: 1rem; }
.assign-controls label { min-width: 0; max-width: 100%; display: grid; gap: .4rem; margin: 0; font-size: .875rem; }
.assign-controls input, .assign-controls select { max-width: 100%; min-height: 38px; padding: .4rem .6rem; border: 1px solid var(--control-border); border-radius: .4rem; background: var(--surface); color: var(--text); }
.faces { display: grid; grid-template-columns: repeat(auto-fill, minmax(145px, 1fr)); gap: 1rem; list-style: none; margin: 0; padding: 0; }
.faces li { min-width: 0; padding: .75rem; border: 1px solid var(--border); border-radius: .75rem; background: var(--surface); }
.faces li.selected { border-color: var(--accent); background: var(--accent-soft); }
.faces label { display: flex; gap: .4rem; align-items: start; margin: 0; cursor: pointer; }
.faces img { width: min(112px, 100%); height: auto; aspect-ratio: 1; border-radius: .6rem; object-fit: cover; }
.faces p { margin: .6rem 0; font-size: .85rem; overflow-wrap: anywhere; }
.photo-link { padding: .35rem .5rem; font-size: .8rem; }
.muted { color: var(--muted); font-size: .875rem; }
.review { color: var(--warning); }
</style>
