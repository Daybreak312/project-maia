/**
 * Maia REST API 클라이언트
 *
 * 원격 Maia RAG 서버와의 HTTP 통신을 담당하는 단일 모듈.
 * 모든 Tool 핸들러는 이 클라이언트를 통해 Maia에 접근한다.
 */

export interface SearchResult {
  id: string;
  summary: string;
  relevance_score: number;
  /** 이 결과가 나온 출처 워크스페이스 (교차 검색 시 구분용) */
  workspace?: string;
  matched_facts?: string[];
  /** 그래프 확장으로 추가된 결과의 유래 — 어느 검색 결과 문서의 이웃인지 (agent 검색). */
  expanded_from?: string;
}

/** agent(deep) 검색의 탐색 과정 요약 (구버전 서버·기존 검색은 미포함). */
export interface AgentSearchMeta {
  /** 수행된 검색 라운드 수 (초기 1회 + 재작성 재검색). */
  rounds: number;
  /** 실제 시도된 쿼리 목록 (원 쿼리가 첫 번째). */
  queries: string[];
  /** 그래프 이웃 확장이 수행되었는지 여부. */
  graph_expanded: boolean;
  /** 그래프 확장으로 결과에 추가된 문서 수. */
  expansion_count: number;
  /** LLM 판단 실패/미설정으로 폴백(초기 결과 반환)했는지 여부. */
  fallback: boolean;
  /** 종료/폴백 사유. */
  reason: string;
}

export interface SearchResponse {
  results: SearchResult[];
  sources_used: string[];
  total: number;
  mode: string;
  /** agent 검색 메타데이터 (기존 검색은 미포함). */
  agent?: AgentSearchMeta;
}

export interface IngestResponse {
  id: string;
  summary: string;
  entities: Entity[];
  facts?: string[];
  // Phase 2 에이전트 전략 메타데이터 (구버전 서버는 미포함 — 모두 옵셔널)
  strategy?: string;
  document_ids?: string[];
  edges_created?: number;
  fallback?: boolean;
  reason?: string;
}

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

export interface Entity {
  entity_type: string;
  value: string;
  context: string | null;
}

export interface DocumentResponse {
  id: string;
  raw_content: string;
  summary: string;
  entities: Entity[];
  created_at: string;
}

export interface ListResponse {
  documents: DocumentResponse[];
  total: number;
  limit: number;
  offset: number;
}

export class MaiaClient {
  constructor(
    private readonly baseUrl: string,
    private readonly apiKey?: string,
  ) {}

  async search(
    query: string,
    limit: number = 5,
    mode: string = "hybrid",
    workspace?: string,
  ): Promise<SearchResponse> {
    return this.post(this.withWorkspace("/search", workspace), {
      query,
      limit,
      offset: 0,
      mode,
    });
  }

  /**
   * agent(deep) 검색 — 서버의 Search Agent가 충분성 평가·쿼리 재작성·그래프 확장으로
   * 능동 탐색한다. 기존 `/search`에 `agent: true`를 실어 opt-in한다(엔드포인트 동일).
   */
  async deepSearch(query: string, workspace?: string): Promise<SearchResponse> {
    return this.post(this.withWorkspace("/search", workspace), {
      query,
      agent: true,
    });
  }

  async ingest(content: string, workspace?: string): Promise<IngestResponse> {
    return this.post(this.withWorkspace("/ingest", workspace), { content });
  }

  async getDocument(id: string, workspace?: string): Promise<DocumentResponse> {
    return this.get(this.withWorkspace(`/documents/${id}`, workspace));
  }

  /** 지식 그래프에서 시작 문서의 이웃을 depth 상한과 함께 조회한다. */
  async neighbors(
    id: string,
    depth: number = 1,
    workspace?: string,
  ): Promise<NeighborsResponse> {
    return this.get(
      this.withWorkspace(`/documents/${id}/neighbors?depth=${depth}`, workspace),
    );
  }

  async listRecent(limit: number = 10, workspace?: string): Promise<ListResponse> {
    return this.get(this.withWorkspace(`/recent?limit=${limit}&offset=0`, workspace));
  }

  /** 워크스페이스가 지정되면 경로에 `workspace` 쿼리 파라미터를 덧붙인다. */
  private withWorkspace(path: string, workspace?: string): string {
    if (!workspace) return path;
    const sep = path.includes("?") ? "&" : "?";
    return `${path}${sep}workspace=${encodeURIComponent(workspace)}`;
  }

  private authHeaders(): Record<string, string> {
    const headers: Record<string, string> = { "Content-Type": "application/json" };
    if (this.apiKey) {
      headers["Authorization"] = `Bearer ${this.apiKey}`;
    }
    return headers;
  }

  private async post<T>(path: string, body: unknown): Promise<T> {
    const res = await fetch(`${this.baseUrl}${path}`, {
      method: "POST",
      headers: this.authHeaders(),
      body: JSON.stringify(body),
    });
    if (!res.ok) {
      const text = await res.text();
      throw new Error(`Maia API error (${res.status}): ${text}`);
    }
    return res.json() as Promise<T>;
  }

  private async get<T>(path: string): Promise<T> {
    const res = await fetch(`${this.baseUrl}${path}`, {
      headers: this.authHeaders(),
    });
    if (!res.ok) {
      const text = await res.text();
      throw new Error(`Maia API error (${res.status}): ${text}`);
    }
    return res.json() as Promise<T>;
  }
}
