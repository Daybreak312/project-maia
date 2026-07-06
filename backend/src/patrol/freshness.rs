//! freshness 기준점 저장 — "유효" 판단 시각을 문서별로 기록한다.
//!
//! staleness 탐지기가 이 기준점 이후의 나이로 노화를 재는 근거다: "유효"로 판단된 문서는
//! 기준점이 갱신되어 당분간 다시 staleness로 플래그되지 않는다(유예).
//!
//! **왜 문서가 아니라 별도 파일인가:** Patrol 불변식은 "문서 내용을 변경하지 않는다"이다.
//! freshness는 거버넌스 메타데이터이므로 문서 raw JSON을 건드리지 않고 워크스페이스별
//! 작은 맵 파일(`workspaces/{id}/patrol/freshness.json`)에 둔다.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::atomic_write_json;

/// 문서별 "유효" 판단 시각 맵을 파일로 저장/로드한다(워크스페이스별 격리).
pub struct FreshnessStore {
    data_dir: PathBuf,
    /// read-modify-write 직렬화(동시 touch가 서로를 덮어쓰지 않게).
    write_lock: Mutex<()>,
}

impl FreshnessStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            write_lock: Mutex::new(()),
        }
    }

    fn path(&self, workspace_id: &str) -> PathBuf {
        self.data_dir
            .join("workspaces")
            .join(workspace_id)
            .join("patrol")
            .join("freshness.json")
    }

    /// 문서별 freshness 기준점 맵을 로드한다(파일 없으면 빈 맵).
    pub async fn load(&self, workspace_id: &str) -> Result<HashMap<Uuid, DateTime<Utc>>> {
        let path = self.path(workspace_id);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => serde_json::from_str(&content).context("freshness 맵 파싱 실패"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(e).context("freshness 맵 읽기 실패"),
        }
    }

    /// 문서의 freshness 기준점을 `now`로 갱신한다(멱등 — 같은 값 재설정은 무해).
    pub async fn touch(&self, workspace_id: &str, doc_id: Uuid, now: DateTime<Utc>) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let mut map = self.load(workspace_id).await?;
        map.insert(doc_id, now);
        atomic_write_json(&self.path(workspace_id), &map).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_load_missing_is_empty() {
        let tmp = TempDir::new().unwrap();
        let store = FreshnessStore::new(tmp.path());
        assert!(store.load("default").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_touch_then_load() {
        let tmp = TempDir::new().unwrap();
        let store = FreshnessStore::new(tmp.path());
        let doc = Uuid::new_v4();
        let now = Utc::now();
        store.touch("default", doc, now).await.unwrap();

        let map = store.load("default").await.unwrap();
        assert_eq!(map.get(&doc), Some(&now));
    }

    #[tokio::test]
    async fn test_touch_idempotent_and_updates() {
        let tmp = TempDir::new().unwrap();
        let store = FreshnessStore::new(tmp.path());
        let doc = Uuid::new_v4();
        let t1 = Utc::now() - chrono::Duration::days(1);
        let t2 = Utc::now();

        store.touch("default", doc, t1).await.unwrap();
        store.touch("default", doc, t2).await.unwrap(); // 갱신
        let map = store.load("default").await.unwrap();
        assert_eq!(map.len(), 1, "같은 문서는 한 항목");
        assert_eq!(map.get(&doc), Some(&t2), "최신 시각으로 갱신");
    }

    #[tokio::test]
    async fn test_workspace_isolation() {
        let tmp = TempDir::new().unwrap();
        let store = FreshnessStore::new(tmp.path());
        let doc = Uuid::new_v4();
        store.touch("personal", doc, Utc::now()).await.unwrap();
        assert_eq!(store.load("personal").await.unwrap().len(), 1);
        assert!(store.load("work").await.unwrap().is_empty(), "다른 워크스페이스는 격리");
    }

    #[tokio::test]
    async fn test_multiple_docs() {
        let tmp = TempDir::new().unwrap();
        let store = FreshnessStore::new(tmp.path());
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        store.touch("default", a, Utc::now()).await.unwrap();
        store.touch("default", b, Utc::now()).await.unwrap();
        assert_eq!(store.load("default").await.unwrap().len(), 2);
    }
}
