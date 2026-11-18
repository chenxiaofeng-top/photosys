<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import { ElButton, ElIcon } from "element-plus";
import { MagicStick, Setting } from "@element-plus/icons-vue";
import "element-plus/es/components/button/style/css";
import "element-plus/es/components/icon/style/css";
import { formatBytes } from "./formatBytes";
import type { IndexStatus, ModelStatus } from "./aiSearch";

const props = defineProps<{ scanId?: number; libraryBusy: boolean }>();
const emit = defineEmits<{ modelReady: [ready: boolean]; indexChange: [status: IndexStatus]; updated: [] }>();
const model = ref<ModelStatus>();
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

function setModel(value: ModelStatus) { model.value = value; emit("modelReady", value.ready); }
function setIndex(value: IndexStatus) {
  if (value.scanId !== props.scanId) return;
  index.value = value;
  emit("indexChange", value);
}

async function loadModel() {
  modelTask.value = true;
  try { setModel(await invoke<ModelStatus>("ai_model_status")); }
  catch (cause) { modelError.value = String(cause); }
  finally { modelTask.value = false; }
}

async function chooseDirectory() {
  modelTask.value = true;
  modelError.value = "";
  try {
    const directory = await invoke<string | null>("choose_folder", { title: "选择 AI 模型安装目录（多个相册可以共用）" });
    if (directory) {
      if (props.scanId !== undefined) await invoke("ai_cancel_index", { scanId: props.scanId });
      setModel(await invoke<ModelStatus>("ai_set_model_directory", { directory }));
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
    if (props.scanId !== undefined) await invoke("ai_cancel_index", { scanId: props.scanId });
    setModel(await invoke<ModelStatus>("ai_install_model", {
      source: source.value, package: packagePath,
      progress: new Channel<Progress>((update) => { if (mounted && receiving) progress.value = update; }),
    }));
  } catch (cause) { modelError.value = String(cause); }
  finally {
    receiving = false;
    progress.value = undefined;
    cancelling.value = false;
    modelTask.value = false;
    await loadModel();
    await refreshIndex(true);
  }
}

async function cancelInstall() {
  cancelling.value = true;
  try { await invoke("ai_cancel_install"); } catch (cause) { modelError.value = String(cause); }
}

async function refreshIndex(automatic = false) {
  const scanId = props.scanId;
  if (scanId === undefined) return;
  const request = indexRequest;
  try {
    const value = await invoke<IndexStatus>("ai_index_status", { scanId });
    if (!mounted || request !== indexRequest || scanId !== props.scanId) return;
    setIndex(value);
    if (automatic && value.enabled && value.writable && model.value?.ready && !indexTask.value) await updateIndex(true);
  } catch (cause) { if (request === indexRequest && scanId === props.scanId) indexError.value = String(cause); }
}

async function updateIndex(automatic = false) {
  const scanId = props.scanId;
  if (scanId === undefined || indexTask.value || modelTask.value) return;
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
  try { if (props.scanId !== undefined) await invoke("ai_cancel_index", { scanId: props.scanId }); }
  catch (cause) { indexError.value = String(cause); }
}

watch(() => props.scanId, (id, oldId) => {
  indexRequest++;
  indexTask.value = false;
  index.value = undefined;
  issues.value = undefined;
  issuesOpen.value = false;
  issuePage.value = 0;
  indexError.value = "";
  if (oldId !== undefined) void invoke("ai_cancel_index", { scanId: oldId }).catch(() => {});
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

onMounted(async () => { await loadModel(); await refreshIndex(true); });
onBeforeUnmount(() => { mounted = false; indexRequest++; void cancelIndex(); });
defineExpose({ refreshIndex });
</script>

<template>
  <section class="ai-settings" aria-label="AI 搜索设置">
    <div class="ai-summary">
      <span class="ai-label"><ElIcon aria-hidden="true"><MagicStick /></ElIcon>本地 AI</span>
      <span class="model-status" :class="{ ready: model?.ready }" role="status">{{ modelTask ? '正在处理模型…' : model?.ready ? '本地模型已就绪' : '尚未安装可用模型' }}</span>
      <span v-if="index" role="status">{{ indexTask ? '正在建立索引 · ' : '' }}已索引 {{ index.indexed }} / {{ index.total }} 张<span v-if="index.pending"> · 待处理 {{ index.pending }} 张</span><span v-if="index.failed"> · 失败 {{ index.failed }} 张</span></span>
      <ElButton v-if="indexTask" @click="cancelIndex">停止索引</ElButton>
      <ElButton v-else-if="scanId !== undefined && model?.ready"
        :disabled="libraryBusy || modelTask || !index?.writable" @click="updateIndex(false)">{{ index?.enabled ? index.failed ? '更新索引 / 重试失败项' : '更新索引' : '建立索引' }}</ElButton>
      <ElButton class="settings-toggle" text :icon="Setting" :aria-expanded="open" aria-controls="ai-settings-body" @click="open = !open">AI 搜索设置</ElButton>
    </div>
    <div v-show="open" id="ai-settings-body" class="ai-settings-body">
      <p>用中文描述寻找照片。模型安装后可断网使用，照片和搜索内容只在本机处理。</p>
      <p>模型：Chinese-CLIP ViT-B/16 · CPU · {{ formatBytes(model?.totalBytes ?? 755533779) }}</p>
      <div class="ai-directory">
        <label for="ai-model-directory">模型安装目录</label>
        <input id="ai-model-directory" :value="model?.directory ?? ''" placeholder="请手动指定目录" readonly />
        <button class="secondary" type="button" :disabled="modelTask || indexTask" @click="chooseDirectory">选择目录</button>
      </div>
      <p class="description">多个相册可以共用此目录；更换目录不会搬动或删除原有模型。</p>
      <div class="ai-summary">
        <label>下载源 <select v-model="source" :disabled="modelTask">
          <option value="mirror">国内优先 · HF-Mirror（第三方镜像）</option>
          <option value="official">Hugging Face 原源</option>
        </select></label>
        <button type="button" :disabled="!model?.directory || modelTask || indexTask || model?.ready" @click="install(false)">
          {{ model?.resumableBytes ? '继续下载 / 重试安装' : '下载并安装模型' }}
        </button>
        <button class="secondary" type="button" :disabled="!model?.directory || modelTask || indexTask" @click="install(true)">导入离线模型包</button>

      </div>
      <p v-if="model?.resumableBytes && !progress">可继续下载：{{ formatBytes(model.resumableBytes) }}</p>
      <p v-if="index && !index.writable">当前相册不能写入 AI 索引，已有有效索引仍可搜索。</p>
      <p>首次点击“建立索引”；之后打开或刷新相册时自动更新新增和变化的照片。可中途停止，下次继续。</p>
      <p class="description">仅分析照片及实况封面。可导入与此版本匹配的 PhotoSys 离线模型包（ZIP）。</p>
    </div>
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
.ai-settings { margin: 0 0 1.25rem; padding: 0.625rem 0.875rem; border: 1px solid #e0e7de; border-radius: 0.75rem; background: #edf2ec; color: #52655a; }
.ai-label { display: inline-flex; align-items: center; gap: 0.5rem; color: #176252; font-weight: 650; }
.model-status { color: #647369; }
.model-status.ready { color: #176252; }
.ai-settings .ai-summary > .settings-toggle { margin-left: auto; }
.ai-settings-body { color: #42574e; font-size: 0.875rem; }
.ai-directory label { margin: 0; }
.ai-directory input { box-sizing: border-box; min-height: 40px; padding: 0.5rem 0.75rem; border: 1px solid #cbd6ce; border-radius: 8px; background: #fff; color: inherit; font: inherit; text-overflow: ellipsis; }
.ai-settings select { min-height: 40px; padding: 0.375rem; border: 1px solid #cbd6ce; border-radius: 8px; background: #fff; color: inherit; font: inherit; }
.ai-settings-body > .ai-summary > label { display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem; margin: 0; }
@media (max-width: 48rem) { .ai-settings .ai-summary > .settings-toggle { margin-left: 0; } }
.ai-summary { display: flex; flex-wrap: wrap; align-items: center; gap: 0.6rem 1rem; }
.ai-summary span { font-size: 0.9rem; }
.ai-settings-body { margin-top: 1rem; border-top: 1px solid #e2e8e4; padding-top: 0.5rem; }
.ai-directory { display: flex; flex-wrap: wrap; align-items: center; gap: 0.6rem; }
.ai-directory input { flex: 1; min-width: 12rem; }
.ai-settings p { overflow-wrap: anywhere; }
.ai-settings progress { width: 100%; }
.ai-settings select { max-width: 100%; }
</style>
