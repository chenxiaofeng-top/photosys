<script setup lang="ts">
import { computed, onBeforeUnmount, ref, shallowRef, watch } from "vue";
import { Channel, invoke } from "@tauri-apps/api/core";
import AiSearchSettings from "./AiSearchSettings.vue";
import PeopleFaceEditor from "./PeopleFaceEditor.vue";
import { personName, type Page, type Person, type PeopleStatus } from "./people";
const props = defineProps<{ scanId: number; libraryBusy: boolean; selected?: Person; revision: number }>();
const emit = defineEmits<{ select: [person: Person | undefined]; changed: []; preview: [fileId: number]; status: [status: PeopleStatus] }>();
const ready = ref(false);
const state = shallowRef<PeopleStatus>();
const people = shallowRef<Page<Person>>();
const page = ref(0);
const search = ref("");
const committedSearch = ref("");
const error = ref("");
const indexTask = ref(false);
const cancelling = ref(false);
const saving = ref(false);
const loading = ref(false);
const scope = ref<"people" | "pending" | "ignored">("people");
const rename = ref("");
const mergeSearch = ref("");
const mergeTargets = shallowRef<Page<Person>>();
const mergeTarget = ref("");
const issuePage = ref(0);
const issuesOpen = ref(false);
const issues = shallowRef<Page<{ path: string; reason: string }>>();
let request = 0;
let mounted = true;
const running = computed(() => indexTask.value || !!state.value?.running);
const editDisabled = computed(() => props.libraryBusy || saving.value || !state.value?.writable);
function setState(value: PeopleStatus) { if (value.scanId === props.scanId && mounted) { state.value = value; emit("status", value); } }
async function refresh(automatic = false) {
  if (props.libraryBusy) return;
  const scanId = props.scanId; const token = request;
  try {
    const value = await invoke<PeopleStatus>("people_index_status", { scanId });
    if (!mounted || props.libraryBusy || token !== request || scanId !== props.scanId) return;
    setState(value);
    if (automatic && value.enabled && !value.needsRebuild && value.writable && ready.value && !running.value) void updateIndex(true);
  } catch (cause) { if (!props.libraryBusy && mounted && token === request && scanId === props.scanId) error.value = String(cause); }
}
async function updateIndex(automatic = false) {
  if (running.value || props.libraryBusy || !ready.value) return;
  const scanId = props.scanId; const token = ++request; indexTask.value = true; error.value = "";
  try {
    const value = await invoke<PeopleStatus>("people_update_index", { scanId, automatic, progress: new Channel<PeopleStatus>(v => { if (token === request) setState(v); }) });
    if (mounted && token === request && scanId === props.scanId) { setState(value); emit("changed"); }
  } catch (cause) { if (mounted && token === request && scanId === props.scanId) error.value = String(cause); }
  finally { if (mounted && token === request) { indexTask.value = false; cancelling.value = false; void refresh(); } }
}
async function cancel() { cancelling.value = true; try { await invoke("people_cancel_index", { scanId: props.scanId }); } catch (cause) { error.value = String(cause); cancelling.value = false; } }
watch(() => props.libraryBusy, busy => { if (!busy) void refresh(true); });
watch(() => props.scanId, (id, old) => {
  request++; indexTask.value = false; state.value = undefined; error.value = ""; page.value = 0; scope.value = "people"; search.value = ""; committedSearch.value = ""; issuesOpen.value = false; issuePage.value = 0; cancelling.value = false;
  if (old !== undefined) void invoke("people_cancel_index", { scanId: old }).catch(() => {});
  if (id !== undefined) void refresh(true);
}, { immediate: true });
watch(ready, () => { void refresh(true); });
watch(() => props.revision, () => { void refresh(); });
watch(search, (_, __, cleanup) => { const timer = setTimeout(() => { committedSearch.value = search.value; page.value = 0; }, 250); cleanup(() => clearTimeout(timer)); });
watch([() => props.scanId, () => props.revision, committedSearch, page], async (_, __, cleanup) => {
  let current = true; cleanup(() => { current = false; }); loading.value = true;
  try { const value = await invoke<Page<Person>>("people_list", { scanId: props.scanId, query: committedSearch.value, page: page.value }); if (current) people.value = value; }
  catch (cause) { if (current && !props.libraryBusy) error.value = String(cause); }
  finally { if (current) loading.value = false; }
}, { immediate: true });
watch(() => props.selected, person => { rename.value = person?.name ?? ""; mergeTarget.value = ""; mergeSearch.value = ""; });
watch([() => props.scanId, mergeSearch, () => props.revision], (_, __, cleanup) => {
  let current = true; mergeTarget.value = "";
  const timer = setTimeout(async () => {
    try { const value = await invoke<Page<Person>>("people_list", { scanId: props.scanId, query: mergeSearch.value, page: 0 }); if (current) mergeTargets.value = value; }
    catch (cause) { if (current && !props.libraryBusy) error.value = String(cause); }
  }, 200); cleanup(() => { current = false; clearTimeout(timer); });
}, { immediate: true });
watch([() => props.scanId, () => props.revision, issuesOpen, issuePage], async (_, __, cleanup) => {
  if (!issuesOpen.value) return; let current = true; cleanup(() => { current = false; });
  try { const value = await invoke<typeof issues.value>("people_index_issues", { scanId: props.scanId, page: issuePage.value }); if (current) issues.value = value; }
  catch (cause) { if (current && !props.libraryBusy) error.value = String(cause); }
});
async function mutate(kind: "rename" | "merge" | "undo") {
  if (editDisabled.value || (kind !== "undo" && !props.selected)) return;
  saving.value = true; error.value = ""; const scanId = props.scanId; const person = props.selected;
  try {
    if (kind === "undo") await invoke("people_undo", { scanId });
    else await invoke("people_edit", { scanId, edit: kind === "rename" ? { kind, personId: person!.id, name: rename.value } : { kind, sourceId: person!.id, targetId: Number(mergeTarget.value) } });
    if (scanId === props.scanId) { if (kind === "rename") emit("select", { ...person!, name: rename.value.trim() }); else emit("select", undefined); emit("changed"); }
  } catch (cause) { if (scanId === props.scanId) error.value = String(cause); }
  finally { saving.value = false; }
}
onBeforeUnmount(() => { mounted = false; request++; void invoke("people_cancel_index", { scanId: props.scanId }).catch(() => {}); });
</script>
<template>
  <section class="people-panel" aria-label="人物相册">
    <section class="analysis-settings" aria-label="人物分析与模型设置">
    <AiSearchSettings model-kind="people" :scan-id="scanId" :library-busy="libraryBusy" :people-status="state" :analysis-busy="running" @model-ready="ready = $event" @model-changed="request++; indexTask = false; cancelling = false; refresh(false); emit('changed')" @rebuild="updateIndex(false)" />
    <div class="people-actions">
      <span role="status">{{ running ? '正在分析 / 等待其他 AI 任务 · ' : '' }}已分析 {{ state?.indexed ?? 0 }} / {{ state?.total ?? 0 }} 张照片<span v-if="state?.pending"> · 待处理 {{ state.pending }} 张</span><span v-if="state?.failed"> · 失败 {{ state.failed }} 张</span></span>
      <button v-if="running" type="button" class="secondary" :disabled="cancelling" @click="cancel">{{ cancelling ? '正在停止…' : '停止分析' }}</button>
      <button v-else type="button" :disabled="!ready || libraryBusy || !state?.writable" @click="updateIndex(false)">{{ state?.needsRebuild ? '重建人物索引' : state?.enabled ? '更新分析 / 重试失败项' : '建立人物索引' }}</button>
      <button v-if="!selected" type="button" class="secondary" :disabled="editDisabled || !state?.undoDescription" @click="mutate('undo')">{{ state?.undoDescription ? `撤销：${state.undoDescription}` : '暂无可撤销操作' }}</button>
    </div>
    <progress v-if="running && state?.total" :value="state.indexed + state.failed" :max="state.total" aria-label="人物分析进度"></progress>
    <p v-if="state" class="muted" role="status">{{ state.autoGrouping ? '保守自动分组已开启 · 自动生成未命名人物，可为整组命名；不确定的人脸保留在待整理。' : '自动分组未开启，可在待整理中手动指定人物。' }}</p>
    <p v-if="state?.message" class="notice" role="status">{{ state.message }}</p>
    <p v-if="state && !state.writable" class="notice">此相册只读，可以浏览已有的人物和时间线。</p>
    </section>
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <template v-if="selected">
      <div class="people-actions detail-title"><button type="button" class="secondary" @click="emit('select', undefined)">返回人物列表</button><h2>{{ personName(selected) }}</h2><button type="button" class="secondary" :disabled="editDisabled || !state?.undoDescription" @click="mutate('undo')">{{ state?.undoDescription ? `撤销：${state.undoDescription}` : '暂无可撤销操作' }}</button></div>
      <details class="person-management"><summary>修改分组 / 管理人物与人脸</summary>
        <form class="people-actions" @submit.prevent="mutate('rename')"><label>人物名称 <input v-model="rename" maxlength="80" placeholder="例如：妈妈" :disabled="editDisabled" /></label><button type="submit" :disabled="editDisabled">保存名称</button></form>
        <div class="people-actions"><label>查找合并目标 <input v-model="mergeSearch" maxlength="80" placeholder="输入另一人物的名称" /></label><label>合并到 <select v-model="mergeTarget" :disabled="editDisabled"><option value="">选择人物</option><option v-for="p in mergeTargets?.items.filter(p => p.id !== selected!.id)" :key="p.id" :value="String(p.id)">{{ personName(p) }}</option></select></label><button type="button" class="secondary" :disabled="editDisabled || !mergeTarget" @click="mutate('merge')">合并人物</button></div>
        <p class="muted">勾选误分的人脸，可移入已有人物或拆成新人物；同一人的重复分组可合并。人工修改优先，后续自动分析不会覆盖，可撤销。</p>
        <PeopleFaceEditor :scan-id="scanId" :person-id="selected.id" :revision="revision" :writable="state?.writable" :busy="saving || libraryBusy" @changed="emit('changed')" @preview="emit('preview', $event)" />
      </details>
    </template>
    <template v-else>
      <nav class="people-tabs" aria-label="人物分类">
        <button type="button" :class="{ secondary: scope !== 'people' }" :aria-pressed="scope === 'people'" @click="scope = 'people'">人物</button>
        <button type="button" :class="{ secondary: scope !== 'pending' }" :aria-pressed="scope === 'pending'" @click="scope = 'pending'">待整理（{{ state?.pendingFaces ?? 0 }}）</button>
        <button type="button" :class="{ secondary: scope !== 'ignored' }" :aria-pressed="scope === 'ignored'" @click="scope = 'ignored'">已忽略（{{ state?.ignoredFaces ?? 0 }}）</button>
      </nav>
      <template v-if="scope === 'people'">
        <label class="people-search">查找人物 <input v-model="search" placeholder="输入人物名称" maxlength="80" /></label>
        <p v-if="loading" role="status">正在读取人物…</p>
        <div v-else-if="!people?.total" class="people-empty"><h2>把家人的回忆聚在一起</h2><p>建立人物索引后，系统自动生成未命名人物分组，你可以给整组命名；不确定的人脸可在“待整理”中手动归入人物。</p></div>
        <ul v-if="people?.items.length" class="person-grid">
          <li v-for="person in people.items" :key="person.id"><button type="button" class="person-card secondary" @click="emit('select', person)">
            <img v-if="person.thumbnail" :src="person.thumbnail" :alt="personName(person)" width="112" height="112" loading="lazy" /><span v-else class="avatar-placeholder" aria-hidden="true">人</span>
            <strong>{{ personName(person) }}</strong><span>{{ person.photoCount }} 张照片</span>
          </button></li>
        </ul>
        <nav v-if="people && people.total > 24" class="people-actions" aria-label="人物分页"><button type="button" class="secondary" :disabled="loading || people.page === 0" @click="page = people!.page - 1">上一页</button><span>{{ people.page + 1 }} / {{ Math.ceil(people.total / 24) }}</span><button type="button" class="secondary" :disabled="loading || (people.page + 1) * 24 >= people.total" @click="page = people!.page + 1">下一页</button></nav>
      </template>
      <PeopleFaceEditor v-else :scan-id="scanId" :scope="scope" :revision="revision" :writable="state?.writable" :busy="saving || libraryBusy" @changed="emit('changed')" @preview="emit('preview', $event)" />
    </template>
    <details v-if="state?.failed" :open="issuesOpen" @toggle="issuesOpen = ($event.target as HTMLDetailsElement).open"><summary>人物分析问题（{{ state.failed }}）</summary><ul><li v-for="issue in issues?.items" :key="issue.path">{{ issue.path }} · {{ issue.reason }}</li></ul><nav v-if="issues && issues.total > 24" class="people-actions" aria-label="人物分析问题分页"><button type="button" class="secondary" :disabled="issues.page === 0" @click="issuePage = issues!.page - 1">上一页</button><span>{{ issues.page + 1 }} / {{ Math.ceil(issues.total / 24) }}</span><button type="button" class="secondary" :disabled="(issues.page + 1) * 24 >= issues.total" @click="issuePage = issues!.page + 1">下一页</button></nav></details>
  </section>
</template>
<style scoped>
.people-panel { margin: 0 0 1.5rem; }
.analysis-settings { margin-bottom: 1rem; }
.detail-title h2 { flex: 1; min-width: 0; overflow-wrap: anywhere; }
.people-actions, .people-tabs { display: flex; flex-wrap: wrap; align-items: center; gap: .75rem; margin: 1rem 0; }
.people-actions > span { flex: 1; color: var(--muted); }
.people-actions label, .people-search { min-width: 0; max-width: 100%; display: flex; align-items: center; flex-wrap: wrap; gap: .5rem; margin: .5rem 0; font-size: .875rem; }
.people-panel input, .people-panel select { min-height: 40px; max-width: 100%; box-sizing: border-box; padding: .5rem .75rem; border: 1px solid var(--control-border); border-radius: .5rem; background: var(--surface); color: var(--text); font: inherit; }
.people-search input { width: min(24rem, 100%); }
.person-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 170px),1fr)); gap: 12px; padding: 0; margin: 24px 0; list-style: none; }
.person-card { display: flex; width: 100%; flex-direction: column; align-items: center; gap: 12px; padding: 24px 12px; background: transparent; border: 1px solid transparent; border-radius: 12px; color: var(--text); }
.person-card:hover { border-color: var(--accent); background: var(--surface-subtle); }
.person-card img, .avatar-placeholder { width: 112px; height: 112px; border-radius: 50%; object-fit: cover; }
.avatar-placeholder { display: grid; place-items: center; background: var(--accent-soft); font-size: 2rem; color: var(--muted); }
.person-card strong { overflow-wrap: anywhere; max-width: 100%; }
.person-card > span:last-child, .muted { color: var(--muted); font-size: .875rem; }
.people-empty { padding: 3rem 1rem; margin: 1rem 0; text-align: center; border: 1px dashed var(--border); border-radius: 1rem; background: var(--surface); }
.notice { padding: .75rem 1rem; border-radius: .75rem; background: var(--warning-soft); color: var(--warning); font-size: .875rem; }
.person-management { margin: 1rem 0; padding: 1rem; border: 1px solid var(--border); border-radius: .75rem; background: var(--surface); }
.detail-title h2 { margin: 0; }
progress { width: 100%; }
</style>
