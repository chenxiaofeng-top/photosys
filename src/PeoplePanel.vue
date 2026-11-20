<script setup lang="ts">
import { computed, onBeforeUnmount, ref, shallowRef, watch } from "vue";
import { Channel, invoke } from "@tauri-apps/api/core";
import AiSearchSettings from "./AiSearchSettings.vue";
import PeopleFaceEditor from "./PeopleFaceEditor.vue";
import PeopleGroupingSettings from "./PeopleGroupingSettings.vue";
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
const groupingBusy = ref(false);
const loading = ref(false);
const scope = ref<"people" | "pending" | "ignoredPeople" | "ignoredFaces">("people");
const unnamedOnly = ref(false);
const singlePhotoOnly = ref(false);
const selectedIds = ref<number[]>([]);
const managedHidden = shallowRef<Person>();
const dispositionMessage = ref("");
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
const editDisabled = computed(() => props.libraryBusy || saving.value || groupingBusy.value || !state.value?.writable);
const ignoredScope = computed(() => scope.value === "ignoredPeople" || scope.value === "ignoredFaces");
const listScope = computed(() => scope.value === "people" || scope.value === "ignoredPeople");
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
  if (running.value || groupingBusy.value || props.libraryBusy || !ready.value) return;
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
  unnamedOnly.value = false; singlePhotoOnly.value = false; managedHidden.value = undefined; dispositionMessage.value = "";
  if (old !== undefined) void invoke("people_cancel_index", { scanId: old }).catch(() => {});
  if (id !== undefined) void refresh(true);
}, { immediate: true });
watch(ready, () => { void refresh(true); });
watch(() => props.revision, () => { void refresh(); });
watch(search, (_, __, cleanup) => { const timer = setTimeout(() => { committedSearch.value = search.value; page.value = 0; }, 250); cleanup(() => clearTimeout(timer)); });
watch(scope, () => { search.value = ""; committedSearch.value = ""; managedHidden.value = undefined; dispositionMessage.value = ""; }, { flush: "sync" });
watch([scope, unnamedOnly, singlePhotoOnly], () => { page.value = 0; }, { flush: "sync" });
watch([() => props.scanId, () => props.revision, scope, unnamedOnly, singlePhotoOnly, committedSearch, page], async (_, __, cleanup) => {
  let current = true; cleanup(() => { current = false; });
  selectedIds.value = []; people.value = undefined;
  if (!listScope.value) { loading.value = false; return; }
  loading.value = true;
  const hidden = scope.value === "ignoredPeople";
  try {
    const value = await invoke<Page<Person>>("people_list", {
      scanId: props.scanId, query: committedSearch.value, page: page.value,
      filter: { ignored: hidden, unnamedOnly: !hidden && unnamedOnly.value, singlePhotoOnly: !hidden && singlePhotoOnly.value },
    });
    if (current) people.value = value;
  }
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
  saving.value = true; error.value = ""; dispositionMessage.value = ""; const scanId = props.scanId; const person = props.selected;
  try {
    if (kind === "undo") await invoke("people_undo", { scanId });
    else await invoke("people_edit", { scanId, edit: kind === "rename" ? { kind, personId: person!.id, name: rename.value } : { kind, sourceId: person!.id, targetId: Number(mergeTarget.value) } });
    if (scanId === props.scanId) {
      if (kind === "rename") emit("select", { ...person!, name: rename.value.trim() });
      else emit("select", undefined);
      if (kind === "undo") managedHidden.value = undefined;
      emit("changed");
    }
  } catch (cause) { if (scanId === props.scanId) error.value = String(cause); }
  finally { saving.value = false; }
}
async function setPeopleIgnored(personIds: number[], ignored: boolean) {
  if (editDisabled.value || !personIds.length) return;
  const scanId = props.scanId;
  saving.value = true; error.value = ""; dispositionMessage.value = "";
  try {
    await invoke("people_edit", { scanId, edit: { kind: ignored ? "ignorePeople" : "restorePeople", personIds } });
    if (scanId !== props.scanId || !mounted) return;
    if (props.selected && personIds.includes(props.selected.id)) emit("select", undefined);
    if (managedHidden.value && personIds.includes(managedHidden.value.id)) managedHidden.value = undefined;
    dispositionMessage.value = `已${ignored ? "忽略" : "恢复"} ${personIds.length} 个人物。`;
    emit("changed");
  } catch (cause) {
    if (scanId === props.scanId && mounted) error.value = String(cause);
  } finally {
    saving.value = false;
  }
}
function openPerson(person: Person) {
  if (scope.value === "ignoredPeople") managedHidden.value = person;
  else emit("select", person);
}
onBeforeUnmount(() => { mounted = false; request++; void invoke("people_cancel_index", { scanId: props.scanId }).catch(() => {}); });
</script>
<template>
  <section class="people-panel" aria-label="人物相册">
    <section class="analysis-settings" aria-label="人物分析与模型设置">
    <AiSearchSettings model-kind="people" :scan-id="scanId" :library-busy="libraryBusy" :people-status="state" :analysis-busy="running || groupingBusy" @model-ready="ready = $event" @model-changed="request++; indexTask = false; cancelling = false; refresh(false); emit('changed')" @rebuild="updateIndex(false)" />
    <div class="people-actions">
      <span role="status">{{ state?.queued ? '排队中 · ' : running ? '正在分析 · ' : '' }}已分析 {{ state?.indexed ?? 0 }} / {{ state?.total ?? 0 }} 张照片<span v-if="state?.pending"> · 待处理 {{ state.pending }} 张</span><span v-if="state?.failed"> · 失败 {{ state.failed }} 张</span></span>
      <button v-if="running" type="button" class="secondary" :disabled="cancelling" @click="cancel">{{ cancelling ? '正在停止…' : '停止分析' }}</button>
      <button v-else type="button" :disabled="!ready || libraryBusy || groupingBusy || !state?.writable" @click="updateIndex(false)">{{ state?.needsRebuild ? '重建人物索引' : state?.enabled ? '更新分析 / 重试失败项' : '建立人物索引' }}</button>
      <button v-if="!selected" type="button" class="secondary" :disabled="editDisabled || !state?.undoDescription" @click="mutate('undo')">{{ state?.undoDescription ? `撤销：${state.undoDescription}` : '暂无可撤销操作' }}</button>
    </div>
    <progress v-if="running && state?.total" :value="state.indexed + state.failed" :max="state.total" aria-label="人物分析进度"></progress>
    <p v-if="state" class="muted" role="status">{{ state.autoGrouping ? '自动分组已开启。不确定的人脸保留在待整理。' : '自动分组未开启，可在待整理中手动指定人物。' }}</p>
    <p v-if="state?.message" class="notice" role="status">{{ state.message }}</p>
    <p v-if="state && !state.writable" class="notice">此相册只读，可以浏览已有的人物和时间线。</p>
    <PeopleGroupingSettings v-if="state?.groupingSettings" :scan-id="scanId" :settings="state.groupingSettings"
      :pending="state.pendingFaces" :revision="revision" :writable="state.writable" :busy="running || libraryBusy || saving"
      @changed="emit('changed')" @working="groupingBusy = $event" @preview="emit('preview', $event)" />
    </section>
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <p v-if="dispositionMessage" role="status">{{ dispositionMessage }}</p>
    <template v-if="selected">
      <div class="people-actions detail-title"><button type="button" class="secondary" @click="emit('select', undefined)">返回人物列表</button><h2>{{ personName(selected) }}</h2><button type="button" class="secondary" :disabled="editDisabled || !state?.undoDescription" @click="mutate('undo')">{{ state?.undoDescription ? `撤销：${state.undoDescription}` : '暂无可撤销操作' }}</button></div>
      <details class="person-management"><summary>修改分组 / 管理人物与人脸</summary>
        <form class="people-actions" @submit.prevent="mutate('rename')"><label>人物名称 <input v-model="rename" maxlength="80" placeholder="例如：妈妈" :disabled="editDisabled" /></label><button type="submit" :disabled="editDisabled">保存名称</button></form>
        <button type="button" class="secondary" :disabled="editDisabled" @click="setPeopleIgnored([selected.id], true)">忽略此人</button>
        <div class="people-actions"><label>查找合并目标 <input v-model="mergeSearch" maxlength="80" placeholder="输入另一人物的名称" /></label><label>合并到 <select v-model="mergeTarget" :disabled="editDisabled"><option value="">选择人物</option><option v-for="p in mergeTargets?.items.filter(p => p.id !== selected!.id)" :key="p.id" :value="String(p.id)">{{ personName(p) }}</option></select></label><button type="button" class="secondary" :disabled="editDisabled || !mergeTarget" @click="mutate('merge')">合并人物</button></div>
        <p class="muted">勾选误分的人脸，可移入已有人物或拆成新人物；同一人的重复分组可合并。人工修改优先，后续自动分析不会覆盖，可撤销。</p>
        <PeopleFaceEditor :scan-id="scanId" :person-id="selected.id" :revision="revision" :writable="state?.writable" :busy="saving || libraryBusy || groupingBusy" @changed="emit('changed')" @preview="emit('preview', $event)" />
      </details>
    </template>
    <template v-else>
      <nav class="people-tabs" aria-label="人物分类">
        <button type="button" :class="{ secondary: scope !== 'people' }" :aria-pressed="scope === 'people'" @click="scope = 'people'">人物</button>
        <button type="button" :class="{ secondary: scope !== 'pending' }" :aria-pressed="scope === 'pending'" @click="scope = 'pending'">待整理（{{ state?.pendingFaces ?? 0 }}）</button>
        <button type="button" :class="{ secondary: !ignoredScope }" :aria-pressed="ignoredScope" @click="scope = 'ignoredPeople'">已忽略</button>
      </nav>
      <nav v-if="ignoredScope" class="people-tabs" aria-label="已忽略的项目">
        <button type="button" :class="{ secondary: scope !== 'ignoredPeople' }" :aria-pressed="scope === 'ignoredPeople'" @click="scope = 'ignoredPeople'">人物（{{ state?.ignoredPeople ?? 0 }}）</button>
        <button type="button" :class="{ secondary: scope !== 'ignoredFaces' }" :aria-pressed="scope === 'ignoredFaces'" @click="scope = 'ignoredFaces'">单张人脸（{{ state?.ignoredFaces ?? 0 }}）</button>
      </nav>
      <div v-if="managedHidden" class="person-management">
        <div class="people-actions detail-title">
          <button type="button" class="secondary" @click="managedHidden = undefined">返回已忽略人物</button>
          <h2>{{ personName(managedHidden) }}</h2>
          <button type="button" :disabled="editDisabled" @click="setPeopleIgnored([managedHidden.id], false)">恢复此人</button>
        </div>
        <PeopleFaceEditor :scan-id="scanId" :person-id="managedHidden.id" :revision="revision" :writable="state?.writable" :busy="saving || libraryBusy || groupingBusy" @changed="emit('changed')" @preview="emit('preview', $event)" />
      </div>
      <template v-else-if="listScope">
        <label class="people-search">查找人物 <input v-model="search" placeholder="输入人物名称" maxlength="80" /></label>
        <div v-if="scope === 'people'" class="people-actions">
          <label><input v-model="unnamedOnly" type="checkbox" /> 仅未命名</label>
          <label><input v-model="singlePhotoOnly" type="checkbox" /> 仅出现在一张照片</label>
        </div>
        <p v-if="loading" role="status">正在读取人物…</p>
        <p v-else-if="people && !people.total" class="muted">{{ scope === 'ignoredPeople' ? '没有符合条件的已忽略人物。' : state?.enabled ? '没有符合条件的人物。' : '建立人物索引后，可为自动分组的人物命名。' }}</p>
        <div v-if="people?.items.length && state?.writable" class="people-actions">
          <button type="button" class="secondary" :disabled="editDisabled" @click="selectedIds = people!.items.map(person => person.id)">选择本页</button>
          <button v-if="selectedIds.length" type="button" class="secondary" :disabled="editDisabled" @click="selectedIds = []">取消选择</button>
          <button type="button" :disabled="editDisabled || !selectedIds.length" @click="setPeopleIgnored(selectedIds, scope === 'people')">
            {{ scope === 'people' ? '忽略所选人物' : '恢复所选人物' }}{{ selectedIds.length ? `（${selectedIds.length}）` : '' }}
          </button>
        </div>
        <ul v-if="people?.items.length" class="person-grid">
          <li v-for="person in people.items" :key="person.id" class="person-item" :class="{ checked: selectedIds.includes(person.id) }">
            <input v-if="state?.writable" v-model="selectedIds" class="person-checkbox" type="checkbox" :value="person.id" :disabled="editDisabled" :aria-label="`选择 ${personName(person)}`" />
            <button type="button" class="person-card secondary" @click="openPerson(person)">
            <img v-if="person.thumbnail" :src="person.thumbnail" :alt="personName(person)" width="112" height="112" loading="lazy" /><span v-else class="avatar-placeholder" aria-hidden="true">人</span>
            <strong>{{ personName(person) }}</strong><span>{{ person.photoCount }} 张照片</span>
            </button>
          </li>
        </ul>
        <nav v-if="people && people.total > 24" class="people-actions" aria-label="人物分页"><button type="button" class="secondary" :disabled="loading || people.page === 0" @click="page = people!.page - 1">上一页</button><span>{{ people.page + 1 }} / {{ Math.ceil(people.total / 24) }}</span><button type="button" class="secondary" :disabled="loading || (people.page + 1) * 24 >= people.total" @click="page = people!.page + 1">下一页</button></nav>
      </template>
      <PeopleFaceEditor v-else :scan-id="scanId" :scope="scope === 'pending' ? 'pending' : 'ignored'" :revision="revision" :writable="state?.writable" :busy="saving || libraryBusy || groupingBusy" @changed="emit('changed')" @preview="emit('preview', $event)" />
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
.people-panel input:not([type=checkbox]), .people-panel select { min-height: 40px; max-width: 100%; box-sizing: border-box; padding: .5rem .75rem; border: 1px solid var(--control-border); border-radius: .5rem; background: var(--surface); color: var(--text); font: inherit; }
.people-search input { width: min(24rem, 100%); }
.person-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 170px),1fr)); gap: 12px; padding: 0; margin: 24px 0; list-style: none; }
.person-item { position: relative; border-radius: 12px; }
.person-item.checked { background: var(--accent-soft); }
.person-checkbox { position: absolute; z-index: 1; inset-block-start: .625rem; inset-inline-start: .625rem; width: 1.125rem; height: 1.125rem; accent-color: var(--accent); }
.person-card { display: flex; width: 100%; flex-direction: column; align-items: center; gap: 12px; padding: 24px 12px; background: transparent; border: 1px solid transparent; border-radius: 12px; color: var(--text); }
.person-card:hover { border-color: var(--accent); background: var(--surface-subtle); }
.person-card img, .avatar-placeholder { width: 112px; height: 112px; border-radius: 50%; object-fit: cover; }
.avatar-placeholder { display: grid; place-items: center; background: var(--accent-soft); font-size: 2rem; color: var(--muted); }
.person-card strong { overflow-wrap: anywhere; max-width: 100%; }
.person-card > span:last-child, .muted { color: var(--muted); font-size: .875rem; }
.notice { padding: .75rem 1rem; border-radius: .75rem; background: var(--warning-soft); color: var(--warning); font-size: .875rem; }
.person-management { margin: 1rem 0; padding: 1rem; border: 1px solid var(--border); border-radius: .75rem; background: var(--surface); }
.detail-title h2 { margin: 0; }
progress { width: 100%; }
</style>
