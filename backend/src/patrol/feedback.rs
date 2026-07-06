//! 검색 피드백 수집 — "관련 없음" 신호를 일 단위 JSONL로 축적한다.
//!
//! 검색 결과 문서가 실제로는 관련 없다는 소유자 신호를 모은다. 이 phase에서는 **축적 +
//! 문서별 집계**까지만 한다(staleness 탐지기의 입력 신호). 피드백 기반 ML 학습은 범위 밖.
//!
//! search_log와 같은 일 단위 append-only JSONL 패턴을 따른다(무한 성장 방지·롤업 용이·
//! 실패 무해). 경로: `workspaces/{id}/feedback/{YYYY-MM-DD}.jsonl`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// 피드백 유형. 현재는 "관련 없음" 하나이나 확장 가능하게 열거형으로 둔다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackKind {
    /// 이 문서는 이 쿼리에 관련 없다.
    NotRelevant,
}

/// 피드백 한 건의 레코드 (JSONL 한 줄).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedbackRecord {
    pub timestamp: DateTime<Utc>,
    pub workspace: String,
    /// 피드백이 발생한 검색 쿼리.
    pub query: String,
    /// 관련 없다고 표시된 문서.
    pub document_id: Uuid,
    pub kind: FeedbackKind,
}

/// 워크스페이스별 피드백을 일 단위 JSONL에 축적하고 문서별로 집계한다.
pub struct FeedbackStore {
    data_dir: PathBuf,
}

impl FeedbackStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    fn feedback_dir(&self, workspace_id: &str) -> PathBuf {
        self.data_dir
            .join("workspaces")
            .join(workspace_id)
            .join("feedback")
    }

    fn day_path(&self, record: &FeedbackRecord) -> PathBuf {
        let date = record.timestamp.format("%Y-%m-%d").to_string();
        self.feedback_dir(&record.workspace)
            .join(format!("{date}.jsonl"))
    }

    /// 레코드를 해당 날짜 파일에 한 줄로 append한다. I/O 실패는 `Err`.
    pub async fn append(&self, record: &FeedbackRecord) -> Result<()> {
        let path = self.day_path(record);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.context("피드백 디렉토리 생성 실패")?;
        }
        let mut line = serde_json::to_string(record).context("피드백 직렬화 실패")?;
        line.push('\n');

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .context("피드백 파일 열기 실패")?;
        file.write_all(line.as_bytes()).await.context("피드백 기록 실패")?;
        // tokio::fs::File은 내부 버퍼를 쓰므로 flush로 OS까지 밀어내야 실제 기록된다.
        file.flush().await.context("피드백 flush 실패")?;
        Ok(())
    }

    /// **실패 무해** append — 기록 실패를 삼킨다(warn만). 피드백 장애가 검색 응답을
    /// 실패시키지 않도록 API 경로에서 이 메서드를 쓴다.
    pub async fn append_best_effort(&self, record: FeedbackRecord) {
        if let Err(e) = self.append(&record).await {
            tracing::warn!("피드백 append 실패(무해): {}", e);
        }
    }

    /// 워크스페이스의 모든 피드백을 스캔해 문서별 "관련 없음" 건수를 집계한다.
    ///
    /// staleness 탐지기의 입력 신호가 된다("관련 없다"고 표시된 문서일수록 노화 가속).
    /// 파일/디렉토리가 없으면 빈 맵. 파싱 불가한 줄은 건너뛴다(장애 격리).
    pub async fn count_negative_by_doc(&self, workspace_id: &str) -> Result<HashMap<Uuid, usize>> {
        let dir = self.feedback_dir(workspace_id);
        let mut counts: HashMap<Uuid, usize> = HashMap::new();
        let mut entries = match fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(counts),
            Err(e) => return Err(e).context("피드백 디렉토리 읽기 실패"),
        };

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path).await else {
                continue;
            };
            for line in content.lines() {
                if let Ok(rec) = serde_json::from_str::<FeedbackRecord>(line) {
                    if rec.kind == FeedbackKind::NotRelevant {
                        *counts.entry(rec.document_id).or_insert(0) += 1;
                    }
                }
            }
        }
        Ok(counts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn record(doc: Uuid, ymd: (i32, u32, u32)) -> FeedbackRecord {
        FeedbackRecord {
            timestamp: Utc.with_ymd_and_hms(ymd.0, ymd.1, ymd.2, 10, 0, 0).unwrap(),
            workspace: "default".to_string(),
            query: "무관한 쿼리".to_string(),
            document_id: doc,
            kind: FeedbackKind::NotRelevant,
        }
    }

    #[tokio::test]
    async fn test_append_creates_daily_file() {
        let tmp = TempDir::new().unwrap();
        let store = FeedbackStore::new(tmp.path());
        store.append(&record(Uuid::new_v4(), (2026, 7, 6))).await.unwrap();
        assert!(tmp.path().join("workspaces/default/feedback/2026-07-06.jsonl").exists());
    }

    #[tokio::test]
    async fn test_record_roundtrip() {
        let rec = record(Uuid::new_v4(), (2026, 7, 6));
        let json = serde_json::to_string(&rec).unwrap();
        let back: FeedbackRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }

    #[tokio::test]
    async fn test_count_negative_empty_when_none() {
        let tmp = TempDir::new().unwrap();
        let store = FeedbackStore::new(tmp.path());
        assert!(store.count_negative_by_doc("default").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_count_negative_aggregates_per_doc() {
        let tmp = TempDir::new().unwrap();
        let store = FeedbackStore::new(tmp.path());
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        // a: 2건(다른 날 포함), b: 1건
        store.append(&record(a, (2026, 7, 6))).await.unwrap();
        store.append(&record(a, (2026, 7, 7))).await.unwrap();
        store.append(&record(b, (2026, 7, 6))).await.unwrap();

        let counts = store.count_negative_by_doc("default").await.unwrap();
        assert_eq!(counts.get(&a), Some(&2), "a는 2건(여러 날 합산)");
        assert_eq!(counts.get(&b), Some(&1));
    }

    #[tokio::test]
    async fn test_count_negative_skips_malformed() {
        let tmp = TempDir::new().unwrap();
        let store = FeedbackStore::new(tmp.path());
        let a = Uuid::new_v4();
        store.append(&record(a, (2026, 7, 6))).await.unwrap();
        let path = tmp.path().join("workspaces/default/feedback/2026-07-06.jsonl");
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str("broken line\n");
        std::fs::write(&path, content).unwrap();

        let counts = store.count_negative_by_doc("default").await.unwrap();
        assert_eq!(counts.get(&a), Some(&1), "정상 줄만 집계");
    }

    #[tokio::test]
    async fn test_append_best_effort_swallows_error() {
        // feedback 디렉토리 자리에 동명 파일을 만들어 디렉토리 생성이 실패하게 한다.
        let tmp = TempDir::new().unwrap();
        let ws_dir = tmp.path().join("workspaces/default");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(ws_dir.join("feedback"), b"not a dir").unwrap();

        let store = FeedbackStore::new(tmp.path());
        let rec = record(Uuid::new_v4(), (2026, 7, 6));
        assert!(store.append(&rec).await.is_err(), "디렉토리 충돌 시 append 실패");
        store.append_best_effort(rec).await; // 삼켜서 패닉 없음
    }

    #[tokio::test]
    async fn test_workspace_isolation() {
        let tmp = TempDir::new().unwrap();
        let store = FeedbackStore::new(tmp.path());
        let mut rec = record(Uuid::new_v4(), (2026, 7, 6));
        rec.workspace = "personal".to_string();
        store.append(&rec).await.unwrap();

        assert_eq!(store.count_negative_by_doc("personal").await.unwrap().len(), 1);
        assert!(store.count_negative_by_doc("work").await.unwrap().is_empty());
    }
}
