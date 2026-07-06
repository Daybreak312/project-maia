use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 워크스페이스 설정 — 각 워크스페이스의 동작을 정의하는 핵심 구조체.
/// 파일 저장: `data/workspaces/{id}/config.json`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub template: WorkspaceTemplate,
    pub patrol: PatrolConfig,
    pub parsing: ParsingConfig,
    pub search: SearchConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceTemplate {
    Personal,
    Enterprise,
}

/// Patrol 에이전트 설정 (향후 구현용 — 구조만 선점)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatrolConfig {
    /// 순회 주기: "daily", "weekly", 또는 cron 표현식
    pub frequency: String,
    /// 엄격도 (0.0 = 느슨, 1.0 = 엄격)
    pub strictness: f32,
}

/// 파싱 설정 — LLM 파싱 동작 커스터마이징
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsingConfig {
    /// 엔티티 추출 우선순위
    pub entity_priorities: Vec<String>,
    /// 팩트 추출 깊이: "shallow" | "deep"
    pub fact_depth: String,
    /// 파싱 LLM 프로바이더 오버라이드 (None이면 글로벌 설정 사용)
    pub llm_provider: Option<String>,
}

/// 검색 설정 — 워크스페이스별 검색 동작 커스터마이징
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// 시간 감쇠 강도 (0.0이면 감쇠 없음)
    pub time_decay_lambda: f32,
    /// 기본 검색 모드: "hybrid" | "vector" | "keyword"
    pub default_mode: String,
    /// 교차 검색 허용 워크스페이스 ID 목록
    pub cross_workspace: Vec<String>,
    /// agent(deep) 검색의 그래프 이웃 확장 깊이 (Phase 3).
    /// 0이면 확장 비활성, [1, MAX_NEIGHBOR_DEPTH]로 클램프된다.
    /// `#[serde(default)]`로 이 필드가 없는 기존 config.json도 로드된다(하위호환).
    #[serde(default = "default_graph_expansion_depth")]
    pub graph_expansion_depth: usize,
    /// agent(deep) 검색 전체 파이프라인 시간 상한(ms, Phase 3).
    /// 초과 시 그 시점까지의 결과를 반환한다. 0이면 재작성 루프를 돌지 않는다(초기 결과만).
    #[serde(default = "default_deep_search_time_limit_ms")]
    pub deep_search_time_limit_ms: u64,
}

/// agent 검색 그래프 확장 깊이 기본값 — 1-hop 이웃(가장 관련 높은 직접 연결).
fn default_graph_expansion_depth() -> usize {
    1
}

/// agent 검색 시간 상한 기본값(ms) — 다중 라운드 LLM 호출을 감안한 보수적 상한.
fn default_deep_search_time_limit_ms() -> u64 {
    15_000
}

impl WorkspaceConfig {
    /// 템플릿 기반 워크스페이스 생성
    pub fn new(id: String, name: String, template: WorkspaceTemplate) -> Self {
        let (patrol, parsing, search) = match &template {
            WorkspaceTemplate::Personal => Self::personal_preset(),
            WorkspaceTemplate::Enterprise => Self::enterprise_preset(),
        };

        Self {
            id,
            name,
            created_at: Utc::now(),
            template,
            patrol,
            parsing,
            search,
        }
    }

    /// default 워크스페이스 생성 (서버 최초 기동 시)
    pub fn default_workspace() -> Self {
        Self::new(
            "default".to_string(),
            "Default".to_string(),
            WorkspaceTemplate::Personal,
        )
    }

    fn personal_preset() -> (PatrolConfig, ParsingConfig, SearchConfig) {
        (
            PatrolConfig {
                frequency: "weekly".to_string(),
                strictness: 0.3,
            },
            ParsingConfig {
                entity_priorities: vec![
                    "person".to_string(),
                    "company".to_string(),
                    "skill".to_string(),
                ],
                fact_depth: "deep".to_string(),
                llm_provider: None,
            },
            SearchConfig {
                time_decay_lambda: 0.01,
                default_mode: "hybrid".to_string(),
                cross_workspace: vec![],
                graph_expansion_depth: default_graph_expansion_depth(),
                deep_search_time_limit_ms: default_deep_search_time_limit_ms(),
            },
        )
    }

    fn enterprise_preset() -> (PatrolConfig, ParsingConfig, SearchConfig) {
        (
            PatrolConfig {
                frequency: "daily".to_string(),
                strictness: 0.7,
            },
            ParsingConfig {
                entity_priorities: vec![
                    "service".to_string(),
                    "team".to_string(),
                    "api".to_string(),
                ],
                fact_depth: "shallow".to_string(),
                llm_provider: None,
            },
            SearchConfig {
                time_decay_lambda: 0.005,
                default_mode: "hybrid".to_string(),
                cross_workspace: vec![],
                // enterprise는 문서 간 관계 탐색 수요가 커 확장을 2-hop으로 넓힌다.
                graph_expansion_depth: 2,
                deep_search_time_limit_ms: default_deep_search_time_limit_ms(),
            },
        )
    }
}

/// 워크스페이스 ID 유효성 검사
pub fn validate_workspace_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("Workspace ID cannot be empty".to_string());
    }
    if id.len() > 64 {
        return Err("Workspace ID too long (max 64 chars)".to_string());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "Workspace ID can only contain alphanumeric characters, hyphens, and underscores"
                .to_string(),
        );
    }
    if id.starts_with('-') || id.starts_with('_') {
        return Err("Workspace ID cannot start with a hyphen or underscore".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ──── WorkspaceConfig 직렬화/역직렬화 ────

    #[test]
    fn test_config_serialize_roundtrip() {
        let config = WorkspaceConfig::new(
            "test-ws".to_string(),
            "Test Workspace".to_string(),
            WorkspaceTemplate::Personal,
        );

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: WorkspaceConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, "test-ws");
        assert_eq!(deserialized.name, "Test Workspace");
        assert_eq!(deserialized.template, WorkspaceTemplate::Personal);
    }

    #[test]
    fn test_config_deserialize_from_json() {
        let json = r#"{
            "id": "my-ws",
            "name": "My WS",
            "created_at": "2026-04-01T00:00:00Z",
            "template": "enterprise",
            "patrol": {"frequency": "daily", "strictness": 0.5},
            "parsing": {"entity_priorities": ["team"], "fact_depth": "shallow", "llm_provider": null},
            "search": {"time_decay_lambda": 0.01, "default_mode": "hybrid", "cross_workspace": []}
        }"#;

        let config: WorkspaceConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.id, "my-ws");
        assert_eq!(config.template, WorkspaceTemplate::Enterprise);
        assert_eq!(config.patrol.frequency, "daily");
        assert!((config.patrol.strictness - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_config_deserialize_legacy_search_without_phase3_fields() {
        // Phase 3 이전에 저장된 config.json(search에 graph_expansion_depth/
        // deep_search_time_limit_ms 없음)도 serde default로 로드되어야 한다(하위호환).
        // 이 불변식이 깨지면 기동 시 기존 워크스페이스 설정 파싱이 실패해 브릭된다.
        let json = r#"{
            "id": "legacy",
            "name": "Legacy",
            "created_at": "2026-04-01T00:00:00Z",
            "template": "personal",
            "patrol": {"frequency": "weekly", "strictness": 0.3},
            "parsing": {"entity_priorities": [], "fact_depth": "deep", "llm_provider": null},
            "search": {"time_decay_lambda": 0.01, "default_mode": "hybrid", "cross_workspace": []}
        }"#;

        let config: WorkspaceConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.search.graph_expansion_depth, 1, "누락 필드는 기본값 1");
        assert_eq!(config.search.deep_search_time_limit_ms, 15_000, "누락 필드는 기본값 15초");
    }

    #[test]
    fn test_config_serialize_template_lowercase() {
        let personal = serde_json::to_string(&WorkspaceTemplate::Personal).unwrap();
        assert_eq!(personal, "\"personal\"");

        let enterprise = serde_json::to_string(&WorkspaceTemplate::Enterprise).unwrap();
        assert_eq!(enterprise, "\"enterprise\"");
    }

    // ──── 템플릿 프리셋 기본값 검증 ────

    #[test]
    fn test_personal_template_defaults() {
        let config = WorkspaceConfig::new(
            "p".to_string(),
            "Personal".to_string(),
            WorkspaceTemplate::Personal,
        );

        assert_eq!(config.patrol.frequency, "weekly");
        assert!((config.patrol.strictness - 0.3).abs() < f32::EPSILON);
        assert_eq!(config.parsing.fact_depth, "deep");
        assert!(config
            .parsing
            .entity_priorities
            .contains(&"person".to_string()));
        assert!(config.parsing.llm_provider.is_none());
        assert_eq!(config.search.default_mode, "hybrid");
        assert!(config.search.cross_workspace.is_empty());
        assert_eq!(config.search.graph_expansion_depth, 1);
        assert_eq!(config.search.deep_search_time_limit_ms, 15_000);
    }

    #[test]
    fn test_enterprise_template_defaults() {
        let config = WorkspaceConfig::new(
            "e".to_string(),
            "Enterprise".to_string(),
            WorkspaceTemplate::Enterprise,
        );

        assert_eq!(config.patrol.frequency, "daily");
        assert!((config.patrol.strictness - 0.7).abs() < f32::EPSILON);
        assert_eq!(config.parsing.fact_depth, "shallow");
        assert!(config
            .parsing
            .entity_priorities
            .contains(&"service".to_string()));
        assert!((config.search.time_decay_lambda - 0.005).abs() < f32::EPSILON);
        // enterprise는 관계 탐색 수요가 커 확장 깊이가 personal(1)보다 깊다.
        assert_eq!(config.search.graph_expansion_depth, 2);
    }

    #[test]
    fn test_default_workspace() {
        let config = WorkspaceConfig::default_workspace();

        assert_eq!(config.id, "default");
        assert_eq!(config.name, "Default");
        assert_eq!(config.template, WorkspaceTemplate::Personal);
    }

    // ──── ID 유효성 검사 ────

    #[test]
    fn test_validate_id_valid() {
        assert!(validate_workspace_id("default").is_ok());
        assert!(validate_workspace_id("my-workspace").is_ok());
        assert!(validate_workspace_id("work_space_1").is_ok());
        assert!(validate_workspace_id("a").is_ok());
        assert!(validate_workspace_id("ABC123").is_ok());
    }

    #[test]
    fn test_validate_id_empty() {
        let err = validate_workspace_id("").unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn test_validate_id_too_long() {
        let long_id = "a".repeat(65);
        let err = validate_workspace_id(&long_id).unwrap_err();
        assert!(err.contains("too long"));
    }

    #[test]
    fn test_validate_id_invalid_chars() {
        assert!(validate_workspace_id("my workspace").is_err());
        assert!(validate_workspace_id("ws/path").is_err());
        assert!(validate_workspace_id("ws.dot").is_err());
        assert!(validate_workspace_id("ws@at").is_err());
        assert!(validate_workspace_id("한글").is_err());
    }

    #[test]
    fn test_validate_id_leading_special() {
        assert!(validate_workspace_id("-leading").is_err());
        assert!(validate_workspace_id("_leading").is_err());
    }

    #[test]
    fn test_validate_id_max_length() {
        let max_id = "a".repeat(64);
        assert!(validate_workspace_id(&max_id).is_ok());
    }

    // ──── 엣지 케이스 ────

    #[test]
    fn test_config_created_at_is_recent() {
        let before = Utc::now();
        let config = WorkspaceConfig::default_workspace();
        let after = Utc::now();

        assert!(config.created_at >= before);
        assert!(config.created_at <= after);
    }

    #[test]
    fn test_config_with_custom_parsing() {
        let mut config = WorkspaceConfig::new(
            "custom".to_string(),
            "Custom".to_string(),
            WorkspaceTemplate::Personal,
        );

        config.parsing.llm_provider = Some("claude".to_string());
        config.parsing.entity_priorities = vec!["emotion".to_string()];

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: WorkspaceConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(
            deserialized.parsing.llm_provider,
            Some("claude".to_string())
        );
        assert_eq!(
            deserialized.parsing.entity_priorities,
            vec!["emotion".to_string()]
        );
    }
}
