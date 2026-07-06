//! 로컬 디렉토리 커넥터 — 지정 디렉토리의 마크다운/텍스트 변경분을 증분 유입한다.
//!
//! 불변식·방어선:
//! - **증분**: 파일 수정 시각 > 마지막 커서인 파일만 유입. 커서는 스캔 시각(RFC3339).
//! - **심볼릭 링크 방어**: 등록된 루트는 canonicalize로 따르되, 순회 중 발견되는 심볼릭
//!   링크는 따르지 않는다(등록 범위 밖 읽기 차단).
//! - **장애 격리**: 깨진 인코딩·읽기 실패 파일 하나가 스캔 전체를 중단시키지 않는다.
//! - **대용량 보호**: 크기 상한 초과 파일은 읽지 않고 스킵·기록.
//! - **읽기 전용**: 소스 파일을 절대 수정/삭제하지 않는다.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use tokio::fs;

use super::{Connector, ConnectorItem, FetchResult};
use crate::workspace::LocalDirectoryConfig;

/// 소스 타입 식별자.
pub const SOURCE_TYPE: &str = "local_directory";

pub struct LocalDirectoryConnector {
    config: LocalDirectoryConfig,
}

impl LocalDirectoryConnector {
    pub fn new(config: LocalDirectoryConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Connector for LocalDirectoryConnector {
    fn source_type(&self) -> &str {
        SOURCE_TYPE
    }

    async fn fetch_changes(&self, cursor: Option<&str>) -> Result<FetchResult> {
        // 커서 파싱 — 손상된 커서는 None(전체 스캔)으로 방어적 강등한다.
        let after: Option<DateTime<Utc>> = cursor.and_then(|c| {
            match DateTime::parse_from_rfc3339(c) {
                Ok(dt) => Some(dt.with_timezone(&Utc)),
                Err(e) => {
                    tracing::warn!("커넥터 커서 파싱 실패(전체 스캔으로 강등): {c:?} — {e}");
                    None
                }
            }
        });

        // 다음 커서는 스캔 시작 시각. 스캔 중 수정된 파일은 다음 회차에 다시 잡히므로
        // (mtime > next_cursor) 유실이 없다(중복 재유입은 소스 dedup이 흡수).
        let scan_start = Utc::now();

        // 제외 glob 패턴 컴파일 (손상된 패턴은 경고 후 무시).
        let exclude: Vec<glob::Pattern> = self
            .config
            .exclude
            .iter()
            .filter_map(|p| match glob::Pattern::new(p) {
                Ok(pat) => Some(pat),
                Err(e) => {
                    tracing::warn!("커넥터 제외 패턴 무시(컴파일 실패): {p:?} — {e}");
                    None
                }
            })
            .collect();

        let mut items = Vec::new();

        for dir in &self.config.directories {
            // 등록된 루트는 canonicalize로 심볼릭 링크를 해소해 따른다(명시 등록이므로 의도된 것).
            let root = match fs::canonicalize(dir).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("커넥터 대상 디렉토리 접근 불가(스킵): {dir:?} — {e}");
                    continue;
                }
            };
            self.scan_dir(&root, after, &exclude, &mut items).await;
        }

        Ok(FetchResult {
            items,
            next_cursor: Some(scan_start.to_rfc3339()),
        })
    }
}

impl LocalDirectoryConnector {
    /// 루트 아래를 명시적 스택으로 순회한다(async 재귀 boxing 회피).
    /// 순회 중 발견되는 심볼릭 링크는 따르지 않는다(등록 범위 밖 읽기 차단).
    async fn scan_dir(
        &self,
        root: &Path,
        after: Option<DateTime<Utc>>,
        exclude: &[glob::Pattern],
        items: &mut Vec<ConnectorItem>,
    ) {
        let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];

        while let Some(dir) = stack.pop() {
            let mut read = match fs::read_dir(&dir).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("디렉토리 읽기 실패(스킵): {dir:?} — {e}");
                    continue;
                }
            };

            loop {
                let entry = match read.next_entry().await {
                    Ok(Some(e)) => e,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!("디렉토리 항목 열거 실패(스킵): {dir:?} — {e}");
                        break;
                    }
                };

                let path = entry.path();

                // symlink_metadata는 링크를 따르지 않고 링크 자체의 메타데이터를 준다.
                let meta = match fs::symlink_metadata(&path).await {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("파일 메타데이터 조회 실패(스킵): {path:?} — {e}");
                        continue;
                    }
                };

                // 순회 중 발견된 심볼릭 링크는 따르지 않는다(등록 범위 밖 읽기 차단).
                if meta.file_type().is_symlink() {
                    continue;
                }

                if meta.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !meta.is_file() {
                    continue;
                }

                // 값싼 필터부터: 확장자 → 제외 패턴 → 크기 → 수정 시각.
                if !matches_extension(&path, &self.config.extensions) {
                    continue;
                }
                if is_excluded(&path, exclude) {
                    continue;
                }
                if meta.len() > self.config.max_file_bytes {
                    tracing::warn!(
                        "크기 상한({} bytes) 초과 파일 스킵: {path:?} ({} bytes)",
                        self.config.max_file_bytes,
                        meta.len()
                    );
                    continue;
                }
                let modified: DateTime<Utc> = match meta.modified() {
                    Ok(t) => t.into(),
                    Err(e) => {
                        tracing::warn!("수정 시각 조회 실패(스킵): {path:?} — {e}");
                        continue;
                    }
                };
                if !is_modified_after(modified, after) {
                    continue;
                }

                // 원문 읽기 — 깨진 인코딩은 스킵·기록(장애 격리).
                let bytes = match fs::read(&path).await {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!("파일 읽기 실패(스킵): {path:?} — {e}");
                        continue;
                    }
                };
                let content = match String::from_utf8(bytes) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("UTF-8 디코딩 실패(스킵): {path:?} — {e}");
                        continue;
                    }
                };

                items.push(ConnectorItem {
                    source_id: path.to_string_lossy().into_owned(),
                    content,
                    modified_at: modified,
                });
            }
        }
    }
}

/// 파일 확장자가 포함 목록에 있는지(대소문자·선행 점 무시).
pub(crate) fn matches_extension(path: &Path, extensions: &[String]) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let lower = ext.to_lowercase();
            extensions
                .iter()
                .any(|e| e.trim_start_matches('.').to_lowercase() == lower)
        }
        None => false,
    }
}

/// 경로가 제외 패턴에 걸리는지. 전체 경로 매칭 또는 파일명 매칭 중 하나라도 걸리면 제외.
/// (`**/node_modules/**`는 전체 경로로, `*.tmp`는 파일명으로 매칭되게 하는 관용.)
pub(crate) fn is_excluded(path: &Path, patterns: &[glob::Pattern]) -> bool {
    let name = path.file_name().and_then(|n| n.to_str());
    patterns.iter().any(|p| {
        p.matches_path(path) || name.is_some_and(|n| p.matches(n))
    })
}

/// 수정 시각이 커서 이후인지. 커서 None(초기 적재)이면 항상 포함.
pub(crate) fn is_modified_after(modified: DateTime<Utc>, after: Option<DateTime<Utc>>) -> bool {
    match after {
        Some(cursor) => modified > cursor,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    fn config(dir: &Path) -> LocalDirectoryConfig {
        LocalDirectoryConfig {
            directories: vec![dir.to_string_lossy().into_owned()],
            extensions: vec!["md".to_string(), "txt".to_string()],
            exclude: vec![],
            max_file_bytes: 1_048_576,
        }
    }

    async fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.unwrap();
        }
        fs::write(path, content).await.unwrap();
    }

    fn ids(result: &FetchResult) -> Vec<String> {
        result
            .items
            .iter()
            .map(|i| {
                Path::new(&i.source_id)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    // ──── 순수 헬퍼 ────

    #[test]
    fn test_matches_extension() {
        let exts = vec!["md".to_string(), "txt".to_string()];
        assert!(matches_extension(Path::new("/a/b.md"), &exts));
        assert!(matches_extension(Path::new("/a/b.MD"), &exts), "대소문자 무시");
        assert!(matches_extension(Path::new("/a/b.txt"), &exts));
        assert!(!matches_extension(Path::new("/a/b.log"), &exts));
        assert!(!matches_extension(Path::new("/a/noext"), &exts));
    }

    #[test]
    fn test_is_excluded_by_name() {
        let pats = vec![glob::Pattern::new("*.tmp").unwrap()];
        assert!(is_excluded(Path::new("/notes/scratch.tmp"), &pats));
        assert!(!is_excluded(Path::new("/notes/keep.md"), &pats));
    }

    #[test]
    fn test_is_excluded_by_path() {
        let pats = vec![glob::Pattern::new("**/node_modules/**").unwrap()];
        assert!(is_excluded(Path::new("/proj/node_modules/pkg/readme.md"), &pats));
        assert!(!is_excluded(Path::new("/proj/src/readme.md"), &pats));
    }

    #[test]
    fn test_is_modified_after() {
        let base = Utc::now();
        let earlier = base - chrono::Duration::seconds(10);
        let later = base + chrono::Duration::seconds(10);
        assert!(is_modified_after(later, Some(base)));
        assert!(!is_modified_after(earlier, Some(base)));
        assert!(!is_modified_after(base, Some(base)), "동일 시각은 이후가 아님(재유입 안 함)");
        assert!(is_modified_after(earlier, None), "커서 없으면 항상 포함(초기 적재)");
    }

    // ──── 스캔 (tempdir) ────

    #[tokio::test]
    async fn test_scan_includes_matching_extensions() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.md"), "# A").await;
        write(&tmp.path().join("b.txt"), "B").await;
        write(&tmp.path().join("c.log"), "C").await; // 확장자 불일치 → 제외

        let connector = LocalDirectoryConnector::new(config(tmp.path()));
        let result = connector.fetch_changes(None).await.unwrap();

        let mut names = ids(&result);
        names.sort();
        assert_eq!(names, vec!["a.md", "b.txt"]);
    }

    #[tokio::test]
    async fn test_scan_recurses_into_subdirs() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("top.md"), "top").await;
        write(&tmp.path().join("sub/nested.md"), "nested").await;

        let connector = LocalDirectoryConnector::new(config(tmp.path()));
        let result = connector.fetch_changes(None).await.unwrap();

        let mut names = ids(&result);
        names.sort();
        assert_eq!(names, vec!["nested.md", "top.md"], "하위 디렉토리까지 순회해야 한다");
    }

    #[tokio::test]
    async fn test_scan_respects_exclude_pattern() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("keep.md"), "keep").await;
        write(&tmp.path().join("skip.tmp.md"), "skip").await;

        let mut cfg = config(tmp.path());
        cfg.exclude = vec!["*.tmp.md".to_string()];
        let connector = LocalDirectoryConnector::new(cfg);
        let result = connector.fetch_changes(None).await.unwrap();

        assert_eq!(ids(&result), vec!["keep.md"]);
    }

    #[tokio::test]
    async fn test_scan_skips_oversize_files() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("small.md"), "ok").await;
        write(&tmp.path().join("big.md"), &"x".repeat(500)).await;

        let mut cfg = config(tmp.path());
        cfg.max_file_bytes = 100; // 500바이트 파일은 초과
        let connector = LocalDirectoryConnector::new(cfg);
        let result = connector.fetch_changes(None).await.unwrap();

        assert_eq!(ids(&result), vec!["small.md"], "크기 상한 초과 파일은 스킵");
    }

    #[tokio::test]
    async fn test_scan_incremental_by_mtime() {
        // 파일의 실제 mtime을 읽어 커서를 그 전후로 설정해 증분 필터를 결정적으로 검증.
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("note.md");
        write(&file, "content").await;
        let mtime: DateTime<Utc> = fs::metadata(&file).await.unwrap().modified().unwrap().into();

        let connector = LocalDirectoryConnector::new(config(tmp.path()));

        // 커서가 mtime 이후 → 변경분 없음
        let after = (mtime + chrono::Duration::seconds(1)).to_rfc3339();
        let r1 = connector.fetch_changes(Some(&after)).await.unwrap();
        assert!(r1.items.is_empty(), "커서 이후 수정 없으면 유입 안 함");

        // 커서가 mtime 이전 → 파일 포함
        let before = (mtime - chrono::Duration::seconds(1)).to_rfc3339();
        let r2 = connector.fetch_changes(Some(&before)).await.unwrap();
        assert_eq!(ids(&r2), vec!["note.md"]);
    }

    #[tokio::test]
    async fn test_next_cursor_is_rfc3339() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.md"), "a").await;
        let connector = LocalDirectoryConnector::new(config(tmp.path()));
        let result = connector.fetch_changes(None).await.unwrap();
        let cursor = result.next_cursor.expect("next_cursor가 있어야 한다");
        assert!(DateTime::parse_from_rfc3339(&cursor).is_ok(), "커서는 RFC3339여야 한다");
    }

    #[tokio::test]
    async fn test_corrupt_cursor_degrades_to_full_scan() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.md"), "a").await;
        let connector = LocalDirectoryConnector::new(config(tmp.path()));
        // 파싱 불가한 커서 → 전체 스캔으로 강등(에러 아님)
        let result = connector.fetch_changes(Some("not-a-timestamp")).await.unwrap();
        assert_eq!(ids(&result), vec!["a.md"]);
    }

    #[tokio::test]
    async fn test_nonexistent_directory_is_skipped_not_error() {
        let connector = LocalDirectoryConnector::new(LocalDirectoryConfig {
            directories: vec!["/no/such/dir/maia-test".to_string()],
            extensions: vec!["md".to_string()],
            exclude: vec![],
            max_file_bytes: 1024,
        });
        // 존재하지 않는 디렉토리는 스킵하고 빈 결과(전체 실패 아님).
        let result = connector.fetch_changes(None).await.unwrap();
        assert!(result.items.is_empty());
        assert!(result.next_cursor.is_some());
    }

    #[tokio::test]
    async fn test_source_id_is_absolute_path() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.md"), "a").await;
        let connector = LocalDirectoryConnector::new(config(tmp.path()));
        let result = connector.fetch_changes(None).await.unwrap();
        assert_eq!(result.items.len(), 1);
        assert!(
            Path::new(&result.items[0].source_id).is_absolute(),
            "source_id는 절대 경로여야 한다(안정적 식별자)"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_scan_does_not_follow_symlinks() {
        // 순회 중 발견된 심볼릭 링크는 따르지 않아 등록 범위 밖을 읽지 않는다.
        let outside = TempDir::new().unwrap();
        write(&outside.path().join("secret.md"), "secret").await;

        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("real.md"), "real").await;
        // tmp 안에 outside를 가리키는 심볼릭 링크 디렉토리 생성
        std::os::unix::fs::symlink(outside.path(), tmp.path().join("link")).unwrap();

        let connector = LocalDirectoryConnector::new(config(tmp.path()));
        let result = connector.fetch_changes(None).await.unwrap();

        // 심볼릭 링크 너머의 secret.md는 유입되지 않아야 한다.
        assert_eq!(ids(&result), vec!["real.md"]);
    }
}
