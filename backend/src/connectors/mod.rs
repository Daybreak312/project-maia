//! 커넥터 프레임워크 — 외부 소스의 정보를 Maia로 주기적으로 유입시키는 유입 파이프라인.
//!
//! 구조:
//! - [`Connector`] trait: 증분 변경분 조회 + 소스 타입 제공 (새 타입 추가의 확장점).
//! - [`ConnectorIngest`] trait: 유입 실행 추상화 — `Indexer`가 구현하고, 테스트는 mock
//!   으로 주입해 Qdrant·LLM 없이 러너/스케줄러를 검증한다.
//! - 설정 스키마([`ConnectorInstance`]/[`ConnectorSpec`]): 워크스페이스 모듈이 소유하고
//!   (`crate::workspace`) WorkspaceConfig에 저장된다. 여기서는 런타임만 다룬다.
//! - [`local_dir`]: 로컬 디렉토리 커넥터(레퍼런스 구현).
//! - [`sync_state`]: 커넥터별 마지막 실행 시각·커서·결과 요약 영속화.
//! - [`runner`]: 동기화/대량 적재 오케스트레이션(동시성 제한·진행 관측·실패 격리·재개).
//! - [`scheduler`]: 주기 실행 + 오류 격리.

pub mod local_dir;
pub mod runner;
pub mod scheduler;
pub mod sync_state;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::models::DocumentSource;
use crate::workspace::{ConnectorInstance, ConnectorSpec};
use local_dir::LocalDirectoryConnector;

/// 커넥터가 유입시키는 하나의 항목 (파일 하나 등).
///
/// 소스 식별자(`source_id`)는 재유입 중복 방지의 키이며, `modified_at`은 증분 커서와
/// "변경 없으면 스킵" 판단의 기준이다. 이 둘은 유입된 문서의 `DocumentSource`로 각인된다.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectorItem {
    /// 소스 내 고유 식별자 (원본 파일 경로 등).
    pub source_id: String,
    /// 유입될 원문 텍스트.
    pub content: String,
    /// 원본의 수정 시각.
    pub modified_at: DateTime<Utc>,
}

impl ConnectorItem {
    /// 이 항목이 특정 커넥터로부터 유입될 때 문서에 각인될 출처 메타데이터를 만든다.
    pub fn to_source(&self, source_type: &str, connector_id: &str) -> DocumentSource {
        DocumentSource {
            source_type: source_type.to_string(),
            source_id: self.source_id.clone(),
            modified_at: self.modified_at,
            connector_id: connector_id.to_string(),
        }
    }
}

/// 커넥터의 증분 조회 결과 — 변경 항목 + 다음 커서.
///
/// 커서 의미는 커넥터가 온전히 소유한다(로컬 디렉토리는 스캔 시각의 RFC3339, 향후
/// 페이지 토큰 기반 커넥터는 불투명 토큰 등). 러너는 커서를 해석하지 않고 보관만 한다.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchResult {
    /// 커서 이후 변경된 항목들.
    pub items: Vec<ConnectorItem>,
    /// 다음 동기화에 넘길 커서. 이번 조회가 관측한 지점을 표현한다.
    pub next_cursor: Option<String>,
}

/// 커넥터 공통 계약 — 증분 변경분을 조회하고 소스 타입을 제공한다.
///
/// 새 커넥터 타입은 이 trait을 구현하고 [`build_connector`]에 한 줄을 추가하면 된다.
/// 러너/스케줄러/상태 저장은 이 trait에만 의존하므로 신규 타입 추가가 기존 로직을
/// 건드리지 않는다(개방-폐쇄).
#[async_trait]
pub trait Connector: Send + Sync {
    /// 소스 타입 식별자 (Document.source.source_type과 일치). 예: "local_directory".
    fn source_type(&self) -> &str;

    /// `cursor` 이후 변경된 항목을 조회한다. `cursor`가 None이면 전체(초기 적재).
    /// 다음 커서를 [`FetchResult::next_cursor`]로 함께 반환한다.
    async fn fetch_changes(&self, cursor: Option<&str>) -> Result<FetchResult>;
}

/// 설정으로부터 커넥터 인스턴스를 만드는 팩토리 — 타입 분기가 일어나는 유일한 지점.
///
/// 새 커넥터 타입은 여기 match 팔을 하나 추가한다. 이 함수를 제외한 나머지 파이프라인은
/// [`Connector`] trait만 알면 되므로 확장이 격리된다.
pub fn build_connector(instance: &ConnectorInstance) -> Result<Box<dyn Connector>> {
    match &instance.spec {
        ConnectorSpec::LocalDirectory(cfg) => {
            Ok(Box::new(LocalDirectoryConnector::new(cfg.clone())))
        }
    }
}

/// 유입 실행의 모드.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorIngestMode {
    /// LLM 파싱(요약/팩트/엔티티) + 그래프 자동 연결. 품질 우선.
    Parsed,
    /// LLM 없이 원문을 그대로 보관(폴백 요약). rate limit 보호·대량 적재 우선.
    Raw,
}

impl ConnectorIngestMode {
    /// API 문자열("parsed"/"raw")에서 파싱한다. 알 수 없으면 기본(Parsed).
    pub fn from_str_or_default(s: Option<&str>) -> Self {
        match s {
            Some("raw") => ConnectorIngestMode::Raw,
            _ => ConnectorIngestMode::Parsed,
        }
    }
}

/// 한 항목 유입의 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOutcome {
    /// 신규 문서로 저장됨.
    Created(Uuid),
    /// 기존(동일 소스) 문서가 갱신됨.
    Updated(Uuid),
    /// 변경 없음 — 재유입하지 않고 건너뜀.
    Skipped,
}

/// 유입 실행 추상화 — 소스 항목 하나를 유입한다.
///
/// `Indexer`가 구현한다(실제 파싱·임베딩·저장). 러너/스케줄러는 이 trait에만 의존하므로
/// 단위 테스트에서 mock으로 대체해 Qdrant·LLM 없이 오케스트레이션 로직(동시성·실패 격리·
/// 진행 관측·재개)을 검증할 수 있다.
///
/// **중복 방지 계약:** 구현체는 `(source_type, item.source_id)`가 일치하는 기존 문서가
/// 있으면 신규 생성이 아니라 업데이트로 처리하고, 원본이 더 새롭지 않으면 `Skipped`를
/// 반환한다(재유입 안전성).
#[async_trait]
pub trait ConnectorIngest: Send + Sync {
    async fn ingest_item(
        &self,
        workspace_id: &str,
        source_type: &str,
        connector_id: &str,
        item: ConnectorItem,
        mode: ConnectorIngestMode,
    ) -> Result<ItemOutcome>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn item(source_id: &str) -> ConnectorItem {
        ConnectorItem {
            source_id: source_id.to_string(),
            content: "content".to_string(),
            modified_at: Utc.with_ymd_and_hms(2026, 7, 6, 12, 0, 0).unwrap(),
        }
    }

    #[test]
    fn test_item_to_source() {
        let src = item("/notes/a.md").to_source("local_directory", "notes");
        assert_eq!(src.source_type, "local_directory");
        assert_eq!(src.source_id, "/notes/a.md");
        assert_eq!(src.connector_id, "notes");
    }

    #[test]
    fn test_ingest_mode_from_str() {
        assert_eq!(ConnectorIngestMode::from_str_or_default(Some("raw")), ConnectorIngestMode::Raw);
        assert_eq!(
            ConnectorIngestMode::from_str_or_default(Some("parsed")),
            ConnectorIngestMode::Parsed
        );
        // 미지정/알 수 없음 → Parsed 기본
        assert_eq!(ConnectorIngestMode::from_str_or_default(None), ConnectorIngestMode::Parsed);
        assert_eq!(
            ConnectorIngestMode::from_str_or_default(Some("bogus")),
            ConnectorIngestMode::Parsed
        );
    }

    #[test]
    fn test_build_connector_local_directory() {
        use crate::workspace::{ConnectorInstance, ConnectorSpec, LocalDirectoryConfig};
        let inst = ConnectorInstance {
            id: "notes".to_string(),
            enabled: true,
            interval_secs: 3600,
            concurrency: 4,
            spec: ConnectorSpec::LocalDirectory(LocalDirectoryConfig {
                directories: vec!["/notes".to_string()],
                extensions: vec!["md".to_string()],
                exclude: vec![],
                max_file_bytes: 1024,
            }),
        };
        let connector = build_connector(&inst).unwrap();
        assert_eq!(connector.source_type(), "local_directory");
    }
}
