mod api;
mod auth;
mod config;
mod connectors;
mod core;
mod llm;
mod models;
mod patrol;
mod settings;
mod storage;
mod workspace;

use std::sync::Arc;

use axum::{
    middleware,
    routing::{delete, get, patch, post, put},
    Router,
};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use auth::{ApiKeyManager, AuthEngine, SessionManager, UserManager};
use config::Config;
use connectors::runner::ConnectorRunner;
use connectors::scheduler::ConnectorScheduler;
use connectors::sync_state::SyncStateStore;
use core::Indexer;
use patrol::feedback::FeedbackStore;
use patrol::freshness::FreshnessStore;
use patrol::history::PatrolHistoryStore;
use patrol::metrics::MetricsStore;
use patrol::review::ReviewQueueStore;
use patrol::scheduler::PatrolScheduler;
use patrol::{Patrol, PatrolExecutor};
use settings::SettingsManager;
use storage::{DocumentStore, QdrantStorage, SearchLogStore, VersionStore};
use workspace::{MembershipManager, WorkspaceManager};

/// 애플리케이션 상태
pub struct AppState {
    /// 인덱싱·검색 오케스트레이터. 커넥터 유입 실행기(ConnectorIngest)로도 공유되므로 Arc.
    pub indexer: Arc<Indexer>,
    pub settings: Arc<SettingsManager>,
    /// 인증 자격증명 해석기 (마스터키·API 키·세션 쿠키 → AuthContext)
    pub auth: AuthEngine,
    /// 워크스페이스 CRUD 관리자
    pub workspaces: Arc<WorkspaceManager>,
    /// API 키 발급/조회/인증 관리자
    pub api_keys: Arc<ApiKeyManager>,
    /// 계정 관리자 (users.json)
    pub users: Arc<UserManager>,
    /// 로그인 세션 관리자 (sessions.json)
    pub sessions: Arc<SessionManager>,
    /// 워크스페이스 멤버십·공개 설정 관리자 (workspaces/{id}/members.json)
    pub memberships: Arc<MembershipManager>,
    /// 세션 쿠키 Secure 플래그 (로그인/로그아웃 핸들러의 Set-Cookie에 반영)
    pub cookie_secure: bool,
    /// 검색 로그 저장소 (워크스페이스별 일 단위 JSONL 축적)
    pub search_logs: Arc<SearchLogStore>,
    /// 커넥터 동기화 실행기 (수동 트리거·상태 조회가 공유)
    pub connector_runner: Arc<ConnectorRunner>,
    /// 커넥터 동기화 상태 저장소 (상태 조회·삭제)
    pub sync_state: Arc<SyncStateStore>,
    /// Patrol 거버넌스 파사드 (실행/판단/피드백/메트릭 — Phase 5)
    pub patrol: Arc<Patrol>,
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

    // 계정·세션·멤버십 관리자 초기화 (파일 기반, Qdrant 독립)
    let users = Arc::new(UserManager::new(&config.data_dir).await?);
    let sessions = Arc::new(SessionManager::new(&config.data_dir).await?);
    let memberships = Arc::new(MembershipManager::new(&config.data_dir));

    // 스토리지 초기화
    // DocumentStore는 data_dir에 루팅되어 `{data_dir}/workspaces/{id}/documents`에 저장한다
    // (WorkspaceManager와 동일 루트 — 경로 정합성 보장).
    let qdrant = Arc::new(QdrantStorage::new(&config.qdrant_url).await?);
    let documents = Arc::new(DocumentStore::new(&config.data_dir).await?);
    // 업데이트 시 이전 문서 상태를 보관하는 버전 저장소 (동일 data_dir 루트 공유).
    let versions = Arc::new(VersionStore::new(&config.data_dir));
    // 검색 로그 저장소 (동일 data_dir 루트 공유, 워크스페이스별 일 단위 JSONL).
    let search_logs = Arc::new(SearchLogStore::new(&config.data_dir));
    // 커넥터 동기화 상태 저장소 (동일 data_dir 루트 공유).
    let sync_state = Arc::new(SyncStateStore::new(&config.data_dir));

    // Indexer 초기화 — 커넥터 유입 실행기로도 공유되므로 Arc로 감싼다.
    let indexer = Arc::new(Indexer::new(settings.clone(), qdrant, documents, versions));

    // 커넥터 실행기 — Indexer를 ConnectorIngest로 주입. 스케줄러/API가 공유한다.
    let connector_runner = Arc::new(ConnectorRunner::new(
        workspaces.clone(),
        indexer.clone(),
        sync_state.clone(),
    ));

    // 커넥터 스케줄러 기동 — 백그라운드 태스크. 실행 오류는 격리되어 서버를 죽이지 않는다.
    let scheduler = Arc::new(ConnectorScheduler::new(
        connector_runner.clone(),
        workspaces.clone(),
        sync_state.clone(),
    ));
    scheduler.start();

    // Patrol(Phase 5) 저장소 + 오케스트레이터 + 스케줄러 — 모두 동일 data_dir 루트 공유.
    let reviews = Arc::new(ReviewQueueStore::new(&config.data_dir));
    let feedback = Arc::new(FeedbackStore::new(&config.data_dir));
    let freshness = Arc::new(FreshnessStore::new(&config.data_dir));
    let metrics = Arc::new(MetricsStore::new(&config.data_dir));
    let patrol_history = Arc::new(PatrolHistoryStore::new(&config.data_dir));
    // Indexer를 문서 실행기로 주입(전 문서 조회·복구 가능 삭제·엣지 감쇠).
    let patrol_executor: Arc<dyn PatrolExecutor> = indexer.clone();
    let patrol = Arc::new(Patrol::new(
        patrol_executor,
        workspaces.clone(),
        search_logs.clone(),
        sync_state.clone(),
        reviews,
        feedback,
        freshness,
        metrics,
        patrol_history.clone(),
    ));
    // Patrol 스케줄러 기동 — 워크스페이스 patrol 주기로 자동 실행(오류 격리).
    let patrol_scheduler = Arc::new(PatrolScheduler::new(
        patrol.clone(),
        workspaces.clone(),
        patrol_history,
    ));
    patrol_scheduler.start();

    // 인증 엔진 조립 — 해석 순서: dev 옵트인 → 마스터키 → API 키 → 세션 쿠키.
    // 마스터키 미설정은 더 이상 fail-open이 아니다 (users/api_keys/세션으로 동작).
    if config.dev_no_auth {
        tracing::warn!(
            "MAIA_DEV_NO_AUTH=1 — 인증이 비활성화되었습니다 (명시적 개발 모드, 프로덕션 금지)"
        );
    } else if config.api_key.is_some() {
        tracing::info!("Authentication enabled (master key + users/api keys/sessions)");
    } else {
        tracing::info!("Authentication enabled (users/api keys/sessions — master key not set)");
    }
    let auth_engine = AuthEngine::new(
        config.api_key.clone(),
        config.dev_no_auth,
        users.clone(),
        sessions.clone(),
        api_keys.clone(),
        memberships.clone(),
        workspaces.clone(),
    );
    // 인증 수단 전무 상태 안내 (잠금은 유지 — fail-closed)
    auth_engine.warn_if_locked_out().await;

    let state = Arc::new(AppState {
        indexer,
        settings,
        auth: auth_engine,
        workspaces,
        api_keys,
        users,
        sessions,
        memberships,
        cookie_secure: config.cookie_secure,
        search_logs,
        connector_runner,
        sync_state,
        patrol,
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
        // Codex는 키가 아니라 auth.json 임포트로 활성화된다(파싱 전용 구독 provider).
        .route(
            "/api/settings/models/codex/import",
            post(api::settings::import_codex),
        )
        // 워크스페이스 관리 (admin 전용 — 핸들러에서 강제)
        .route("/api/workspaces", get(api::list_workspaces_handler))
        .route("/api/workspaces", post(api::create_workspace_handler))
        .route("/api/workspaces/:id", get(api::get_workspace_handler))
        .route("/api/workspaces/:id", delete(api::delete_workspace_handler))
        .route(
            "/api/workspaces/:id/patrol",
            patch(api::update_workspace_patrol_handler),
        )
        // 워크스페이스 멤버십·공개 설정 (글로벌 admin 또는 해당 ws admin — 핸들러에서 강제)
        .route("/api/workspaces/:id/members", get(api::list_members_handler))
        .route("/api/workspaces/:id/members/:user_id", put(api::upsert_member_handler))
        .route("/api/workspaces/:id/members/:user_id", delete(api::remove_member_handler))
        .route("/api/workspaces/:id/visibility", put(api::set_visibility_handler))
        // 커넥터 관리 (목록·상태는 워크스페이스 접근, 등록·삭제·실행은 admin — 핸들러에서 강제)
        .route("/api/connectors", get(api::list_connectors_handler))
        .route("/api/connectors", post(api::register_connector_handler))
        .route("/api/connectors/:id", delete(api::delete_connector_handler))
        .route("/api/connectors/:id/status", get(api::connector_status_handler))
        .route("/api/connectors/:id/sync", post(api::trigger_connector_handler))
        // API 키 관리 (admin 전용 — 핸들러에서 강제)
        .route("/api/keys", get(api::list_keys_handler))
        .route("/api/keys", post(api::create_key_handler))
        .route("/api/keys/:key_id", delete(api::revoke_key_handler))
        // 계정 관리 (admin 전용, 비밀번호 변경만 본인 세션 허용 — 핸들러에서 강제)
        .route("/api/users", get(api::list_users_handler))
        .route("/api/users", post(api::create_user_handler))
        .route("/api/users/:id", delete(api::delete_user_handler))
        .route("/api/users/:id/password", put(api::change_password_handler))
        // 현재 인증 주체 정보 (모든 인증 소스 허용)
        .route("/api/auth/me", get(api::me_handler))
        // Patrol·거버넌스 (Phase 5): 실행/판단/피드백은 write, 조회는 워크스페이스 접근
        .route("/api/patrol/run", post(api::run_patrol_handler))
        .route("/api/patrol/history", get(api::patrol_history_handler))
        .route("/api/review", get(api::list_reviews_handler))
        .route("/api/review/judge", post(api::judge_reviews_handler))
        .route("/api/feedback", post(api::submit_feedback_handler))
        .route("/api/metrics", get(api::metrics_handler))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    // 인증 불필요한 라우트.
    // login: 자격증명 획득 자체가 목적. logout: 무효 세션이어도 쿠키를 지워야
    // 하므로 인증 뒤에 두지 않는다 (핸들러가 쿠키의 세션만 폐기 — 멱등).
    let public_routes = Router::new()
        .route("/health", get(health_handler))
        .route("/api/auth/login", post(api::login_handler))
        .route("/api/auth/logout", post(api::logout_handler));

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
