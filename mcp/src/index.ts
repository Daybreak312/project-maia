#!/usr/bin/env node

/**
 * Maia MCP Server
 *
 * AI 도구(Claude Desktop, Cursor, Gemini CLI 등)와
 * 원격 Maia RAG 서버 사이의 MCP 브릿지.
 *
 * STDIO transport를 사용하여, AI 도구가 이 프로세스를 직접 spawn한다.
 */

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { MaiaClient } from "./maia-client.js";
import {
  describeServerArg,
  loadServerRegistry,
  resolveServer,
  type ResolvedServer,
  type ServerRegistry,
} from "./servers.js";

/** 설정 오류 시 즉시 종료 — 잘못된 서버로의 조용한 접속보다 명시적 기동 실패가 낫다. */
function fatal(message: string): never {
  console.error(`[maia-mcp] configuration error: ${message}`);
  process.exit(1);
}

// 서버 레지스트리 로드. 소스 우선순위·문서 형식은 servers.ts 상단 주석 참조.
// stderr 로그는 STDIO transport(stdout)와 분리되어 프로토콜을 오염시키지 않는다.
let registry: ServerRegistry;
try {
  registry = loadServerRegistry();
} catch (err) {
  fatal(err instanceof Error ? err.message : String(err));
}

// alias → REST 클라이언트. 기동 시 전부 생성해 호출 시점의 실패 요인을 줄인다.
const clients = new Map<string, MaiaClient>(
  [...registry.servers.values()].map((s) => [s.alias, new MaiaClient(s.url, s.apiKey)]),
);

console.error(
  `[maia-mcp] loaded ${clients.size} server(s) from ${registry.source}: ` +
    [...registry.servers.values()]
      .map((s) => `${s.alias}${s.alias === registry.defaultAlias ? " (default)" : ""} → ${s.url}`)
      .join(", "),
);

/** tool의 `server` 인자를 대상 서버·클라이언트로 해석한다. 미등록 alias는 에러. */
function resolveTarget(server?: string): { client: MaiaClient; info: ResolvedServer } {
  const info = resolveServer(registry, server);
  // resolveServer가 성공한 alias는 반드시 clients에 있다 (동일 레지스트리에서 생성).
  return { client: clients.get(info.alias)!, info };
}

/** tool 인자로 받은 workspace가 없으면 대상 서버에 설정된 기본 워크스페이스를 사용한다. */
function resolveWorkspace(server: ResolvedServer, workspace?: string): string | undefined {
  return workspace ?? server.workspace;
}

/** 모든 tool에 공통으로 추가되는 선택적 workspace 인자 스키마. */
const workspaceArg = z
  .string()
  .optional()
  .describe(
    "Target workspace ID (e.g. 'personal', 'work'). Omit to use the default workspace bound to the API key.",
  );

/** 모든 tool에 공통으로 추가되는 선택적 server 인자 스키마 — 설정된 alias 목록을 노출한다. */
const serverArg = z.string().optional().describe(describeServerArg(registry));

const mcp = new McpServer({
  name: "maia",
  version: "1.1.0",
  description: `Personal knowledge base (Maia). Stores the user's career history, interview experiences, project notes, memos, salary details, skills, and personal records. Use search_context FIRST when the user asks about their personal info. Use ingest_information when they ask to save something. Multiple Maia servers can be configured (e.g. personal / enterprise) — every tool accepts an optional 'server' alias argument.`,
});

// ─── Tool: search_context ────────────────────────────────────────────
mcp.tool(
  "search_context",
  `Search the user's personal knowledge base (Maia) for relevant information.

IMPORTANT: ALWAYS call this tool FIRST when:
- The user asks about their personal experiences, career, interviews, projects, skills, or any stored records
- The user references something they previously mentioned or saved ("that company I interviewed at", "my project", etc.)
- You need the user's personal context to answer accurately
- The user asks "what companies did I...", "how much was the salary at...", "what did I think about...", "내가 면접 본...", "연봉이 얼마였..."
- Do NOT guess or assume personal information — always search first

INTERPRETING RESULTS:
- relevance_score is raw cosine similarity (0-1). Only genuinely relevant results are returned (low-relevance results are filtered out).
- matched_facts are the most precise matches — prioritize these when answering.
- If you need the full original text, use get_document with the result's id.
- Always cite which entries you used when answering the user.`,
  {
    query: z.string().describe("Natural language search query"),
    limit: z
      .number()
      .int()
      .min(1)
      .max(20)
      .default(5)
      .describe("Max results (server may return fewer based on relevance)"),
    mode: z
      .enum(["hybrid", "vector", "keyword"])
      .default("hybrid")
      .describe("Search mode: hybrid (recommended), vector (semantic), keyword (exact match)"),
    workspace: workspaceArg,
    server: serverArg,
  },
  async ({ query, limit, mode, workspace, server }) => {
    const { client, info } = resolveTarget(server);
    const res = await client.search(query, limit, mode, resolveWorkspace(info, workspace));

    if (res.results.length === 0) {
      return {
        content: [
          {
            type: "text" as const,
            text: `No results found for "${query}". The user may not have stored relevant information yet.`,
          },
        ],
      };
    }

    const formatted = res.results
      .map((r, i) => {
        const lines = [
          `[${i + 1}] ${r.summary}`,
          `    relevance: ${(r.relevance_score * 100).toFixed(0)}%`,
        ];
        if (r.workspace) {
          lines.push(`    workspace: ${r.workspace}`);
        }
        if (r.matched_facts && r.matched_facts.length > 0) {
          lines.push(`    matched_facts:`);
          r.matched_facts.forEach((f) => lines.push(`      - ${f}`));
        }
        lines.push(`    id: ${r.id}`);
        return lines.join("\n");
      })
      .join("\n\n");

    return {
      content: [
        {
          type: "text" as const,
          text: `Found ${res.results.length} relevant entries (mode: ${res.mode}):\n\n${formatted}`,
        },
      ],
    };
  },
);

// ─── Tool: deep_search ───────────────────────────────────────────────
mcp.tool(
  "deep_search",
  `Deeply recall everything related to a topic from the user's knowledge base (Maia).

Unlike search_context (a single-shot lookup), this runs the server's Search Agent:
it evaluates whether the initial results are sufficient, rewrites the query to cover
missed angles (bounded rounds), walks the knowledge graph to pull in connected
documents, then de-duplicates and re-ranks into one broader result set.

USE THIS (instead of search_context) when:
- The user wants EVERYTHING about a topic/entity ("all about company A", "이사 관련 전부")
- A first search felt incomplete and you suspect related context exists
- You need the full cluster of connected knowledge, not just the top matches

INTERPRETING RESULTS:
- Results are a synthesized cluster (no duplicates, ranked by relevance).
- expanded_from marks a result pulled in via the graph (neighbor of another result).
- The exploration summary reports rounds, the queries tried, and whether the graph was
  walked. If fallback is true, the LLM was unavailable and these are the initial results
  (still valid — just not agent-expanded).
- If nothing is found, the tried queries are shown — treat as a genuine knowledge gap,
  do NOT fabricate an answer.`,
  {
    query: z.string().describe("Natural language topic to recall broadly"),
    workspace: workspaceArg,
    server: serverArg,
  },
  async ({ query, workspace, server }) => {
    const { client, info } = resolveTarget(server);
    const res = await client.deepSearch(query, resolveWorkspace(info, workspace));
    const meta = res.agent;

    // 탐색 과정 요약 라인 (관측성 — 어떻게 회상했는지).
    const summaryLines: string[] = [];
    if (meta) {
      summaryLines.push(
        `Exploration: ${meta.rounds} round(s), queries tried: ${meta.queries
          .map((q) => `"${q}"`)
          .join(", ")}`,
      );
      if (meta.graph_expanded) {
        summaryLines.push(`Graph expansion: ${meta.expansion_count} connected document(s) added`);
      }
      if (meta.fallback) {
        summaryLines.push(
          `⚠ Fallback: agent judgement unavailable — showing initial results (${meta.reason})`,
        );
      }
    }

    if (res.results.length === 0) {
      const triedNote = meta ? `\n\n${summaryLines.join("\n")}` : "";
      return {
        content: [
          {
            type: "text" as const,
            text: `No related knowledge found for "${query}". The user likely hasn't stored this yet — do not fabricate.${triedNote}`,
          },
        ],
      };
    }

    const formatted = res.results
      .map((r, i) => {
        const lines = [
          `[${i + 1}] ${r.summary}`,
          `    relevance: ${(r.relevance_score * 100).toFixed(0)}%`,
        ];
        if (r.workspace) {
          lines.push(`    workspace: ${r.workspace}`);
        }
        if (r.expanded_from) {
          lines.push(`    ↳ via graph (neighbor of ${r.expanded_from})`);
        }
        if (r.matched_facts && r.matched_facts.length > 0) {
          lines.push(`    matched_facts:`);
          r.matched_facts.forEach((f) => lines.push(`      - ${f}`));
        }
        lines.push(`    id: ${r.id}`);
        return lines.join("\n");
      })
      .join("\n\n");

    const header = `Recalled ${res.results.length} related entries:`;
    const footer = summaryLines.length > 0 ? `\n\n${summaryLines.join("\n")}` : "";

    return {
      content: [
        {
          type: "text" as const,
          text: `${header}\n\n${formatted}${footer}`,
        },
      ],
    };
  },
);

// ─── Tool: ingest_information ────────────────────────────────────────
mcp.tool(
  "ingest_information",
  `Save new information to the user's personal knowledge base (Maia).

Call this when:
- The user says "remember this", "save this", "note this down", "기억해둬", "저장해줘"
- The user shares personal information and asks you to keep it
- The user wants to update their records

Input can be any natural language text. The system automatically extracts: summary, tags, entities (companies, people, skills, money, dates), and atomic facts.`,
  {
    content: z
      .string()
      .describe("Information to store (natural language, no length limit)"),
    workspace: workspaceArg,
    server: serverArg,
  },
  async ({ content, workspace, server }) => {
    const { client, info } = resolveTarget(server);
    const res = await client.ingest(content, resolveWorkspace(info, workspace));

    const parts = [`Saved successfully (id: ${res.id})`];

    // Phase 2: 에이전트 판단 전략 표시 (구버전 서버는 strategy 미포함 → 생략)
    if (res.strategy) {
      const label: Record<string, string> = {
        new: "new document",
        update: "updated existing document",
        split: "split into multiple documents",
        duplicate: "detected as duplicate (original kept, linked)",
        raw: "raw store (agent bypassed)",
      };
      parts.push(`Strategy: ${res.strategy} — ${label[res.strategy] ?? res.strategy}`);
      if (res.document_ids && res.document_ids.length > 1) {
        parts.push(`Documents affected: ${res.document_ids.length}`);
      }
      if (typeof res.edges_created === "number" && res.edges_created > 0) {
        parts.push(`Edges created: ${res.edges_created}`);
      }
      if (res.fallback) {
        parts.push(`⚠ Fallback: agent judgement unavailable, stored as raw (no info lost)`);
      }
      if (res.reason) {
        parts.push(`Reason: ${res.reason}`);
      }
    }

    parts.push(``, `Summary: ${res.summary}`);

    if (res.entities.length > 0) {
      parts.push(`Entities:`);
      res.entities.forEach((e) => parts.push(`  - [${e.entity_type}] ${e.value}`));
    }

    if (res.facts && res.facts.length > 0) {
      parts.push(`Facts (${res.facts.length}):`);
      res.facts.forEach((f) => parts.push(`  - ${f}`));
    }

    return {
      content: [{ type: "text" as const, text: parts.join("\n") }],
    };
  },
);

// ─── Tool: get_neighbors ─────────────────────────────────────────────
mcp.tool(
  "get_neighbors",
  `Explore documents connected to a given document in the user's knowledge graph.

Use this to walk relationships from a starting document — after finding a document via
search_context, call this to discover related context the search didn't surface directly.
Relations: related_to, updates, contradicts, references, part_of.

depth=1 returns direct neighbors; depth=2 walks two hops (e.g. interview note → prep note
→ related tech doc). Cycles are handled safely.`,
  {
    id: z.string().uuid().describe("Starting document UUID (from a search result)"),
    depth: z
      .number()
      .int()
      .min(1)
      .max(5)
      .default(1)
      .describe("Traversal depth (1 = direct neighbors, capped at 5)"),
    workspace: workspaceArg,
    server: serverArg,
  },
  async ({ id, depth, workspace, server }) => {
    const { client, info } = resolveTarget(server);
    const res = await client.neighbors(id, depth, resolveWorkspace(info, workspace));

    if (res.neighbors.length === 0) {
      return {
        content: [
          {
            type: "text" as const,
            text: `No connected documents found for ${id} within depth ${res.depth}.`,
          },
        ],
      };
    }

    const formatted = res.neighbors
      .map((n, i) =>
        [
          `[${i + 1}] ${n.summary}`,
          `    relation: ${n.relation} (depth ${n.depth}, via ${n.via})`,
          `    id: ${n.id}`,
        ].join("\n"),
      )
      .join("\n\n");

    return {
      content: [
        {
          type: "text" as const,
          text: `Connected documents (depth ${res.depth}, ${res.neighbors.length} found):\n\n${formatted}`,
        },
      ],
    };
  },
);

// ─── Tool: get_document ──────────────────────────────────────────────
mcp.tool(
  "get_document",
  `Retrieve the full original content of a specific document from Maia.

Use this when:
- You need the complete raw text after finding a document via search_context
- The user asks to see the original content of a specific entry
- search_context summary is insufficient and you need more detail

Requires the document UUID from a previous search result.`,
  {
    id: z.string().uuid().describe("Document UUID (from search results)"),
    workspace: workspaceArg,
    server: serverArg,
  },
  async ({ id, workspace, server }) => {
    const { client, info } = resolveTarget(server);
    const doc = await client.getDocument(id, resolveWorkspace(info, workspace));

    const parts = [
      `Document: ${doc.id}`,
      `Created: ${doc.created_at}`,
      `Summary: ${doc.summary}`,
    ];

    if (doc.entities.length > 0) {
      parts.push(`Entities:`);
      doc.entities.forEach((e) => parts.push(`  - [${e.entity_type}] ${e.value}`));
    }

    parts.push(``, `--- Original Content ---`, doc.raw_content);

    return {
      content: [{ type: "text" as const, text: parts.join("\n") }],
    };
  },
);

// ─── Tool: list_recent_documents ─────────────────────────────────────
mcp.tool(
  "list_recent_documents",
  `List recently stored documents in Maia.

Use when:
- The user asks "what have I saved recently?", "show my records", "최근 기록 보여줘"
- You want to give the user an overview of their stored knowledge`,
  {
    limit: z
      .number()
      .int()
      .min(1)
      .max(50)
      .default(10)
      .describe("Number of documents to retrieve"),
    workspace: workspaceArg,
    server: serverArg,
  },
  async ({ limit, workspace, server }) => {
    const { client, info } = resolveTarget(server);
    const res = await client.listRecent(limit, resolveWorkspace(info, workspace));

    if (res.documents.length === 0) {
      return {
        content: [
          { type: "text" as const, text: "No documents stored yet." },
        ],
      };
    }

    const formatted = res.documents
      .map(
        (d, i) =>
          `[${i + 1}] ${d.summary}\n    created: ${d.created_at}\n    id: ${d.id}`,
      )
      .join("\n\n");

    return {
      content: [
        {
          type: "text" as const,
          text: `Recent documents (${res.documents.length}/${res.total} total):\n\n${formatted}`,
        },
      ],
    };
  },
);

// ─── 서버 시작 ───────────────────────────────────────────────────────
async function main() {
  const transport = new StdioServerTransport();
  await mcp.connect(transport);
}

main().catch((err) => {
  console.error("Maia MCP server failed to start:", err);
  process.exit(1);
});
