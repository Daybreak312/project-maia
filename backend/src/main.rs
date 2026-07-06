mod api;
mod auth;
mod config;
mod core;
mod llm;
mod models;
mod settings;
mod storage;
mod workspace;

use std::sync::Arc;

use axum::{
    middleware,
    routing::{delete, get, post, put},
    Router,
};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use auth::ApiKeyManager;
use config::Config;
use core::Indexer;
use settings::SettingsManager;
use storage::{DocumentStore, QdrantStorage, SearchLogStore, VersionStore};
use workspace::WorkspaceManager;

/// 애플리케이션 상태
pub struct AppState {
    pub indexer: Indexer,
    pub settings: Arc<SettingsManager>,
    /// 마스터 API 키 (MAIA_API_KEY). None이면 인증 비활성(개발 모드).
    pub api_key: Option<String>,
    /// 워크스페이스 CRUD 관리자
    pub workspaces: Arc<WorkspaceManager>,
    /// API 키 발급/조회/인증 관리자
    pub api_keys: Arc<ApiKeyManager>,
    /// 검색 로그 저장소 (워크스페이스별 일 단위 JSONL 축적)
    pub search_logs: Arc<SearchLogStore>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 로깅 초기화
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("maia=info".parse()?),
        )
        .init();

    // 설정 로드
    let config = Config::from_env()?;
    tracing::info!("Starting Maia on port {}", config.server_port);

    // 설정 관리자 초기화
    let settings = Arc::new(SettingsManager::new(&config.data_dir).await?);

    // 워크스페이스 관리자 초기화 + default 워크스페이스 보장(레거시 마이그레이션 포함)
    let workspaces = Arc::new(WorkspaceManager::new(&config.data_dir).await?);
    workspaces.ensure_default().await?;

    // API 키 관리자 초기화 (파일 기반, Qdrant 독립)
    let api_keys = Arc::new(ApiKeyManager::new(&config.data_dir).await?);

    // 스토리지 초기화
    // DocumentStore는 data_dir에 루팅되어 `{data_dir}/workspaces/{id}/documents`에 저장한다
    // (WorkspaceManager와 동일 루트 — 경로 정합성 보장).
    let qdrant = Arc::new(QdrantStorage::new(&config.qdrant_url).await?);
    let documents = Arc::new(DocumentStore::new(&config.data_dir).await?);
    // 업데이트 시 이전 문서 상태를 보관하는 버전 저장소 (동일 data_dir 루트 공유).
    let versions = Arc::new(VersionStore::new(&config.data_dir));
    // 검색 로그 저장소 (동일 data_dir 루트 공유, 워크스페이스별 일 단위 JSONL).
    let search_logs = Arc::new(SearchLogStore::new(&config.data_dir));

    // Indexer 초기화
    let indexer = Indexer::new(settings.clone(), qdrant, documents, versions);

    // AppState 생성
    if config.api_key.is_some() {
        tracing::info!("API key authentication enabled");
    } else {
        tracing::warn!("API key not set (MAIA_API_KEY). All endpoints are open.");
    }
    let state = Arc::new(AppState {
        indexer,
        settings,
        api_key: config.api_key,
        workspaces,
        api_keys,
        search_logs,
    });

    // 인증이 필요한 API 라우트
    let api_routes = Router::new()
        .route("/ingest", post(api::ingest_handler))
        .route("/search", post(api::search_handler))
        .route("/documents/:id", get(api::get_document_handler))
        .route("/documents/:id", put(api::update_document_handler))
        .route("/documents/:id", delete(api::delete_document_handler))
        // 그래프: 이웃 조회 + 수동 엣지 추가/제거
        .route("/documents/:id/neighbors", get(api::neighbors_handler))
        .route("/documents/:id/edges", post(api::add_edge_handler))
        .route("/documents/:id/edges/:target", delete(api::remove_edge_handler))
        .route("/recent", get(api::recent_handler))
        .route("/api/reindex", post(api::reindex_handler))
        .route("/api/settings", get(api::settings::get_settings))
        .route("/api/settings", put(api::settings::update_settings))
        .route(
            "/api/settings/models/:provider/key",
            post(api::settings::set_api_key),
        )
        .route(
            "/api/settings/models/:provider/key",
            delete(api::settings::delete_api_key),
        )
        .route(
            "/api/settings/models/:provider/test",
            post(api::settings::test_api_key),
        )
        // 워크스페이스 관리 (admin 전용 — 핸들러에서 강제)
        .route("/api/workspaces", get(api::list_workspaces_handler))
        .route("/api/workspaces", post(api::create_workspace_handler))
        .route("/api/workspaces/:id", get(api::get_workspace_handler))
        .route("/api/workspaces/:id", delete(api::delete_workspace_handler))
        // API 키 관리 (admin 전용 — 핸들러에서 강제)
        .route("/api/keys", get(api::list_keys_handler))
        .route("/api/keys", post(api::create_key_handler))
        .route("/api/keys/:key_id", delete(api::revoke_key_handler))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ));

    // 인증 불필요한 라우트
    let public_routes = Router::new()
        .route("/health", get(health_handler));

    // 정적 파일 서빙 (환경변수 우선, 없으면 기본 경로)
    let static_dir = std::env::var("STATIC_DIR")
        .unwrap_or_else(|_| format!("{}/static", env!("CARGO_MANIFEST_DIR")));

    let app = Router::new()
        .merge(api_routes)
        .merge(public_routes)
        .fallback_service(ServeDir::new(&static_dir).append_index_html_on_directories(true))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // 서버 시작
    let addr = format!("0.0.0.0:{}", config.server_port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Server listening on {}", addr);
    tracing::info!("Static files served from {}", static_dir);

    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_handler() -> &'static str {
    "OK"
}
