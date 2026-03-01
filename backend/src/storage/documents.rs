use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::fs;
use uuid::Uuid;

use crate::models::Document;

/// 원본 문서를 파일 시스템에 저장
pub struct DocumentStore {
    base_path: PathBuf,
}

impl DocumentStore {
    pub async fn new(base_path: impl Into<PathBuf>) -> Result<Self> {
        let base_path = base_path.into();
        fs::create_dir_all(&base_path)
            .await
            .context("Failed to create document storage directory")?;

        Ok(Self { base_path })
    }

    pub async fn save(&self, doc: &Document) -> Result<PathBuf> {
        let file_path = self.base_path.join(format!("{}.json", doc.id));
        let content = serde_json::to_string_pretty(doc)?;

        fs::write(&file_path, content)
            .await
            .context("Failed to write document file")?;

        Ok(file_path)
    }

    pub async fn load(&self, id: Uuid) -> Result<Document> {
        let file_path = self.base_path.join(format!("{}.json", id));
        let content = fs::read_to_string(&file_path)
            .await
            .context("Failed to read document file")?;

        let doc: Document = serde_json::from_str(&content)?;
        Ok(doc)
    }

    pub async fn exists(&self, id: Uuid) -> bool {
        let file_path = self.base_path.join(format!("{}.json", id));
        file_path.exists()
    }

    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let file_path = self.base_path.join(format!("{}.json", id));
        fs::remove_file(&file_path)
            .await
            .context("Failed to delete document file")?;
        Ok(())
    }

    pub async fn list_recent(&self, limit: usize) -> Result<Vec<Document>> {
        let mut entries = fs::read_dir(&self.base_path)
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
