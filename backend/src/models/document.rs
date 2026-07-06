use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 저장되는 문서의 핵심 구조
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: Uuid,
    pub raw_content: String,
    pub summary: String,
    pub entities: Vec<Entity>,
    #[serde(default)]
    pub facts: Vec<String>,
    /// 다른 문서로의 방향성 관계(지식 그래프의 간선).
    /// `#[serde(default)]`로 edges 필드가 없는 기존 JSON도 로드 가능하다(하위호환).
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// 문서의 출처 — 커넥터(로컬 디렉토리 등)로 유입된 경우 어느 소스의 어느 항목에서
    /// 왔는지 추적한다. 수동 입력(API/MCP)은 None. `#[serde(default)]`로 이 필드가 없는
    /// 기존 문서 JSON도 로드된다(하위호환). None이면 직렬화에서 생략된다.
    ///
    /// **재유입 중복 방지의 키:** `(source.source_type, source.source_id)`가 동일한 문서를
    /// 찾으면 신규 생성이 아니라 업데이트 경로로 보내, 같은 파일이 여러 문서로 난립하는
    /// 것을 막는다. raw JSON(SSoT)에 저장되므로 reindex에서 그대로 살아남는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DocumentSource>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Document {
    pub fn new(
        raw_content: String,
        summary: String,
        entities: Vec<Entity>,
        facts: Vec<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            raw_content,
            summary,
            entities,
            facts,
            edges: Vec::new(),
            source: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// 출처 메타데이터를 부여한 문서를 반환한다(빌더 스타일).
    /// 커넥터 유입 경로가 신규 문서에 소스 식별자를 각인할 때 쓴다.
    pub fn with_source(mut self, source: DocumentSource) -> Self {
        self.source = Some(source);
        self
    }

    /// 대상 문서로의 엣지를 추가한다.
    ///
    /// 동일한 `(target, relation)` 쌍의 엣지가 이미 있으면 새로 추가하지 않고
    /// 가중치·생성 시각만 갱신한다 — 같은 관계가 중복 누적되어 그래프가 오염되는
    /// 것을 막는다(멱등에 가까운 upsert 시맨틱).
    pub fn add_edge(&mut self, edge: Edge) {
        // 자기 자신으로의 엣지는 그래프에서 의미가 없으므로 무시한다(순환 방지의 1차 방어).
        if edge.target == self.id {
            return;
        }
        if let Some(existing) = self
            .edges
            .iter_mut()
            .find(|e| e.target == edge.target && e.relation == edge.relation)
        {
            existing.weight = edge.weight;
            existing.created_at = edge.created_at;
            // 관계 재확인 = 새 기준. 다음 감쇠가 이 새 weight로부터 재계산하도록
            // base를 리셋한다(옛 base로 잘못 감쇠되는 것 방지).
            existing.base_weight = None;
        } else {
            self.edges.push(edge);
        }
    }

    /// 특정 대상 문서로 향하는 모든 엣지를 제거한다. 제거된 엣지 수를 반환한다.
    pub fn remove_edge(&mut self, target: Uuid) -> usize {
        let before = self.edges.len();
        self.edges.retain(|e| e.target != target);
        before - self.edges.len()
    }
}

/// 문서 간 방향성 관계의 종류. 확장 가능한 열거형.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelationType {
    /// 일반적 연관 (주제·맥락이 겹침)
    RelatedTo,
    /// 이 문서가 대상 문서를 갱신·대체함
    Updates,
    /// 이 문서가 대상 문서와 상충함
    Contradicts,
    /// 이 문서가 대상 문서를 참조·인용함
    References,
    /// 이 문서가 대상 문서의 부분임
    PartOf,
}

impl RelationType {
    /// payload 비정규화·프롬프트에 쓰는 소문자 스네이크 표기.
    pub fn as_str(&self) -> &'static str {
        match self {
            RelationType::RelatedTo => "related_to",
            RelationType::Updates => "updates",
            RelationType::Contradicts => "contradicts",
            RelationType::References => "references",
            RelationType::PartOf => "part_of",
        }
    }
}

impl std::fmt::Display for RelationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for RelationType {
    type Err = ();

    /// LLM이 반환한 관계 타입 문자열을 관대하게 파싱한다(하이픈/복수형/축약 허용).
    /// 알 수 없는 값은 에러 — 호출 측에서 기본값(RelatedTo)으로 폴백하게 한다.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().replace('-', "_").as_str() {
            "related_to" | "related" | "relatedto" | "relates_to" => Ok(RelationType::RelatedTo),
            "updates" | "update" => Ok(RelationType::Updates),
            "contradicts" | "contradict" | "contradiction" => Ok(RelationType::Contradicts),
            "references" | "reference" | "refers_to" => Ok(RelationType::References),
            "part_of" | "partof" | "part" => Ok(RelationType::PartOf),
            _ => Err(()),
        }
    }
}

/// 문서 간 방향성 엣지 (지식 그래프의 간선).
///
/// 엣지는 출발 문서의 raw JSON(`Document.edges`)에 저장되어 Single Source of
/// Truth가 되고, 인덱싱 시 summary chunk payload로 비정규화되어 reindex에서
/// 복원된다.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Edge {
    /// 대상 문서 ID (엣지가 향하는 곳)
    pub target: Uuid,
    /// 관계 타입
    pub relation: RelationType,
    /// 가중치 (0.0 ~ 1.0). 관련도·확신도를 표현하며 Phase 5에서 감쇠 재계산된다.
    pub weight: f32,
    /// 감쇠의 **기준 가중치** — 생성 시점의 원본 가중치. Phase 5의 시간 감쇠는 이
    /// base로부터 created_at 나이만큼 매번 재계산하므로 반복 실행해도 결과가 같다(멱등).
    /// None이면 `weight`를 base로 간주하고 최초 감쇠 시 고정한다(구버전 엣지 하위호환).
    /// 감쇠 전에는 직렬화에서 생략된다(기존 JSON 노이즈 방지).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_weight: Option<f32>,
    /// 엣지 생성 시각
    pub created_at: DateTime<Utc>,
}

impl Edge {
    /// 엣지를 생성한다. 가중치는 방어적으로 [0.0, 1.0]으로 클램프한다
    /// (LLM/외부 입력이 범위를 벗어난 값을 줘도 불변식 유지).
    pub fn new(target: Uuid, relation: RelationType, weight: f32) -> Self {
        Self {
            target,
            relation,
            weight: weight.clamp(0.0, 1.0),
            // 생성 시 base는 아직 고정하지 않는다(최초 감쇠 시 현재 weight로 고정).
            base_weight: None,
            created_at: Utc::now(),
        }
    }
}

/// 문서의 출처 — 커넥터로 유입된 문서가 "어느 소스의 어느 항목에서 왔는지"를 기록한다.
///
/// `(source_type, source_id)`가 재유입 중복 방지의 키다. 예컨대 로컬 디렉토리 커넥터가
/// 유입한 문서는 `source_type = "local_directory"`, `source_id = 원본 파일 경로`를 갖는다.
/// 같은 파일이 다시 스캔되면 이 키로 기존 문서를 찾아 업데이트 경로로 보낸다.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocumentSource {
    /// 소스 타입 식별자 (커넥터 종류). 예: "local_directory".
    pub source_type: String,
    /// 소스 내 고유 식별자 (원본 경로/ID). 중복 방지·업데이트 경로의 키.
    pub source_id: String,
    /// 원본의 수정 시각. 증분 스캔 커서·"변경 없으면 스킵" 판단의 기준.
    pub modified_at: DateTime<Utc>,
    /// 이 문서를 유입시킨 커넥터 인스턴스 ID (관측·추적용).
    pub connector_id: String,
}

/// 추출된 엔티티 (회사명, 금액, 날짜 등)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub entity_type: EntityType,
    pub value: String,
    pub context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    Company,
    Person,
    Money,
    Date,
    Skill,
    Project,
    Location,
    Other(String),
}

/// LLM 파싱 결과
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedContent {
    pub summary: String,
    pub entities: Vec<Entity>,
    #[serde(default)]
    pub facts: Vec<String>,
}

/// API Request/Response 타입들
pub mod api {
    use super::*;

    #[derive(Debug, Deserialize)]
    pub struct IngestRequest {
        pub content: String,
    }

    #[derive(Debug, Serialize)]
    pub struct IngestResponse {
        pub id: Uuid,
        pub summary: String,
        pub entities: Vec<Entity>,
        pub facts: Vec<String>,
    }

    /// Ingest Agent 판단 결과를 포함한 확장 응답.
    ///
    /// 기존 `IngestResponse`의 필드(id/summary/entities/facts)를 그대로 포함하는
    /// 상위집합이라, 이 필드만 읽던 기존 클라이언트(MCP/frontend)와 하위호환된다.
    /// 여기에 전략 메타데이터(strategy/document_ids/edges_created/fallback/reason)가
    /// 덧붙는다. `mode=raw` 우회 경로도 이 형태로 응답한다(strategy="raw").
    #[derive(Debug, Serialize)]
    pub struct IngestOutcome {
        // ── 대표 문서 (분할 시 첫 번째) — IngestResponse 호환 필드 ──
        pub id: Uuid,
        pub summary: String,
        pub entities: Vec<Entity>,
        pub facts: Vec<String>,
        // ── 전략 메타데이터 ──
        /// "new" | "update" | "split" | "duplicate" | "raw"
        pub strategy: String,
        /// 이 입력으로 영향받은 모든 문서 ID (분할 시 여러 개)
        pub document_ids: Vec<Uuid>,
        /// 자동 생성된 엣지 수
        pub edges_created: usize,
        /// 에이전트 판단을 우회/실패해 raw 저장했는지 여부
        pub fallback: bool,
        /// 판단 근거(정상) 또는 폴백 사유
        pub reason: String,
    }

    impl IngestOutcome {
        /// 기존 `IngestResponse`를 전략 메타데이터로 감싸 `IngestOutcome`을 만든다.
        pub fn from_response(
            resp: IngestResponse,
            strategy: impl Into<String>,
            document_ids: Vec<Uuid>,
            edges_created: usize,
            fallback: bool,
            reason: impl Into<String>,
        ) -> Self {
            Self {
                id: resp.id,
                summary: resp.summary,
                entities: resp.entities,
                facts: resp.facts,
                strategy: strategy.into(),
                document_ids,
                edges_created,
                fallback,
                reason: reason.into(),
            }
        }
    }

    #[derive(Debug, Deserialize)]
    pub struct SearchRequest {
        pub query: String,
        #[serde(default = "default_limit")]
        pub limit: usize,
        #[serde(default)]
        pub offset: usize,
        /// 검색 모드: "vector", "keyword", "hybrid" (기본값: hybrid)
        #[serde(default)]
        pub mode: Option<String>,
        /// 교차 워크스페이스 검색 제어.
        /// None: 워크스페이스 설정(cross_workspace)을 따름 (기본).
        /// Some(false): 대상 워크스페이스 단일 검색으로 강제.
        /// Some(true): 설정 목록 기반 교차 검색을 명시적으로 요청.
        #[serde(default)]
        pub cross_workspace: Option<bool>,
        /// 시간 감쇠 적용 여부 (opt-in). true면 동일 유사도에서 최신 문서가 상위에 온다.
        /// 감쇠 강도(lambda)는 워크스페이스 설정(time_decay_lambda)을 따른다.
        #[serde(default)]
        pub time_decay: Option<bool>,
        /// 기간 필터: 이 시각(포함) 이후 생성된 문서만 (created_at 기준).
        #[serde(default)]
        pub since: Option<DateTime<Utc>>,
        /// 기간 필터: 이 시각(포함) 이전 생성된 문서만.
        #[serde(default)]
        pub until: Option<DateTime<Utc>>,
        /// agent(deep) 검색 활성화 (opt-in, Phase 3). None/false면 기존 단일 검색 동작.
        /// true면 Search Agent가 충분성 평가·쿼리 재작성·그래프 확장으로 능동 탐색한다.
        #[serde(default)]
        pub agent: Option<bool>,
    }

    fn default_limit() -> usize {
        10
    }

    #[derive(Debug, Deserialize)]
    pub struct ListRequest {
        #[serde(default = "default_limit")]
        pub limit: usize,
        #[serde(default)]
        pub offset: usize,
    }

    #[derive(Debug, Serialize)]
    pub struct SearchResponse {
        pub results: Vec<SearchResult>,
        pub sources_used: Vec<Uuid>,
        /// 전체 결과 수 (페이지네이션용)
        pub total: usize,
        /// 사용된 검색 모드
        pub mode: String,
        /// Phase 3 agent(deep) 검색 메타데이터. 기본(비 agent) 검색은 None →
        /// 직렬화 생략(하위호환: 기존 클라이언트는 이 필드를 몰라도 무방).
        #[serde(skip_serializing_if = "Option::is_none")]
        pub agent: Option<AgentSearchMeta>,
    }

    /// agent(deep) 검색의 탐색 과정 요약 — 에이전트가 어떻게 회상했는지를 관측 가능하게 한다.
    ///
    /// PRD 인수 조건: 응답에 라운드 수·사용된 쿼리들·그래프 확장 여부·폴백 여부가 포함된다.
    #[derive(Debug, Clone, Serialize)]
    pub struct AgentSearchMeta {
        /// 수행된 검색 라운드 수 (초기 1회 + 재작성 재검색 횟수).
        pub rounds: usize,
        /// 실제로 시도된 쿼리 목록 (원 쿼리가 항상 첫 번째).
        pub queries: Vec<String>,
        /// 그래프 이웃 확장이 수행되었는지 여부.
        pub graph_expanded: bool,
        /// 그래프 확장으로 결과에 추가된 문서 수.
        pub expansion_count: usize,
        /// LLM 판단 실패/미설정으로 폴백(초기 결과 반환)했는지 여부.
        pub fallback: bool,
        /// 폴백 사유 또는 정상 종료 사유 (조기 종료·상한 도달 등).
        pub reason: String,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct SearchResult {
        pub id: Uuid,
        pub summary: String,
        pub relevance_score: f32,
        /// 이 결과가 나온 출처 워크스페이스 ID (교차 검색 시 구분용)
        pub workspace: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        pub matched_facts: Vec<String>,
        /// 문서 생성 시각 (시간 인식 검색·표시용). Qdrant payload에서 유래.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub created_at: Option<DateTime<Utc>>,
        /// 그래프 확장으로 추가된 결과의 유래 — 어느 검색 결과 문서의 이웃인지(Phase 3).
        /// 직접 검색으로 매칭된 결과는 None(직렬화 생략).
        #[serde(skip_serializing_if = "Option::is_none")]
        pub expanded_from: Option<Uuid>,
    }

    #[derive(Debug, Serialize)]
    pub struct DocumentResponse {
        pub id: Uuid,
        pub raw_content: String,
        pub summary: String,
        pub entities: Vec<Entity>,
        pub created_at: DateTime<Utc>,
        /// 문서 출처 (커넥터 유입 문서만). 수동 입력은 None → 직렬화 생략.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub source: Option<DocumentSource>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn make_doc() -> Document {
        Document::new(
            "raw".to_string(),
            "summary".to_string(),
            vec![],
            vec!["fact1".to_string()],
        )
    }

    // ──── 하위호환 직렬화 (edges 없는 기존 JSON) ────

    #[test]
    fn test_document_loads_legacy_json_without_edges() {
        // edges 필드가 없는 Phase 1 시절의 JSON도 로드되어야 한다(#[serde(default)]).
        let legacy = r#"{
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "raw_content": "old content",
            "summary": "old summary",
            "entities": [],
            "facts": ["a fact"],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }"#;

        let doc: Document = serde_json::from_str(legacy).unwrap();
        assert_eq!(doc.raw_content, "old content");
        assert!(doc.edges.is_empty(), "누락된 edges는 빈 벡터로 기본값 처리되어야 한다");
    }

    #[test]
    fn test_document_with_edges_roundtrip() {
        // edges를 가진 문서가 직렬화 후 온전히 복원되어야 한다.
        let mut doc = make_doc();
        let target = Uuid::new_v4();
        doc.add_edge(Edge::new(target, RelationType::Updates, 0.8));

        let json = serde_json::to_string(&doc).unwrap();
        let restored: Document = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.edges.len(), 1);
        assert_eq!(restored.edges[0].target, target);
        assert_eq!(restored.edges[0].relation, RelationType::Updates);
        assert!((restored.edges[0].weight - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn test_new_document_has_empty_edges() {
        assert!(make_doc().edges.is_empty());
    }

    // ──── 출처 메타데이터 (DocumentSource) ────

    fn make_source() -> DocumentSource {
        DocumentSource {
            source_type: "local_directory".to_string(),
            source_id: "/notes/daily/2026-07-06.md".to_string(),
            modified_at: Utc::now(),
            connector_id: "notes".to_string(),
        }
    }

    #[test]
    fn test_new_document_has_no_source() {
        // 수동 입력 문서는 출처가 없다(None).
        assert!(make_doc().source.is_none());
    }

    #[test]
    fn test_with_source_builder() {
        let doc = make_doc().with_source(make_source());
        let source = doc.source.expect("source가 설정되어야 한다");
        assert_eq!(source.source_type, "local_directory");
        assert_eq!(source.source_id, "/notes/daily/2026-07-06.md");
        assert_eq!(source.connector_id, "notes");
    }

    #[test]
    fn test_document_source_roundtrip() {
        // 출처를 가진 문서가 직렬화 후 온전히 복원되어야 한다(reindex 생존 불변식의 raw 측).
        let doc = make_doc().with_source(make_source());
        let json = serde_json::to_string(&doc).unwrap();
        let restored: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.source, doc.source);
    }

    #[test]
    fn test_document_without_source_omits_field() {
        // 출처 없는 문서는 source 필드를 직렬화하지 않는다(하위호환·간결성).
        let doc = make_doc();
        let json = serde_json::to_value(&doc).unwrap();
        assert!(json.get("source").is_none(), "None source는 생략되어야 한다");
    }

    #[test]
    fn test_document_loads_legacy_json_without_source() {
        // source 필드가 없는 기존 문서 JSON도 로드되어야 한다(#[serde(default)]).
        let legacy = r#"{
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "raw_content": "old content",
            "summary": "old summary",
            "entities": [],
            "facts": [],
            "edges": [],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }"#;
        let doc: Document = serde_json::from_str(legacy).unwrap();
        assert!(doc.source.is_none(), "누락된 source는 None으로 기본값 처리되어야 한다");
    }

    // ──── RelationType 직렬화/파싱 ────

    #[test]
    fn test_relation_type_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&RelationType::RelatedTo).unwrap(), "\"related_to\"");
        assert_eq!(serde_json::to_string(&RelationType::PartOf).unwrap(), "\"part_of\"");
        assert_eq!(serde_json::to_string(&RelationType::Contradicts).unwrap(), "\"contradicts\"");
    }

    #[test]
    fn test_relation_type_as_str() {
        assert_eq!(RelationType::RelatedTo.as_str(), "related_to");
        assert_eq!(RelationType::Updates.as_str(), "updates");
        assert_eq!(RelationType::References.as_str(), "references");
    }

    #[test]
    fn test_relation_type_from_str_canonical() {
        assert_eq!(RelationType::from_str("related_to"), Ok(RelationType::RelatedTo));
        assert_eq!(RelationType::from_str("updates"), Ok(RelationType::Updates));
        assert_eq!(RelationType::from_str("contradicts"), Ok(RelationType::Contradicts));
        assert_eq!(RelationType::from_str("references"), Ok(RelationType::References));
        assert_eq!(RelationType::from_str("part_of"), Ok(RelationType::PartOf));
    }

    #[test]
    fn test_relation_type_from_str_lenient() {
        // LLM이 표기를 흔들어도 관대하게 파싱한다(하이픈/대문자/공백/축약).
        assert_eq!(RelationType::from_str("RELATED-TO"), Ok(RelationType::RelatedTo));
        assert_eq!(RelationType::from_str(" Part-Of "), Ok(RelationType::PartOf));
        assert_eq!(RelationType::from_str("reference"), Ok(RelationType::References));
        assert_eq!(RelationType::from_str("update"), Ok(RelationType::Updates));
    }

    #[test]
    fn test_relation_type_from_str_unknown_errors() {
        // 알 수 없는 값은 Err — 호출 측이 기본값으로 폴백할 수 있게 한다.
        assert!(RelationType::from_str("nonsense").is_err());
        assert!(RelationType::from_str("").is_err());
    }

    // ──── Edge 생성/불변식 ────

    #[test]
    fn test_edge_weight_clamped() {
        // 범위를 벗어난 가중치는 [0,1]로 클램프되어야 한다(방어적 설계).
        let t = Uuid::new_v4();
        assert!((Edge::new(t, RelationType::RelatedTo, 1.5).weight - 1.0).abs() < f32::EPSILON);
        assert!((Edge::new(t, RelationType::RelatedTo, -0.3).weight - 0.0).abs() < f32::EPSILON);
        assert!((Edge::new(t, RelationType::RelatedTo, 0.5).weight - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_edge_new_has_no_base_weight() {
        // 생성 시 base는 미고정(None) — 최초 감쇠 시 현재 weight로 고정된다.
        let e = Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.5);
        assert!(e.base_weight.is_none());
    }

    #[test]
    fn test_edge_omits_base_weight_when_none() {
        // 감쇠 전 엣지는 base_weight를 직렬화하지 않는다(기존 JSON 노이즈 방지·하위호환).
        let e = Edge::new(Uuid::new_v4(), RelationType::RelatedTo, 0.5);
        let json = serde_json::to_value(&e).unwrap();
        assert!(json.get("base_weight").is_none(), "None base_weight는 생략되어야 한다");
    }

    #[test]
    fn test_edge_loads_legacy_json_without_base_weight() {
        // base_weight 필드가 없는 Phase 4 이전 엣지 JSON도 로드되어야 한다(#[serde(default)]).
        let legacy = format!(
            r#"{{"target":"{}","relation":"related_to","weight":0.7,"created_at":"2026-01-01T00:00:00Z"}}"#,
            Uuid::new_v4()
        );
        let edge: Edge = serde_json::from_str(&legacy).unwrap();
        assert!(edge.base_weight.is_none(), "누락된 base_weight는 None으로 기본값 처리");
        assert!((edge.weight - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn test_add_edge_upsert_resets_base_weight() {
        // 관계 재확인(upsert)은 base_weight를 리셋한다(새 weight가 다음 감쇠의 기준).
        let mut doc = make_doc();
        let target = Uuid::new_v4();
        let mut first = Edge::new(target, RelationType::RelatedTo, 0.5);
        first.base_weight = Some(0.9); // 이전 감쇠로 base가 고정된 상태를 모사
        doc.add_edge(first);
        doc.add_edge(Edge::new(target, RelationType::RelatedTo, 0.6));
        assert_eq!(doc.edges.len(), 1);
        assert!(doc.edges[0].base_weight.is_none(), "재확인 시 base는 리셋되어야 한다");
        assert!((doc.edges[0].weight - 0.6).abs() < f32::EPSILON);
    }

    // ──── add_edge / remove_edge ────

    #[test]
    fn test_add_edge_dedups_same_target_and_relation() {
        // 같은 (target, relation)은 중복 추가되지 않고 갱신된다.
        let mut doc = make_doc();
        let target = Uuid::new_v4();
        doc.add_edge(Edge::new(target, RelationType::RelatedTo, 0.5));
        doc.add_edge(Edge::new(target, RelationType::RelatedTo, 0.9));

        assert_eq!(doc.edges.len(), 1, "동일 관계는 하나로 유지되어야 한다");
        assert!((doc.edges[0].weight - 0.9).abs() < f32::EPSILON, "가중치가 갱신되어야 한다");
    }

    #[test]
    fn test_add_edge_different_relation_coexists() {
        // 같은 target이라도 relation이 다르면 별개 엣지로 공존한다.
        let mut doc = make_doc();
        let target = Uuid::new_v4();
        doc.add_edge(Edge::new(target, RelationType::RelatedTo, 0.5));
        doc.add_edge(Edge::new(target, RelationType::References, 0.5));

        assert_eq!(doc.edges.len(), 2);
    }

    #[test]
    fn test_add_edge_ignores_self_loop() {
        // 자기 자신으로의 엣지는 무시된다(순환 1차 방어).
        let mut doc = make_doc();
        let self_id = doc.id;
        doc.add_edge(Edge::new(self_id, RelationType::RelatedTo, 1.0));
        assert!(doc.edges.is_empty(), "자기 참조 엣지는 추가되면 안 된다");
    }

    #[test]
    fn test_remove_edge() {
        let mut doc = make_doc();
        let target_a = Uuid::new_v4();
        let target_b = Uuid::new_v4();
        doc.add_edge(Edge::new(target_a, RelationType::RelatedTo, 0.5));
        doc.add_edge(Edge::new(target_a, RelationType::References, 0.5));
        doc.add_edge(Edge::new(target_b, RelationType::RelatedTo, 0.5));

        // target_a로 향하는 두 엣지가 모두 제거된다.
        let removed = doc.remove_edge(target_a);
        assert_eq!(removed, 2);
        assert_eq!(doc.edges.len(), 1);
        assert_eq!(doc.edges[0].target, target_b);
    }

    #[test]
    fn test_remove_edge_nonexistent_returns_zero() {
        let mut doc = make_doc();
        assert_eq!(doc.remove_edge(Uuid::new_v4()), 0);
    }

    // ──── IngestOutcome (전략 메타데이터 응답) ────

    #[test]
    fn test_ingest_outcome_from_response() {
        let resp = api::IngestResponse {
            id: Uuid::new_v4(),
            summary: "s".to_string(),
            entities: vec![],
            facts: vec!["f".to_string()],
        };
        let id = resp.id;
        let outcome = api::IngestOutcome::from_response(resp, "new", vec![id], 3, false, "판단 근거");

        assert_eq!(outcome.id, id);
        assert_eq!(outcome.summary, "s");
        assert_eq!(outcome.facts, vec!["f"]);
        assert_eq!(outcome.strategy, "new");
        assert_eq!(outcome.document_ids, vec![id]);
        assert_eq!(outcome.edges_created, 3);
        assert!(!outcome.fallback);
        assert_eq!(outcome.reason, "판단 근거");
    }

    // ──── SearchRequest agent opt-in (기본 동작 불변) ────

    #[test]
    fn test_search_request_agent_defaults_none() {
        // agent 미지정 → None. 핸들러가 기존 단일 검색 경로를 타는 근거(기본 동작 불변).
        let req: api::SearchRequest = serde_json::from_str(r#"{"query":"hi"}"#).unwrap();
        assert_eq!(req.agent, None, "agent 미지정 시 None이어야 한다");
        assert_eq!(req.limit, 10, "limit 기본값 유지");
        assert_eq!(req.offset, 0);
        assert!(req.mode.is_none());
    }

    #[test]
    fn test_search_request_agent_opt_in() {
        let req: api::SearchRequest = serde_json::from_str(r#"{"query":"hi","agent":true}"#).unwrap();
        assert_eq!(req.agent, Some(true), "agent:true로 opt-in");
    }

    #[test]
    fn test_search_request_agent_false() {
        let req: api::SearchRequest =
            serde_json::from_str(r#"{"query":"hi","agent":false}"#).unwrap();
        assert_eq!(req.agent, Some(false));
    }

    // ──── SearchResponse/SearchResult 하위호환 직렬화 ────

    #[test]
    fn test_search_response_omits_agent_when_none() {
        // 기존(비 agent) 검색은 agent 메타데이터를 직렬화하지 않는다(하위호환).
        let resp = api::SearchResponse {
            results: vec![],
            sources_used: vec![],
            total: 0,
            mode: "hybrid".to_string(),
            agent: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("agent").is_none(), "agent None은 생략되어야 한다");
    }

    #[test]
    fn test_search_response_includes_agent_when_present() {
        let resp = api::SearchResponse {
            results: vec![],
            sources_used: vec![],
            total: 0,
            mode: "agent".to_string(),
            agent: Some(api::AgentSearchMeta {
                rounds: 2,
                queries: vec!["a".to_string(), "b".to_string()],
                graph_expanded: true,
                expansion_count: 1,
                fallback: false,
                reason: "충분".to_string(),
            }),
        };
        let json = serde_json::to_value(&resp).unwrap();
        let agent = json.get("agent").expect("agent 메타데이터가 포함되어야 한다");
        assert_eq!(agent.get("rounds").unwrap(), 2);
        assert_eq!(agent.get("graph_expanded").unwrap(), true);
    }

    #[test]
    fn test_search_result_omits_expanded_from_when_none() {
        // 직접 검색 결과는 expanded_from을 직렬화하지 않는다(하위호환).
        let r = api::SearchResult {
            id: Uuid::new_v4(),
            summary: "s".to_string(),
            relevance_score: 0.5,
            workspace: "default".to_string(),
            matched_facts: vec![],
            created_at: None,
            expanded_from: None,
        };
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.get("expanded_from").is_none(), "None은 생략되어야 한다");
    }

    #[test]
    fn test_search_result_includes_expanded_from_when_present() {
        let origin = Uuid::new_v4();
        let r = api::SearchResult {
            id: Uuid::new_v4(),
            summary: "s".to_string(),
            relevance_score: 0.5,
            workspace: "default".to_string(),
            matched_facts: vec![],
            created_at: None,
            expanded_from: Some(origin),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(
            json.get("expanded_from").unwrap().as_str().unwrap(),
            origin.to_string(),
            "확장 유래가 직렬화되어야 한다"
        );
    }

    #[test]
    fn test_ingest_outcome_serialize_backward_compatible() {
        // 직렬화에 기존 IngestResponse 필드가 모두 포함되어야 한다(하위호환).
        let resp = api::IngestResponse {
            id: Uuid::new_v4(),
            summary: "sum".to_string(),
            entities: vec![],
            facts: vec![],
        };
        let outcome = api::IngestOutcome::from_response(resp, "raw", vec![], 0, true, "폴백");
        let json = serde_json::to_value(&outcome).unwrap();

        // 기존 클라이언트가 읽던 필드
        assert!(json.get("id").is_some());
        assert!(json.get("summary").is_some());
        assert!(json.get("entities").is_some());
        assert!(json.get("facts").is_some());
        // 새 메타데이터
        assert_eq!(json.get("strategy").unwrap(), "raw");
        assert_eq!(json.get("fallback").unwrap(), true);
    }
}
