//! Kiro IDE 端点
//!
//! 对应 KAM 1.7.5 的反代行为：
//! - Enterprise / External IdP 流式生成走 `codewhisperer.{region}.amazonaws.com`
//! - 模型列表走 `q.{region}.amazonaws.com/ListAvailableModels`
//! - Builder ID 的占位 profileArn 不会出现在流式生成请求里
//!
//! 请求头使用 aws-sdk-js User-Agent 标识。请求体会在根对象上注入 `profileArn`。

use reqwest::RequestBuilder;
use uuid::Uuid;

use super::{KiroEndpoint, RequestContext};

/// Kiro IDE 端点名称
pub const IDE_ENDPOINT_NAME: &str = "ide";

/// Kiro IDE 端点
pub struct IdeEndpoint;

impl IdeEndpoint {
    pub fn new() -> Self {
        Self
    }

    fn api_region<'a>(&self, ctx: &'a RequestContext<'_>) -> &'a str {
        ctx.credentials.effective_api_region(ctx.config)
    }

    fn runtime_region(&self, ctx: &RequestContext<'_>) -> &'static str {
        if self
            .api_region(ctx)
            .trim()
            .to_ascii_lowercase()
            .starts_with("eu-")
        {
            "eu-central-1"
        } else {
            "us-east-1"
        }
    }

    fn q_host(&self, ctx: &RequestContext<'_>) -> String {
        format!("q.{}.amazonaws.com", self.runtime_region(ctx))
    }

    fn codewhisperer_host(&self, ctx: &RequestContext<'_>) -> String {
        format!("codewhisperer.{}.amazonaws.com", self.runtime_region(ctx))
    }

    fn api_host(&self, ctx: &RequestContext<'_>) -> String {
        if ctx.credentials.is_enterprise_auth() {
            self.codewhisperer_host(ctx)
        } else {
            self.q_host(ctx)
        }
    }

    fn ide_x_amz_user_agent(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "aws-sdk-js/1.0.34 KiroIDE {} {}",
            ctx.config.kiro_version, ctx.machine_id
        )
    }

    fn ide_user_agent(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "aws-sdk-js/1.0.34 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererstreaming#1.0.34 m/E KiroIDE-{}-{}",
            ctx.config.system_version,
            ctx.config.node_version,
            ctx.config.kiro_version,
            ctx.machine_id
        )
    }

    fn add_token_type_headers(
        &self,
        mut req: RequestBuilder,
        ctx: &RequestContext<'_>,
    ) -> RequestBuilder {
        if ctx.credentials.is_api_key_credential() {
            req = req.header("tokentype", "API_KEY");
        }
        if ctx.credentials.is_external_idp_auth() {
            req = req.header("TokenType", "EXTERNAL_IDP");
        }
        req
    }
}

impl Default for IdeEndpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl KiroEndpoint for IdeEndpoint {
    fn name(&self) -> &'static str {
        IDE_ENDPOINT_NAME
    }

    fn api_url(&self, ctx: &RequestContext<'_>) -> String {
        format!("https://{}/generateAssistantResponse", self.api_host(ctx))
    }

    fn mcp_url(&self, ctx: &RequestContext<'_>) -> String {
        format!("https://{}/mcp", self.q_host(ctx))
    }

    fn models_url(&self, ctx: &RequestContext<'_>) -> Option<String> {
        Some(format!("https://{}/ListAvailableModels", self.q_host(ctx)))
    }

    fn profiles_url(&self, ctx: &RequestContext<'_>) -> Option<String> {
        Some(format!(
            "https://{}/ListAvailableProfiles",
            self.codewhisperer_host(ctx)
        ))
    }

    fn decorate_api(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let req = req
            .header("x-amzn-codewhisperer-optout", "true")
            .header("x-amzn-kiro-agent-mode", "vibe")
            .header("x-amz-user-agent", self.ide_x_amz_user_agent(ctx))
            .header("user-agent", self.ide_user_agent(ctx))
            .header("host", self.api_host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        self.add_token_type_headers(req, ctx)
    }

    fn decorate_models(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let req = req
            .header("x-amzn-codewhisperer-optout", "true")
            .header("x-amz-user-agent", self.ide_x_amz_user_agent(ctx))
            .header("user-agent", self.ide_user_agent(ctx))
            .header("host", self.q_host(ctx))
            .header("Authorization", format!("Bearer {}", ctx.token));

        self.add_token_type_headers(req, ctx)
    }

    fn decorate_profiles(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        req.header("x-amz-user-agent", self.ide_x_amz_user_agent(ctx))
            .header("user-agent", self.ide_user_agent(ctx))
            .header("host", self.codewhisperer_host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=1")
            .header("Authorization", format!("Bearer {}", ctx.token))
    }

    fn decorate_mcp(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let mut req = req
            .header("x-amz-user-agent", self.ide_x_amz_user_agent(ctx))
            .header("user-agent", self.ide_user_agent(ctx))
            .header("host", self.q_host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        if let Some(ref arn) = ctx.credentials.profile_arn {
            req = req.header("x-amzn-kiro-profile-arn", arn);
        }
        self.add_token_type_headers(req, ctx)
    }

    fn transform_api_body(&self, body: &str, ctx: &RequestContext<'_>) -> String {
        inject_profile_arn(body, &ctx.credentials.resolved_stream_profile_arn())
    }
}

/// 将 profile_arn 注入到请求体 JSON 根对象
fn inject_profile_arn(request_body: &str, profile_arn: &Option<String>) -> String {
    if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(request_body) {
        if let Some(arn) = profile_arn {
            json["profileArn"] = serde_json::Value::String(arn.clone());
            if let Ok(body) = serde_json::to_string(&json) {
                return body;
            }
        } else if let Some(object) = json.as_object_mut() {
            object.remove("profileArn");
            if let Ok(body) = serde_json::to_string(&json) {
                return body;
            }
        }
    }
    request_body.to_string()
}

#[cfg(test)]
mod tests {
    use super::inject_profile_arn;
    use serde_json::Value;

    #[test]
    fn test_inject_profile_arn_with_some() {
        let body = r#"{"conversationState":{"conversationId":"c1"}}"#;
        let arn = Some("arn:aws:codewhisperer:us-east-1:123:profile/ABC".to_string());
        let result = inject_profile_arn(body, &arn);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            json["profileArn"],
            "arn:aws:codewhisperer:us-east-1:123:profile/ABC"
        );
        assert_eq!(json["conversationState"]["conversationId"], "c1");
    }

    #[test]
    fn test_inject_profile_arn_with_none() {
        let body = r#"{"conversationState":{"conversationId":"c1"}}"#;
        let result = inject_profile_arn(body, &None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert!(json.get("profileArn").is_none());
        assert_eq!(json["conversationState"]["conversationId"], "c1");
    }

    #[test]
    fn test_inject_profile_arn_with_none_removes_existing() {
        let body = r#"{"conversationState":{},"profileArn":"placeholder"}"#;
        let result = inject_profile_arn(body, &None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert!(json.get("profileArn").is_none());
        assert!(json.get("conversationState").is_some());
    }

    #[test]
    fn test_inject_profile_arn_overwrites_existing() {
        let body = r#"{"conversationState":{},"profileArn":"old-arn"}"#;
        let arn = Some("new-arn".to_string());
        let result = inject_profile_arn(body, &arn);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["profileArn"], "new-arn");
    }

    #[test]
    fn test_inject_profile_arn_invalid_json() {
        let body = "not-valid-json";
        let arn = Some("arn:test".to_string());
        let result = inject_profile_arn(body, &arn);
        assert_eq!(result, "not-valid-json");
    }
}
