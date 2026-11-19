import type { Item, Page } from "./libraryTypes";
export interface Album { id: number; name: string; count: number; cover: Item | null; }
export interface AlbumPage extends Page<Album> { writable: boolean; }
export type AlbumEdit =
  | { kind: "create"; name: string; fileIds?: number[] }
  | { kind: "rename"; albumId: number; name: string }
  | { kind: "delete"; albumId: number }
  | { kind: "add" | "remove"; albumId: number; fileIds: number[] };
export interface AlbumEditResult { albumId: number; }
export function validAlbumName(name: string) { return name.trim().length > 0 && [...name.trim()].length <= 80; }
