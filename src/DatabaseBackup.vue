<script setup lang="ts">
import { Channel, invoke } from "@tauri-apps/api/core";
import { computed, ref, shallowRef, watch } from "vue";
import { ElButton, ElDialog } from "element-plus";
import "element-plus/es/components/dialog/style/css";
import { formatBytes } from "./formatBytes";
import type { BackupInspection, DatabaseRestoreResult } from "./databaseBackup";

const props = defineProps<{ scanId?: number; directory: string; libraryBusy: boolean }>();
const emit = defineEmits<{ busyChange: [busy: boolean]; restoring: []; restored: [result: DatabaseRestoreResult]; restoreFailed: []; exported: [] }>();
const open = ref(false);
const working = ref(false);
const error = ref("");
const notice = ref("");
const source = ref("");
const target = ref("");
const backupName = ref("");
const inspection = shallowRef<BackupInspection>();
const phase = ref("");
const currentDatabase = ref("");
const labels: Record<string, string> = { checking: "正在校验备份", snapshot: "正在生成数据库快照", packing: "正在打包备份", preserving: "正在保存回退副本", restoring: "正在恢复数据库", rollingBack: "正在回滚到原数据库", reopening: "正在重新打开相册", complete: "备份完成" };
const rollbackPath = computed(() => target.value ? `${target.value.replace(/[\\/]$/, "")}/.photosys/${backupName.value}` : "");
const canAct = computed(() => !working.value && !props.libraryBusy);
const backupTime = computed(() => inspection.value ? new Date(inspection.value.manifest.createdAt).toLocaleString("zh-CN", { hour12: false }) : "");
watch(() => props.directory, () => { if (!working.value) resetSelection(); });
function resetSelection() {
  source.value = ""; inspection.value = undefined; target.value = ""; backupName.value = "";
}
function setWorking(value: boolean) { working.value = value; emit("busyChange", value); }
function channel() {
  return new Channel<{ phase: string; database: string | null }>(p => { phase.value = labels[p.phase] ?? p.phase; currentDatabase.value = p.database ?? ""; });
}
function show() { error.value = ""; notice.value = ""; resetSelection(); open.value = true; }
async function exportBackup() {
  if (!canAct.value || props.scanId === undefined) return;
  setWorking(true); error.value = ""; notice.value = ""; phase.value = "选择备份保存位置"; currentDatabase.value = "";
  try {
    const destination = await invoke<string | null>("database_backup_file", { save: true });
    if (!destination) return;
    phase.value = "正在等待后台索引停止";
    await invoke("database_export", { scanId: props.scanId, destination, progress: channel() });
    notice.value = `数据库备份已保存：${destination}`;
  } catch (cause) { error.value = String(cause); }
  finally { setWorking(false); phase.value = ""; currentDatabase.value = ""; emit("exported"); }
}
async function chooseBackup() {
  if (!canAct.value) return;
  setWorking(true); error.value = ""; notice.value = ""; phase.value = "选择备份文件"; currentDatabase.value = "";
  try {
    const selected = await invoke<string | null>("database_backup_file", { save: false });
    if (!selected) return;
    resetSelection();
    phase.value = "正在校验备份";
    const value = await invoke<BackupInspection>("database_inspect", { source: selected });
    source.value = selected; inspection.value = value; target.value = props.directory;
    backupName.value = `restore-backup-${crypto.randomUUID()}`;
  } catch (cause) { error.value = String(cause); }
  finally { setWorking(false); phase.value = ""; }
}
async function chooseTarget() {
  if (!canAct.value) return;
  setWorking(true); error.value = ""; phase.value = "选择恢复目标目录";
  try {
    const selected = await invoke<string | null>("choose_folder", { title: "选择要恢复数据库的相册目录" });
    if (selected) { target.value = selected; backupName.value = `restore-backup-${crypto.randomUUID()}`; }
  } catch (cause) { error.value = String(cause); }
  finally { setWorking(false); phase.value = ""; }
}
async function restoreBackup() {
  if (!canAct.value || !inspection.value || !target.value) return;
  setWorking(true); error.value = ""; notice.value = ""; phase.value = "正在等待后台索引停止"; currentDatabase.value = "";
  emit("restoring");
  let failed = false;
  try {
    const result = await invoke<DatabaseRestoreResult>("database_restore", { source: source.value, directory: target.value, fingerprint: inspection.value.fingerprint, backupName: backupName.value, progress: channel() });
    notice.value = `数据库已恢复。回退副本：${result.rollbackPath}`;
    if (result.warning) notice.value += `\n${result.warning}`;
    if (result.reopenError) notice.value += `\n数据库恢复成功，但重新读取相册失败：${result.reopenError}。请刷新或重新选择目录。`;
    resetSelection();
    emit("restored", result);
  } catch (cause) { error.value = String(cause); failed = true; }
  finally { setWorking(false); phase.value = ""; currentDatabase.value = ""; }
  if (failed) emit("restoreFailed");
}
</script>

<template>
  <ElButton :disabled="libraryBusy" @click="show">数据库备份与恢复</ElButton>
  <ElDialog v-model="open" class="database-backup-dialog" top="6vh" title="数据库备份与恢复" width="min(42rem, 94vw)" :close-on-click-modal="false" :close-on-press-escape="!working" :show-close="!working" append-to-body>
    <p>备份收藏、旋转方向、位置缓存、AI 搜索索引、人物与人脸数据。照片视频、模型和离线地图需要另行保存。</p>
    <div class="backup-actions">
      <ElButton :disabled="!canAct || scanId === undefined" @click="exportBackup">备份当前相册</ElButton>
      <ElButton :disabled="!canAct" @click="chooseBackup">选择备份文件恢复</ElButton>
    </div>
    <p v-if="scanId === undefined" class="backup-hint">打开相册后可以创建备份；相册无法打开时，也可以选择备份文件和目标目录进行恢复。</p>
    <section v-if="inspection" class="restore-confirmation" aria-label="确认数据库恢复">
      <h3>确认恢复内容</h3>
      <dl>
        <dt>备份文件</dt><dd>{{ source }}</dd>
        <dt>备份时间</dt><dd>{{ backupTime }}</dd>
        <dt>来源相册</dt><dd>{{ inspection.manifest.sourceAlbum }}</dd>
        <dt>应用版本</dt><dd>{{ inspection.manifest.appVersion }}</dd>
      </dl>
      <ul><li v-for="database in inspection.manifest.databases" :key="database.name">{{ database.name }} · {{ formatBytes(database.size) }}</li></ul>
      <p><strong>目标相册：</strong><span>{{ target || '请先选择目标目录' }}</span></p>
      <ElButton :disabled="!canAct" @click="chooseTarget">{{ target ? '更换目标目录' : '选择目标目录' }}</ElButton>
      <p><strong>回退副本：</strong><span>{{ rollbackPath || '选择目标目录后显示' }}</span></p>
      <p class="overwrite-notice">确认后将完整覆盖目标相册的三个数据库，备份中没有的可选索引也会清除。原数据库先保留为回退副本，恢复失败时自动回滚。相册中的媒体按相对路径匹配。</p>
      <ElButton type="danger" :disabled="!canAct || !target" @click="restoreBackup">确认覆盖并恢复数据库</ElButton>
    </section>
    <div v-if="working" role="status" aria-live="polite">
      <p>{{ phase }}<span v-if="currentDatabase"> · {{ currentDatabase }}</span></p>
      <progress aria-label="数据库备份与恢复进度"></progress>
    </div>
    <p v-if="notice" class="backup-result" role="status">{{ notice }}</p>
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <template #footer><ElButton :disabled="working" @click="open = false">关闭</ElButton></template>
  </ElDialog>
</template>

<style scoped>
.backup-actions { display: flex; flex-wrap: wrap; gap: .75rem; }
.backup-actions :deep(.el-button + .el-button) { margin-left: 0; }
.backup-hint { color: var(--muted); }
.restore-confirmation { margin-top: 1.25rem; border: 1px solid var(--border); border-radius: .6rem; padding: 1rem; }
.restore-confirmation h3 { margin-top: 0; }
.restore-confirmation dl { display: grid; grid-template-columns: 5rem minmax(0, 1fr); gap: .5rem; }
.restore-confirmation dd { margin: 0; font-size: inherit; font-weight: 400; }
.restore-confirmation dd, .restore-confirmation p, .backup-result { overflow-wrap: anywhere; }
.overwrite-notice { padding: .75rem; background: var(--warning-soft); border-radius: .4rem; color: var(--warning); }
.backup-result { white-space: pre-wrap; color: var(--accent); }
</style>

<style>
.database-backup-dialog .el-dialog__body { max-height: calc(88vh - 9rem); overflow-y: auto; }
</style>
