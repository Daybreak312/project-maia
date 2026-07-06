//! 검색 로그 저장소 — 모든 검색(기존·agent)을 워크스페이스별로 파일에 축적한다.
//!
//! Phase 5(거버넌스)의 신호원: 소유자가 나중에 zero-result 쿼리들을 보고 지식
//! 공백을 파악하고, 검색 품질 추이를 관측한다. 이 phase에서는 **축적만** 한다.
//!
//! 설계 원칙:
//! - **일 단위 파일 분리**: `search_logs/{YYYY-MM-DD}.jsonl`. 무한 성장을 막고 롤업이
//!   쉬운 구조(한 줄 = 한 레코드, append-only JSONL).
//! - **실패 무해성**: 로그 기록 실패가 검색을 실패시키지 않는다. append는 `Result`를
//!   반환하되, 호출 측은 `append_best_effort`로 실패를 삼킨다(침묵하지 않고 warn).
//! - **순수 파생**: 결과에서 로그 지표(결과 수·최고 점수·zero-result)를 계산하는
//!   로직은 순수 함수로 분리해 mock 없이 검증한다.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::models::api::SearchResult;

/// 검색 한 건의 로그 레코드 (JSONL 한 줄).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchLogRecord {
    /// 검색 시각.
    pub timestamp: DateTime<Utc>,
    /// 검색이 수행된(primary) 워크스페이스.
    pub workspace: String,
    /// 원 질의.
    pub query: String,
    /// 검색 모드 ("hybrid"/"vector"/"keyword"/"agent").
    pub mode: String,
    /// 반환된 결과 수.
    pub result_count: usize,
    /// 최고 관련도 점수 (결과 없으면 0.0).
    pub top_score: f32,
    /// 결과가 0건이었는지 여부 (지식 공백 신호).
    pub zero_result: bool,
    /// 소요 시간(ms).
    pub duration_ms: u64,
    /// agent 모드의 검색 라운드 수 (기존 모드는 None → 직렬화 생략).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rounds: Option<usize>,
}

/// 결과 목록에서 로그 파생 지표를 계산한다 (순수 함수): (결과 수, 최고 점수, zero-result).
///
/// 최고 점수는 결과가 비어 있으면 0.0이고, zero_result는 결과 수가 0일 때 true다.
pub fn derive_metrics(results: &[SearchResult]) -> (usize, f32, bool) {
    let count = results.len();
    let top = results
        .iter()
        .map(|r| r.relevance_score)
        .fold(0.0_f32, f32::max);
    (count, top, count == 0)
}

/// 워크스페이스별 검색 로그를 일 단위 JSONL 파일에 append 저장한다.
///
/// 경로: `{data_dir}/workspaces/{workspace}/search_logs/{YYYY-MM-DD}.jsonl`
pub struct SearchLogStore {
    data_dir: PathBuf,
}

impl SearchLogStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    /// 레코드가 저장될 일 단위 파일 경로 (timestamp의 날짜 기준).
    fn log_path(&self, record: &SearchLogRecord) -> PathBuf {
        let date = record.timestamp.format("%Y-%m-%d").to_string();
        self.data_dir
            .join("workspaces")
            .join(&record.workspace)
            .join("search_logs")
            .join(format!("{date}.jsonl"))
    }

    /// 특정 날짜(`YYYY-MM-DD`)의 검색 로그 레코드를 읽는다 (Phase 5 메트릭 롤업용).
    ///
    /// 파일이 없으면 빈 목록(그 날 검색이 없었음). 파싱 불가한 줄은 조용히 건너뛴다
    /// — 하나의 손상된 줄이 그 날 메트릭 전체를 실패시키지 않는다(장애 격리).
    pub async fn read_day(&self, workspace_id: &str, date: &str) -> Result<Vec<SearchLogRecord>> {
        let path = self
            .data_dir
            .join("workspaces")
            .join(workspace_id)
            .join("search_logs")
            .join(format!("{date}.jsonl"));
        match fs::read_to_string(&path).await {
            Ok(content) => Ok(content
                .lines()
                .filter_map(|line| serde_json::from_str::<SearchLogRecord>(line).ok())
                .collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e).context("검색 로그 읽기 실패"),
        }
    }

    /// 레코드를 해당 날짜 파일에 한 줄로 append한다. I/O 실패는 `Err`로 반환한다.
    pub async fn append(&self, record: &SearchLogRecord) -> Result<()> {
        let path = self.log_path(record);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .context("검색 로그 디렉토리 생성 실패")?;
        }

        let mut line = serde_json::to_string(record).context("검색 로그 직렬화 실패")?;
        line.push('\n');

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .context("검색 로그 파일 열기 실패")?;
        file.write_all(line.as_bytes())
            .await
            .context("검색 로그 기록 실패")?;
        // tokio::fs::File은 내부 버퍼를 쓰므로 flush로 OS까지 밀어내야 실제로 기록된다
        // (flush 누락 시 drop 시점 버퍼 유실 — 로그가 조용히 사라진다).
        file.flush().await.context("검색 로그 flush 실패")?;
        Ok(())
    }

    /// **실패 무해** append — 기록 실패를 삼킨다(warn만). 로그 장애가 검색 응답을
    /// 실패시키지 않도록 검색 경로에서 이 메서드를 쓴다(PRD: 로그 실패의 무해성).
    pub async fn append_best_effort(&self, record: SearchLogRecord) {
        if let Err(e) = self.append(&record).await {
            tracing::warn!("검색 로그 append 실패(검색은 정상 처리됨): {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn result(score: f32) -> SearchResult {
        SearchResult {
            id: Uuid::new_v4(),
            summary: String::new(),
            relevance_score: score,
            workspace: "default".to_string(),
            matched_facts: vec![],
            created_at: None,
            expanded_from: None,
        }
    }

    fn record_at(ymd: (i32, u32, u32), workspace: &str, mode: &str) -> SearchLogRecord {
        let ts = Utc.with_ymd_and_hms(ymd.0, ymd.1, ymd.2, 12, 0, 0).unwrap();
        SearchLogRecord {
            timestamp: ts,
            workspace: workspace.to_string(),
            query: "q".to_string(),
            mode: mode.to_string(),
            result_count: 1,
            top_score: 0.8,
            zero_result: false,
            duration_ms: 5,
            rounds: None,
        }
    }

    // ──── 순수 함수: derive_metrics ────

    #[test]
    fn test_derive_metrics_empty() {
        let (count, top, zero) = derive_metrics(&[]);
        assert_eq!(count, 0);
        assert_eq!(top, 0.0);
        assert!(zero, "결과 없으면 zero_result true");
    }

    #[test]
    fn test_derive_metrics_nonempty_takes_max() {
        let (count, top, zero) = derive_metrics(&[result(0.3), result(0.9), result(0.6)]);
        assert_eq!(count, 3);
        assert!((top - 0.9).abs() < f32::EPSILON, "최고 점수를 취해야 한다");
        assert!(!zero);
    }

    // ──── 레코드 직렬화 ────

    #[test]
    fn test_record_roundtrip() {
        let rec = record_at((2026, 7, 6), "default", "agent");
        let json = serde_json::to_string(&rec).unwrap();
        let back: SearchLogRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }

    #[test]
    fn test_record_omits_none_rounds() {
        let rec = record_at((2026, 7, 6), "default", "hybrid");
        let json = serde_json::to_string(&rec).unwrap();
        assert!(!json.contains("rounds"), "기존 모드는 rounds 필드를 생략해야 한다");
    }

    // ──── 파일 append (tempdir) ────

    #[tokio::test]
    async fn test_append_creates_daily_file() {
        let tmp = TempDir::new().unwrap();
        let store = SearchLogStore::new(tmp.path());
        let rec = record_at((2026, 7, 6), "default", "hybrid");

        store.append(&rec).await.unwrap();

        let path = tmp
            .path()
            .join("workspaces/default/search_logs/2026-07-06.jsonl");
        assert!(path.exists(), "일 단위 파일이 생성되어야 한다");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 1);
    }

    #[tokio::test]
    async fn test_append_same_day_appends_lines() {
        let tmp = TempDir::new().unwrap();
        let store = SearchLogStore::new(tmp.path());
        let rec = record_at((2026, 7, 6), "default", "hybrid");

        store.append(&rec).await.unwrap();
        store.append(&rec).await.unwrap();

        let path = tmp
            .path()
            .join("workspaces/default/search_logs/2026-07-06.jsonl");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 2, "같은 날은 같은 파일에 누적");
    }

    #[tokio::test]
    async fn test_append_different_days_separate_files() {
        let tmp = TempDir::new().unwrap();
        let store = SearchLogStore::new(tmp.path());

        store.append(&record_at((2026, 7, 6), "default", "hybrid")).await.unwrap();
        store.append(&record_at((2026, 7, 7), "default", "hybrid")).await.unwrap();

        let base = tmp.path().join("workspaces/default/search_logs");
        assert!(base.join("2026-07-06.jsonl").exists());
        assert!(base.join("2026-07-07.jsonl").exists(), "다른 날은 별도 파일로 분리");
    }

    #[tokio::test]
    async fn test_append_workspace_isolation() {
        let tmp = TempDir::new().unwrap();
        let store = SearchLogStore::new(tmp.path());

        store.append(&record_at((2026, 7, 6), "personal", "hybrid")).await.unwrap();
        store.append(&record_at((2026, 7, 6), "work", "hybrid")).await.unwrap();

        assert!(tmp.path().join("workspaces/personal/search_logs/2026-07-06.jsonl").exists());
        assert!(tmp.path().join("workspaces/work/search_logs/2026-07-06.jsonl").exists());
    }

    #[tokio::test]
    async fn test_read_day_returns_records() {
        let tmp = TempDir::new().unwrap();
        let store = SearchLogStore::new(tmp.path());
        store.append(&record_at((2026, 7, 6), "default", "hybrid")).await.unwrap();
        store.append(&record_at((2026, 7, 6), "default", "agent")).await.unwrap();

        let records = store.read_day("default", "2026-07-06").await.unwrap();
        assert_eq!(records.len(), 2, "그 날의 두 레코드를 읽어야 한다");
    }

    #[tokio::test]
    async fn test_read_day_missing_is_empty() {
        let tmp = TempDir::new().unwrap();
        let store = SearchLogStore::new(tmp.path());
        let records = store.read_day("default", "2026-01-01").await.unwrap();
        assert!(records.is_empty(), "검색 없던 날은 빈 목록");
    }

    #[tokio::test]
    async fn test_read_day_skips_malformed_lines() {
        // 손상된 줄이 섞여 있어도 정상 줄은 읽고 깨진 줄만 건너뛴다.
        let tmp = TempDir::new().unwrap();
        let store = SearchLogStore::new(tmp.path());
        store.append(&record_at((2026, 7, 6), "default", "hybrid")).await.unwrap();
        let path = tmp.path().join("workspaces/default/search_logs/2026-07-06.jsonl");
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str("this is not json\n");
        std::fs::write(&path, content).unwrap();

        let records = store.read_day("default", "2026-07-06").await.unwrap();
        assert_eq!(records.len(), 1, "정상 줄만 읽어야 한다");
    }

    #[tokio::test]
    async fn test_append_best_effort_swallows_error() {
        // 로그 실패가 검색을 실패시키지 않아야 한다: 디렉토리 위치에 파일을 미리 만들어
        // create_dir_all이 실패하도록 유도해도, append_best_effort는 패닉/전파하지 않는다.
        let tmp = TempDir::new().unwrap();
        // search_logs가 놓일 자리에 '동명의 파일'을 만들어 디렉토리 생성이 실패하게 한다.
        let ws_dir = tmp.path().join("workspaces/default");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(ws_dir.join("search_logs"), b"not a dir").unwrap();

        let store = SearchLogStore::new(tmp.path());
        let rec = record_at((2026, 7, 6), "default", "hybrid");

        // append는 Err를 반환하지만…
        assert!(store.append(&rec).await.is_err(), "디렉토리 충돌 시 append는 실패");
        // …best-effort는 삼켜서 아무 일도 일어나지 않는다(패닉 없음).
        store.append_best_effort(rec).await;
    }
}
