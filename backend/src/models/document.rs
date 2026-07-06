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
            created_at: now,
            updated_at: now,
        }
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
            created_at: Utc::now(),
        }
    }
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
    }

    #[derive(Debug, Serialize)]
    pub struct DocumentResponse {
        pub id: Uuid,
        pub raw_content: String,
        pub summary: String,
        pub entities: Vec<Entity>,
        pub created_at: DateTime<Utc>,
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
