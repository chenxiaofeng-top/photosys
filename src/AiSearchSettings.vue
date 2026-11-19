<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { onBeforeUnmount, onMounted, ref, watch, computed } from "vue";
import { ElButton, ElIcon, ElDialog } from "element-plus";
import { MagicStick, Setting } from "@element-plus/icons-vue";
import "element-plus/es/components/button/style/css";
import "element-plus/es/components/icon/style/css";
import { formatBytes } from "./formatBytes";
import type { IndexStatus, ModelStatus, ModelOption } from "./aiSearch";
import type { PeopleStatus } from "./people";
import "element-plus/es/components/dialog/style/css";

const props = defineProps<{ scanId?: number; libraryBusy: boolean; modelKind?: "search" | "people"; peopleStatus?: PeopleStatus; analysisBusy?: boolean }>();
const peopleModel = computed(() => props.modelKind === "people");
const settingsId = computed(() => peopleModel.value ? "people-ai-settings" : "ai-settings-body");
const directoryId = computed(() => peopleModel.value ? "people-model-directory" : "ai-model-directory");
const emit = defineEmits<{ modelReady: [ready: boolean]; indexChange: [status: IndexStatus]; updated: []; modelChanged: []; rebuild: [] }>();
const model = ref<ModelStatus>();
const catalog = ref<ModelOption[]>([]);
const selectedModelId = ref("");
const selectedModel = computed(() => catalog.value.find(m => m.id === selectedModelId.value));
const activeModel = computed(() => catalog.value.find(m => m.id === model.value?.activeModelId));
const pendingSelection = computed(() => selectedModelId.value !== model.value?.activeModelId);
const displayedIndex = computed(() => peopleModel.value ? props.peopleStatus : index.value);
const confirmOpen = ref(false);
const affectedCount = ref<number>();
const switchNotice = computed(() => peopleModel.value
  ? '更换后需要重新计算人脸特征。系统尽量保留人物名称和人工归属，无法可靠对应的人脸会提示核对。点击“重建人物索引”开始，可停止后继续。'
  : '更换后需要重新建立照片内容索引。重建前，仅能搜索与新模型兼容的已索引照片；可继续按文件名浏览和搜索。点击“重建内容索引”开始，可停止后继续。');
let modelRequest = 0;
const index = ref<IndexStatus>();
const open = ref(false);
const source = ref<"mirror" | "official">("mirror");
const modelTask = ref(false);
const indexTask = ref(false);
const cancelling = ref(false);
const modelError = ref("");
const indexError = ref("");
interface Progress { phase: "downloading" | "verifying" | "testing" | "importing"; file: string; downloaded: number; total: number; source: "mirror" | "official" }
const progress = ref<Progress>();
const issuePage = ref(0);
const issues = ref<{ page: number; total: number; items: { path: string; reason: string }[] }>();
const issuesOpen = ref(false);
let mounted = true;
let indexRequest = 0;

function setModel(value: ModelStatus) { model.value = value; emit("modelReady", value.activeReady); }
function setIndex(value: IndexStatus) {
  if (value.scanId !== props.scanId) return;
  index.value = value;
  emit("indexChange", value);
}

async function loadModel() {
  const request = ++modelRequest;
  modelTask.value = true;
  try {
    const value = await invoke<ModelStatus>("ai_model_status", { kind: props.modelKind, modelId: selectedModelId.value || undefined });
    if (!mounted || request !== modelRequest) return;
    if (!selectedModelId.value) selectedModelId.value = value.modelId;
    setModel(value);
  } catch (cause) { if (mounted && request === modelRequest) modelError.value = String(cause); }
  finally { if (mounted && request === modelRequest) modelTask.value = false; }
}
async function selectModel() { modelError.value = ""; confirmOpen.value = false; await loadModel(); }
async function prepareActivation() {
  modelError.value = ""; modelTask.value = true;
  const scanId = props.scanId; const id = selectedModelId.value;
  try {
    affectedCount.value = scanId === undefined ? undefined : await invoke<number>("ai_model_change_impact", { scanId, kind: props.modelKind, modelId: id });
    if (mounted && scanId === props.scanId && id === selectedModelId.value) confirmOpen.value = true;
  } catch (cause) { modelError.value = String(cause); }
  finally { modelTask.value = false; }
}
async function activateModel() {
  modelTask.value = true; modelError.value = "";
  try {
    setModel(await invoke<ModelStatus>("ai_activate_model", { kind: props.modelKind, modelId: selectedModelId.value }));
    confirmOpen.value = false;
    indexRequest++; indexTask.value = false;
    emit("modelChanged");
    await refreshIndex(false);
  } catch (cause) { modelError.value = String(cause); }
  finally { modelTask.value = false; }
}
async function openModelSource() {
  try { await invoke("ai_open_model_source", { kind: props.modelKind, modelId: selectedModelId.value }); }
  catch (cause) { modelError.value = String(cause); }
}
function rebuildIndex() { if (peopleModel.value) emit("rebuild"); else void updateIndex(false); }

async function chooseDirectory() {
  modelTask.value = true;
  modelError.value = "";
  try {
    const directory = await invoke<string | null>("choose_folder", { title: "选择 AI 模型安装目录（多个相册可以共用）" });
    if (directory) {
      if (props.scanId !== undefined) await invoke(peopleModel.value ? "people_cancel_index" : "ai_cancel_index", { scanId: props.scanId });
      await invoke("ai_set_model_directory", { directory, kind: props.modelKind });
      await loadModel();
      emit("modelChanged");
      await refreshIndex(false);
    }
  } catch (cause) { modelError.value = String(cause); }
  finally { modelTask.value = false; }
}

async function install(offline: boolean) {
  modelTask.value = true;
  modelError.value = "";
  progress.value = undefined;
  let receiving = true;
  try {
    const packagePath = offline ? await invoke<string | null>("ai_choose_model_package") : null;
    if (offline && !packagePath) return;
    setModel(await invoke<ModelStatus>("ai_install_model", {
      source: source.value, package: packagePath, kind: props.modelKind, modelId: selectedModelId.value,
      progress: new Channel<Progress>((update) => { if (mounted && receiving) progress.value = update; }),
    }));
  } catch (cause) { modelError.value = String(cause); }
  finally {
    receiving = false;
    progress.value = undefined;
    cancelling.value = false;
    modelTask.value = false;
    await loadModel();
    await refreshIndex(false);
  }
}

async function cancelInstall() {
  cancelling.value = true;
  try { await invoke("ai_cancel_install", { kind: props.modelKind }); } catch (cause) { modelError.value = String(cause); }
}

async function refreshIndex(automatic = false) {
  if (peopleModel.value || props.libraryBusy) return;
  const scanId = props.scanId;
  if (scanId === undefined) return;
  const request = indexRequest;
  try {
    const value = await invoke<IndexStatus>("ai_index_status", { scanId });
    if (!mounted || props.libraryBusy || request !== indexRequest || scanId !== props.scanId) return;
    setIndex(value);
    if (automatic && !props.libraryBusy && value.enabled && !value.needsRebuild && value.writable && model.value?.activeReady && !indexTask.value) await updateIndex(true);
  } catch (cause) { if (!props.libraryBusy && request === indexRequest && scanId === props.scanId) indexError.value = String(cause); }
}

async function updateIndex(automatic = false) {
  const scanId = props.scanId;
  if (props.libraryBusy || scanId === undefined || indexTask.value || modelTask.value || !model.value?.activeReady) return;
  const request = ++indexRequest;
  indexTask.value = true;
  indexError.value = "";
  try {
    const value = await invoke<IndexStatus>("ai_update_index", {
      scanId, automatic,
      progress: new Channel<IndexStatus>((value) => { if (mounted && request === indexRequest) setIndex(value); }),
    });
    if (mounted && request === indexRequest) { setIndex(value); emit("updated"); }
  } catch (cause) {
    if (mounted && request === indexRequest) indexError.value = String(cause);
  } finally {
    if (mounted && request === indexRequest) {
      indexTask.value = false;
      await refreshIndex();
    }
  }
}

async function cancelIndex() {
  if (peopleModel.value) return;
  try { if (props.scanId !== undefined) await invoke(peopleModel.value ? "people_cancel_index" : "ai_cancel_index", { scanId: props.scanId }); }
  catch (cause) { indexError.value = String(cause); }
}

watch(() => props.libraryBusy, busy => { if (!busy) void refreshIndex(true); });

watch(() => props.scanId, (id, oldId) => {
  indexRequest++;
  indexTask.value = false;
  index.value = undefined;
  issues.value = undefined;
  issuesOpen.value = false;
  issuePage.value = 0;
  indexError.value = "";
  confirmOpen.value = false;
  if (!peopleModel.value && oldId !== undefined) void invoke("ai_cancel_index", { scanId: oldId }).catch(() => {});
  if (id !== undefined) void refreshIndex(true);
});

watch([issuesOpen, issuePage, () => index.value?.failed], async () => {
  const scanId = props.scanId;
  const request = indexRequest;
  if (!issuesOpen.value || scanId === undefined) return;
  try {
    const value = await invoke<typeof issues.value>("ai_index_issues", { scanId, page: issuePage.value });
    if (mounted && request === indexRequest && scanId === props.scanId) issues.value = value;
  } catch (cause) { if (request === indexRequest) indexError.value = String(cause); }
});

onMounted(async () => {
  try { catalog.value = await invoke<ModelOption[]>("ai_model_catalog", { kind: props.modelKind }); }
  catch (cause) { modelError.value = String(cause); }
  await loadModel(); await refreshIndex(true);
});
onBeforeUnmount(() => { mounted = false; indexRequest++; modelRequest++; void cancelIndex(); });
defineExpose({ refreshIndex });
</script>

<template>
  <section class="ai-settings" :aria-label="peopleModel ? '人物模型设置' : 'AI 搜索设置'">
    <div class="ai-summary">
      <span class="ai-label"><ElIcon aria-hidden="true"><MagicStick /></ElIcon>本地 AI</span>
      <span class="model-status" :class="{ ready: model?.activeReady }" role="status">{{ modelTask ? '正在处理模型…' : model?.activeReady ? '当前模型已就绪' : '尚未安装可用模型' }}</span>
      <span v-if="index" role="status">{{ indexTask ? '正在建立索引 · ' : '' }}已索引 {{ index.indexed }} / {{ index.total }} 张<span v-if="index.pending"> · 待处理 {{ index.pending }} 张</span><span v-if="index.failed"> · 失败 {{ index.failed }} 张</span></span>
      <ElButton v-if="indexTask" @click="cancelIndex">停止索引</ElButton>
      <ElButton v-else-if="!peopleModel && scanId !== undefined && model?.activeReady"
        :disabled="libraryBusy || modelTask || !index?.writable" @click="updateIndex(false)">{{ index?.needsRebuild ? '重建内容索引' : index?.enabled ? index.failed ? '更新索引 / 重试失败项' : '更新索引' : '建立索引' }}</ElButton>
      <ElButton class="settings-toggle" text :icon="Setting" :aria-expanded="open" :aria-controls="settingsId" @click="open = !open">{{ peopleModel ? '人物模型设置' : 'AI 搜索设置' }}</ElButton>
    </div>
    <div v-show="open" :id="settingsId" class="ai-settings-body">
      <p>{{ peopleModel ? "在本机检出人脸，自动按人物分组，可命名和修改归属。" : "用中文描述寻找照片。" }}模型安装后可断网使用，照片只在本机处理。</p>
      <p>当前使用：<strong>{{ activeModel?.name ?? '正在读取…' }}</strong></p>
      <label class="model-picker">选择模型
        <select v-model="selectedModelId" :disabled="modelTask" @change="selectModel">
          <option v-for="option in catalog" :key="option.id" :value="option.id">{{ option.name }}{{ option.id === model?.activeModelId ? '（当前使用）' : '' }}</option>
        </select>
      </label>
      <article v-if="selectedModel" class="model-description">
        <strong>{{ selectedModel.name }}</strong>
        <span class="model-badge">{{ pendingSelection ? '待选择' : '当前使用' }} · {{ modelTask ? '正在检查／处理模型…' : model?.modelId === selectedModelId && model.ready ? '已安装并通过自检' : '尚未安装可用模型' }}</span>
        <p>{{ selectedModel.description }}</p>
        <p>下载大小：{{ formatBytes(selectedModel.assets.reduce((sum, asset) => sum + asset.size, 0)) }} · CPU：{{ selectedModel.cpu }}</p>
        <p class="description">{{ selectedModel.measurement }}</p>
        <p>许可：{{ selectedModel.license }} · <a :href="selectedModel.source" target="_blank" rel="noopener noreferrer" @click.prevent="openModelSource">模型来源与说明</a></p>
        <p>仅分析照片与实况封面，不分析视频。人物名称需要手动整理；内容搜索不提供专门的图片文字识别。</p>
      </article>
      <div class="ai-directory">
        <label :for="directoryId">模型安装目录</label>
        <input :id="directoryId" :value="model?.directory ?? ''" placeholder="请手动指定目录" readonly />
        <button class="secondary" type="button" :disabled="modelTask || indexTask || analysisBusy" @click="chooseDirectory">选择目录</button>
      </div>
      <p class="description">多个相册可以共用此目录；更换目录不会搬动或删除原有模型。</p>
      <div class="ai-summary">
        <label>下载源 <select v-model="source" :disabled="modelTask">
          <option value="mirror">国内优先 · HF-Mirror（第三方镜像）</option>
          <option value="official">Hugging Face 原源</option>
        </select></label>
        <button type="button" :disabled="!model?.directory || modelTask || model?.ready" @click="install(false)">
          {{ model?.resumableBytes ? '继续下载 / 重试安装' : '下载并安装模型' }}
        </button>
        <button class="secondary" type="button" :disabled="!model?.directory || modelTask" @click="install(true)">导入离线模型包</button>
        <button v-if="pendingSelection" type="button" :disabled="modelTask || !model?.ready" @click="prepareActivation">使用此模型</button>

      </div>
      <p v-if="model?.resumableBytes && !progress">可继续下载：{{ formatBytes(model.resumableBytes) }}</p>
      <p v-if="index && !index.writable">当前相册不能写入 AI 索引，已有有效索引仍可搜索。</p>
      <p v-if="!peopleModel">首次点击“建立索引”；之后打开或刷新相册时自动更新新增和变化的照片。可中途停止，下次继续。</p>
      <p class="description">选择和安装不会更换当前模型；点击“使用此模型”后才生效。可导入与所选模型匹配的 PhotoSys 离线模型包（ZIP）。</p>
      <p class="description">模型选择对所有相册统一生效。{{ switchNotice }}</p>
      <p class="description">原照片、收藏和浏览方向保留，旧模型文件保留。其他相册下次打开时分别提示重建。</p>
    </div>
    <div v-if="displayedIndex?.needsRebuild" class="rebuild-notice" role="status">
      <strong>需要重建{{ peopleModel ? '人物索引' : '内容索引' }}</strong>
      <p>{{ displayedIndex.rebuildReason }} 当前待处理 {{ displayedIndex.pending }} 张照片。</p>
      <p>{{ switchNotice }}</p>
      <p v-if="!displayedIndex.writable">此相册只读，取得写入权限后才能重建。</p>
      <ElButton :disabled="!model?.activeReady || !displayedIndex.writable || libraryBusy || modelTask || indexTask || analysisBusy" @click="rebuildIndex">{{ peopleModel ? '重建人物索引' : '重建内容索引' }}</ElButton>
    </div>
    <ElDialog v-model="confirmOpen" title="更换本地 AI 模型" width="min(36rem, 94vw)" :close-on-click-modal="false" :close-on-press-escape="!modelTask" :show-close="!modelTask" append-to-body>
      <p>从「{{ activeModel?.name }}」更换为「{{ selectedModel?.name }}」。</p>
      <p v-if="affectedCount !== undefined">当前相册需要重新处理 {{ affectedCount }} 张照片；已有兼容结果会复用。</p>
      <p v-else>尚未打开相册，下次打开时将检查并提示重建。</p>
      <p>{{ switchNotice }}</p>
      <p>正在运行的该类分析任务会停止。原照片、收藏、浏览方向及旧模型文件保留。模型选择对所有相册统一生效，其他相册下次打开时分别提示重建。</p>
      <p v-if="modelError" class="error" role="alert">{{ modelError }}</p>
      <template #footer><ElButton :disabled="modelTask" @click="confirmOpen = false">取消</ElButton><ElButton type="primary" :loading="modelTask" @click="activateModel">更换模型，稍后手动重建</ElButton></template>
    </ElDialog>
      <div v-if="progress" role="status">
        <p>{{ { downloading: '正在下载', verifying: '正在校验', testing: '正在加载自检', importing: '正在导入' }[progress.phase] }} · {{ progress.file }}<span v-if="progress.phase === 'downloading'"> · {{ progress.source === 'mirror' ? 'HF-Mirror' : 'Hugging Face' }}</span></p>
        <progress :value="progress.downloaded" :max="progress.total" aria-label="模型安装进度"></progress>
        <p>{{ formatBytes(progress.downloaded) }} / {{ formatBytes(progress.total) }}</p>
        <ElButton :disabled="cancelling" @click="cancelInstall">{{ cancelling ? '正在取消…' : '取消安装' }}</ElButton>
      </div>
      <p v-if="model?.message" role="status">{{ model.message }}</p>
      <p v-if="modelError" class="error" role="alert">{{ modelError }}</p>
    <p v-if="index?.message" class="error" role="alert">{{ index.message }}</p>
    <p v-if="indexError" class="error" role="alert">{{ indexError }}</p>
    <details v-if="index?.failed" :open="issuesOpen" @toggle="issuesOpen = ($event.target as HTMLDetailsElement).open">
      <summary>AI 索引问题（{{ index.failed }}）</summary>
      <ul v-if="issues"><li v-for="issue in issues.items" :key="issue.path">{{ issue.path }} · {{ issue.reason }}</li></ul>
      <nav v-if="issues && issues.total > 24" class="ai-summary" aria-label="AI 索引问题分页">
        <button class="secondary" type="button" :disabled="issues.page === 0" @click="issuePage--">上一页</button>
        <span>{{ issues.page + 1 }} / {{ Math.ceil(issues.total / 24) }}</span>
        <button class="secondary" type="button" :disabled="(issues.page + 1) * 24 >= issues.total" @click="issuePage++">下一页</button>
      </nav>
    </details>
  </section>
</template>

<style scoped>
.model-picker { display:flex; flex-wrap:wrap; gap:.75rem; align-items:center; }
.model-description { margin:1rem 0; padding:1rem; border:1px solid var(--control-border); border-radius:.6rem; background:var(--surface); }
.model-badge { display:block; margin-top:.4rem; color:var(--muted); }
.rebuild-notice { margin-top:1rem; padding:1rem; border:1px solid var(--warning); border-radius:.6rem; background:var(--warning-soft); color:var(--warning); }
.ai-settings { margin: 0 0 20px; padding: 8px 0; border-bottom: 1px solid var(--border); color: var(--muted); }
.ai-label { display: inline-flex; align-items: center; gap: 0.5rem; color: var(--accent); font-weight: 650; }
.model-status { color: var(--muted); }
.model-status.ready { color: var(--accent); }
.ai-settings .ai-summary > .settings-toggle { margin-left: auto; }
.ai-settings-body { color: var(--text-secondary); font-size: 0.875rem; }
.ai-directory label { margin: 0; }
.ai-directory input { box-sizing: border-box; min-height: 40px; padding: 0.5rem 0.75rem; border: 1px solid var(--control-border); border-radius: 8px; background: var(--surface); color: inherit; font: inherit; text-overflow: ellipsis; }
.ai-settings select { min-height: 40px; padding: 0.375rem; border: 1px solid var(--control-border); border-radius: 8px; background: var(--surface); color: inherit; font: inherit; }
.ai-settings-body > .ai-summary > label { display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem; margin: 0; }
@media (max-width: 48rem) { .ai-settings .ai-summary > .settings-toggle { margin-left: 0; } }
.ai-summary { display: flex; flex-wrap: wrap; align-items: center; gap: 0.6rem 1rem; }
.ai-summary span { font-size: 13px; }
.ai-settings-body { margin-top: 1rem; border-top: 1px solid var(--border); padding-top: 0.5rem; }
.ai-directory { display: flex; flex-wrap: wrap; align-items: center; gap: 0.6rem; }
.ai-directory input { flex: 1; min-width: 12rem; }
.ai-settings p { overflow-wrap: anywhere; }
.ai-settings progress { width: 100%; }
.ai-settings select { max-width: 100%; }
</style>
