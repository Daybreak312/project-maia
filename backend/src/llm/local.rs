//! 로컬 임베딩 provider — 외부 호출 없이 임베딩을 생성한다.
//!
//! fastembed(-rs)의 `multilingual-e5-small`(384차원, 다국어)를 쓴다. 코퍼스가
//! 한국어 중심이라 다국어 모델을 택했고, 개인 규모(수십~수천 문서)에서 정밀도가
//! 충분하다(상위 모델 승격은 백로그). 모델 파일은 `DATA_DIR/models`에 캐시되어
//! 도커 볼륨에 영속되므로 재기동 시 재다운로드하지 않는다.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

use super::{EmbeddingProvider, ProviderType};

/// multilingual-e5-small 임베딩 차원.
pub const LOCAL_EMBED_DIM: usize = 384;

/// 로컬 임베딩 모델 식별자(settings 응답·UI 표시용). 고정 모델.
pub const LOCAL_EMBED_MODEL: &str = "multilingual-e5-small";

/// e5 계열 **문서(passage)** 접두. 문서/쿼리 비대칭 임베딩 규약(성능 핵심).
fn e5_passage(text: &str) -> String {
    format!("passage: {text}")
}

/// e5 계열 **쿼리** 접두.
fn e5_query(text: &str) -> String {
    format!("query: {text}")
}

/// 로컬 임베딩 provider. 모델은 첫 embed 호출 시 lazy 로드/다운로드된다.
///
/// fastembed `TextEmbedding::embed`가 `&mut self`를 요구하므로 `Mutex`로 내부
/// 가변성을 감싼다. 초기화·추론은 블로킹 CPU 작업이라 `spawn_blocking`으로 런타임
/// 워커 스레드를 막지 않는다.
pub struct LocalEmbeddingProvider {
    cache_dir: PathBuf,
    model: OnceCell<Arc<Mutex<TextEmbedding>>>,
}

impl LocalEmbeddingProvider {
    /// `cache_dir`은 모델 파일 캐시 위치(보통 `DATA_DIR/models`).
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            model: OnceCell::new(),
        }
    }

    /// lazy 초기화 — 최초 호출에서만 모델을 로드/다운로드한다. 초기화 실패는
    /// 명확한 에러로 반환되어 상위 raw 폴백 불변식이 그대로 발동한다.
    async fn model(&self) -> Result<Arc<Mutex<TextEmbedding>>> {
        self.model
            .get_or_try_init(|| async {
                let cache = self.cache_dir.clone();
                let model = tokio::task::spawn_blocking(move || {
                    // 캐시 디렉토리 보장(도커 볼륨 영속).
                    let _ = std::fs::create_dir_all(&cache);
                    tracing::info!(
                        "로컬 임베딩 모델 로드 시작 (multilingual-e5-small, cache={})",
                        cache.display()
                    );
                    TextEmbedding::try_new(
                        InitOptions::new(EmbeddingModel::MultilingualE5Small)
                            .with_cache_dir(cache)
                            .with_show_download_progress(true),
                    )
                })
                .await
                .context("로컬 임베딩 모델 초기화 태스크 실패")?
                .context("multilingual-e5-small 로드 실패 (모델 다운로드/캐시를 확인하세요)")?;
                tracing::info!("로컬 임베딩 모델 로드 완료");
                Ok::<_, anyhow::Error>(Arc::new(Mutex::new(model)))
            })
            .await
            .cloned()
    }

    /// 접두가 붙은 텍스트를 임베딩한다(passage/query 공용 실행 경로).
    async fn embed_prefixed(&self, prefixed: String) -> Result<Vec<f32>> {
        let model = self.model().await?;
        let mut vectors = tokio::task::spawn_blocking(move || {
            let mut guard = model
                .lock()
                .map_err(|_| anyhow!("임베딩 모델 락이 오염되었습니다"))?;
            guard
                .embed(vec![prefixed], None)
                .context("로컬 임베딩 추론 실패")
        })
        .await
        .context("로컬 임베딩 태스크 실패")??;
        // 단건 입력 → 단건 출력.
        if vectors.is_empty() {
            return Err(anyhow!("로컬 임베딩 결과가 비어 있습니다"));
        }
        Ok(vectors.swap_remove(0))
    }
}

#[async_trait]
impl EmbeddingProvider for LocalEmbeddingProvider {
    fn provider_type(&self) -> ProviderType {
        ProviderType::Local
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_prefixed(e5_passage(text)).await
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_prefixed(e5_query(text)).await
    }

    fn dimension(&self) -> usize {
        LOCAL_EMBED_DIM
    }

    async fn validate_api_key(&self) -> Result<bool> {
        // local은 키 불요 — "모델 로드 + 1회 임베딩 + 차원 확인"으로 검증(FR3).
        let vector = self.embed("ping").await?;
        Ok(vector.len() == LOCAL_EMBED_DIM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_e5_prefixes() {
        // e5 규약: 문서는 passage:, 쿼리는 query: 접두.
        assert_eq!(e5_passage("hello world"), "passage: hello world");
        assert_eq!(e5_query("무엇을 찾니"), "query: 무엇을 찾니");
    }

    #[test]
    fn test_dimension_constant() {
        let provider = LocalEmbeddingProvider::new(PathBuf::from("/tmp/does-not-matter"));
        assert_eq!(provider.dimension(), 384);
        assert_eq!(LOCAL_EMBED_DIM, 384);
    }

    /// 실제 모델 로드 + 임베딩 통합 테스트. 모델 다운로드(수백 MB)가 필요하므로
    /// 기본 `cargo test`에서는 제외한다(외부 의존 없이 통과 불변식). 실행:
    /// `cargo test --release test_local_embed_smoke -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn test_local_embed_smoke() {
        let dir = tempfile::tempdir().unwrap();
        let provider = LocalEmbeddingProvider::new(dir.path().join("models"));

        let passage = provider.embed("한국어 문서 임베딩 테스트").await.unwrap();
        assert_eq!(passage.len(), LOCAL_EMBED_DIM);

        let query = provider.embed_query("임베딩").await.unwrap();
        assert_eq!(query.len(), LOCAL_EMBED_DIM);

        assert!(provider.validate_api_key().await.unwrap());
    }
}
