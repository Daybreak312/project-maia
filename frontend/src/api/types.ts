export interface Entity {
  entity_type: string | { other: string };
  value: string;
  context?: string;
}

export function getEntityTypeLabel(entityType: string | { other: string }): string {
  if (typeof entityType === 'string') {
    return entityType;
  }
  return entityType.other || 'other';
}

/** 문서 출처 (커넥터 유입 문서에만 존재). */
export interface DocumentSource {
  source_type: string;
  source_id: string;
  modified_at: string;
  connector_id: string;
}

export interface Document {
  id: string;
  raw_content: string;
  summary: string;
  entities: Entity[];
  created_at: string;
  /** 커넥터 유입 문서의 출처 (수동 입력은 없음). */
  source?: DocumentSource;
}

export interface IngestResponse {
  id: string;
  summary: string;
  entities: Entity[];
  // Phase 2 에이전트 전략 메타데이터 (구버전 응답엔 없음 — 모두 옵셔널)
  strategy?: string;
  document_ids?: string[];
  edges_created?: number;
  fallback?: boolean;
  reason?: string;
}

/** 그래프 이웃 조회 결과의 한 항목. */
export interface NeighborView {
  id: string;
  summary: string;
  depth: number;
  relation: string;
  via: string;
  weight: number;
}

export interface NeighborsResponse {
  start: string;
  depth: number;
  neighbors: NeighborView[];
}

export interface SearchResult {
  id: string;
  summary: string;
  relevance_score: number;
  /** 결과 출처 워크스페이스 (교차 검색 시 구분) */
  workspace?: string;
}

export interface SearchResponse {
  results: SearchResult[];
  sources_used: string[];
  total: number;
  mode: string;
}

export interface SearchOptions {
  query: string;
  limit?: number;
  offset?: number;
  mode?: 'vector' | 'keyword' | 'hybrid';
}

export interface ListResponse {
  documents: Document[];
  total: number;
  limit: number;
  offset: number;
}

export interface ListOptions {
  limit?: number;
  offset?: number;
}

export interface ProviderInfo {
  provider: string;
  has_api_key: boolean;
  api_key_preview: string | null;
}

/** Codex(구독 OAuth) 상태 — 키가 아니라 auth.json 임포트 기반. */
export interface CodexStatus {
  has_auth: boolean;
  account_preview: string | null;
  last_refresh: string | null;
}

/** 로컬 임베딩 상태 — 키 불요, 모델/차원/캐시 준비 여부. */
export interface LocalStatus {
  model: string;
  dim: number;
  cache_ready: boolean;
}

export interface SettingsResponse {
  providers: ProviderInfo[];
  parsing_provider: string | null;
  embedding_provider: string | null;
  codex: CodexStatus;
  local: LocalStatus;
}

// ─── 워크스페이스 ────────────────────────────────────────────────
export type WorkspaceTemplate = 'personal' | 'enterprise';

/** 백엔드 WorkspaceConfig 중 프론트에서 사용하는 필드 (전체 구조의 부분집합). */
export interface WorkspaceSummary {
  id: string;
  name: string;
  template: WorkspaceTemplate;
  created_at: string;
  search: {
    cross_workspace: string[];
    default_mode: string;
    time_decay_lambda: number;
  };
}

export interface CreateWorkspaceRequest {
  id: string;
  name: string;
  template?: WorkspaceTemplate;
}

// ─── API 키 ──────────────────────────────────────────────────────
export type Permission = 'read_only' | 'read_write' | 'admin';

/** 키 공개 뷰 (해시 미포함). */
export interface ApiKeyInfo {
  key_id: string;
  label: string;
  workspaces: string[];
  permissions: Permission;
  created_at: string;
  last_used_at: string | null;
  expires_at: string | null;
  /** 소유 계정 user_id (null = 시스템 키) */
  owner: string | null;
}

export interface CreateKeyRequest {
  label: string;
  workspaces: string[];
  permissions: Permission;
  expires_at?: string | null;
}

export interface CreateKeyResponse {
  /** 평문 키 — 이 응답에서만 확인 가능 */
  api_key: string;
  key: ApiKeyInfo;
}

// ─── 인증 · 계정 (Phase 2) ───────────────────────────────────────
/** 계정 공개 뷰 (password_hash 미포함 — 백엔드 UserInfo). */
export interface UserInfo {
  user_id: string;
  username: string;
  display_name: string;
  is_admin: boolean;
  created_at: string;
}

/** 인증 소스 — `GET /api/auth/me`의 auth_source. */
export type AuthSource = 'master' | 'dev' | 'api_key' | 'session';

/** 접근 가능한 워크스페이스 한 항목 (id + 유효 권한). */
export interface WorkspaceAccess {
  id: string;
  permission: Permission;
}

/** `GET /api/auth/me` 응답 — 앱 전역 인증 상태의 원천. */
export interface MeResponse {
  auth_source: AuthSource;
  /** 인증된 계정 (세션·소유 키). 마스터키·시스템 키·dev는 null. */
  user: UserInfo | null;
  is_admin: boolean;
  workspaces: WorkspaceAccess[];
}

/** `POST /api/auth/login` 응답 (세션 토큰은 HttpOnly 쿠키로만 전달). */
export interface LoginResponse {
  user: UserInfo;
}

export interface CreateUserRequest {
  username: string;
  /** 초기 비밀번호 (admin이 지정) */
  password: string;
  display_name?: string;
  is_admin?: boolean;
}

/** 계정 생성 응답 — 자동 생성된 개인 워크스페이스 id 포함. */
export interface CreateUserResponse {
  user: UserInfo;
  personal_workspace: string;
}

// ─── 워크스페이스 멤버십 · 공개 설정 ─────────────────────────────
export type WorkspaceVisibility = 'private' | 'public';

/** 멤버 목록 항목 — 계정이 삭제된 잔존 멤버십은 username/display_name이 null. */
export interface MemberView {
  user_id: string;
  username: string | null;
  display_name: string | null;
  role: Permission;
}

/** `GET /api/workspaces/:id/members` 응답. */
export interface MembersResponse {
  visibility: WorkspaceVisibility;
  /** public일 때 비멤버 로그인 계정에게 부여되는 권한 (admin 불가). */
  public_permission: Permission;
  members: MemberView[];
}

export interface SetVisibilityRequest {
  visibility: WorkspaceVisibility;
  /** 미지정 시 기존 값 유지. */
  public_permission?: Permission;
}

// ─── 커넥터 (Phase 4) ────────────────────────────────────────────
export interface LocalDirectoryConfig {
  directories: string[];
  extensions: string[];
  exclude: string[];
  max_file_bytes: number;
}

/** 타입 태그가 인라인된 커넥터 스펙 (백엔드 `#[serde(tag="type")]`). */
export type ConnectorSpec = { type: 'local_directory' } & LocalDirectoryConfig;

export interface ConnectorInstance {
  id: string;
  enabled: boolean;
  interval_secs: number;
  concurrency: number;
  spec: ConnectorSpec;
}

export interface SyncFailure {
  source_id: string;
  error: string;
}

export interface SyncSummary {
  started_at: string;
  finished_at: string;
  processed: number;
  created: number;
  updated: number;
  skipped: number;
  failed: number;
  failures: SyncFailure[];
}

export interface SyncState {
  last_run_at: string | null;
  cursor: string | null;
  last_result: SyncSummary | null;
}

export interface RunProgress {
  running: boolean;
  total: number;
  processed: number;
  created: number;
  updated: number;
  skipped: number;
  failed: number;
}

/** 커넥터 조회 뷰 — 설정 + 동기화 상태 + (실행 중이면) 진행. */
export interface ConnectorView {
  instance: ConnectorInstance;
  state: SyncState;
  progress?: RunProgress;
}

export interface RegisterConnectorRequest {
  id: string;
  enabled?: boolean;
  interval_secs?: number;
  concurrency?: number;
  spec: ConnectorSpec;
}

export interface SyncTriggerRequest {
  mode?: 'parsed' | 'raw';
  full?: boolean;
  concurrency?: number;
}

// ─── Patrol · 거버넌스 (Phase 5) ─────────────────────────────────
export type ReviewStatus = 'pending' | 'valid' | 'needs_fix' | 'deleted' | 'dismissed';
export type DetectorKind = 'staleness' | 'duplicate' | 'orphan' | 'external_mismatch';
export type ReviewDecision = 'valid' | 'needs_fix' | 'deleted' | 'dismissed';
/** 판단 주체 — 사람(수동) vs AI 자동 판정(review_mode=auto). */
export type DecidedBy = 'human' | 'auto';

/** Review Queue 한 항목 (탐지 후보 + 판단 상태). */
export interface ReviewItem {
  id: string;
  workspace: string;
  document_id: string;
  kind: DetectorKind;
  reason: string;
  /** 탐지기가 남긴 근거 수치(유형별 상이). */
  evidence: Record<string, unknown>;
  status: ReviewStatus;
  created_at: string;
  decided_at?: string | null;
  /** 판단 주체(대기 중이면 없음). auto 판정의 감사 추적용. */
  decided_by?: DecidedBy | null;
  /** 판단 근거(주로 auto 판정의 LLM/재유입 사유). */
  decision_reason?: string | null;
}

export interface JudgeResponse {
  items: ReviewItem[];
}

export interface DetectionCounts {
  staleness: number;
  duplicate: number;
  orphan: number;
  external_mismatch: number;
  total: number;
}

/** Patrol 실행 한 건의 기록. */
export interface PatrolRun {
  started_at: string;
  finished_at: string;
  trigger: string;
  detections: DetectionCounts;
  enqueued: number;
  edges_decayed: number;
  failed_detectors: string[];
}

export interface PatrolState {
  last_run_at: string | null;
  history: PatrolRun[];
}

export interface SearchMetrics {
  count: number;
  zero_result_rate: number;
  avg_top_score: number;
}
export interface GraphMetrics {
  nodes: number;
  edges: number;
  orphans: number;
  avg_degree: number;
}
export interface IngestMetrics {
  document_count: number;
  strategy_distribution: Record<string, number>;
}
export interface PatrolMetrics {
  detections: number;
  open_items: number;
  resolved_items: number;
  resolution_rate: number;
}

/** 하루치 메트릭 롤업. */
export interface DailyRollup {
  date: string;
  workspace: string;
  search: SearchMetrics;
  graph: GraphMetrics;
  ingest: IngestMetrics;
  patrol: PatrolMetrics;
  generated_at: string;
}
