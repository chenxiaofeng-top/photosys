<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { computed, nextTick, onBeforeUnmount, ref, shallowRef, watch } from "vue";
import * as maplibregl from "maplibre-gl";
import { type GeoJSONSource, type Map as LibreMap, type MapMouseEvent, type StyleSpecification } from "maplibre-gl";
import workerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?url";
import { layers, namedFlavor } from "@protomaps/basemaps";
import "maplibre-gl/dist/maplibre-gl.css";
import OfflineMapSettings from "./OfflineMapSettings.vue";
import { mapArchives, type Coordinates, type MapPackage, type MapPoints, type MediaFilter, type Selection } from "./locations";

const props = defineProps<{ scanId: number; filter: MediaFilter; personId: number | null; contentQuery: string | null; revision: number; focus: Coordinates | null }>();
const emit = defineEmits<{ select: [selection: Selection | null] }>();
const open = ref(false);
const settingsVisited = ref(false);
const container = ref<HTMLDivElement>();
const points = shallowRef<MapPoints>();
const loading = ref(false);
const error = ref("");
const renderingError = ref("");
const drawing = ref(false);
const drawStart = ref<{ x: number; y: number }>();
const drawEnd = ref<{ x: number; y: number }>();
const selectedBounds = shallowRef<[number, number, number, number] | null>(null);
const packages = shallowRef<MapPackage[]>([]);
const covered = ref(false);
let map: LibreMap | undefined;
const mapReady = ref(false);
let disposed = false;
let archiveList = mapArchives([]);
const protocol = `photosys-map-${Math.random().toString(36).slice(2)}`;
maplibregl.setWorkerUrl(workerUrl);
maplibregl.addProtocol(protocol, async (params, abort) => {
  const match = params.url.match(/tiles\/(\d+)\/(\d+)\/(\d+)/);
  if (!match) throw new Error("无效的地图瓦片请求。");
  const [, sz, sx, sy] = match;
  const z = Number(sz), x = Number(sx), y = Number(sy), n = 2 ** z;
  const west = x / n * 360 - 180, east = (x + 1) / n * 360 - 180;
  const latitude = (v: number) => Math.atan(Math.sinh(Math.PI * (1 - 2 * v / n))) * 180 / Math.PI;
  const north = latitude(y), south = latitude(y + 1);
  for (const { record, archive } of archiveList) {
    const [w, s, e, nn] = record.bounds;
    if (east < w || west > e || north < s || south > nn || z < record.minZoom || z > record.maxZoom) continue;
    const tile = await archive.getZxy(z, x, y, abort.signal);
    if (tile) return { data: new Uint8Array(tile.data) };
  }
  return { data: new Uint8Array() };
});
const drawStyle = computed(() => {
  if (!drawStart.value || !drawEnd.value) return {};
  return { left: `${Math.min(drawStart.value.x, drawEnd.value.x)}px`, top: `${Math.min(drawStart.value.y, drawEnd.value.y)}px`,
    width: `${Math.abs(drawStart.value.x - drawEnd.value.x)}px`, height: `${Math.abs(drawStart.value.y - drawEnd.value.y)}px` };
});
function geojson() {
  return { type: "FeatureCollection" as const, features: (points.value?.points ?? []).map(p => ({ type: "Feature" as const,
    properties: { id: p.id }, geometry: { type: "Point" as const, coordinates: [p.longitude, Math.max(-85.051129, Math.min(85.051129, p.latitude))] } })) };
}
function selectionData() {
  const s = props.filter.locationSelection;
  return { type: "FeatureCollection" as const, features: s?.type === "bounds" ? [{ type: "Feature" as const, properties: {},
    geometry: { type: "Polygon" as const, coordinates: [[[s.west, s.south], [s.east, s.south], [s.east, s.north], [s.west, s.north], [s.west, s.south]]] } }] : [] };
}
function style(): StyleSpecification {
  return { version: 8, glyphs: `${new URL("/maps/fonts/", location.href).href}{fontstack}/{range}.pbf`, sprite: new URL("/maps/sprites/light", location.href).href,
    sources: {
      overview: { type: "geojson", data: new URL("/maps/overview.geojson", location.href).href },
      streets: { type: "vector", tiles: [`${protocol}://tiles/{z}/{x}/{y}`], minzoom: 0, maxzoom: 15, attribution: '© OpenStreetMap contributors · Protomaps' },
      media: { type: "geojson", data: geojson(), cluster: true, clusterRadius: 45, clusterMaxZoom: 18 },
      selection: { type: "geojson", data: selectionData() },
    },
    layers: [
      { id: "background", type: "background", paint: { "background-color": "#e7eef0" } },
      { id: "land", type: "fill", source: "overview", paint: { "fill-color": "#f2f3eb", "fill-outline-color": "#bcc9bc" } },
      ...layers("streets", namedFlavor("light"), { lang: "zh-Hans" }).filter(l => l.type !== "background").map(l => {
        if (l.type === "symbol" && l.layout?.["text-font"]) return { ...l, layout: { ...l.layout, "text-font": ["Noto Sans Regular"] } };
        return l;
      }),
      { id: "selection-fill", type: "fill", source: "selection", paint: { "fill-color": "#386d49", "fill-opacity": .12 } },
      { id: "selection-outline", type: "line", source: "selection", paint: { "line-color": "#386d49", "line-width": 2 } },
      { id: "clusters", type: "circle", source: "media", filter: ["has", "point_count"], paint: { "circle-color": "#386d49", "circle-radius": ["step", ["get", "point_count"], 19, 100, 25, 1000, 31], "circle-stroke-width": 2, "circle-stroke-color": "#fff" } },
      { id: "cluster-count", type: "symbol", source: "media", filter: ["has", "point_count"], layout: { "text-field": ["get", "point_count_abbreviated"], "text-font": ["Noto Sans Regular"], "text-size": 13 }, paint: { "text-color": "#fff" } },
      { id: "media-point", type: "circle", source: "media", filter: ["!", ["has", "point_count"]], paint: { "circle-color": "#d58440", "circle-radius": 7, "circle-stroke-width": 2, "circle-stroke-color": "#fff" } },
    ] };
}
function updateCoverage() {
  if (!map) return;
  const c = map.getCenter();
  covered.value = packages.value.some(p => c.lng >= p.bounds[0] && c.lng <= p.bounds[2] && c.lat >= p.bounds[1] && c.lat <= p.bounds[3]);
}
function updatePackages(value: MapPackage[]) {
  if (JSON.stringify(packages.value) === JSON.stringify(value)) return;
  packages.value = value; archiveList = mapArchives(value);
  if (map) { renderingError.value = ""; map.setStyle(style()); updateCoverage(); }
}
async function initialize() {
  if (map || !container.value || disposed) return;
  try {
    const font = new FontFace("PhotoSys Map CJK", "url('/maps/chinese.ttf')");
    document.fonts.add(await font.load());
    if (disposed || !container.value || !open.value) return;
    map = new maplibregl.Map({ container: container.value, style: style(), center: [104, 35], zoom: 3,
      localIdeographFontFamily: "PhotoSys Map CJK", renderWorldCopies: false, dragRotate: false, pitchWithRotate: false });
    mapReady.value = true;
    map.on("style.load", () => {
      (map?.getSource("media") as GeoJSONSource | undefined)?.setData(geojson());
      (map?.getSource("selection") as GeoJSONSource | undefined)?.setData(selectionData());
    });
    map.addControl(new maplibregl.NavigationControl({ showCompass: false }), "top-right");
    map.on("load", () => { fitPoints(); if (props.focus) focusLocation(props.focus); });
    map.on("moveend", updateCoverage);
    map.on("error", event => { renderingError.value = `地图加载失败：${event.error.message}`; });
    map.on("click", "clusters", async event => {
      if (drawing.value || !map) return;
      const feature = event.features?.[0];
      if (!feature || feature.geometry.type !== "Point") return;
      try {
        const z = await (map.getSource("media") as GeoJSONSource).getClusterExpansionZoom(Number(feature.properties.cluster_id));
        map.easeTo({ center: feature.geometry.coordinates as [number, number], zoom: Math.min(z, 19) });
      } catch (cause) { error.value = String(cause); }
    });
    map.on("click", "media-point", event => {
      if (drawing.value) return;
      const id = Number(event.features?.[0]?.properties.id);
      const point = points.value?.points.find(p => p.id === id);
      if (point) emit("select", { type: "point", latitude: point.latitude, longitude: point.longitude });
    });
    map.on("mousedown", beginDraw); map.on("mousemove", moveDraw); map.on("mouseup", endDraw);
  } catch (cause) { renderingError.value = `无法显示地图。${String(cause)}`; }
}
function fitPoints() {
  const values = points.value?.points;
  if (!map || !values?.length) return;
  const bounds = new maplibregl.LngLatBounds();
  for (const p of values) bounds.extend([p.longitude, Math.max(-85, Math.min(85, p.latitude))]);
  map.fitBounds(bounds, { padding: 45, maxZoom: 15, duration: 0 });
}
function focusLocation(p: Coordinates) { map?.easeTo({ center: [p.longitude, Math.max(-85, Math.min(85, p.latitude))], zoom: 15 }); }
watch(() => props.focus, async value => { if (value) { open.value = true; await nextTick(); await initialize(); focusLocation(value); } });
watch(open, async value => { if (value) { settingsVisited.value = true; await nextTick(); await initialize(); map?.resize(); } else cancelDrawing(); });
watch([() => props.scanId, () => props.filter, () => props.personId, () => props.contentQuery, () => props.revision, open], async (_, __, cleanup) => {
  if (!open.value) return;
  let active = true; cleanup(() => { active = false; }); loading.value = true; error.value = "";
  try {
    const data = await invoke<MapPoints>("library_map_points", { scanId: props.scanId, filter: props.filter, personId: props.personId, contentQuery: props.contentQuery });
    if (active) { const first = !points.value; points.value = data; if (map?.isStyleLoaded()) (map.getSource("media") as GeoJSONSource)?.setData(geojson()); if (first) fitPoints(); }
  } catch (cause) { if (active) { error.value = String(cause); points.value = undefined; (map?.getSource("media") as GeoJSONSource | undefined)?.setData(geojson()); } }
  finally { if (active) loading.value = false; }
});
watch(() => props.filter.locationSelection, () => { if (map?.isStyleLoaded()) (map.getSource("selection") as GeoJSONSource)?.setData(selectionData()); });
function cancelDrawing() { drawing.value = false; drawStart.value = undefined; drawEnd.value = undefined; map?.dragPan.enable(); }
function startDrawing() { drawing.value = true; map?.dragPan.disable(); }
function beginDraw(event: MapMouseEvent) { if (drawing.value && event.originalEvent.button === 0) { drawStart.value = { x: event.point.x, y: event.point.y }; drawEnd.value = drawStart.value; } }
function moveDraw(event: MapMouseEvent) { if (drawing.value && drawStart.value) drawEnd.value = { x: event.point.x, y: event.point.y }; }
function endDraw(event: MapMouseEvent) {
  if (!drawing.value || !drawStart.value || !map) return;
  const start = drawStart.value, end = event.point;
  if (Math.abs(start.x - end.x) < 4 || Math.abs(start.y - end.y) < 4) { cancelDrawing(); return; }
  const a = map.unproject([Math.min(start.x, end.x), Math.max(start.y, end.y)]), b = map.unproject([Math.max(start.x, end.x), Math.min(start.y, end.y)]);
  const bounds: [number, number, number, number] = [Math.max(-180, a.lng), Math.max(-85.051129, a.lat), Math.min(180, b.lng), Math.min(85.051129, b.lat)];
  if (bounds[0] >= bounds[2] || bounds[1] >= bounds[3]) { error.value = "请选择地图有效范围内的矩形。"; cancelDrawing(); return; }
  error.value = "";
  selectedBounds.value = bounds;
  emit("select", { type: "bounds", west: bounds[0], south: bounds[1], east: bounds[2], north: bounds[3] });
  cancelDrawing();
}
onBeforeUnmount(() => { disposed = true; map?.remove(); maplibregl.removeProtocol(protocol); });
</script>

<template>
  <section class="location-section" aria-label="拍摄位置地图">
    <button type="button" class="secondary map-toggle" :aria-expanded="open" @click="open = !open">{{ open ? '收起地图' : '展开位置地图' }}</button>
    <div v-show="open" class="map-panel">
      <div class="map-toolbar">
        <button type="button" class="secondary" :disabled="!mapReady || drawing" @click="startDrawing">框选</button>
        <button v-if="drawing" type="button" class="secondary" @click="cancelDrawing">取消框选</button>
        <button type="button" class="secondary" @click="emit('select', null)">清除位置选择</button>
        <button type="button" class="secondary" @click="fitPoints">显示全部结果点位</button>
        <span v-if="loading" role="status">正在加载点位…</span>
        <span v-else-if="points" role="status">{{ points.total - points.unknown }} 个有位置 · {{ points.unknown }} 个位置未知</span>
      </div>
      <p v-if="drawing" role="status">在地图上拖出矩形，选择其中的照片与视频。按 Esc 取消。</p>
      <p v-if="!covered">当前中心未安装街道地图，正在显示概览底图。可在下方下载区域地图。</p>
      <div class="map-container" :class="{ drawing }" @keydown.esc="cancelDrawing">
        <div ref="container" class="map-canvas" tabindex="0" aria-label="媒体地理分布；可缩放、点击点位，或使用框选工具" />
        <div v-if="drawing && drawStart && drawEnd" class="draw-box" :style="drawStyle" />
      </div>
      <p v-if="!loading && points && !points.points.length">当前结果没有可显示的位置。可调整筛选，或清除位置选择。</p>
      <p v-if="error || renderingError" class="error" role="alert">{{ error || renderingError }}</p>
      <OfflineMapSettings v-if="settingsVisited" :bounds="selectedBounds" @changed="updatePackages" />
    </div>
  </section>
</template>

<style scoped>
.location-section { margin: .8rem 0; }
.map-toggle { font-size: .9rem; }
.map-panel { margin-top: .7rem; padding: 1rem; background: #f7f9f4; border: 1px solid #dce4d9; border-radius: 12px; }
.map-toolbar { display: flex; flex-wrap: wrap; gap: .6rem; align-items: center; }
.map-toolbar span,p { font-size: .875rem; color: #566252; }
.map-container { position: relative; margin-top: .75rem; }
.map-canvas { height: clamp(300px, 48vh, 600px); width: 100%; border-radius: 8px; overflow: hidden; }
.drawing :deep(canvas) { cursor: crosshair !important; }
.draw-box { position: absolute; pointer-events: none; border: 2px solid #386d49; background: #386d4920; }
.error { color: #a13927; }
</style>
