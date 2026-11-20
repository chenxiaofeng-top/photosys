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

export interface MediaDimensions { width: number; height: number; }
export interface PhotoDetails {
  dimensions: MediaDimensions | null;
  capturedAt: string | null;
  make: string | null;
  model: string | null;
  lens: string | null;
  focalLengthMm: number | null;
  aperture: number | null;
  exposureSeconds: number | null;
  iso: number | null;
}
export interface VideoDetails {
  dimensions: MediaDimensions | null;
  createdAt: string | null;
  durationSeconds: number | null;
  averageFrameRate: number | null;
  codec: string | null;
  bitRate: number | null;
  hasAudio: boolean | null;
  audioCodec: string | null;
  sampleRate: number | null;
  channels: number | null;
}
export interface MediaDetails {
  modifiedAt: string | null;
  photo: PhotoDetails | null;
  video: VideoDetails | null;
  liveVideo: VideoDetails | null;
  partial: boolean;
}

export interface Page<T> { page: number; total: number; items: T[]; }
export type LibraryMode = "library" | "people" | "albums";
