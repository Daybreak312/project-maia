//! 커넥터 인스턴스 설정 — 워크스페이스 설정(`WorkspaceConfig.connectors`)에 저장된다.
//!
//! 설계 원칙:
//! - **개방-폐쇄**: 새 커넥터 타입은 `ConnectorSpec`에 variant를 추가하고 `Connector`
//!   trait을 구현하면 된다. 공통 필드(id/enabled/interval/concurrency)는 인스턴스가
//!   소유하고, 타입별 설정은 `spec`에 격리된다.
//! - **하위호환**: `#[serde(default)]`로 필드가 없는 기존 config.json도 로드된다.

use serde::{Deserialize, Serialize};

/// 동시성 기본값 — LLM rate limit을 감안한 보수적 병렬도.
pub const DEFAULT_CONCURRENCY: usize = 4;

/// 주기 기본값(초) — 1시간. 데일리 노트/리포트 유입에 충분히 잦고 rate limit에 안전.
pub const DEFAULT_INTERVAL_SECS: u64 = 3_600;

/// 로컬 디렉토리 커넥터의 파일 크기 상한 기본값(바이트) — 1 MiB.
/// 초과 파일은 스킵하고 기록한다(대용량 파일 보호).
pub const DEFAULT_MAX_FILE_BYTES: u64 = 1_048_576;

/// 하나의 커넥터 인스턴스. 워크스페이스에 여러 개 등록될 수 있다.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectorInstance {
    /// 워크스페이스 내 고유 인스턴스 ID (동기화 상태 파일명·API 경로에 쓰임).
    pub id: String,
    /// 스케줄러가 이 커넥터를 자동 실행할지 여부. false면 수동 트리거만 가능.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 자동 실행 주기(초). 마지막 실행 + 이 값 <= now이면 스케줄러가 실행한다.
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    /// 유입 시 동시 처리 수 (LLM rate limit 보호). 미지정 시 기본값.
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// 타입별 설정 (커넥터 종류를 결정하는 확장점).
    pub spec: ConnectorSpec,
}

fn default_enabled() -> bool {
    true
}
fn default_interval_secs() -> u64 {
    DEFAULT_INTERVAL_SECS
}
fn default_concurrency() -> usize {
    DEFAULT_CONCURRENCY
}

impl ConnectorInstance {
    /// 커넥터가 유입시키는 소스의 타입 식별자 (Document.source.source_type과 일치).
    pub fn source_type(&self) -> &'static str {
        self.spec.source_type()
    }

    /// 인스턴스 설정 유효성 검사. 등록 시점의 방어선.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("Connector id cannot be empty".to_string());
        }
        if self.id.len() > 64 {
            return Err("Connector id too long (max 64 chars)".to_string());
        }
        if !self
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(
                "Connector id can only contain alphanumerics, hyphens, and underscores".to_string(),
            );
        }
        if self.interval_secs == 0 {
            return Err("interval_secs must be greater than 0".to_string());
        }
        if self.concurrency == 0 {
            return Err("concurrency must be greater than 0".to_string());
        }
        self.spec.validate()
    }
}

/// 커넥터 타입별 설정. 새 커넥터 타입 추가의 확장점.
///
/// `#[serde(tag = "type")]`로 `{"type": "local_directory", ...}` 형태로 직렬화된다.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectorSpec {
    /// 로컬 디렉토리(마크다운/텍스트) 커넥터 — 레퍼런스 구현.
    LocalDirectory(LocalDirectoryConfig),
}

impl ConnectorSpec {
    pub fn source_type(&self) -> &'static str {
        match self {
            ConnectorSpec::LocalDirectory(_) => "local_directory",
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            ConnectorSpec::LocalDirectory(cfg) => cfg.validate(),
        }
    }
}

/// 로컬 디렉토리 커넥터 설정.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocalDirectoryConfig {
    /// 대상 디렉토리(복수 가능). 명시 등록된 경로만 스캔한다(민감 경로 방어).
    pub directories: Vec<String>,
    /// 포함할 확장자 (점 없이, 소문자). 예: ["md", "txt"].
    #[serde(default = "default_extensions")]
    pub extensions: Vec<String>,
    /// 제외할 glob 패턴. 예: ["**/node_modules/**", "*.tmp"].
    #[serde(default)]
    pub exclude: Vec<String>,
    /// 파일 크기 상한(바이트). 초과 파일은 스킵하고 기록한다.
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: u64,
}

fn default_extensions() -> Vec<String> {
    vec!["md".to_string(), "markdown".to_string(), "txt".to_string()]
}
fn default_max_file_bytes() -> u64 {
    DEFAULT_MAX_FILE_BYTES
}

impl LocalDirectoryConfig {
    fn validate(&self) -> Result<(), String> {
        if self.directories.is_empty() {
            return Err("local_directory connector requires at least one directory".to_string());
        }
        if self.directories.iter().any(|d| d.trim().is_empty()) {
            return Err("directory paths cannot be empty".to_string());
        }
        if self.max_file_bytes == 0 {
            return Err("max_file_bytes must be greater than 0".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_dir_instance() -> ConnectorInstance {
        ConnectorInstance {
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
        }
    }

    #[test]
    fn test_instance_roundtrip() {
        let inst = local_dir_instance();
        let json = serde_json::to_string(&inst).unwrap();
        let back: ConnectorInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(inst, back);
    }

    #[test]
    fn test_spec_serializes_with_type_tag() {
        let inst = local_dir_instance();
        let json = serde_json::to_value(&inst).unwrap();
        assert_eq!(json["spec"]["type"], "local_directory");
        assert_eq!(json["spec"]["directories"][0], "/notes");
    }

    #[test]
    fn test_source_type() {
        assert_eq!(local_dir_instance().source_type(), "local_directory");
    }

    #[test]
    fn test_deserialize_applies_defaults() {
        // enabled/interval/concurrency/extensions/exclude/max_file_bytes 누락 시 기본값.
        let json = r#"{
            "id": "min",
            "spec": { "type": "local_directory", "directories": ["/a"] }
        }"#;
        let inst: ConnectorInstance = serde_json::from_str(json).unwrap();
        assert!(inst.enabled, "enabled 기본값 true");
        assert_eq!(inst.interval_secs, DEFAULT_INTERVAL_SECS);
        assert_eq!(inst.concurrency, DEFAULT_CONCURRENCY);
        // 현재 단일 variant라 irrefutable — 변형 추가 시 컴파일러가 match를 강제한다.
        let ConnectorSpec::LocalDirectory(cfg) = &inst.spec;
        assert!(cfg.extensions.contains(&"md".to_string()), "확장자 기본값에 md 포함");
        assert!(cfg.exclude.is_empty());
        assert_eq!(cfg.max_file_bytes, DEFAULT_MAX_FILE_BYTES);
    }

    #[test]
    fn test_validate_ok() {
        assert!(local_dir_instance().validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_empty_id() {
        let mut inst = local_dir_instance();
        inst.id = "".to_string();
        assert!(inst.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_bad_id_chars() {
        let mut inst = local_dir_instance();
        inst.id = "bad id/slash".to_string();
        assert!(inst.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_zero_interval() {
        let mut inst = local_dir_instance();
        inst.interval_secs = 0;
        assert!(inst.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_zero_concurrency() {
        let mut inst = local_dir_instance();
        inst.concurrency = 0;
        assert!(inst.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_no_directories() {
        let mut inst = local_dir_instance();
        inst.spec = ConnectorSpec::LocalDirectory(LocalDirectoryConfig {
            directories: vec![],
            extensions: vec!["md".to_string()],
            exclude: vec![],
            max_file_bytes: 1024,
        });
        assert!(inst.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_zero_max_bytes() {
        let mut inst = local_dir_instance();
        inst.spec = ConnectorSpec::LocalDirectory(LocalDirectoryConfig {
            directories: vec!["/a".to_string()],
            extensions: vec!["md".to_string()],
            exclude: vec![],
            max_file_bytes: 0,
        });
        assert!(inst.validate().is_err());
    }
}
