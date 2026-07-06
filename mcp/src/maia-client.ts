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
}

export interface SearchResponse {
  results: SearchResult[];
  sources_used: string[];
  total: number;
  mode: string;
}

export interface IngestResponse {
  id: string;
  summary: string;
  entities: Entity[];
  facts?: string[];
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

  async ingest(content: string, workspace?: string): Promise<IngestResponse> {
    return this.post(this.withWorkspace("/ingest", workspace), { content });
  }

  async getDocument(id: string, workspace?: string): Promise<DocumentResponse> {
    return this.get(this.withWorkspace(`/documents/${id}`, workspace));
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
