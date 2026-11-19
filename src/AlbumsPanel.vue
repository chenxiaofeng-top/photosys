<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { computed, nextTick, ref, shallowRef, watch } from "vue";
import LibraryThumbnail from "./LibraryThumbnail.vue";
import { validAlbumName, type Album, type AlbumPage, type AlbumEdit, type AlbumEditResult } from "./albums";
const props = defineProps<{ scanId: number; libraryBusy: boolean; revision: number; selected?: Album }>();
const emit = defineEmits<{ select: [album: Album | undefined]; changed: []; add: [album: Album]; writable: [value: boolean]; busyChange: [value: boolean] }>();
const query = ref("");
const committedQuery = ref("");
const page = ref(0);
const result = shallowRef<AlbumPage>();
const loading = ref(false);
const error = ref("");
const saving = ref(false);
const newName = ref("");
const rename = ref("");
const retry = ref(0);
const deleteDialog = ref<HTMLDialogElement>();
const disabled = computed(() => props.libraryBusy || saving.value || !result.value?.writable);
watch(query, (_, __, cleanup) => {
  const timer = setTimeout(() => { committedQuery.value = query.value; page.value = 0; }, 250);
  cleanup(() => clearTimeout(timer));
});
watch(() => props.scanId, () => { page.value = 0; query.value = ""; committedQuery.value = ""; newName.value = ""; result.value = undefined; deleteDialog.value?.close(); });
watch(() => props.selected, value => { rename.value = value?.name ?? ""; deleteDialog.value?.close(); }, { immediate: true });
watch([() => props.scanId, () => props.revision, committedQuery, page, retry, () => props.selected?.id], async (_, __, cleanup) => {
  let current = true; cleanup(() => { current = false; });
  loading.value = true; error.value = "";
  try {
    const value = await invoke<AlbumPage>("albums_list", { scanId: props.scanId, query: committedQuery.value, page: page.value });
    if (current) { result.value = value; emit("writable", value.writable); }
  } catch (cause) { if (current) { error.value = String(cause); result.value = undefined; emit("writable", false); } }
  finally { if (current) loading.value = false; }
}, { immediate: true });
async function mutate(edit: AlbumEdit) {
  if (disabled.value) return;
  const scanId = props.scanId;
  saving.value = true; emit("busyChange", true); error.value = "";
  try {
    const value = await invoke<AlbumEditResult>("albums_edit", { scanId, edit });
    if (scanId !== props.scanId) return;
    if (edit.kind === "delete") { deleteDialog.value?.close(); emit("select", undefined); }
    if (edit.kind === "rename" && props.selected) emit("select", { ...props.selected, name: edit.name.trim() });
    if (edit.kind === "create") { newName.value = ""; emit("select", { id: value.albumId, name: edit.name.trim(), count: 0, cover: null }); }
    emit("changed");
  } catch (cause) { if (scanId === props.scanId) error.value = String(cause); }
  finally { saving.value = false; emit("busyChange", false); }
}
async function confirmDelete() { await nextTick(); deleteDialog.value?.showModal(); }
</script>

<template>
  <section class="albums-panel" aria-label="主题相册">
    <p v-if="result && !result.writable" class="notice" role="status">此媒体库只读，可以浏览已有相册，无法创建、改名或调整归类。</p>
    <p v-if="error" class="error" role="alert">{{ error }} <button v-if="!deleteDialog?.open" type="button" class="secondary" :disabled="libraryBusy || loading" @click="retry++">重试读取</button></p>
    <template v-if="selected">
      <div class="album-actions">
        <button type="button" class="secondary" :disabled="libraryBusy" @click="emit('select', undefined)">返回相册列表</button>
        <h2>{{ selected.name }}</h2>
        <button type="button" :disabled="disabled" @click="emit('add', selected)">去媒体库添加</button>
      </div>
      <details class="album-management"><summary>管理相册</summary>
        <form class="album-actions" @submit.prevent="mutate({ kind: 'rename', albumId: selected.id, name: rename })">
          <label>相册名称 <input v-model="rename" maxlength="160" :disabled="disabled" required /></label>
          <button type="submit" :disabled="disabled || !validAlbumName(rename)">保存名称</button>
          <button type="button" class="secondary" :disabled="disabled" @click="confirmDelete">删除相册</button>
        </form>
        <p v-if="rename.trim() && !validAlbumName(rename)" class="error" role="alert">相册名称最多 80 个字符。</p>
        <p class="status">删除相册只移除归类记录，原照片、视频及其他相册归属均保留。</p>
      </details>
    </template>
    <template v-else>
      <div class="album-actions">
        <label>查找相册 <input v-model="query" type="search" placeholder="输入活动或情景名称" /></label>
        <form class="album-actions" @submit.prevent="mutate({ kind: 'create', name: newName })">
          <label>新相册名称 <input v-model="newName" maxlength="160" placeholder="例如：周末露营" :disabled="disabled" required /></label>
          <button type="submit" :disabled="disabled || !validAlbumName(newName)">创建相册</button>
        </form>
      </div>
      <p v-if="newName.trim() && !validAlbumName(newName)" class="error" role="alert">相册名称最多 80 个字符。</p>
      <p v-if="loading" role="status">正在读取相册…</p>
      <p v-else-if="result?.total === 0" class="album-empty" role="status">{{ committedQuery ? '没有找到匹配的相册。' : '还没有主题相册。起个名字，把同一次出游或情景里的照片和视频放在一起。' }}</p>
      <ul v-if="result?.items.length" class="album-grid" :aria-busy="loading">
        <li v-for="album in result.items" :key="album.id">
          <button type="button" class="album-card secondary" :disabled="libraryBusy || loading" @click="emit('select', album)">
            <span class="album-cover">
              <LibraryThumbnail v-if="album.cover" :key="`${scanId}:${album.cover.id}`" :scan-id="scanId" :file-id="album.cover.id" :video="album.cover.kind === 'video'" :rotation="album.cover.rotation" />
              <span v-else>暂无封面</span>
            </span>
            <strong>{{ album.name }}</strong><span class="status">{{ album.count }} 个媒体文件</span>
          </button>
        </li>
      </ul>
      <nav v-if="result && result.total > 24" class="album-actions" aria-label="相册分页">
        <button type="button" class="secondary" :disabled="libraryBusy || loading || result.page === 0" @click="page = result.page - 1">上一页</button>
        <span>{{ result.page + 1 }} / {{ Math.ceil(result.total / 24) }}</span>
        <button type="button" class="secondary" :disabled="libraryBusy || loading || (result.page + 1) * 24 >= result.total" @click="page = result.page + 1">下一页</button>
      </nav>
    </template>
    <dialog ref="deleteDialog" aria-labelledby="delete-album-title" @cancel.prevent="!saving && deleteDialog?.close()">
      <h2 id="delete-album-title">删除相册“{{ selected?.name }}”？</h2>
      <p>只删除这个相册的归类记录，原照片和视频会保留。</p>
      <p v-if="error" class="error" role="alert">{{ error }}</p>
      <div class="album-actions">
        <button type="button" class="secondary" :disabled="saving" @click="deleteDialog?.close()">取消</button>
        <button type="button" :disabled="disabled || !selected" @click="selected && mutate({ kind: 'delete', albumId: selected.id })">{{ saving ? '正在删除…' : '删除相册' }}</button>
      </div>
    </dialog>
  </section>
</template>

<style scoped>
.albums-panel { margin-block: 20px; }
.album-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; }
.album-actions h2 { flex: 1; overflow-wrap: anywhere; }
.album-actions label { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.album-actions input { min-width: 0; max-width: 100%; }
.album-management { margin-block: 16px; }
.album-management form { margin-top: 12px; }
.album-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(min(100%, 210px), 1fr)); gap: 16px; padding: 0; list-style: none; }
.album-card { display: flex; flex-direction: column; gap: 10px; text-align: left; padding: 0 0 12px; width: 100%; overflow: hidden; }
.album-card strong, .album-card > .status { padding-inline: 12px; overflow-wrap: anywhere; }
.album-cover { display: grid; place-items: center; width: 100%; aspect-ratio: 4 / 3; overflow: hidden; background: var(--preview); color: var(--muted); }
.album-empty { padding: 48px 16px; text-align: center; background: var(--surface); border-radius: 12px; }
</style>
