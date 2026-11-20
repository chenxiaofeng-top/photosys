<script setup lang="ts">
import { computed } from "vue";
import { formatBytes } from "./formatBytes";
import type { Item, MediaDetails, MediaDimensions, VideoDetails } from "./libraryTypes";

const props = defineProps<{
  item: Item;
  details?: MediaDetails;
  loading: boolean;
  error: string;
}>();

const decimal = new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 });
const localTime = new Intl.DateTimeFormat("zh-CN", {
  dateStyle: "medium", timeStyle: "medium", hourCycle: "h23",
});
const formatNames: Record<string, string> = { jpg: "JPEG", jpeg: "JPEG", tif: "TIFF", tiff: "TIFF", webm: "WebM" };
const codecNames: Record<string, string> = { h264: "H.264", hevc: "H.265 (HEVC)", h263: "H.263", prores: "ProRes" };

interface Row { label: string; value: string | null; }
interface Group { title: string; rows: Row[]; }

function embeddedTime(value: string) {
  return value.replace("T", " ").replace(/(Z|[+-]\d{2}:\d{2})$/, (_, offset) => ` ${offset === "Z" ? "UTC" : offset}`);
}

function dimensions(value: MediaDimensions | null) {
  return value && `${value.width} × ${value.height} 像素`;
}

function exposure(value: number) {
  if (value < 1) {
    const reciprocal = 1 / value;
    if (Math.abs(reciprocal - Math.round(reciprocal)) < 0.0001) return `1/${Math.round(reciprocal)} 秒`;
  }
  return `${Number(value.toPrecision(4))} 秒`;
}

function duration(seconds: number) {
  const milliseconds = Math.max(1, Math.round(seconds * 1000));
  const whole = Math.floor(milliseconds / 1000);
  const hours = Math.floor(whole / 3600);
  const minutes = String(Math.floor(whole / 60) % 60).padStart(2, "0");
  const remainder = String(whole % 60).padStart(2, "0");
  const fraction = String(milliseconds % 1000).padStart(3, "0").replace(/0+$/, "");
  return `${hours ? `${hours}:` : ""}${minutes}:${remainder}${fraction ? `.${fraction}` : ""}`;
}

function videoRows(video: VideoDetails, embedded = false): Row[] {
  const bitRate = video.bitRate;
  return [
    { label: "媒体创建时间", value: embedded && video.createdAt ? embeddedTime(video.createdAt) : null },
    { label: "原始分辨率", value: dimensions(video.dimensions) },
    { label: "时长", value: video.durationSeconds === null ? null : duration(video.durationSeconds) },
    { label: "平均帧率", value: video.averageFrameRate === null ? null : `${decimal.format(video.averageFrameRate)} fps` },
    { label: "视频编码", value: video.codec && (codecNames[video.codec] ?? video.codec.toUpperCase()) },
    { label: "总码率", value: bitRate === null ? null : bitRate >= 1_000_000
      ? `${decimal.format(bitRate / 1_000_000)} Mb/s`
      : `${decimal.format(bitRate / 1000)} kb/s` },
    { label: "音频", value: video.hasAudio === false ? "无音轨" : null },
    { label: "音频编码", value: video.audioCodec?.toUpperCase() ?? null },
    { label: "采样率", value: video.sampleRate === null ? null : `${decimal.format(video.sampleRate / 1000)} kHz` },
    { label: "声道数", value: video.channels === null ? null : decimal.format(video.channels) },
  ];
}

const groups = computed(() => {
  const { item, details } = props;
  const photo = details?.photo;
  const video = details?.video;
  const extension = item.name.slice(item.name.lastIndexOf(".") + 1).toLowerCase();
  const time = photo?.capturedAt ?? video?.createdAt;
  const base: Group = {
    title: "基本信息",
    rows: [
      { label: time ? (video ? "媒体创建时间" : "拍摄时间") : "媒体日期", value: time ? embeddedTime(time) : item.capturedOn ?? "日期未知" },
      { label: "文件格式", value: formatNames[extension] ?? extension.toUpperCase() },
      { label: "文件大小", value: formatBytes(item.size) },
      { label: "修改时间", value: details?.modifiedAt ? localTime.format(new Date(details.modifiedAt)) : null },
    ],
  };
  const groups = [base];
  if (photo) {
    const { make, model } = photo;
    const modelIncludesMake = make && model?.toLowerCase().startsWith(`${make.toLowerCase()} `);
    const camera = modelIncludesMake || make?.toLowerCase() === model?.toLowerCase()
      ? model : [make, model].filter(Boolean).join(" ");
    groups.push({ title: "拍摄参数", rows: [
      { label: "原始尺寸", value: dimensions(photo.dimensions) },
      { label: "相机", value: camera },
      { label: "镜头", value: photo.lens },
      { label: "焦距", value: photo.focalLengthMm === null ? null : `${decimal.format(photo.focalLengthMm)} mm` },
      { label: "光圈", value: photo.aperture === null ? null : `f/${decimal.format(photo.aperture)}` },
      { label: "快门速度", value: photo.exposureSeconds === null ? null : exposure(photo.exposureSeconds) },
      { label: "ISO", value: photo.iso === null ? null : decimal.format(photo.iso) },
    ] });
  }
  if (video) groups.push({ title: "视频参数", rows: videoRows(video) });
  if (details?.liveVideo) groups.push({ title: "实况视频", rows: videoRows(details.liveVideo, true) });
  return groups.map(group => ({ ...group, rows: group.rows.filter(row => row.value) }))
    .filter(group => group.rows.length);
});
</script>

<template>
  <section v-for="group in groups" :key="group.title" class="info-group">
    <h4>{{ group.title }}</h4>
    <dl>
      <template v-for="row in group.rows" :key="row.label">
        <dt>{{ row.label }}</dt>
        <dd>{{ row.value }}</dd>
      </template>
    </dl>
  </section>
  <p v-if="loading" class="details-note" role="status">正在读取媒体参数…</p>
  <p v-else-if="error" class="details-note" role="status">{{ error }}</p>
  <p v-else-if="details?.partial" class="details-note" role="status">部分媒体参数无法读取。</p>
</template>

<style scoped>
.info-group { padding-block: 14px; border-top: 1px solid var(--border); }
h4 { margin: 0 0 9px; font-size: 13px; font-weight: 650; }
dl { display: grid; grid-template-columns: 6.6em minmax(0, 1fr); gap: 6px 10px; margin: 0; font-size: 13px; line-height: 1.5; }
dt { color: var(--text-secondary); }
dd { margin: 0; overflow-wrap: anywhere; font-size: inherit; font-weight: 500; font-variant-numeric: tabular-nums; }
.details-note { margin: 0 0 14px; font-size: 13px; color: var(--text-secondary); overflow-wrap: anywhere; }
</style>
