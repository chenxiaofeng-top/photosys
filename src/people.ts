export interface Page<T> { page: number; total: number; items: T[] }
export interface Person { id: number; name: string; photoCount: number; faceCount: number; thumbnail: string | null }
export interface Face { id: number; fileId: number; fileName: string; personId: number | null; personName: string | null; thumbnail: string; bbox: number[]; manual: boolean; ignored: boolean; needsReview: boolean }
export interface PersonSuggestion { id: number; name: string }
export interface FaceSuggestions { featureVersion: string; faces: { faceId: number; people: PersonSuggestion[] }[] }
export interface PeopleStatus { modelId: string; needsRebuild: boolean; rebuildReason: string | null; scanId: number; enabled: boolean; running: boolean; queued: boolean; total: number; indexed: number; pending: number; failed: number; reused: number; writable: boolean; message: string | null; pendingFaces: number; ignoredFaces: number; undoDescription: string | null; autoGrouping: boolean }
export interface Month { month: string | null; count: number }
export function personName(person: { id: number; name: string }): string { return person.name || `未命名人物 ${person.id}`; }
