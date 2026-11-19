<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, nextTick, ref } from "vue";
import { ElConfigProvider, ElIcon } from "element-plus";
import zhCn from "element-plus/es/locale/lang/zh-cn";
import { Picture, FolderOpened, CopyDocument, Connection, User, Collection, Sunny, Moon, Monitor, Menu, Close } from "@element-plus/icons-vue";
import "element-plus/es/components/icon/style/css";
import DuplicatesPanel from "./DuplicatesPanel.vue";
import MediaLibrary from "./MediaLibrary.vue";
import SimilarPhotosPanel from "./SimilarPhotosPanel.vue";
import { formatBytes } from "./formatBytes";
import appIcon from "../src-tauri/icons/32x32.png";
import { themePreference, setThemePreference, type ThemePreference } from "./theme";

interface ArchiveSummary {
  found: number;
  copied: number;
  duplicates: number;
  renamed: number;
  other: number;
  failed: number;
  scanErrors: number;
  reportPath: string | null;
  reportError: string | null;
}

interface CopyProgress {
  bytesWritten: number;
  totalBytes: number;
}

type ArchiveProgress =
  | { phase: "scanning"; found: number }
  | { phase: "archiving"; processed: number; total: number; currentFile: string; transfer: CopyProgress | null }
  | { phase: "reporting"; processed: number; total: number };

const source = ref("");
const destination = ref("");
const choosing = ref(false);
const running = ref(false);
const result = ref<ArchiveSummary>();
const progress = ref<ArchiveProgress>();
const error = ref("");
const viewTitles = {
  library: "媒体库", archive: "照片与视频归档", duplicates: "重复文件", similar: "相似照片", people: "人物", albums: "相册",
};
const viewIcons = { library: Picture, archive: FolderOpened, duplicates: CopyDocument, similar: Connection, people: User, albums: Collection };
type View = keyof typeof viewTitles;
const view = ref<View>("library");
const navigationOpen = ref(false);
const navigationGroups: { title: string; views: View[] }[] = [
  { title: "浏览", views: ["library", "people", "albums"] },
  { title: "整理", views: ["archive", "duplicates", "similar"] },
];
const themeOptions = [
  { value: "light", label: "浅色", icon: Sunny },
  { value: "dark", label: "深色", icon: Moon },
  { value: "system", label: "跟随系统", icon: Monitor },
] satisfies { value: ThemePreference; label: string; icon: typeof Sunny }[];
const libraryDirectory = ref("");
const libraryBusy = ref(false);
const duplicatesBusy = ref(false);
const similarBusy = ref(false);
const duplicatesPanel = ref<InstanceType<typeof DuplicatesPanel>>();
const similarPanel = ref<InstanceType<typeof SimilarPhotosPanel>>();
const fileTaskBusy = computed(() =>
  libraryBusy.value || choosing.value || running.value || duplicatesBusy.value || similarBusy.value,
);

function navigate(target: View) {
  if (fileTaskBusy.value) return;
  const directory = libraryDirectory.value;
  if (["library", "people", "albums"].includes(view.value) && directory) {
    if (target === "archive" && source.value !== directory) {
      source.value = directory;
      result.value = undefined;
      error.value = "";
    } else if (target === "duplicates") {
      duplicatesPanel.value?.useDirectory(directory);
    } else if (target === "similar") {
      similarPanel.value?.useDirectory(directory);
    }
  }
  view.value = target;
  const wasOpen = navigationOpen.value;
  navigationOpen.value = false;
  if (wasOpen) void nextTick(() => document.getElementById("workspace")?.focus({ preventScroll: true }));
}

function closeNavigation() {
  if (!navigationOpen.value) return;
  navigationOpen.value = false;
  void nextTick(() => document.querySelector<HTMLButtonElement>(".navigation-toggle")?.focus());
}

async function chooseFolder(kind: "source" | "destination") {
  if (choosing.value || running.value) return;
  choosing.value = true;
  error.value = "";

  try {
    const folder = await invoke<string | null>("choose_folder", {
      title: kind === "source" ? "选择源目录" : "选择目标目录",
    });
    if (folder !== null) {
      const selected = kind === "source" ? source : destination;
      if (selected.value !== folder) {
        selected.value = folder;
        result.value = undefined;
      }
    }
  } catch (cause) {
    error.value = `无法选择目录。${String(cause)}`;
  } finally {
    choosing.value = false;
  }
}

async function startArchive() {
  if (!source.value || !destination.value || choosing.value || running.value) return;
  running.value = true;
  error.value = "";
  result.value = undefined;
  progress.value = { phase: "scanning", found: 0 };
  let receivingProgress = true;

  try {
    const channel = new Channel<ArchiveProgress>((update) => {
      if (receivingProgress) progress.value = update;
    });
    result.value = await invoke<ArchiveSummary>("archive_media", {
      source: source.value,
      destination: destination.value,
      progress: channel,
    });
  } catch (cause) {
    error.value = String(cause);
  } finally {
    receivingProgress = false;
    progress.value = undefined;
    running.value = false;
  }
}
</script>

<template>
  <ElConfigProvider :locale="zhCn" size="large">
  <div class="app-shell" :class="{ 'navigation-open': navigationOpen }">
  <a class="skip-link" href="#workspace">跳到主要内容</a>
  <aside class="app-sidebar" @keydown.esc="closeNavigation">
    <div class="sidebar-brand">
      <p class="app-name"><img :src="appIcon" alt="" width="28" height="28" /><span>PhotoSys</span></p>
      <button type="button" class="navigation-toggle secondary" :aria-expanded="navigationOpen" aria-controls="sidebar-navigation"
        :aria-label="navigationOpen ? '收起导航' : '展开导航'" @click="navigationOpen = !navigationOpen">
        <ElIcon aria-hidden="true"><component :is="navigationOpen ? Close : Menu" /></ElIcon>
      </button>
    </div>
    <div id="sidebar-navigation" class="sidebar-content">
      <nav class="view-nav" aria-label="功能">
        <div v-for="group in navigationGroups" :key="group.title" class="navigation-group">
          <p class="navigation-label">{{ group.title }}</p>
          <button v-for="target in group.views" :key="target" type="button" :title="viewTitles[target]" :aria-label="viewTitles[target]"
            :aria-pressed="view === target" :aria-current="view === target ? 'page' : undefined" :disabled="fileTaskBusy" @click="navigate(target)">
            <ElIcon aria-hidden="true"><component :is="viewIcons[target]" /></ElIcon><span>{{ viewTitles[target] }}</span>
          </button>
        </div>
      </nav>
      <div class="sidebar-footer">
        <p class="sidebar-caption">珍藏每一段回忆</p>
        <div class="theme-picker" role="group" aria-label="外观主题">
          <button v-for="option in themeOptions" :key="option.value" type="button" :aria-label="option.label" :title="option.label"
            :aria-pressed="themePreference === option.value" @click="setThemePreference(option.value)">
            <ElIcon aria-hidden="true"><component :is="option.icon" /></ElIcon><span>{{ option.label }}</span>
          </button>
        </div>
      </div>
    </div>
  </aside>

  <main id="workspace" tabindex="-1" :class="{ 'library-layout': view === 'library' || view === 'people' || view === 'albums' }">
    <header v-if="view !== 'library' && view !== 'people' && view !== 'albums'">
      <h1>{{ viewTitles[view] }}</h1>
      <p v-if="view === 'archive'" class="description">复制源目录及其子目录中的媒体，按年份和月份整理到目标目录。</p>
      <p v-else-if="view === 'duplicates'" class="description">找出相同文件，给珍贵回忆留出更多空间。</p>
      <p v-else-if="view === 'similar'" class="description">并排看看相似的瞬间，慢慢选出最喜欢的一张。</p>
    </header>

    <MediaLibrary v-show="view === 'library' || view === 'people' || view === 'albums'" :active="view === 'library' || view === 'people' || view === 'albums'" :mode="view === 'people' ? 'people' : view === 'albums' ? 'albums' : 'library'" @mode-change="navigate($event)" @busy-change="libraryBusy = $event"
      @directory-change="libraryDirectory = $event" />

    <form v-if="view === 'archive'" @submit.prevent="startArchive">
      <div class="folder">
        <label for="source">源目录</label>
        <div class="folder-row">
          <input id="source" :value="source" placeholder="尚未选择源目录" readonly />
          <button type="button" class="secondary" :disabled="choosing || running" @click="chooseFolder('source')">
            选择源目录
          </button>
        </div>
      </div>

      <div class="folder">
        <label for="destination">目标目录</label>
        <div class="folder-row">
          <input id="destination" :value="destination" placeholder="尚未选择目标目录" readonly />
          <button type="button" class="secondary" :disabled="choosing || running" @click="chooseFolder('destination')">
            选择目标目录
          </button>
        </div>
      </div>

      <div class="actions">
        <button type="submit" :disabled="!source || !destination || choosing || running">
          {{ running ? "正在复制…" : "开始复制" }}
        </button>
      </div>
      <div v-if="progress" class="archive-progress">
        <template v-if="progress.phase === 'scanning'">
          <p role="status">正在扫描媒体文件…</p>
          <progress aria-label="扫描进度"></progress>
          <p>已发现 {{ progress.found }} 个文件</p>
        </template>
        <template v-else>
          <template v-if="progress.total > 0">
            <p>已处理 {{ progress.processed }} / {{ progress.total }} 个文件（{{ Math.floor(progress.processed / progress.total * 100) }}%）</p>
            <progress aria-label="总体归档进度" :value="progress.processed" :max="progress.total"></progress>
          </template>
          <p v-if="progress.phase === 'reporting'" role="status">正在生成复制说明…</p>
          <template v-else>
            <p class="current-file">正在处理 {{ progress.currentFile }}</p>
            <template v-if="progress.transfer && progress.transfer.totalBytes > 0">
              <progress aria-label="当前文件复制进度" :value="progress.transfer.bytesWritten" :max="progress.transfer.totalBytes"></progress>
              <p>{{ formatBytes(progress.transfer.bytesWritten) }} / {{ formatBytes(progress.transfer.totalBytes) }}</p>
            </template>
          </template>
        </template>
      </div>
      <p v-if="error" class="error" role="alert">{{ error }}</p>
    </form>

    <section v-if="view === 'archive' && result" aria-labelledby="archive-result" role="status">
      <h2 id="archive-result">
        {{ result.failed || result.scanErrors || result.reportError ? "归档结束，有错误" : "归档完成" }}
      </h2>
      <dl>
        <div><dt>发现媒体</dt><dd>{{ result.found }}</dd></div>
        <div><dt>成功复制</dt><dd>{{ result.copied }}</dd></div>
        <div><dt>重复跳过</dt><dd>{{ result.duplicates }}</dd></div>
        <div><dt>重命名</dt><dd>{{ result.renamed }}</dd></div>
        <div><dt>归入其它</dt><dd>{{ result.other }}</dd></div>
        <div><dt>复制失败</dt><dd>{{ result.failed }}</dd></div>
        <div v-if="result.scanErrors"><dt>扫描错误</dt><dd>{{ result.scanErrors }}</dd></div>
      </dl>
      <p v-if="result.reportPath" class="report">复制说明：<span>{{ result.reportPath }}</span></p>
      <p v-if="result.reportError" class="error">无法写入复制说明。{{ result.reportError }}</p>
    </section>
    <DuplicatesPanel class="utility-panel" v-show="view === 'duplicates'" ref="duplicatesPanel" @busy-change="duplicatesBusy = $event" />
    <SimilarPhotosPanel class="utility-panel" v-show="view === 'similar'" ref="similarPanel" @busy-change="similarBusy = $event" />
  </main>
  </div>
  </ElConfigProvider>
</template>

<style scoped>
.app-shell { --sidebar-width: 224px; min-height: 100vh; }
.app-sidebar { position: fixed; inset: 0 auto 0 0; z-index: 20; display: flex; flex-direction: column; width: var(--sidebar-width); padding: 32px 16px 20px; border-right: 1px solid var(--border); background: var(--sidebar); }
.sidebar-brand { display: flex; align-items: center; justify-content: space-between; padding: 0 12px; }
.app-name { display: flex; align-items: center; gap: 10px; margin: 0; font-size: 20px; font-weight: 700; letter-spacing: -.5px; }
.sidebar-content { display: flex; flex: 1; flex-direction: column; min-height: 0; }
.view-nav { flex: 1; overflow-y: auto; margin-top: 44px; }
.navigation-group + .navigation-group { margin-top: 30px; }
.navigation-label { margin: 0 0 10px 14px; color: var(--muted); font-size: 12px; letter-spacing: .1em; }
.view-nav button { display: flex; align-items: center; gap: 12px; width: 100%; min-height: 44px; margin: 4px 0; padding: 10px 14px; border: 0; background: transparent; color: var(--text-secondary); text-align: left; font-weight: 500; }
.view-nav .el-icon { flex: none; font-size: 19px; }
.view-nav button:hover:not(:disabled) { background: var(--hover); color: var(--text); }
.view-nav button[aria-pressed="true"] { background: var(--accent-soft); color: var(--accent); font-weight: 650; }
.view-nav button:disabled { opacity: .55; background: transparent; }
.view-nav button:disabled[aria-pressed="true"] { background: var(--accent-soft); opacity: 1; }
.sidebar-footer { padding-top: 24px; }
.sidebar-caption { margin: 0 0 14px; color: var(--muted); font-size: 12px; text-align: center; }
.theme-picker { display: flex; gap: 3px; padding: 4px; border: 1px solid var(--border); border-radius: 10px; background: var(--surface); }
.theme-picker button { display: grid; flex: 1; justify-items: center; align-content: center; gap: 4px; min-height: 50px; padding: 6px 2px; background: transparent; color: var(--muted); font-size: 12px; font-weight: 500; }
.theme-picker .el-icon { font-size: 16px; }
.theme-picker button:hover { background: var(--hover); }
.theme-picker button[aria-pressed="true"] { color: var(--accent); background: var(--accent-soft); }
.navigation-toggle { display: none; }
main { min-width: 0; margin-left: var(--sidebar-width); padding: 40px clamp(20px, 3vw, 48px) 48px; }
main > header { margin-bottom: 28px; }
main > form, main > section, .utility-panel { max-width: 1000px; }
.skip-link { position: fixed; top: -80px; left: 16px; z-index: 100; padding: 12px; background: var(--surface); border: 1px solid var(--accent); border-radius: 8px; }
.skip-link:focus { top: 12px; }
@media (min-width: 1904px) { main { max-width: 1680px; } }
@media (max-width: 959px) and (min-width: 640px) {
  .app-shell { --sidebar-width: 72px; }
  .app-sidebar { padding: 26px 10px 16px; }
  .sidebar-brand { padding: 0; justify-content: center; }
  .app-name span, .view-nav button span, .navigation-label, .sidebar-caption, .theme-picker button span { display: none; }
  .view-nav button { justify-content: center; padding: 12px; }
  .view-nav { margin-top: 36px; }
  .navigation-group + .navigation-group { padding-top: 18px; margin-top: 18px; border-top: 1px solid var(--border); }
  .theme-picker { flex-direction: column; }
  .theme-picker button { min-height: 36px; }
  main { padding-top: 28px; }
}
@media (max-width: 639px) {
  .app-shell { --sidebar-width: 0px; }
  .app-sidebar { position: sticky; inset: 0 0 auto; width: 100%; padding: 12px 16px; border-right: 0; border-bottom: 1px solid var(--border); }
  .sidebar-brand { padding: 0; }
  .app-name { font-size: 18px; }
  .navigation-toggle { display: grid; place-items: center; width: 40px; padding: 8px; }
  .sidebar-content { display: none; max-height: calc(100dvh - 72px); overflow-y: auto; }
  .navigation-open .sidebar-content { display: flex; }
  .view-nav { margin-top: 20px; overflow: visible; }
  .navigation-group + .navigation-group { margin-top: 16px; }
  .sidebar-footer { padding-top: 16px; }
  .sidebar-caption { display: none; }
  .theme-picker button { display: flex; justify-content: center; min-height: 36px; gap: 8px; }
  .theme-picker button span { font-size: 12px; }
  main { padding: 24px 16px 32px; }
}
</style>
