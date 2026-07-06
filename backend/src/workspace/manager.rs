use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::sync::RwLock;

use super::config::{validate_workspace_id, WorkspaceConfig};

/// 워크스페이스 CRUD 및 파일 시스템 관리.
/// 각 워크스페이스는 `{data_dir}/workspaces/{id}/` 디렉토리에 저장된다.
pub struct WorkspaceManager {
    data_dir: PathBuf,
    workspaces: RwLock<Vec<WorkspaceConfig>>,
}

impl WorkspaceManager {
    pub async fn new(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        let workspaces_dir = data_dir.join("workspaces");
        fs::create_dir_all(&workspaces_dir)
            .await
            .context("Failed to create workspaces directory")?;

        let manager = Self {
            data_dir,
            workspaces: RwLock::new(Vec::new()),
        };

        manager.load_all().await?;

        Ok(manager)
    }

    /// 디스크에서 모든 워크스페이스 설정을 로드
    async fn load_all(&self) -> Result<()> {
        let workspaces_dir = self.data_dir.join("workspaces");
        let mut entries = fs::read_dir(&workspaces_dir)
            .await
            .context("Failed to read workspaces directory")?;

        let mut configs = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                let config_path = path.join("config.json");
                if config_path.exists() {
                    match fs::read_to_string(&config_path).await {
                        Ok(content) => match serde_json::from_str::<WorkspaceConfig>(&content) {
                            Ok(config) => configs.push(config),
                            Err(e) => {
                                tracing::warn!("Invalid config at {:?}: {}", config_path, e);
                            }
                        },
                        Err(e) => {
                            tracing::warn!("Failed to read {:?}: {}", config_path, e);
                        }
                    }
                }
            }
        }

        // 생성 시각 기준 정렬 (안정적인 순서 보장)
        configs.sort_by(|a, b| a.created_at.cmp(&b.created_at));

        let mut workspaces = self.workspaces.write().await;
        *workspaces = configs;

        Ok(())
    }

    /// default 워크스페이스가 없으면 생성하고, 레거시 데이터 마이그레이션 수행
    pub async fn ensure_default(&self) -> Result<()> {
        let exists = {
            let workspaces = self.workspaces.read().await;
            workspaces.iter().any(|w| w.id == "default")
        };

        if exists {
            return Ok(());
        }

        let default = WorkspaceConfig::default_workspace();
        self.create_internal(default).await?;

        // 레거시 data/raw/ → workspaces/default/documents/ 마이그레이션
        self.migrate_legacy_data().await?;

        Ok(())
    }

    /// 레거시 data/raw/ 디렉토리의 문서를 default 워크스페이스로 복사
    async fn migrate_legacy_data(&self) -> Result<()> {
        let legacy_dir = self.data_dir.join("raw");
        if !legacy_dir.exists() {
            return Ok(());
        }

        let target_dir = self.documents_path("default");
        fs::create_dir_all(&target_dir).await?;

        let mut entries = fs::read_dir(&legacy_dir).await?;
        let mut count = 0u32;

        while let Some(entry) = entries.next_entry().await? {
            let src = entry.path();
            if src
                .extension()
                .map_or(false, |ext| ext == "json")
            {
                if let Some(filename) = src.file_name() {
                    let dest = target_dir.join(filename);
                    if !dest.exists() {
                        fs::copy(&src, &dest).await.with_context(|| {
                            format!("Failed to migrate {:?} to {:?}", src, dest)
                        })?;
                        count += 1;
                    }
                }
            }
        }

        if count > 0 {
            tracing::info!(
                "Migrated {} documents from legacy data/raw/ to default workspace",
                count
            );
        }

        Ok(())
    }

    /// 워크스페이스 생성
    pub async fn create(&self, config: WorkspaceConfig) -> Result<WorkspaceConfig> {
        validate_workspace_id(&config.id).map_err(|e| anyhow!(e))?;

        {
            let workspaces = self.workspaces.read().await;
            if workspaces.iter().any(|w| w.id == config.id) {
                return Err(anyhow!("Workspace '{}' already exists", config.id));
            }
        }

        self.create_internal(config.clone()).await?;
        Ok(config)
    }

    /// 내부 생성 (유효성 검사 건너뜀 — ensure_default에서 사용)
    async fn create_internal(&self, config: WorkspaceConfig) -> Result<()> {
        let ws_dir = self.data_dir.join("workspaces").join(&config.id);
        fs::create_dir_all(&ws_dir).await?;
        fs::create_dir_all(ws_dir.join("documents")).await?;

        let config_path = ws_dir.join("config.json");
        let content = serde_json::to_string_pretty(&config)?;
        fs::write(&config_path, content).await?;

        let id = config.id.clone();
        let mut workspaces = self.workspaces.write().await;
        workspaces.push(config);

        tracing::info!("Created workspace: {}", id);
        Ok(())
    }

    /// 워크스페이스 조회
    pub async fn get(&self, id: &str) -> Result<WorkspaceConfig> {
        let workspaces = self.workspaces.read().await;
        workspaces
            .iter()
            .find(|w| w.id == id)
            .cloned()
            .ok_or_else(|| anyhow!("Workspace '{}' not found", id))
    }

    /// 전체 워크스페이스 목록
    pub async fn list(&self) -> Vec<WorkspaceConfig> {
        self.workspaces.read().await.clone()
    }

    /// 워크스페이스 설정 업데이트 (id, created_at는 변경 불가)
    pub async fn update(&self, id: &str, mut updated: WorkspaceConfig) -> Result<WorkspaceConfig> {
        // id 변경 방지
        updated.id = id.to_string();

        let config_path = self
            .data_dir
            .join("workspaces")
            .join(id)
            .join("config.json");

        if !config_path.exists() {
            return Err(anyhow!("Workspace '{}' not found", id));
        }

        // created_at 보존
        {
            let workspaces = self.workspaces.read().await;
            if let Some(existing) = workspaces.iter().find(|w| w.id == id) {
                updated.created_at = existing.created_at;
            }
        }

        let content = serde_json::to_string_pretty(&updated)?;
        fs::write(&config_path, content).await?;

        let mut workspaces = self.workspaces.write().await;
        if let Some(ws) = workspaces.iter_mut().find(|w| w.id == id) {
            *ws = updated.clone();
        }

        Ok(updated)
    }

    /// 워크스페이스 삭제 (default는 삭제 불가)
    pub async fn delete(&self, id: &str) -> Result<()> {
        if id == "default" {
            return Err(anyhow!("Cannot delete the default workspace"));
        }

        let ws_dir = self.data_dir.join("workspaces").join(id);
        if !ws_dir.exists() {
            return Err(anyhow!("Workspace '{}' not found", id));
        }

        fs::remove_dir_all(&ws_dir)
            .await
            .context("Failed to delete workspace directory")?;

        let mut workspaces = self.workspaces.write().await;
        workspaces.retain(|w| w.id != id);

        tracing::info!("Deleted workspace: {}", id);
        Ok(())
    }

    /// 워크스페이스 존재 여부 확인
    pub async fn exists(&self, id: &str) -> bool {
        let workspaces = self.workspaces.read().await;
        workspaces.iter().any(|w| w.id == id)
    }

    /// 워크스페이스의 문서 저장 경로
    pub fn documents_path(&self, workspace_id: &str) -> PathBuf {
        self.data_dir
            .join("workspaces")
            .join(workspace_id)
            .join("documents")
    }

    /// data_dir 참조 (DocumentStore 등에서 사용)
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::config::WorkspaceTemplate;
    use tempfile::TempDir;

    async fn setup() -> (TempDir, WorkspaceManager) {
        let tmp = TempDir::new().unwrap();
        let manager = WorkspaceManager::new(tmp.path()).await.unwrap();
        (tmp, manager)
    }

    // ──── ensure_default ────

    #[tokio::test]
    async fn test_ensure_default_creates_workspace() {
        let (_tmp, manager) = setup().await;
        manager.ensure_default().await.unwrap();

        let ws = manager.get("default").await.unwrap();
        assert_eq!(ws.id, "default");
        assert_eq!(ws.name, "Default");
        assert_eq!(ws.template, WorkspaceTemplate::Personal);
    }

    #[tokio::test]
    async fn test_ensure_default_idempotent() {
        let (_tmp, manager) = setup().await;

        manager.ensure_default().await.unwrap();
        manager.ensure_default().await.unwrap(); // 두 번째 호출도 성공

        let list = manager.list().await;
        let default_count = list.iter().filter(|w| w.id == "default").count();
        assert_eq!(default_count, 1);
    }

    #[tokio::test]
    async fn test_ensure_default_creates_documents_dir() {
        let (tmp, manager) = setup().await;
        manager.ensure_default().await.unwrap();

        let docs_dir = tmp.path().join("workspaces/default/documents");
        assert!(docs_dir.exists());
    }

    // ──── create ────

    #[tokio::test]
    async fn test_create_workspace() {
        let (_tmp, manager) = setup().await;
        let config = WorkspaceConfig::new(
            "work".to_string(),
            "Work".to_string(),
            WorkspaceTemplate::Enterprise,
        );

        let created = manager.create(config).await.unwrap();
        assert_eq!(created.id, "work");
        assert_eq!(created.template, WorkspaceTemplate::Enterprise);
    }

    #[tokio::test]
    async fn test_create_workspace_persists_to_disk() {
        let (tmp, manager) = setup().await;
        let config = WorkspaceConfig::new(
            "persist-test".to_string(),
            "Persist".to_string(),
            WorkspaceTemplate::Personal,
        );
        manager.create(config).await.unwrap();

        let config_path = tmp
            .path()
            .join("workspaces/persist-test/config.json");
        assert!(config_path.exists());

        let content = fs::read_to_string(&config_path).await.unwrap();
        let loaded: WorkspaceConfig = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded.id, "persist-test");
    }

    #[tokio::test]
    async fn test_create_duplicate_workspace_fails() {
        let (_tmp, manager) = setup().await;
        let config1 = WorkspaceConfig::new(
            "dup".to_string(),
            "Dup 1".to_string(),
            WorkspaceTemplate::Personal,
        );
        let config2 = WorkspaceConfig::new(
            "dup".to_string(),
            "Dup 2".to_string(),
            WorkspaceTemplate::Personal,
        );

        manager.create(config1).await.unwrap();
        let err = manager.create(config2).await.unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn test_create_workspace_invalid_id_rejected() {
        let (_tmp, manager) = setup().await;

        // 빈 ID
        let config = WorkspaceConfig::new(
            "".to_string(),
            "Empty".to_string(),
            WorkspaceTemplate::Personal,
        );
        assert!(manager.create(config).await.is_err());

        // 특수문자
        let config = WorkspaceConfig::new(
            "my workspace".to_string(),
            "Spaces".to_string(),
            WorkspaceTemplate::Personal,
        );
        assert!(manager.create(config).await.is_err());

        // 하이픈으로 시작
        let config = WorkspaceConfig::new(
            "-leading".to_string(),
            "Bad".to_string(),
            WorkspaceTemplate::Personal,
        );
        assert!(manager.create(config).await.is_err());
    }

    // ──── get ────

    #[tokio::test]
    async fn test_get_existing_workspace() {
        let (_tmp, manager) = setup().await;
        manager.ensure_default().await.unwrap();

        let ws = manager.get("default").await.unwrap();
        assert_eq!(ws.id, "default");
    }

    #[tokio::test]
    async fn test_get_nonexistent_workspace_fails() {
        let (_tmp, manager) = setup().await;
        let err = manager.get("nonexistent").await.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    // ──── list ────

    #[tokio::test]
    async fn test_list_empty() {
        let (_tmp, manager) = setup().await;
        let list = manager.list().await;
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_list_multiple_workspaces() {
        let (_tmp, manager) = setup().await;
        manager.ensure_default().await.unwrap();

        let work = WorkspaceConfig::new(
            "work".to_string(),
            "Work".to_string(),
            WorkspaceTemplate::Enterprise,
        );
        manager.create(work).await.unwrap();

        let list = manager.list().await;
        assert_eq!(list.len(), 2);

        let ids: Vec<&str> = list.iter().map(|w| w.id.as_str()).collect();
        assert!(ids.contains(&"default"));
        assert!(ids.contains(&"work"));
    }

    // ──── update ────

    #[tokio::test]
    async fn test_update_workspace() {
        let (_tmp, manager) = setup().await;
        manager.ensure_default().await.unwrap();

        let mut updated = manager.get("default").await.unwrap();
        updated.name = "Updated Default".to_string();
        updated.patrol.frequency = "daily".to_string();

        let result = manager.update("default", updated).await.unwrap();
        assert_eq!(result.name, "Updated Default");
        assert_eq!(result.patrol.frequency, "daily");

        // 재조회해도 반영되어 있는지 확인
        let fetched = manager.get("default").await.unwrap();
        assert_eq!(fetched.name, "Updated Default");
    }

    #[tokio::test]
    async fn test_update_preserves_created_at() {
        let (_tmp, manager) = setup().await;
        manager.ensure_default().await.unwrap();
        let original = manager.get("default").await.unwrap();
        let original_created = original.created_at;

        let mut updated = original.clone();
        updated.name = "Changed".to_string();
        // created_at을 임의로 변경해도 원래 값이 보존되는지 확인
        updated.created_at = chrono::Utc::now();

        let result = manager.update("default", updated).await.unwrap();
        assert_eq!(result.created_at, original_created);
    }

    #[tokio::test]
    async fn test_update_nonexistent_workspace_fails() {
        let (_tmp, manager) = setup().await;
        let config = WorkspaceConfig::new(
            "ghost".to_string(),
            "Ghost".to_string(),
            WorkspaceTemplate::Personal,
        );
        let err = manager.update("ghost", config).await.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    // ──── delete ────

    #[tokio::test]
    async fn test_delete_workspace() {
        let (tmp, manager) = setup().await;
        let config = WorkspaceConfig::new(
            "to-delete".to_string(),
            "Delete Me".to_string(),
            WorkspaceTemplate::Personal,
        );
        manager.create(config).await.unwrap();
        assert!(manager.exists("to-delete").await);

        manager.delete("to-delete").await.unwrap();

        assert!(!manager.exists("to-delete").await);
        assert!(!tmp.path().join("workspaces/to-delete").exists());
    }

    #[tokio::test]
    async fn test_delete_default_workspace_fails() {
        let (_tmp, manager) = setup().await;
        manager.ensure_default().await.unwrap();

        let err = manager.delete("default").await.unwrap_err();
        assert!(err.to_string().contains("Cannot delete"));
    }

    #[tokio::test]
    async fn test_delete_nonexistent_workspace_fails() {
        let (_tmp, manager) = setup().await;
        let err = manager.delete("ghost").await.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    // ──── exists ────

    #[tokio::test]
    async fn test_exists() {
        let (_tmp, manager) = setup().await;
        assert!(!manager.exists("default").await);

        manager.ensure_default().await.unwrap();
        assert!(manager.exists("default").await);
    }

    // ──── documents_path ────

    #[tokio::test]
    async fn test_documents_path() {
        let (tmp, manager) = setup().await;
        let path = manager.documents_path("my-ws");
        assert_eq!(
            path,
            tmp.path().join("workspaces/my-ws/documents")
        );
    }

    // ──── 레거시 데이터 마이그레이션 ────

    #[tokio::test]
    async fn test_migrate_legacy_data() {
        let (tmp, manager) = setup().await;

        // 레거시 data/raw/ 디렉토리에 테스트 파일 생성
        let legacy_dir = tmp.path().join("raw");
        fs::create_dir_all(&legacy_dir).await.unwrap();
        fs::write(legacy_dir.join("doc1.json"), r#"{"id": "test1"}"#)
            .await
            .unwrap();
        fs::write(legacy_dir.join("doc2.json"), r#"{"id": "test2"}"#)
            .await
            .unwrap();
        // JSON이 아닌 파일은 마이그레이션 제외
        fs::write(legacy_dir.join("notes.txt"), "not json")
            .await
            .unwrap();

        manager.ensure_default().await.unwrap();

        let target_dir = tmp.path().join("workspaces/default/documents");
        assert!(target_dir.join("doc1.json").exists());
        assert!(target_dir.join("doc2.json").exists());
        assert!(!target_dir.join("notes.txt").exists());
    }

    #[tokio::test]
    async fn test_migrate_no_legacy_dir() {
        let (_tmp, manager) = setup().await;
        // data/raw/ 없이도 정상 동작
        manager.ensure_default().await.unwrap();
        assert!(manager.exists("default").await);
    }

    #[tokio::test]
    async fn test_migrate_does_not_overwrite() {
        let (tmp, manager) = setup().await;

        // 레거시 파일 생성
        let legacy_dir = tmp.path().join("raw");
        fs::create_dir_all(&legacy_dir).await.unwrap();
        fs::write(legacy_dir.join("existing.json"), r#"{"version": "old"}"#)
            .await
            .unwrap();

        // 대상에 이미 같은 이름 파일 존재
        let target_dir = tmp.path().join("workspaces/default/documents");
        fs::create_dir_all(&target_dir).await.unwrap();
        fs::write(target_dir.join("existing.json"), r#"{"version": "new"}"#)
            .await
            .unwrap();

        // config.json도 만들어 줘야 default가 "이미 존재"하는 것처럼 동작
        let ws_dir = tmp.path().join("workspaces/default");
        let config = WorkspaceConfig::default_workspace();
        fs::write(
            ws_dir.join("config.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .await
        .unwrap();

        // 재로드
        let manager2 = WorkspaceManager::new(tmp.path()).await.unwrap();
        manager2.ensure_default().await.unwrap();

        // 기존 파일이 덮어쓰여지지 않았는지 확인
        let content = fs::read_to_string(target_dir.join("existing.json"))
            .await
            .unwrap();
        assert!(content.contains("new"));

        drop(manager);
    }

    // ──── 디스크에서 재로드 ────

    #[tokio::test]
    async fn test_reload_from_disk() {
        let (tmp, manager) = setup().await;
        manager.ensure_default().await.unwrap();

        let work = WorkspaceConfig::new(
            "work".to_string(),
            "Work".to_string(),
            WorkspaceTemplate::Enterprise,
        );
        manager.create(work).await.unwrap();

        // 새 매니저 인스턴스로 디스크에서 재로드
        let manager2 = WorkspaceManager::new(tmp.path()).await.unwrap();
        let list = manager2.list().await;

        assert_eq!(list.len(), 2);
        assert!(manager2.exists("default").await);
        assert!(manager2.exists("work").await);
    }
}
