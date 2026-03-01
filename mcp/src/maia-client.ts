/**
 * Maia REST API 클라이언트
 *
 * 원격 Maia RAG 서버와의 HTTP 통신을 담당하는 단일 모듈.
 * 모든 Tool 핸들러는 이 클라이언트를 통해 Maia에 접근한다.
 */

export interface SearchResult {
  id: string;
  summary: string;
  tags: string[];
  relevance_score: number;
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
  tags: string[];
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
  tags: string[];
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
    tags?: string[],
  ): Promise<SearchResponse> {
    return this.post("/search", { query, limit, offset: 0, mode, tags });
  }

  async ingest(content: string): Promise<IngestResponse> {
    return this.post("/ingest", { content });
  }

  async getDocument(id: string): Promise<DocumentResponse> {
    return this.get(`/documents/${id}`);
  }

  async listRecent(limit: number = 10): Promise<ListResponse> {
    return this.get(`/recent?limit=${limit}&offset=0`);
  }

  async getTags(): Promise<string[]> {
    return this.get("/tags");
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
