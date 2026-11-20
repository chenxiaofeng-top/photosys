export interface Page<T> { page: number; total: number; items: T[] }
export interface Person { id: number; name: string; photoCount: number; faceCount: number; thumbnail: string | null; avatarFaceId: number | null; important: boolean }
export interface Face { id: number; fileId: number; fileName: string; personId: number | null; personName: string | null; thumbnail: string; bbox: number[]; manual: boolean; ignored: boolean; personIgnored: boolean; needsReview: boolean }
export interface SimilarFaceCriteria { targetPersonId: number; anchorFaceId: number; threshold: number }
export interface SimilarFace { faceId: number; fileId: number; score: number; sourcePersonId: number | null; sourcePersonName: string | null; thumbnail: string; token: string }
export interface SimilarFaceResults { anchorToken: string; faces: Page<SimilarFace> }
export interface PersonSuggestion { id: number; name: string }
export interface FaceSuggestions { featureVersion: string; faces: { faceId: number; people: PersonSuggestion[] }[] }
export interface GroupingParameters { threshold: number; margin: number }
export interface GroupingSettings { featureVersion: string; parameters: GroupingParameters; defaults: GroupingParameters }
export interface PendingGroupSample { faceId: number; fileId: number; thumbnail: string; personId: number; personName: string; personIgnored: boolean }
export interface PendingGroupPreview { token: string; visibleMatches: number; ignoredMatches: number; samples: PendingGroupSample[] }
export interface PeopleStatus { modelId: string; needsRebuild: boolean; rebuildReason: string | null; scanId: number; enabled: boolean; running: boolean; queued: boolean; total: number; indexed: number; pending: number; failed: number; reused: number; writable: boolean; message: string | null; pendingFaces: number; ignoredFaces: number; ignoredPeople: number; undoDescription: string | null; autoGrouping: boolean; groupingSettings: GroupingSettings | null }
export interface Month { month: string | null; count: number }
export function personName(person: { id: number; name: string }): string { return person.name || `未命名人物 ${person.id}`; }
