use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub server_port: u16,
    pub qdrant_url: String,
    pub data_dir: String,
    /// 마스터 API 키 (MAIA_API_KEY). 이제 선택적 부트스트랩 수단 —
    /// 미설정이어도 인증은 활성이다 (users/api_keys/세션으로 동작).
    pub api_key: Option<String>,
    /// MAIA_DEV_NO_AUTH=1일 때만 true — 명시적 옵트인 개발 모드(인증 전체 skip).
    /// 과거 "마스터키 미설정 = 전체 개방"(fail-open)을 대체한다.
    pub dev_no_auth: bool,
    /// 세션 쿠키 Secure 플래그 (MAIA_COOKIE_SECURE). 기본 on —
    /// http 로컬 개발에서만 "0"/"false"로 끈다.
    pub cookie_secure: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        Ok(Self {
            server_port: std::env::var("SERVER_PORT")
                .unwrap_or_else(|_| "8080".to_string())
                .parse()
                .context("Invalid SERVER_PORT")?,
            qdrant_url: std::env::var("QDRANT_URL")
                .unwrap_or_else(|_| "http://localhost:6333".to_string()),
            data_dir: std::env::var("DATA_DIR")
                .unwrap_or_else(|_| "./data".to_string()),
            api_key: std::env::var("MAIA_API_KEY").ok().filter(|k| !k.is_empty()),
            dev_no_auth: flag_enabled(std::env::var("MAIA_DEV_NO_AUTH").ok().as_deref()),
            cookie_secure: secure_default_on(std::env::var("MAIA_COOKIE_SECURE").ok().as_deref()),
        })
    }
}

/// 옵트인 플래그 해석: "1"/"true"(대소문자 무관)만 참.
/// 미설정·그 외 값은 거짓 — 안전 기본값(인증 활성) 유지.
fn flag_enabled(val: Option<&str>) -> bool {
    matches!(
        val.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true")
    )
}

/// 옵트아웃 플래그 해석: "0"/"false"(대소문자 무관)만 거짓.
/// 미설정·그 외 값은 참 — 안전 기본값(Secure 쿠키) 유지.
fn secure_default_on(val: Option<&str>) -> bool {
    !matches!(
        val.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("0") | Some("false")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flag_enabled_opt_in_only() {
        // 명시적 "1"/"true"만 개발 모드를 연다 (fail-open 재발 방지의 핵심 계약)
        assert!(flag_enabled(Some("1")));
        assert!(flag_enabled(Some("true")));
        assert!(flag_enabled(Some("TRUE")));

        assert!(!flag_enabled(None));
        assert!(!flag_enabled(Some("")));
        assert!(!flag_enabled(Some("0")));
        assert!(!flag_enabled(Some("false")));
        assert!(!flag_enabled(Some("yes"))); // 애매한 값은 안전한 쪽(거짓)으로
    }

    #[test]
    fn test_secure_default_on() {
        // 미설정이면 Secure on (프로덕션 안전 기본값)
        assert!(secure_default_on(None));
        assert!(secure_default_on(Some("1")));
        assert!(secure_default_on(Some("anything")));

        // 명시적으로만 끌 수 있다 (http 로컬 개발용)
        assert!(!secure_default_on(Some("0")));
        assert!(!secure_default_on(Some("false")));
        assert!(!secure_default_on(Some("FALSE")));
    }
}
