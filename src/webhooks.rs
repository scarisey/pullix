use std::str::FromStr;

use crate::{
    config::{WebhookConfig, WebhooksConfig},
    deploy::Deployed,
};
use anyhow::{Context, Result};
use reqwest::{
    self, Method,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use tracing::info;

/// Replaces every `<VAR_NAME>` placeholder (uppercase letters, digits,
/// underscores; must start with a letter) with the value of the corresponding
/// environment variable.  Returns an error if a referenced variable is unset.
/// Angle-bracket sequences that do not match the pattern are left unchanged.
pub fn expand_env_vars(s: &str) -> Result<String> {
    let mut result = String::with_capacity(s.len());
    let mut remaining = s;

    while let Some(start) = remaining.find('<') {
        result.push_str(&remaining[..start]);
        remaining = &remaining[start + 1..];

        if let Some(end) = remaining.find('>') {
            let var_name = &remaining[..end];
            let is_env_var_name = !var_name.is_empty()
                && var_name
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_alphabetic())
                    .unwrap_or(false)
                && var_name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');

            if is_env_var_name {
                let value = std::env::var(var_name)
                    .with_context(|| format!("Environment variable '{}' not found", var_name))?;
                result.push_str(&value);
                remaining = &remaining[end + 1..];
            } else {
                // Not an env-var placeholder — emit '<' and retry from the same position.
                result.push('<');
            }
        } else {
            // No closing '>' — emit '<' and continue.
            result.push('<');
        }
    }

    result.push_str(remaining);
    Ok(result)
}

/// Parses a `"Key: Value"` header string, splitting on the first `':'`.
fn parse_header(s: &str) -> Result<(String, String)> {
    let pos = s
        .find(':')
        .ok_or_else(|| anyhow::anyhow!("Invalid header '{}': missing ':'", s))?;
    let key = s[..pos].trim().to_string();
    let value = s[pos + 1..].trim().to_string();
    Ok((key, value))
}

pub trait Webhook {
    async fn send(&self) -> Result<String>;
}
pub struct WebhookImpl {
    client: reqwest::Client,
    request: reqwest::Request,
}
impl WebhookImpl {
    pub fn new(config: &WebhookConfig) -> Result<Self> {
        let client = reqwest::Client::new();
        let url = expand_env_vars(&config.url)?;
        let mut request_builder =
            client.request(Method::from_bytes(config.method.as_bytes())?, url.as_str());
        if !config.headers.is_empty() {
            let mut headers = HeaderMap::new();
            for raw_header in &config.headers {
                let expanded = expand_env_vars(raw_header)?;
                let (key, value) = parse_header(&expanded)?;
                headers.insert(
                    HeaderName::from_str(key.as_str())?,
                    HeaderValue::from_str(value.as_str())?,
                );
            }
            request_builder = request_builder.headers(headers);
        }
        if config.method.to_uppercase() == "POST"
            && let Some(ref body) = config.data
        {
            let expanded_body = expand_env_vars(body)?;
            request_builder = request_builder.body(expanded_body);
        }
        let request = request_builder.build()?;
        Ok(Self { client, request })
    }
}
impl Webhook for WebhookImpl {
    async fn send(&self) -> Result<String> {
        let r = self
            .request
            .try_clone()
            .ok_or(anyhow::anyhow!("request is not cloneable"))?;
        let response = self.client.execute(r).await?;

        Ok(response.status().to_string())
    }
}
pub trait Webhooks {
    async fn deployed_then_call(&self, deployed: &Deployed) -> Result<()>;
}
pub struct WebhooksImpl {
    on_test_success: Option<WebhookImpl>,
    on_test_failure: Option<WebhookImpl>,
    on_prod_success: Option<WebhookImpl>,
    on_prod_failure: Option<WebhookImpl>,
}
impl WebhooksImpl {
    pub fn new(config: &WebhooksConfig) -> Result<Self> {
        Ok(Self {
            on_test_success: config
                .on_test_success
                .as_ref()
                .map(WebhookImpl::new)
                .transpose()?,
            on_test_failure: config
                .on_test_failure
                .as_ref()
                .map(WebhookImpl::new)
                .transpose()?,
            on_prod_success: config
                .on_prod_success
                .as_ref()
                .map(WebhookImpl::new)
                .transpose()?,
            on_prod_failure: config
                .on_prod_failure
                .as_ref()
                .map(WebhookImpl::new)
                .transpose()?,
        })
    }
}
impl Webhooks for WebhooksImpl {
    async fn deployed_then_call(&self, deployed: &Deployed) -> Result<()> {
        match deployed {
            Deployed::Init => (),
            Deployed::TestAligned(_) => {
                if let Some(webhook) = &self.on_test_success {
                    let response = webhook.send().await?;
                    info!("Webhook on test success called and returned {}", response);
                }
            }

            Deployed::ProdAligned(_) => {
                if let Some(webhook) = &self.on_prod_success {
                    let response = webhook.send().await?;
                    info!("Webhook on prod success called and returned {}", response);
                }
            }
            Deployed::TestFailed(_) => {
                if let Some(webhook) = &self.on_test_failure {
                    let response = webhook.send().await?;
                    info!("Webhook on test failure called and returned {}", response);
                }
            }
            Deployed::ProdFailed(_) => {
                if let Some(webhook) = &self.on_prod_failure {
                    let response = webhook.send().await?;
                    info!("Webhook on prod failure called and returned {}", response);
                }
            }
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{WebhookConfig, WebhooksConfig};
    use crate::deploy::Deployed;
    use crate::git::Commit;
    use wiremock::matchers::{body_string, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ─── expand_env_vars ─────────────────────────────────────────────────────

    #[test]
    fn expand_no_placeholders() {
        assert_eq!(
            expand_env_vars("https://example.com/hook").unwrap(),
            "https://example.com/hook"
        );
    }

    #[test]
    fn expand_single_var() {
        unsafe { std::env::set_var("PULLIX_WH_TEST_SINGLE", "my-token") };
        assert_eq!(
            expand_env_vars("Bearer <PULLIX_WH_TEST_SINGLE>").unwrap(),
            "Bearer my-token"
        );
    }

    #[test]
    fn expand_multiple_vars() {
        unsafe {
            std::env::set_var("PULLIX_WH_TEST_HOST", "example.com");
            std::env::set_var("PULLIX_WH_TEST_PATH", "webhook");
        }
        assert_eq!(
            expand_env_vars("https://<PULLIX_WH_TEST_HOST>/<PULLIX_WH_TEST_PATH>").unwrap(),
            "https://example.com/webhook"
        );
    }

    #[test]
    fn expand_missing_var_returns_error() {
        unsafe { std::env::remove_var("PULLIX_WH_TEST_MISSING") };
        let err = expand_env_vars("Bearer <PULLIX_WH_TEST_MISSING>").unwrap_err();
        assert!(err.to_string().contains("PULLIX_WH_TEST_MISSING"));
    }

    #[test]
    fn expand_preserves_lowercase_angle_brackets() {
        // "<html>" is not a valid env-var name (lowercase) → left as-is.
        assert_eq!(
            expand_env_vars("value is <not-a-var>").unwrap(),
            "value is <not-a-var>"
        );
    }

    #[test]
    fn expand_preserves_unclosed_angle_bracket() {
        assert_eq!(expand_env_vars("value < more").unwrap(), "value < more");
    }

    // ─── parse_header ─────────────────────────────────────────────────────────

    #[test]
    fn parse_header_simple() {
        let (k, v) = parse_header("Content-Type: application/json").unwrap();
        assert_eq!(k, "Content-Type");
        assert_eq!(v, "application/json");
    }

    #[test]
    fn parse_header_bearer() {
        let (k, v) = parse_header("Authorization: Bearer ghp_tok123").unwrap();
        assert_eq!(k, "Authorization");
        assert_eq!(v, "Bearer ghp_tok123");
    }

    #[test]
    fn parse_header_value_contains_colon() {
        // Only the first ':' is used as separator.
        let (k, v) = parse_header("X-Custom: http://example.com/path").unwrap();
        assert_eq!(k, "X-Custom");
        assert_eq!(v, "http://example.com/path");
    }

    #[test]
    fn parse_header_missing_colon_returns_error() {
        assert!(parse_header("InvalidHeader").is_err());
    }

    // ─── WebhookImpl construction ─────────────────────────────────────────────

    #[test]
    fn webhook_new_expands_url_env_var() {
        unsafe { std::env::set_var("PULLIX_WH_URL_HOST", "my-webhook-host") };
        let config = WebhookConfig {
            url: "https://<PULLIX_WH_URL_HOST>/hook".to_string(),
            method: "POST".to_string(),
            headers: vec![],
            data: None,
        };
        let webhook = WebhookImpl::new(&config).unwrap();
        assert_eq!(
            webhook.request.url().as_str(),
            "https://my-webhook-host/hook"
        );
    }

    #[test]
    fn webhook_new_expands_header_env_var() {
        unsafe { std::env::set_var("PULLIX_WH_HEADER_TOKEN", "ghp_secret") };
        let config = WebhookConfig {
            url: "https://example.com/hook".to_string(),
            method: "POST".to_string(),
            headers: vec!["Authorization: Bearer <PULLIX_WH_HEADER_TOKEN>".to_string()],
            data: None,
        };
        let webhook = WebhookImpl::new(&config).unwrap();
        let auth = webhook
            .request
            .headers()
            .get("Authorization")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(auth, "Bearer ghp_secret");
    }

    #[test]
    fn webhook_new_expands_data_env_var() {
        unsafe { std::env::set_var("PULLIX_WH_DATA_REV", "deadbeef") };
        let config = WebhookConfig {
            url: "https://example.com/hook".to_string(),
            method: "POST".to_string(),
            headers: vec![],
            data: Some(r#"{"rev":"<PULLIX_WH_DATA_REV>"}"#.to_string()),
        };
        let webhook = WebhookImpl::new(&config).unwrap();
        let body = webhook.request.body().and_then(|b| b.as_bytes()).unwrap();
        assert_eq!(body, br#"{"rev":"deadbeef"}"#);
    }

    #[test]
    fn webhook_new_missing_env_var_returns_error() {
        unsafe { std::env::remove_var("PULLIX_WH_ABSENT_VAR") };
        let config = WebhookConfig {
            url: "https://<PULLIX_WH_ABSENT_VAR>/hook".to_string(),
            method: "POST".to_string(),
            headers: vec![],
            data: None,
        };
        assert!(WebhookImpl::new(&config).is_err());
    }

    // ─── WebhookImpl send ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn webhook_send_post() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let config = WebhookConfig {
            url: format!("{}/hook", server.uri()),
            method: "POST".to_string(),
            headers: vec![],
            data: None,
        };
        let webhook = WebhookImpl::new(&config).unwrap();
        let status = webhook.send().await.unwrap();
        assert_eq!(status, "200 OK");
    }

    #[tokio::test]
    async fn webhook_send_get() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ping"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let config = WebhookConfig {
            url: format!("{}/ping", server.uri()),
            method: "GET".to_string(),
            headers: vec![],
            data: None,
        };
        let webhook = WebhookImpl::new(&config).unwrap();
        let status = webhook.send().await.unwrap();
        assert_eq!(status, "204 No Content");
    }

    #[tokio::test]
    async fn webhook_send_with_headers_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .and(header("content-type", "application/json"))
            .and(body_string(r#"{"event":"deploy"}"#))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let config = WebhookConfig {
            url: format!("{}/hook", server.uri()),
            method: "POST".to_string(),
            headers: vec!["content-type: application/json".to_string()],
            data: Some(r#"{"event":"deploy"}"#.to_string()),
        };
        let webhook = WebhookImpl::new(&config).unwrap();
        webhook.send().await.unwrap();
    }

    // ─── WebhooksImpl::deployed_then_call ────────────────────────────────────

    fn make_commit() -> Commit {
        Commit::from("abc123def456789012345678901234567890abcd")
    }

    fn webhook_config_for(server: &MockServer, path_str: &str) -> WebhookConfig {
        WebhookConfig {
            url: format!("{}{}", server.uri(), path_str),
            method: "POST".to_string(),
            headers: vec![],
            data: None,
        }
    }

    #[tokio::test]
    async fn webhooks_dispatches_on_test_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/test-success"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let config = WebhooksConfig {
            on_test_success: Some(webhook_config_for(&server, "/test-success")),
            ..Default::default()
        };
        let webhooks = WebhooksImpl::new(&config).unwrap();
        webhooks
            .deployed_then_call(&Deployed::TestAligned(make_commit()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn webhooks_dispatches_on_test_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/test-failure"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let config = WebhooksConfig {
            on_test_failure: Some(webhook_config_for(&server, "/test-failure")),
            ..Default::default()
        };
        let webhooks = WebhooksImpl::new(&config).unwrap();
        webhooks
            .deployed_then_call(&Deployed::TestFailed(make_commit()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn webhooks_dispatches_on_prod_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/prod-success"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let config = WebhooksConfig {
            on_prod_success: Some(webhook_config_for(&server, "/prod-success")),
            ..Default::default()
        };
        let webhooks = WebhooksImpl::new(&config).unwrap();
        webhooks
            .deployed_then_call(&Deployed::ProdAligned(make_commit()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn webhooks_dispatches_on_prod_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/prod-failure"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let config = WebhooksConfig {
            on_prod_failure: Some(webhook_config_for(&server, "/prod-failure")),
            ..Default::default()
        };
        let webhooks = WebhooksImpl::new(&config).unwrap();
        webhooks
            .deployed_then_call(&Deployed::ProdFailed(make_commit()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn webhooks_init_does_not_call_any_webhook() {
        let server = MockServer::start().await;
        // No mocks mounted → any unexpected request would cause the test to fail.
        let config = WebhooksConfig {
            on_test_success: Some(webhook_config_for(&server, "/should-not-be-called")),
            on_test_failure: Some(webhook_config_for(&server, "/should-not-be-called")),
            on_prod_success: Some(webhook_config_for(&server, "/should-not-be-called")),
            on_prod_failure: Some(webhook_config_for(&server, "/should-not-be-called")),
        };
        let webhooks = WebhooksImpl::new(&config).unwrap();
        webhooks.deployed_then_call(&Deployed::Init).await.unwrap();
        // wiremock asserts 0 requests were received when the server is dropped.
    }
}
