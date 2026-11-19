<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, onBeforeUnmount, ref, shallowRef, watch } from "vue";
import { formatBytes } from "./formatBytes";
import DuplicatePager from "./DuplicatePager.vue";
import DuplicatePreview from "./DuplicatePreview.vue";
import type { BatchSummary, Confirmation, FileRow, Filter, Group, GroupPage, Issue, KeepRule, Page, Progress, ScanOutcome, SelectionAction, Sort, Summary, TrashOutcome } from "./duplicates";

const emit = defineEmits<{ busyChange: [busy: boolean] }>();
const directory = ref("");
const task = ref<"choosing" | "scanning" | "trashing" | "selecting">();
const busy = computed(() => task.value !== undefined);
watch(busy, value => emit("busyChange", value), { flush: "sync" });
const summary = shallowRef<Summary>();
const progress = shallowRef<Progress>();
const error = ref("");
const notice = ref("");
const cancelling = ref(false);
let activeScan: number | undefined;
let lastScanId = 0;
const phaseLabels = { scanning: "读取目录", sampling: "抽样预筛", hashing: "完整哈希", comparing: "核对内容" };
const dateFormatter = new Intl.DateTimeFormat("zh-CN", { dateStyle: "short", timeStyle: "medium" });
const kindLabels = { photo: "照片", video: "视频", other: "文件" };
const statusLabels = { moved: "已移入回收站", skipped: "已跳过", failed: "失败" };
const filter = ref<Filter>({ query: "", kind: null });
const search = ref("");
const composing = ref(false);
let searchTimer: ReturnType<typeof setTimeout> | undefined;
const sort = ref<Sort>("extraBytes");
const page = ref(0);
const groups = shallowRef<GroupPage>();
const loadingGroups = ref(false);
let groupRequest = 0;
const expanded = shallowRef<Group>();
const members = shallowRef<Page<FileRow>>();
const memberPage = ref(0);
const loadingMembers = ref(false);
let memberRequest = 0;
const keepRule = ref<KeepRule>("shallow");
const ruleLabels = { shallow: "最靠近根目录", oldest: "修改时间最早", newest: "修改时间最新" };
const preferredDirectory = ref("");
const issuesOpen = ref(false);
const issues = shallowRef<Page<Issue>>();
const issuePage = ref(0);
const loadingIssues = ref(false);
let issueRequest = 0;
const batch = shallowRef<BatchSummary>();
const outcomesOpen = ref(false);
const outcomes = shallowRef<Page<TrashOutcome>>();
const outcomePage = ref(0);
const loadingOutcomes = ref(false);
let outcomeRequest = 0;
const confirmationDialog = ref<HTMLDialogElement>();
const confirmation = shallowRef<Confirmation>();
const confirmPage = ref(0);
const confirmRevision = ref(0);
const confirmError = ref("");
const loadingConfirmation = ref(false);
let confirmRequest = 0;
const detailDialog = ref<HTMLDialogElement>();
const detail = shallowRef<FileRow>();
const controlsDisabled = computed(() => busy.value || loadingGroups.value || loadingMembers.value);

function closeGroup() {
  memberRequest++;
  expanded.value = undefined;
  members.value = undefined;
  memberPage.value = 0;
  loadingMembers.value = false;
}
function resetResults() {
  groupRequest++; issueRequest++; outcomeRequest++; confirmRequest++;
  summary.value = undefined; groups.value = undefined; progress.value = undefined;
  page.value = 0; closeGroup(); batch.value = undefined;
  issues.value = undefined; outcomes.value = undefined;
  issuesOpen.value = false; outcomesOpen.value = false;
  issuePage.value = 0; outcomePage.value = 0;
  loadingGroups.value = loadingIssues.value = loadingOutcomes.value = loadingConfirmation.value = false;
  confirmationDialog.value?.close(); detailDialog.value?.close();
  confirmation.value = undefined; detail.value = undefined;
}
function useDirectory(folder: string) {
  if (busy.value || directory.value === folder) return;
  directory.value = folder;
  error.value = notice.value = "";
  resetResults();
  search.value = ""; filter.value = { query: "", kind: null };
  sort.value = "extraBytes"; keepRule.value = "shallow"; preferredDirectory.value = "";
}
defineExpose({ useDirectory });
async function chooseDirectory() {
  if (busy.value) return;
  task.value = "choosing"; error.value = "";
  try {
    const folder = await invoke<string | null>("choose_folder", { title: "选择查重目录" });
    task.value = undefined;
    if (folder !== null) useDirectory(folder);
  } catch (cause) { error.value = String(cause); }
  finally { task.value = undefined; }
}
async function scanDirectory() {
  if (!directory.value || busy.value) return;
  resetResults(); error.value = notice.value = "";
  task.value = "scanning";
  const id = Math.max(Date.now(), lastScanId + 1);
  lastScanId = id; activeScan = id;
  progress.value = { phase: "scanning", found: 0, checked: 0, total: 0, bytesRead: 0, currentFile: "", elapsedMs: 0 };
  try {
    const channel = new Channel<Progress>(update => { if (activeScan === id) progress.value = update; });
    const result = await invoke<ScanOutcome>("scan_duplicates", { directory: directory.value, scanId: id, progress: channel });
    if (activeScan !== id) return;
    if (result.status === "finished") { summary.value = result.summary; await loadGroups(); }
    else notice.value = "扫描已取消，可以重新扫描。";
  } catch (cause) { if (activeScan === id) error.value = String(cause); }
  finally {
    if (activeScan === id) { activeScan = undefined; task.value = undefined; cancelling.value = false; progress.value = undefined; }
  }
}
async function cancelScan() {
  if (activeScan === undefined || cancelling.value) return;
  const id = activeScan; cancelling.value = true;
  try { await invoke("cancel_duplicates", { scanId: id }); }
  catch (cause) { if (activeScan === id) { cancelling.value = false; error.value = String(cause); } }
}
async function loadGroups() {
  const id = summary.value?.scanId;
  if (id === undefined) return;
  const request = ++groupRequest;
  loadingGroups.value = true;
  try {
    const result = await invoke<GroupPage>("duplicate_groups", { scanId: id, filter: { ...filter.value }, sort: sort.value, page: page.value });
    if (request !== groupRequest || summary.value?.scanId !== id) return;
    const last = Math.max(0, Math.ceil(result.total / 25) - 1);
    if (page.value > last) { page.value = last; await loadGroups(); return; }
    groups.value = result; summary.value = result.summary;
    if (expanded.value) {
      const g = result.items.find(g => g.id === expanded.value?.id);
      if (!g) closeGroup(); else expanded.value = g;
    }
  } catch (cause) { if (request === groupRequest) { groups.value = undefined; error.value = String(cause); } }
  finally { if (request === groupRequest) loadingGroups.value = false; }
}
async function loadMembers() {
  const id = summary.value?.scanId, groupId = expanded.value?.id;
  if (id === undefined || groupId === undefined) return;
  const request = ++memberRequest; loadingMembers.value = true;
  try {
    const result = await invoke<Page<FileRow>>("duplicate_members", { scanId: id, groupId, page: memberPage.value });
    if (request === memberRequest && summary.value?.scanId === id && expanded.value?.id === groupId) members.value = result;
  } catch (cause) { if (request === memberRequest) { members.value = undefined; error.value = String(cause); } }
  finally { if (request === memberRequest) loadingMembers.value = false; }
}
function expand(group: Group) {
  if (controlsDisabled.value) return;
  if (expanded.value?.id === group.id) { closeGroup(); return; }
  closeGroup(); expanded.value = group; void loadMembers();
}
async function changeSelection(action: SelectionAction) {
  const id = summary.value?.scanId;
  if (id === undefined || controlsDisabled.value) return;
  task.value = "selecting"; error.value = "";
  try {
    summary.value = await invoke<Summary>("duplicate_selection", { scanId: id, action, filter: { ...filter.value } });
    await loadGroups(); await loadMembers();
  } catch (cause) { error.value = String(cause); }
  finally { task.value = undefined; }
}
function applyRule() { void changeSelection({ kind: "apply", filter: { ...filter.value }, rule: keepRule.value, preferredDirectory: preferredDirectory.value }); }
function startComposition() { composing.value = true; clearTimeout(searchTimer); }
function scheduleSearch() {
  clearTimeout(searchTimer);
  if (composing.value) return;
  searchTimer = setTimeout(() => { filter.value = { ...filter.value, query: search.value }; }, 250);
}
watch(search, scheduleSearch);
watch([filter, sort], () => { page.value = 0; closeGroup(); void loadGroups(); }, { deep: true });
function changePage(value: number) { page.value = value; closeGroup(); void loadGroups(); }
function changeMemberPage(value: number) { memberPage.value = value; void loadMembers(); }
async function loadIssues(value = issuePage.value) {
  const id = summary.value?.scanId; if (id === undefined) return;
  issuePage.value = value;
  const request = ++issueRequest; loadingIssues.value = true;
  try {
    const result = await invoke<Page<Issue>>("duplicate_issues", { scanId: id, page: value });
    if (request === issueRequest && summary.value?.scanId === id) issues.value = result;
  } catch (cause) { if (request === issueRequest) error.value = String(cause); }
  finally { if (request === issueRequest) loadingIssues.value = false; }
}
async function loadOutcomes(value = outcomePage.value) {
  const id = summary.value?.scanId; if (id === undefined) return;
  outcomePage.value = value;
  const request = ++outcomeRequest; loadingOutcomes.value = true;
  try {
    const result = await invoke<Page<TrashOutcome>>("duplicate_outcomes", { scanId: id, page: value });
    if (request === outcomeRequest && summary.value?.scanId === id) outcomes.value = result;
  } catch (cause) { if (request === outcomeRequest) error.value = String(cause); }
  finally { if (request === outcomeRequest) loadingOutcomes.value = false; }
}
watch(issuesOpen, open => { if (open) void loadIssues(); });
watch(outcomesOpen, open => { if (open) void loadOutcomes(); });
async function loadConfirmation(value = confirmPage.value) {
  const id = summary.value?.scanId; if (id === undefined) return;
  confirmPage.value = value; confirmation.value = undefined; confirmError.value = "";
  const request = ++confirmRequest; loadingConfirmation.value = true;
  try {
    const result = await invoke<Confirmation>("duplicate_confirmation", { scanId: id, revision: confirmRevision.value, page: value });
    if (request === confirmRequest && summary.value?.scanId === id && confirmationDialog.value?.open) confirmation.value = result;
  } catch (cause) { if (request === confirmRequest) confirmError.value = String(cause); }
  finally { if (request === confirmRequest) loadingConfirmation.value = false; }
}
function openConfirmation() {
  if (!summary.value?.selected || controlsDisabled.value) return;
  confirmRevision.value = summary.value.revision;
  confirmPage.value = 0; confirmationDialog.value?.showModal(); void loadConfirmation();
}
async function trashSelection() {
  const id = summary.value?.scanId;
  if (id === undefined || busy.value || !confirmationDialog.value?.open || !confirmation.value || loadingConfirmation.value) return;
  confirmationDialog.value.close(); detailDialog.value?.close(); detail.value = undefined;
  task.value = "trashing"; error.value = notice.value = "";
  try {
    batch.value = await invoke<BatchSummary>("trash_duplicates", { scanId: id, revision: confirmRevision.value });
    closeGroup(); outcomes.value = undefined; outcomePage.value = 0;
    await loadGroups();
    outcomesOpen.value = true;
    await loadOutcomes();
  } catch (cause) { error.value = String(cause); }
  finally { task.value = undefined; }
}
function showDetail(file: FileRow) { detail.value = file; detailDialog.value?.showModal(); }
async function fileAction(command: "reveal_duplicate" | "copy_duplicate_path", file: FileRow) {
  if (!summary.value || busy.value) return;
  error.value = notice.value = "";
  try {
    const result = await invoke<string | null>(command, { scanId: summary.value.scanId, fileId: file.id });
    notice.value = command === "copy_duplicate_path" ? "已复制完整路径。" : result === "openedParent" ? "无法选中文件，已打开所在目录。" : "已在文件管理器中显示。";
  } catch (cause) { error.value = String(cause); }
}
onBeforeUnmount(() => { clearTimeout(searchTimer); groupRequest++; memberRequest++; issueRequest++; outcomeRequest++; confirmRequest++; });
</script>

<template>
  <div>
    <form @submit.prevent="scanDirectory">
      <label for="duplicate-dir">
        扫描目录
      </label>
      <div class="folder-row">
        <input id="duplicate-dir" :value="directory" placeholder="尚未选择扫描目录" readonly />
        <button type="button" class="secondary" :disabled="busy" @click="chooseDirectory">
          选择目录
        </button>
      </div>
      <p class="file-meta">
        递归检查所有类型文件，仅将内容完全相同的文件列为重复；跳过符号链接和 .photosys 数据目录。
      </p>
      <div class="actions">
        <button type="submit" :disabled="busy || !directory">
          {{ task === 'scanning' ? '正在扫描…' : summary ? '重新扫描' : '开始扫描' }}
        </button>
        <button v-if="task === 'scanning'" type="button" class="secondary" :disabled="cancelling" @click="cancelScan">
          {{ cancelling ? '正在取消…' : '取消扫描' }}
        </button>
      </div>
      <div v-if="progress" class="archive-progress" role="status">
        <p>
          {{ phaseLabels[progress.phase] }} · 已发现 {{ progress.found }} 个文件
          <template v-if="progress.phase !== 'scanning'">
            · {{ progress.checked }} / {{ progress.total }}
          </template>
        </p>
        <progress v-if="progress.phase !== 'scanning' && progress.total" :value="progress.checked" :max="progress.total" aria-label="当前阶段文件进度">
        </progress>
        <progress v-else aria-label="读取目录进度">
        </progress>
        <p>
          已读取 {{ formatBytes(progress.bytesRead) }} · 耗时 {{ (progress.elapsedMs / 1000).toFixed(1) }} 秒
        </p>
        <p class="current-file">
          {{ progress.currentFile }}
        </p>
      </div>
      <p v-if="task === 'trashing'" role="status">
        正在逐项核验副本并移到回收站…
      </p>
      <p v-if="error" class="error" role="alert">
        {{ error }}
      </p>
      <p v-if="notice" role="status">
        {{ notice }}
      </p>
    </form>
    <section v-if="summary" aria-labelledby="duplicates-results">
      <h2 id="duplicates-results">
        查重结果
      </h2>
      <dl aria-live="polite">
        <div>
          <dt>
            扫描文件
          </dt>
          <dd>
            {{ summary.scanned }}
          </dd>
        </div>
        <div>
          <dt>
            重复组 / 文件
          </dt>
          <dd>
            {{ summary.groups }} / {{ summary.files }}
          </dd>
        </div>
        <div>
          <dt>
            可整理副本
          </dt>
          <dd>
            {{ summary.extras }} 个 · {{ formatBytes(summary.extraBytes) }}
          </dd>
        </div>
      </dl>
      <p class="file-meta">
        本次读取 {{ formatBytes(summary.bytesRead) }}，耗时 {{ (summary.elapsedMs / 1000).toFixed(1) }} 秒。大小均为逻辑大小，实际释放空间取决于回收站清空情况。
      </p>
      <div v-if="summary.issues" class="details-section">
        <button type="button" class="secondary" :aria-expanded="issuesOpen" @click="issuesOpen = !issuesOpen">
          扫描问题 {{ summary.issues }} 项
        </button>
        <div v-if="issuesOpen">
          <p v-if="loadingIssues" role="status">
            加载问题明细…
          </p>
          <ul class="duplicate-list">
            <li v-for="(issue, index) in issues?.items" :key="index">
              <strong>
                {{ issue.path }}
              </strong>
              <span>
                {{ issue.reason }}
              </span>
            </li>
          </ul>
          <DuplicatePager :page="issuePage" :total="summary.issues" label="问题明细分页" :disabled="loadingIssues || busy" @change="loadIssues" />
        </div>
      </div>
      <div v-if="batch" class="batch-result">
        <p role="status">
          已移入回收站 {{ batch.moved }} 个，跳过 {{ batch.skipped }} 个，失败 {{ batch.failed }} 个。有效失败项保留勾选，可重试。
        </p>
        <button type="button" class="secondary" :aria-expanded="outcomesOpen" @click="outcomesOpen = !outcomesOpen">
          处理明细
        </button>
        <div v-if="outcomesOpen">
          <p v-if="loadingOutcomes" role="status">
            加载处理明细…
          </p>
          <ul class="duplicate-list">
            <li v-for="outcome in outcomes?.items" :key="outcome.id">
              <strong>
                {{ outcome.relativePath }}
              </strong>
              <span>
                {{ statusLabels[outcome.status] }}
                <template v-if="outcome.reason">
                  · {{ outcome.reason }}
                </template>
              </span>
            </li>
          </ul>
          <DuplicatePager :page="outcomePage" :total="batch.moved + batch.skipped + batch.failed" label="处理明细分页" :disabled="loadingOutcomes || busy" @change="loadOutcomes" />
        </div>
      </div>
      <div class="duplicate-filters">
        <label>
          搜索文件名或相对路径
          <input v-model="search" type="search" :disabled="busy" placeholder="输入关键字" @compositionstart="startComposition" @compositionend="composing = false; scheduleSearch()" />
        </label>
        <label>
          文件类别
          <select v-model="filter.kind" :disabled="busy">
            <option :value="null">
              全部文件
            </option>
            <option value="photo">
              照片与实况照片
            </option>
            <option value="video">
              视频
            </option>
            <option value="other">
              其他文件
            </option>
          </select>
        </label>
        <label>
          排序
          <select v-model="sort" :disabled="busy">
            <option value="extraBytes">
              副本逻辑大小优先
            </option>
            <option value="size">
              文件大小优先
            </option>
            <option value="count">
              文件数量优先
            </option>
          </select>
        </label>
      </div>
      <details class="retention-options">
        <summary>
          保留规则：{{ preferredDirectory ? `优先 ${preferredDirectory}，再按` : '' }}{{ ruleLabels[keepRule] }}
        </summary>
        <fieldset :disabled="controlsDisabled" class="retention">
          <legend>
            自动保留规则
          </legend>
          <div class="duplicate-filters">
            <label>
              优先保留子目录
              <input v-model="preferredDirectory" placeholder="相对路径，如 精选/原片（可留空）" />
            </label>
            <label>
              组内保留顺序
              <select v-model="keepRule">
                <option value="shallow">
                  最靠近根目录
                </option>
                <option value="oldest">
                  修改时间最早
                </option>
                <option value="newest">
                  修改时间最新
                </option>
              </select>
            </label>
          </div>
          <p class="file-meta">
            优先保留指定目录中的文件，再应用组内规则；修改时间不是拍摄时间。硬链接全部保留，每组至少保留一份。
          </p>
        </fieldset>
      </details>
      <p class="file-meta">
        自动勾选会替换全部已有勾选；筛选命中任一文件时，按完整重复组处理。硬链接全部保留，每组至少保留一份。
      </p>
      <div class="actions">
        <button type="button" class="secondary" :disabled="controlsDisabled || !groups?.total" @click="applyRule">
          按规则勾选当前筛选结果
        </button>
        <button type="button" class="secondary" :disabled="controlsDisabled || !summary.selected" @click="changeSelection({ kind: 'clear' })">
          清除全部勾选
        </button>
        <button type="button" class="danger" :disabled="controlsDisabled || !summary.selected" @click="openConfirmation">
          移到回收站
        </button>
      </div>
      <p role="status">
        已勾选 {{ summary.selected }} 个文件 · {{ formatBytes(summary.selectedBytes) }}
        <template v-if="summary.hiddenSelected">
          ，其中 {{ summary.hiddenSelected }} 个在当前筛选范围外，清理时也会处理
        </template>
      </p>
      <p v-if="loadingGroups" role="status">
        加载结果…
      </p>
      <button v-if="!groups && !loadingGroups" type="button" class="secondary" @click="loadGroups">
        重试加载结果
      </button>
      <p v-if="groups && !groups.total">
        {{ summary.groups ? '当前筛选下没有重复组。' : batch ? '当前结果中已无重复组。' : '未发现内容完全相同的文件。' }}
      </p>
      <article v-for="group in groups?.items" :key="group.id" class="duplicate-group" :aria-busy="loadingGroups">
        <div class="group-header">
          <button v-if="group.representative.previewAvailable" type="button" class="preview-button" :disabled="busy" :aria-label="`预览 ${group.representative.name}`" @click="showDetail(group.representative)">
            <DuplicatePreview :scan-id="summary.scanId" :file-id="group.representative.id" :name="group.representative.name" />
          </button>
          <span v-else class="type-icon" role="img" :aria-label="kindLabels[group.representative.kind]">
            ▤
          </span>
          <div>
            <h3>
              {{ group.count }} 个文件 · 每个 {{ formatBytes(group.size) }}
            </h3>
            <p class="file-path">
              {{ group.representative.relativePath }}
            </p>
            <p class="file-meta">
              可整理 {{ formatBytes(group.extraBytes) }} · 已勾选 {{ group.selected }} 个
            </p>
          </div>
          <button type="button" class="secondary" :disabled="controlsDisabled" :aria-expanded="expanded?.id === group.id" @click="expand(group)">
            {{ expanded?.id === group.id ? '收起' : '查看文件' }}
          </button>
        </div>
        <p v-if="group.needsRescan" class="error">
          本组文件已变化，请重新扫描后处理。
        </p>
        <div v-if="expanded?.id === group.id">
          <p v-if="loadingMembers" role="status">
            加载组内文件…
          </p>
          <button v-if="!members && !loadingMembers" type="button" class="secondary" @click="loadMembers">
            重试加载组内文件
          </button>
          <ul class="duplicate-list" :aria-busy="loadingMembers">
            <li v-for="file in members?.items" :key="file.id" class="file-row">
              <label class="duplicate-file">
                <input type="checkbox" :checked="file.selected" :disabled="controlsDisabled || file.disabled" @change="changeSelection({ kind: 'toggle', fileId: file.id })" />
                <span>
                  <strong>
                    {{ file.name }}
                  </strong>
                  <span class="file-path">
                    {{ file.relativePath }}
                  </span>
                  <span class="file-meta">
                    <time :datetime="file.modifiedAt">
                      {{ dateFormatter.format(new Date(file.modifiedAt)) }}
                    </time>
                    · {{ kindLabels[file.kind] }}
                    <template v-if="file.hardLink">
                      · 硬链接，保留
                    </template>
                    <template v-else-if="!file.selected && group.selected === group.count - 1">
                      · 本组保留项
                    </template>
                  </span>
                </span>
              </label>
              <div class="actions file-actions">
                <button v-if="file.previewAvailable" type="button" class="secondary" :disabled="busy" @click="showDetail(file)">
                  预览
                </button>
                <button type="button" class="secondary" :disabled="busy" @click="fileAction('reveal_duplicate', file)">
                  在文件管理器中显示
                </button>
                <button type="button" class="secondary" :disabled="busy" @click="fileAction('copy_duplicate_path', file)">
                  复制完整路径
                </button>
              </div>
            </li>
          </ul>
          <DuplicatePager :page="memberPage" :total="group.count" label="组内文件分页" :disabled="controlsDisabled" @change="changeMemberPage" />
        </div>
      </article>
      <DuplicatePager v-if="groups" :page="page" :total="groups.total" label="重复组分页" :disabled="controlsDisabled" @change="changePage" />
    </section>
    <dialog ref="confirmationDialog" class="duplicate-dialog" aria-labelledby="trash-heading" @close="confirmRequest++; confirmation = undefined">
      <h2 id="trash-heading">
        确认移到回收站
      </h2>
      <p>
        将移入 {{ summary?.selected ?? 0 }} 个文件，逻辑大小共 {{ formatBytes(summary?.selectedBytes ?? 0) }}。包含筛选范围外的勾选项，可从系统回收站恢复。
      </p>
      <p class="file-meta">
        清空回收站后才可能释放实际磁盘空间。
      </p>
      <p class="current-file">
        扫描目录 {{ directory }}
      </p>
      <p v-if="loadingConfirmation" role="status">
        加载确认明细…
      </p>
      <p v-if="confirmError" class="error" role="alert">
        {{ confirmError }}
      </p>
      <div class="trash-preview" tabindex="0" aria-label="待处理和保留的文件">
        <ul class="duplicate-list">
          <li v-for="row in confirmation?.items" :key="row.file.id">
            <strong>
              {{ row.moving ? '将移入回收站' : '保留' }} · 组 {{ row.groupId }}
            </strong>
            <span>
              {{ row.file.relativePath }}
            </span>
            <span class="file-meta">
              {{ formatBytes(row.size) }}
              <template v-if="row.file.hardLink">
                · 硬链接
              </template>
            </span>
          </li>
        </ul>
      </div>
      <DuplicatePager v-if="confirmation" :page="confirmPage" :total="confirmation.total" label="确认明细分页" :disabled="loadingConfirmation" @change="loadConfirmation" />
      <div class="actions">
        <button type="button" class="secondary" autofocus @click="confirmationDialog?.close()">
          取消
        </button>
        <button type="button" class="danger" :disabled="!confirmation || loadingConfirmation || busy" @click="trashSelection">
          确认移到回收站
        </button>
      </div>
    </dialog>
    <dialog ref="detailDialog" class="duplicate-dialog" aria-labelledby="duplicate-preview-heading" @close="detail = undefined">
      <template v-if="detail && summary">
        <h2 id="duplicate-preview-heading">
          {{ detail.name }}
        </h2>
        <DuplicatePreview :scan-id="summary.scanId" :file-id="detail.id" :name="detail.name" detail />
        <p class="file-path">
          {{ detail.relativePath }}
        </p>
        <p class="file-meta">
          {{ formatBytes(detail.size) }} · 修改时间 {{ dateFormatter.format(new Date(detail.modifiedAt)) }}
        </p>
        <p v-if="notice" role="status">
          {{ notice }}
        </p>
        <p v-if="error" class="error" role="alert">
          {{ error }}
        </p>
        <div class="actions">
          <button type="button" class="secondary" :disabled="busy" @click="fileAction('reveal_duplicate', detail)">
            在文件管理器中显示
          </button>
          <button type="button" class="secondary" :disabled="busy" @click="fileAction('copy_duplicate_path', detail)">
            复制完整路径
          </button>
          <button type="button" autofocus @click="detailDialog?.close()">
            关闭
          </button>
        </div>
      </template>
    </dialog>
  </div>
</template>

<style scoped>
.batch-result, .details-section { margin-block: 1rem; padding-block: .75rem; border-block-end: 1px solid var(--border); }
.duplicate-filters { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 13rem), 1fr)); gap: .75rem; margin-block: 1rem; }
.duplicate-filters label { min-width: 0; }
.duplicate-filters :is(input, select) { box-sizing: border-box; width: 100%; margin-top: .35rem; }
.retention-options { margin-block: .75rem; }
.retention-options > summary { cursor: pointer; color: var(--accent); overflow-wrap: anywhere; }
.retention { margin-block: 1rem; border: 1px solid var(--border); border-radius: .625rem; padding: .875rem; min-width: 0; }
.duplicate-group { margin-top: .875rem; padding: .875rem 1rem; border: 1px solid var(--border); border-radius: .625rem; }
.group-header { display: flex; flex-wrap: wrap; align-items: center; gap: .75rem; }
.group-header > div { flex: 1; min-width: min(100%, 10rem); }
h3 { margin: 0; font-size: 1rem; overflow-wrap: anywhere; }
.duplicate-list { display: grid; gap: .625rem; list-style: none; padding: 0; margin: .625rem 0 0; }
.duplicate-list li > :is(strong, span), .file-path, .file-meta { display: block; overflow-wrap: anywhere; }
.file-path, .file-meta { color: var(--muted); margin: .25rem 0 0; }
.file-meta { font-size: .875rem; }
.file-row { border-block-start: 1px solid var(--border); padding-block: .75rem; }
.duplicate-file { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: .625rem; align-items: start; margin: 0; font-weight: 400; cursor: pointer; }
.duplicate-file:has(input:disabled) { cursor: default; }
.duplicate-file input { width: 1rem; height: 1rem; margin: .15rem 0 0; accent-color: var(--accent); }
.file-actions { margin-block-end: 0; }
.preview-button { padding: 0; border: 0; background: transparent; flex: 0 0 auto; }
.type-icon { width: 3rem; font-size: 2rem; text-align: center; color: var(--muted); }
.duplicate-dialog { box-sizing: border-box; width: min(44rem, calc(100% - 2rem)); max-height: calc(100% - 2rem); padding: clamp(1rem, 4vw, 1.5rem); border: 1px solid var(--control-border); border-radius: .875rem; color: inherit; overflow-wrap: anywhere; }
.duplicate-dialog::backdrop { background: var(--backdrop); }
.trash-preview { max-height: 35vh; overflow: auto; border-block: 1px solid var(--border); padding: .5rem; }
.trash-preview li + li { border-block-start: 1px solid var(--border); padding-top: .5rem; }
.duplicate-dialog > .actions { justify-content: end; }
.danger { background: var(--danger); }
</style>
