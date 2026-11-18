import { invoke } from "@tauri-apps/api/core";
import { PMTiles, type Source } from "pmtiles";

export interface Coordinates { latitude: number; longitude: number }
export interface Location extends Coordinates { address: string | null }
export type LocationMode = "all" | "known" | "unknown";
export type Selection = ({ type: "point" } & Coordinates) | { type: "bounds"; west: number; south: number; east: number; north: number };
export interface MediaFilter { query: string; kind: "photo" | "video" | null; from: string | null; to: string | null; unknownDate: boolean; favoritesOnly: boolean; locationMode: LocationMode; locationSelection: Selection | null }
export interface MapPoints { total: number; unknown: number; points: ({ id: number } & Coordinates)[] }
export interface MapPackage { id: string; name: string; bounds: [number, number, number, number]; minZoom: number; maxZoom: number; size: number; installedAt: number; source: string; version: string; warning: string | null }
export interface MapState { directory: string; packages: MapPackage[]; interrupted: boolean; interruptedDownload: { name: string; bounds: [number, number, number, number] } | null }
export interface Region { id: string; name: string; level: number; bounds: [number, number, number, number] }
export function shortLocation(location: Location | null): string {
  if (!location) return "位置未知";
  return location.address?.split(" · ").slice(-2).join(" · ") || `${location.latitude.toFixed(4)}, ${location.longitude.toFixed(4)}`;
}

export class LocalMapSource implements Source {
  constructor(private readonly id: string) {}
  getKey() { return `photosys-map:${this.id}`; }
  async getBytes(offset: number, length: number, signal?: AbortSignal) {
    signal?.throwIfAborted();
    const bytes = await invoke<ArrayBuffer>("offline_map_read", { id: this.id, offset, length });
    signal?.throwIfAborted();
    return { data: bytes instanceof ArrayBuffer ? bytes : new Uint8Array(bytes).buffer };
  }
}
export function mapArchives(packages: MapPackage[]) {
  return packages.filter(p => !p.warning).sort((a, b) => b.installedAt - a.installedAt)
    .map(record => ({ record, archive: new PMTiles(new LocalMapSource(record.id)) }));
}
