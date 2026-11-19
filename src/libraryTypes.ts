import type { Location } from "./locations";
import type { Rotation } from "./RotatableImage.vue";

export type Kind = "photo" | "video";
export type Sort = "dateDesc" | "dateAsc" | "nameAsc" | "nameDesc" | "sizeDesc" | "sizeAsc";
export interface Item {
  id: number;
  name: string;
  relativePath: string;
  size: number;
  kind: Kind;
  capturedOn: string | null;
  location: Location | null;
  previewAvailable: boolean;
  isLivePhoto: boolean;
  favorite: boolean;
  rotation: Rotation;
}
export interface Page<T> { page: number; total: number; items: T[]; }
export type LibraryMode = "library" | "people" | "albums";
