use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;
use uuid::Uuid;

use crate::models::Document;

/// 문서의 이전 버전 스냅샷 레코드.
///
/// 업데이트 경로에서 기존 문서 상태를 통째로 보관한다. 잘못된 업데이트 판단으로
/// 문서가 오염되어도 이전 상태로 되돌아볼 수 있는 안전망이다(복원 UI는 백로그).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionRecord {
    /// 보관 시점의 문서 전체 스냅샷
    pub document: Document,
    /// 보관 시각
    pub archived_at: DateTime<Utc>,
}

/// 문서 업데이트 시 이전 버전을 보관하는 저장소 (워크스페이스별 격리).
///
/// 저장 경로: `{data_dir}/workspaces/{id}/versions/{doc_id}/{millis}.json`
/// DocumentStore와 동일한 data_dir 루트를 공유해 워크스페이스 경로 정합성을 지킨다.
pub struct VersionStore {
    data_dir: PathBuf,
}

impl VersionStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    /// 특정 문서의 버전 디렉토리 경로.
    fn versions_path(&self, workspace_id: &str, doc_id: Uuid) -> PathBuf {
        self.data_dir
            .join("workspaces")
            .join(workspace_id)
            .join("versions")
            .join(doc_id.to_string())
    }

    /// 문서의 현재 상태를 이전 버전으로 보관한다. 보관된 파일 경로를 반환.
    ///
    /// 파일명은 밀리초 유닉스 타임스탬프(콜론 없는 FS 안전 이름)를 쓰고, 동일
    /// 밀리초에 두 번 보관되면 짧은 uuid를 붙여 유일성을 보장한다(스냅샷 유실 방지).
    pub async fn archive(&self, doc: &Document, workspace_id: &str) -> Result<PathBuf> {
        let dir = self.versions_path(workspace_id, doc.id);
        fs::create_dir_all(&dir)
            .await
            .context("Failed to create versions directory")?;

        let archived_at = Utc::now();
        let record = VersionRecord {
            document: doc.clone(),
            archived_at,
        };

        let stamp = archived_at.timestamp_millis();
        let mut file_path = dir.join(format!("{}.json", stamp));
        if file_path.exists() {
            let suffix = Uuid::new_v4().to_string();
            file_path = dir.join(format!("{}_{}.json", stamp, &suffix[..8]));
        }

        let content = serde_json::to_string_pretty(&record)?;
        fs::write(&file_path, content)
            .await
            .context("Failed to write version file")?;

        Ok(file_path)
    }

    /// 문서의 보관된 버전 목록을 최신순(archived_at 내림차순)으로 반환한다.
    /// 버전이 없으면 빈 목록(디렉토리 미존재 포함).
    pub async fn list_versions(
        &self,
        doc_id: Uuid,
        workspace_id: &str,
    ) -> Result<Vec<VersionRecord>> {
        let dir = self.versions_path(workspace_id, doc_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut entries = fs::read_dir(&dir)
            .await
            .context("Failed to read versions directory")?;
        let mut records = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "json") {
                if let Ok(content) = fs::read_to_string(&path).await {
                    if let Ok(record) = serde_json::from_str::<VersionRecord>(&content) {
                        records.push(record);
                    }
                }
            }
        }

        records.sort_by(|a, b| b.archived_at.cmp(&a.archived_at));
        Ok(records)
    }

    /// 문서의 보관된 버전 개수.
    pub async fn version_count(&self, doc_id: Uuid, workspace_id: &str) -> Result<usize> {
        Ok(self.list_versions(doc_id, workspace_id).await?.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Document, Edge, RelationType};
    use tempfile::TempDir;

    fn make_doc(content: &str) -> Document {
        Document::new(
            content.to_string(),
            format!("summary of {}", content),
            vec![],
            vec![],
        )
    }

    fn setup() -> (TempDir, VersionStore) {
        let tmp = TempDir::new().unwrap();
        let store = VersionStore::new(tmp.path());
        (tmp, store)
    }

    #[tokio::test]
    async fn test_archive_then_listed() {
        let (_tmp, store) = setup();
        let doc = make_doc("v1");

        store.archive(&doc, "default").await.unwrap();
        let versions = store.list_versions(doc.id, "default").await.unwrap();

        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].document.id, doc.id);
        assert_eq!(versions[0].document.raw_content, "v1");
    }

    #[tokio::test]
    async fn test_list_versions_empty_when_none() {
        let (_tmp, store) = setup();
        let versions = store.list_versions(Uuid::new_v4(), "default").await.unwrap();
        assert!(versions.is_empty());
    }

    #[tokio::test]
    async fn test_archive_preserves_full_snapshot_with_edges() {
        // 스냅샷은 엣지를 포함한 문서 전체를 보존해야 한다.
        let (_tmp, store) = setup();
        let mut doc = make_doc("with edges");
        doc.add_edge(Edge::new(Uuid::new_v4(), RelationType::Updates, 0.5));

        store.archive(&doc, "default").await.unwrap();
        let versions = store.list_versions(doc.id, "default").await.unwrap();

        assert_eq!(versions[0].document.edges.len(), 1);
        assert_eq!(versions[0].document.edges[0].relation, RelationType::Updates);
    }

    #[tokio::test]
    async fn test_multiple_versions_same_millis_no_overwrite() {
        // 같은 밀리초에 두 번 보관해도 덮어쓰지 않고 둘 다 남아야 한다.
        let (_tmp, store) = setup();
        let doc = make_doc("snap");

        store.archive(&doc, "default").await.unwrap();
        store.archive(&doc, "default").await.unwrap();

        let count = store.version_count(doc.id, "default").await.unwrap();
        assert_eq!(count, 2, "동일 밀리초 보관도 유실 없이 2개여야 한다");
    }

    #[tokio::test]
    async fn test_versions_workspace_isolation() {
        let (_tmp, store) = setup();
        let doc = make_doc("iso");

        store.archive(&doc, "personal").await.unwrap();

        assert_eq!(store.version_count(doc.id, "personal").await.unwrap(), 1);
        assert_eq!(store.version_count(doc.id, "work").await.unwrap(), 0);
    }
}
