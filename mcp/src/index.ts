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

const MAIA_URL = process.env.MAIA_URL ?? "http://localhost:8080";
const MAIA_API_KEY = process.env.MAIA_API_KEY;
const client = new MaiaClient(MAIA_URL, MAIA_API_KEY);

const server = new McpServer({
  name: "maia",
  version: "1.0.0",
  description: `Personal knowledge base (Maia). Stores the user's career history, interview experiences, project notes, memos, salary details, skills, and personal records. Use search_context FIRST when the user asks about their personal info. Use ingest_information when they ask to save something.`,
});

// ─── Tool: search_context ────────────────────────────────────────────
server.tool(
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
    tags: z
      .array(z.string())
      .optional()
      .describe("Filter by tags (optional). Use get_tags first to see available tags."),
  },
  async ({ query, limit, mode, tags }) => {
    const res = await client.search(query, limit, mode, tags);

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
          `    tags: ${r.tags.join(", ")}`,
        ];
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

// ─── Tool: ingest_information ────────────────────────────────────────
server.tool(
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
  },
  async ({ content }) => {
    const res = await client.ingest(content);

    const parts = [
      `Saved successfully (id: ${res.id})`,
      ``,
      `Summary: ${res.summary}`,
      `Tags: ${res.tags.join(", ")}`,
    ];

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

// ─── Tool: get_document ──────────────────────────────────────────────
server.tool(
  "get_document",
  `Retrieve the full original content of a specific document from Maia.

Use this when:
- You need the complete raw text after finding a document via search_context
- The user asks to see the original content of a specific entry
- search_context summary is insufficient and you need more detail

Requires the document UUID from a previous search result.`,
  {
    id: z.string().uuid().describe("Document UUID (from search results)"),
  },
  async ({ id }) => {
    const doc = await client.getDocument(id);

    const parts = [
      `Document: ${doc.id}`,
      `Created: ${doc.created_at}`,
      `Summary: ${doc.summary}`,
      `Tags: ${doc.tags.join(", ")}`,
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
server.tool(
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
  },
  async ({ limit }) => {
    const res = await client.listRecent(limit);

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
          `[${i + 1}] ${d.summary}\n    tags: ${d.tags.join(", ")}\n    created: ${d.created_at}\n    id: ${d.id}`,
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

// ─── Tool: get_tags ──────────────────────────────────────────────────
server.tool(
  "get_tags",
  `List all tags in the user's Maia knowledge base.

Use when:
- You want to know what categories of information the user has stored
- Before using tag-based filtering in search_context`,
  {},
  async () => {
    const tags = await client.getTags();

    return {
      content: [
        {
          type: "text" as const,
          text:
            tags.length > 0
              ? `Available tags (${tags.length}): ${tags.join(", ")}`
              : "No tags yet. The knowledge base is empty.",
        },
      ],
    };
  },
);

// ─── 서버 시작 ───────────────────────────────────────────────────────
async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);
}

main().catch((err) => {
  console.error("Maia MCP server failed to start:", err);
  process.exit(1);
});
