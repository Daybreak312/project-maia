/**
 * Maia 서버 레지스트리 — 다중 서버 설정의 로딩·검증·해석.
 *
 * Maia는 온프레미스 설치가 기본 전제다. 한 사용자가 개인 서버와 조직 서버를
 * 동시에 쓰는 상황(personal / enterprise …)을 위해, 서버들을 alias → 접속 정보의
 * key-value 레지스트리로 관리하고 모든 tool이 선택적 `server` 인자로 대상을 고른다.
 *
 * 설정 소스 우선순위 — 첫 번째로 발견된 소스 "하나만" 사용한다(병합 없음).
 * 소스 간 병합은 어떤 서버가 어디서 왔는지 추적을 어렵게 하므로 의도적으로 배제한다.
 *
 *   1. MAIA_SERVERS        — 인라인 JSON (레지스트리 문서 전체)
 *   2. MAIA_SERVERS_FILE   — 레지스트리 JSON 파일 경로 (없으면 에러 — 명시 설정은 fail-fast)
 *   3. ~/.maia/servers.json — 기본 경로 (존재할 때만)
 *   4. MAIA_URL / MAIA_API_KEY / MAIA_WORKSPACE — 레거시 단일 서버 (alias "default")
 *
 * 레지스트리 문서 형식:
 * {
 *   "defaultServer": "personal",
 *   "servers": {
 *     "personal":   { "url": "https://maia.example.com", "apiKeyFile": "~/.maia/personal.key" },
 *     "enterprise": { "url": "https://maia.corp.example.com", "apiKey": "...", "workspace": "team" }
 *   }
 * }
 */

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { z } from "zod";

/** 레지스트리 문서의 서버 한 항목. apiKey와 apiKeyFile은 상호 배타. */
const serverEntrySchema = z
  .object({
    url: z.string().url(),
    apiKey: z.string().min(1).optional(),
    apiKeyFile: z.string().min(1).optional(),
    workspace: z.string().min(1).optional(),
  })
  .strict();

/** 레지스트리 문서 전체. strict — 오타 난 키는 조용히 무시하지 않고 즉시 실패시킨다. */
const registryDocSchema = z
  .object({
    defaultServer: z.string().min(1).optional(),
    servers: z.record(serverEntrySchema),
  })
  .strict();

export type RegistryDoc = z.infer<typeof registryDocSchema>;

/** 해석 완료된 서버 접속 정보 (apiKeyFile은 이미 읽혀 apiKey로 평탄화됨). */
export interface ResolvedServer {
  alias: string;
  url: string;
  apiKey?: string;
  /** 이 서버로 향하는 호출의 기본 워크스페이스 (tool 인자가 우선). */
  workspace?: string;
}

export interface ServerRegistry {
  /** 어떤 설정 소스에서 로드됐는지 — 기동 로그·오류 메시지용. */
  source: string;
  defaultAlias: string;
  servers: Map<string, ResolvedServer>;
}

/** `~/` 접두 경로를 홈 디렉토리로 전개한다. */
function expandHome(p: string): string {
  if (p === "~") return os.homedir();
  if (p.startsWith("~/")) return path.join(os.homedir(), p.slice(2));
  return p;
}

/** apiKey/apiKeyFile을 단일 apiKey로 해석한다. 둘 다 지정은 모호하므로 에러. */
function resolveApiKey(
  alias: string,
  entry: z.infer<typeof serverEntrySchema>,
  source: string,
): string | undefined {
  if (entry.apiKey && entry.apiKeyFile) {
    throw new Error(
      `server '${alias}' in ${source} sets both apiKey and apiKeyFile — use exactly one`,
    );
  }
  if (entry.apiKey) return entry.apiKey;
  if (entry.apiKeyFile) {
    const file = expandHome(entry.apiKeyFile);
    let raw: string;
    try {
      raw = fs.readFileSync(file, "utf-8");
    } catch (err) {
      throw new Error(
        `server '${alias}' in ${source}: cannot read apiKeyFile '${file}' (${
          err instanceof Error ? err.message : String(err)
        })`,
      );
    }
    const key = raw.trim();
    if (!key) {
      throw new Error(`server '${alias}' in ${source}: apiKeyFile '${file}' is empty`);
    }
    return key;
  }
  return undefined;
}

/** 검증된 레지스트리 문서를 ResolvedServer 맵으로 변환하고 기본 서버를 확정한다. */
export function buildRegistry(doc: RegistryDoc, source: string): ServerRegistry {
  const aliases = Object.keys(doc.servers);
  if (aliases.length === 0) {
    throw new Error(`no servers defined in ${source}`);
  }

  const servers = new Map<string, ResolvedServer>();
  for (const alias of aliases) {
    const entry = doc.servers[alias];
    servers.set(alias, {
      alias,
      url: entry.url.replace(/\/+$/, ""),
      apiKey: resolveApiKey(alias, entry, source),
      workspace: entry.workspace,
    });
  }

  let defaultAlias: string;
  if (doc.defaultServer !== undefined) {
    if (!servers.has(doc.defaultServer)) {
      throw new Error(
        `defaultServer '${doc.defaultServer}' in ${source} is not a defined server (defined: ${aliases.join(", ")})`,
      );
    }
    defaultAlias = doc.defaultServer;
  } else if (aliases.length === 1) {
    defaultAlias = aliases[0];
  } else {
    // 다중 서버에서 기본값 추측은 잘못된 서버로의 조용한 오접속 위험 — 명시를 요구한다.
    throw new Error(
      `defaultServer is required in ${source} when multiple servers are configured (defined: ${aliases.join(", ")})`,
    );
  }

  return { source, defaultAlias, servers };
}

/** JSON 텍스트를 파싱·검증해 레지스트리로 만든다. */
function registryFromJson(text: string, source: string): ServerRegistry {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (err) {
    throw new Error(
      `invalid JSON in ${source}: ${err instanceof Error ? err.message : String(err)}`,
    );
  }
  const result = registryDocSchema.safeParse(parsed);
  if (!result.success) {
    const issues = result.error.issues
      .map((i) => `${i.path.join(".") || "(root)"}: ${i.message}`)
      .join("; ");
    throw new Error(`invalid server registry in ${source}: ${issues}`);
  }
  return buildRegistry(result.data, source);
}

/** 기본 레지스트리 파일 경로: ~/.maia/servers.json */
export function defaultRegistryPath(): string {
  return path.join(os.homedir(), ".maia", "servers.json");
}

/**
 * 환경으로부터 서버 레지스트리를 로드한다 (소스 우선순위는 파일 상단 주석 참조).
 *
 * 발견된 소스가 깨져 있으면 레거시로 폴백하지 않고 즉시 던진다 — 사용자가 그 소스를
 * 의도한 이상, 조용한 폴백은 엉뚱한 서버로의 접속이라는 더 나쁜 실패로 이어진다.
 */
export function loadServerRegistry(env: NodeJS.ProcessEnv = process.env): ServerRegistry {
  if (env.MAIA_SERVERS) {
    return registryFromJson(env.MAIA_SERVERS, "MAIA_SERVERS (inline env)");
  }

  if (env.MAIA_SERVERS_FILE) {
    const file = expandHome(env.MAIA_SERVERS_FILE);
    let text: string;
    try {
      text = fs.readFileSync(file, "utf-8");
    } catch (err) {
      throw new Error(
        `cannot read MAIA_SERVERS_FILE '${file}' (${err instanceof Error ? err.message : String(err)})`,
      );
    }
    return registryFromJson(text, `MAIA_SERVERS_FILE (${file})`);
  }

  const defaultFile = defaultRegistryPath();
  if (fs.existsSync(defaultFile)) {
    return registryFromJson(fs.readFileSync(defaultFile, "utf-8"), defaultFile);
  }

  // 레거시 단일 서버 환경변수 — 기존 배포와의 하위 호환 경로.
  const doc: RegistryDoc = {
    defaultServer: "default",
    servers: {
      default: {
        url: env.MAIA_URL ?? "http://localhost:8080",
        ...(env.MAIA_API_KEY ? { apiKey: env.MAIA_API_KEY } : {}),
        ...(env.MAIA_WORKSPACE ? { workspace: env.MAIA_WORKSPACE } : {}),
      },
    },
  };
  return buildRegistry(doc, "legacy env (MAIA_URL/MAIA_API_KEY/MAIA_WORKSPACE)");
}

/** tool의 `server` 인자 설명 — 설정된 alias 목록을 그대로 노출해 호출자가 고르게 한다. */
export function describeServerArg(registry: ServerRegistry): string {
  const list = [...registry.servers.values()]
    .map((s) => {
      const marker = s.alias === registry.defaultAlias ? " (default)" : "";
      return `'${s.alias}'${marker} → ${s.url}`;
    })
    .join(", ");
  return `Target Maia server alias. Configured: ${list}. Omit to use the default server.`;
}

/** alias(미지정 시 기본 서버)를 ResolvedServer로 해석한다. 미등록 alias는 에러. */
export function resolveServer(registry: ServerRegistry, alias?: string): ResolvedServer {
  const target = alias ?? registry.defaultAlias;
  const server = registry.servers.get(target);
  if (!server) {
    const known = [...registry.servers.keys()].join(", ");
    throw new Error(
      `Unknown Maia server alias '${target}'. Configured aliases: ${known} (default: ${registry.defaultAlias}).`,
    );
  }
  return server;
}
