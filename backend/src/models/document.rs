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
            created_at: now,
            updated_at: now,
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
