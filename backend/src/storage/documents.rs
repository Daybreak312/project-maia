use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::fs;
use uuid::Uuid;

use crate::models::Document;

/// 원본 문서를 파일 시스템에 저장 (워크스페이스별 경로 분리)
///
/// 저장 경로: `{data_dir}/workspaces/{workspace_id}/documents/{doc_id}.json`
pub struct DocumentStore {
    data_dir: PathBuf,
}

impl DocumentStore {
    pub async fn new(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        Ok(Self { data_dir })
    }

    /// 워크스페이스의 문서 디렉토리 경로
    fn workspace_docs_path(&self, workspace_id: &str) -> PathBuf {
        self.data_dir
            .join("workspaces")
            .join(workspace_id)
            .join("documents")
    }

    pub async fn save(&self, doc: &Document, workspace_id: &str) -> Result<PathBuf> {
        let base = self.workspace_docs_path(workspace_id);
        fs::create_dir_all(&base)
            .await
            .context("Failed to create document storage directory")?;

        let file_path = base.join(format!("{}.json", doc.id));
        let content = serde_json::to_string_pretty(doc)?;

        fs::write(&file_path, content)
            .await
            .context("Failed to write document file")?;

        Ok(file_path)
    }

    pub async fn load(&self, id: Uuid, workspace_id: &str) -> Result<Document> {
        let file_path = self
            .workspace_docs_path(workspace_id)
            .join(format!("{}.json", id));
        let content = fs::read_to_string(&file_path)
            .await
            .context("Failed to read document file")?;

        let doc: Document = serde_json::from_str(&content)?;
        Ok(doc)
    }

    pub async fn exists(&self, id: Uuid, workspace_id: &str) -> bool {
        self.workspace_docs_path(workspace_id)
            .join(format!("{}.json", id))
            .exists()
    }

    pub async fn delete(&self, id: Uuid, workspace_id: &str) -> Result<()> {
        let file_path = self
            .workspace_docs_path(workspace_id)
            .join(format!("{}.json", id));
        fs::remove_file(&file_path)
            .await
            .context("Failed to delete document file")?;
        Ok(())
    }

    pub async fn list_recent(&self, limit: usize, workspace_id: &str) -> Result<Vec<Document>> {
        let base = self.workspace_docs_path(workspace_id);

        // 디렉토리가 없으면 빈 목록 반환
        if !base.exists() {
            return Ok(Vec::new());
        }

        let mut entries = fs::read_dir(&base)
            .await
            .context("Failed to read document directory")?;

        let mut docs = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "json") {
                if let Ok(content) = fs::read_to_string(&path).await {
                    if let Ok(doc) = serde_json::from_str::<Document>(&content) {
                        docs.push(doc);
                    }
                }
            }
        }

        // 최신순 정렬
        docs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        docs.truncate(limit);

        Ok(docs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Document;
    use tempfile::TempDir;

    fn make_doc(content: &str) -> Document {
        Document::new(
            content.to_string(),
            format!("Summary of {}", content),
            vec![],
            vec![],
        )
    }

    async fn setup() -> (TempDir, DocumentStore) {
        let tmp = TempDir::new().unwrap();
        let store = DocumentStore::new(tmp.path()).await.unwrap();
        (tmp, store)
    }

    #[tokio::test]
    async fn test_save_and_load() {
        let (_tmp, store) = setup().await;
        let doc = make_doc("hello world");
        let id = doc.id;

        store.save(&doc, "default").await.unwrap();
        let loaded = store.load(id, "default").await.unwrap();

        assert_eq!(loaded.id, id);
        assert_eq!(loaded.raw_content, "hello world");
    }

    #[tokio::test]
    async fn test_save_creates_workspace_dir() {
        let (tmp, store) = setup().await;
        let doc = make_doc("test");

        store.save(&doc, "my-ws").await.unwrap();

        let ws_dir = tmp.path().join("workspaces/my-ws/documents");
        assert!(ws_dir.exists());
    }

    #[tokio::test]
    async fn test_load_nonexistent_fails() {
        let (_tmp, store) = setup().await;
        let result = store.load(Uuid::new_v4(), "default").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_workspace_isolation() {
        let (_tmp, store) = setup().await;
        let doc = make_doc("isolated");
        let id = doc.id;

        store.save(&doc, "ws-a").await.unwrap();

        assert!(store.exists(id, "ws-a").await);
        assert!(!store.exists(id, "ws-b").await);
        assert!(store.load(id, "ws-b").await.is_err());
    }

    #[tokio::test]
    async fn test_delete() {
        let (_tmp, store) = setup().await;
        let doc = make_doc("will delete");
        let id = doc.id;

        store.save(&doc, "default").await.unwrap();
        assert!(store.exists(id, "default").await);

        store.delete(id, "default").await.unwrap();
        assert!(!store.exists(id, "default").await);
    }

    #[tokio::test]
    async fn test_delete_nonexistent_fails() {
        let (_tmp, store) = setup().await;
        let result = store.delete(Uuid::new_v4(), "default").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_recent_empty_workspace() {
        let (_tmp, store) = setup().await;
        let docs = store.list_recent(10, "empty-ws").await.unwrap();
        assert!(docs.is_empty());
    }

    #[tokio::test]
    async fn test_list_recent_returns_docs() {
        let (_tmp, store) = setup().await;

        store.save(&make_doc("first"), "default").await.unwrap();
        store.save(&make_doc("second"), "default").await.unwrap();

        let docs = store.list_recent(10, "default").await.unwrap();
        assert_eq!(docs.len(), 2);
    }

    #[tokio::test]
    async fn test_list_recent_respects_limit() {
        let (_tmp, store) = setup().await;

        for i in 0..5 {
            store
                .save(&make_doc(&format!("doc {}", i)), "default")
                .await
                .unwrap();
        }

        let docs = store.list_recent(3, "default").await.unwrap();
        assert_eq!(docs.len(), 3);
    }

    #[tokio::test]
    async fn test_same_id_coexists_across_workspaces() {
        // 동일 문서 ID가 서로 다른 워크스페이스에서 충돌 없이 존재해야 한다.
        let (_tmp, store) = setup().await;
        let shared_id = Uuid::new_v4();

        let mut doc_a = make_doc("personal content");
        doc_a.id = shared_id;
        let mut doc_b = make_doc("work content");
        doc_b.id = shared_id;

        store.save(&doc_a, "personal").await.unwrap();
        store.save(&doc_b, "work").await.unwrap();

        // 각 워크스페이스에서 같은 ID를 로드해도 서로 다른 내용이 나와야 한다
        let loaded_a = store.load(shared_id, "personal").await.unwrap();
        let loaded_b = store.load(shared_id, "work").await.unwrap();

        assert_eq!(loaded_a.raw_content, "personal content");
        assert_eq!(loaded_b.raw_content, "work content");
        assert_eq!(loaded_a.id, loaded_b.id, "ID는 같지만 격리되어 있어야 한다");
    }

    #[tokio::test]
    async fn test_list_recent_workspace_isolation() {
        let (_tmp, store) = setup().await;

        store.save(&make_doc("a"), "ws-a").await.unwrap();
        store.save(&make_doc("b"), "ws-b").await.unwrap();

        let a_docs = store.list_recent(10, "ws-a").await.unwrap();
        let b_docs = store.list_recent(10, "ws-b").await.unwrap();

        assert_eq!(a_docs.len(), 1);
        assert_eq!(b_docs.len(), 1);
        assert_eq!(a_docs[0].raw_content, "a");
        assert_eq!(b_docs[0].raw_content, "b");
    }
}
