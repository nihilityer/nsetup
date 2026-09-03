//! 应用拥有的 Authelia `OpenID Connect` 客户端声明。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// 应用声明的单个 Authelia `OpenID Connect` 客户端。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutheliaOidcClientConfig {
    /// 授权页面展示的客户端名称。
    pub client_name: String,
    /// Authelia 保存的客户端密钥摘要；客户端应用使用对应明文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret_hash: Option<String>,
    /// 是否为不能安全保存密钥的公共客户端。
    #[serde(default, skip_serializing_if = "is_false")]
    pub public: bool,
    /// 授权请求要求的认证级别。
    #[serde(
        default = "default_authorization_policy",
        skip_serializing_if = "is_default_authorization_policy"
    )]
    pub authorization_policy: OidcAuthorizationPolicy,
    /// 必须与客户端应用完全一致的回调 URI。
    pub redirect_uris: Vec<String>,
    /// 允许客户端请求的 scopes。
    #[serde(default = "default_scopes", skip_serializing_if = "is_default_scopes")]
    pub scopes: Vec<String>,
    /// 允许客户端使用的授权类型。
    #[serde(
        default = "default_grant_types",
        skip_serializing_if = "is_default_grant_types"
    )]
    pub grant_types: Vec<String>,
    /// 是否强制使用 S256 PKCE。
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_pkce: bool,
    /// 客户端访问 token endpoint 时采用的认证方式。
    #[serde(default, skip_serializing_if = "is_default_token_endpoint_auth_method")]
    pub token_endpoint_auth_method: OidcTokenEndpointAuthMethod,
}

/// OIDC 客户端授权请求使用的认证策略。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OidcAuthorizationPolicy {
    /// 用户名和密码认证。
    OneFactor,
    /// 用户名、密码和第二认证因素。
    #[default]
    TwoFactor,
}

impl OidcAuthorizationPolicy {
    /// 返回 Authelia 配置使用的稳定名称。
    const fn as_str(self) -> &'static str {
        match self {
            Self::OneFactor => "one_factor",
            Self::TwoFactor => "two_factor",
        }
    }
}

/// OIDC token endpoint 支持的客户端认证方式。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OidcTokenEndpointAuthMethod {
    /// 通过 HTTP Basic 传递客户端凭据。
    #[default]
    ClientSecretBasic,
    /// 通过表单正文传递客户端凭据。
    ClientSecretPost,
    /// 公共客户端不发送客户端凭据。
    None,
}

impl OidcTokenEndpointAuthMethod {
    /// 返回 Authelia 配置使用的稳定名称。
    const fn as_str(self) -> &'static str {
        match self {
            Self::ClientSecretBasic => "client_secret_basic",
            Self::ClientSecretPost => "client_secret_post",
            Self::None => "none",
        }
    }
}

/// 序列化到 Authelia YAML 的客户端视图。
#[derive(Serialize)]
struct OidcClientDocument<'a> {
    /// 由 TOML 映射键提供的客户端 ID。
    client_id: &'a str,
    /// 授权页面展示名称。
    client_name: &'a str,
    /// 机密客户端使用摘要，公共客户端使用空字符串。
    client_secret: &'a str,
    /// 是否为公共客户端。
    public: bool,
    /// 此客户端要求的授权策略。
    authorization_policy: &'static str,
    /// 精确允许的回调 URI。
    redirect_uris: &'a [String],
    /// 允许的 scopes。
    scopes: &'a [String],
    /// 允许的授权类型。
    grant_types: &'a [String],
    /// 只允许安全的 authorization code response。
    response_types: [&'static str; 1],
    /// 是否要求 PKCE。
    require_pkce: bool,
    /// PKCE 固定使用 S256。
    pkce_challenge_method: &'static str,
    /// token endpoint 的客户端认证方式。
    token_endpoint_auth_method: &'static str,
}

/// 校验一个应用拥有的全部 OIDC 客户端及跨字段约束。
pub(super) fn validate_clients(
    clients: &BTreeMap<String, AutheliaOidcClientConfig>,
) -> anyhow::Result<()> {
    if clients.is_empty() {
        anyhow::bail!("[authelia] 至少需要一个 oidc_clients 客户端");
    }
    for (client_id, client) in clients {
        validate_client_id(client_id)?;
        client.validate(client_id)?;
    }
    Ok(())
}

/// 将一个应用的具名客户端映射序列化为 Authelia YAML 列表片段。
pub(super) fn clients_yaml(
    clients: &BTreeMap<String, AutheliaOidcClientConfig>,
) -> anyhow::Result<String> {
    validate_clients(clients)?;
    let documents: Vec<OidcClientDocument<'_>> = clients
        .iter()
        .map(|(client_id, client)| client.document(client_id))
        .collect();
    let output = serde_yaml::to_string(&documents)?;
    Ok(output.strip_prefix("---\n").unwrap_or(&output).to_string())
}

impl AutheliaOidcClientConfig {
    /// 校验客户端名称、密钥类型、回调 URI、scope 与授权类型。
    fn validate(&self, client_id: &str) -> anyhow::Result<()> {
        if self.client_name.trim().is_empty()
            || self.client_name.len() > 100
            || self.client_name.chars().any(char::is_control)
        {
            anyhow::bail!("Authelia OIDC 客户端 {client_id} 的 client_name 无效");
        }
        validate_client_secret(self, client_id)?;
        if self.redirect_uris.is_empty() {
            anyhow::bail!("Authelia OIDC 客户端 {client_id} 至少需要一个 redirect_uri");
        }
        let mut redirect_uris = BTreeSet::new();
        for redirect_uri in &self.redirect_uris {
            validate_redirect_uri(client_id, redirect_uri)?;
            if !redirect_uris.insert(redirect_uri) {
                anyhow::bail!("Authelia OIDC 客户端 {client_id} 的 redirect_uri 重复");
            }
        }
        validate_scopes(client_id, &self.scopes)?;
        validate_grant_types(client_id, &self.grant_types)?;
        Ok(())
    }

    /// 转换为 Authelia `clients` 列表中的 YAML 文档。
    fn document<'a>(&'a self, client_id: &'a str) -> OidcClientDocument<'a> {
        OidcClientDocument {
            client_id,
            client_name: &self.client_name,
            client_secret: self.client_secret_hash.as_deref().unwrap_or_default(),
            public: self.public,
            authorization_policy: self.authorization_policy.as_str(),
            redirect_uris: &self.redirect_uris,
            scopes: &self.scopes,
            grant_types: &self.grant_types,
            response_types: ["code"],
            require_pkce: self.require_pkce,
            pkce_challenge_method: "S256",
            token_endpoint_auth_method: self.token_endpoint_auth_method.as_str(),
        }
    }
}

/// 校验 TOML 映射键形式的 OIDC client ID。
fn validate_client_id(value: &str) -> anyhow::Result<()> {
    if value.is_empty() || value.len() > 100 || !value.bytes().all(is_rfc3986_unreserved) {
        anyhow::bail!("Authelia OIDC client_id 必须为 1 到 100 个 RFC3986 非保留字符: {value}");
    }
    Ok(())
}

/// 校验公共与机密客户端的密钥和 token endpoint 认证方式相符。
fn validate_client_secret(
    client: &AutheliaOidcClientConfig,
    client_id: &str,
) -> anyhow::Result<()> {
    if client.public {
        if client.client_secret_hash.is_some() {
            anyhow::bail!("Authelia OIDC 公共客户端 {client_id} 不能配置 client_secret_hash");
        }
        if !client.require_pkce {
            anyhow::bail!("Authelia OIDC 公共客户端 {client_id} 必须启用 require_pkce");
        }
        if client.token_endpoint_auth_method != OidcTokenEndpointAuthMethod::None {
            anyhow::bail!(
                "Authelia OIDC 公共客户端 {client_id} 的 token_endpoint_auth_method 必须为 none"
            );
        }
        return Ok(());
    }
    let Some(secret) = client.client_secret_hash.as_deref() else {
        anyhow::bail!("Authelia OIDC 机密客户端 {client_id} 缺少 client_secret_hash");
    };
    if secret.len() < 32
        || !secret.starts_with('$')
        || secret.contains(char::is_whitespace)
        || secret.contains("replace-with")
    {
        anyhow::bail!("Authelia OIDC 客户端 {client_id} 必须使用有效客户端密钥摘要");
    }
    if client.token_endpoint_auth_method == OidcTokenEndpointAuthMethod::None {
        anyhow::bail!("Authelia OIDC 机密客户端 {client_id} 不能使用 none 认证方式");
    }
    Ok(())
}

/// 校验 OIDC 回调 URI 的 scheme、authority 和安全字符。
fn validate_redirect_uri(client_id: &str, value: &str) -> anyhow::Result<()> {
    if value.len() > 2048
        || value.contains('#')
        || value.contains(['\'', '"'])
        || value.chars().any(char::is_whitespace)
    {
        anyhow::bail!("Authelia OIDC 客户端 {client_id} 的 redirect_uri 无效: {value}");
    }
    let (secure, remainder) = if let Some(remainder) = value.strip_prefix("https://") {
        (true, remainder)
    } else if let Some(remainder) = value.strip_prefix("http://") {
        (false, remainder)
    } else {
        anyhow::bail!("Authelia OIDC 客户端 {client_id} 的 redirect_uri 必须使用 http(s)");
    };
    let authority = remainder.split(['/', '?']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        anyhow::bail!("Authelia OIDC 客户端 {client_id} 的 redirect_uri 缺少有效主机");
    }
    if !secure && !is_loopback_authority(authority) {
        anyhow::bail!("Authelia OIDC 客户端 {client_id} 仅允许回环地址使用 http");
    }
    Ok(())
}

/// 判断 HTTP 回调 authority 是否指向本机回环地址。
fn is_loopback_authority(authority: &str) -> bool {
    authority == "localhost"
        || authority.starts_with("localhost:")
        || authority == "127.0.0.1"
        || authority.starts_with("127.0.0.1:")
        || authority == "[::1]"
        || authority.starts_with("[::1]:")
}

/// 校验 scopes 非空、无重复并包含 OIDC 必需的 `openid`。
fn validate_scopes(client_id: &str, scopes: &[String]) -> anyhow::Result<()> {
    let mut unique = BTreeSet::new();
    for scope in scopes {
        if scope.is_empty()
            || scope.len() > 64
            || !scope.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
            })
        {
            anyhow::bail!("Authelia OIDC 客户端 {client_id} 的 scope 无效: {scope}");
        }
        if !unique.insert(scope.as_str()) {
            anyhow::bail!("Authelia OIDC 客户端 {client_id} 的 scope 重复: {scope}");
        }
    }
    if !unique.contains("openid") {
        anyhow::bail!("Authelia OIDC 客户端 {client_id} 的 scopes 必须包含 openid");
    }
    Ok(())
}

/// 校验授权类型只使用模板支持的授权码与刷新令牌流程。
fn validate_grant_types(client_id: &str, grant_types: &[String]) -> anyhow::Result<()> {
    let mut unique = BTreeSet::new();
    for grant_type in grant_types {
        if !matches!(grant_type.as_str(), "authorization_code" | "refresh_token") {
            anyhow::bail!("Authelia OIDC 客户端 {client_id} 不支持 grant_type: {grant_type}");
        }
        if !unique.insert(grant_type.as_str()) {
            anyhow::bail!("Authelia OIDC 客户端 {client_id} 的 grant_type 重复: {grant_type}");
        }
    }
    if !unique.contains("authorization_code") {
        anyhow::bail!(
            "Authelia OIDC 客户端 {client_id} 的 grant_types 必须包含 authorization_code"
        );
    }
    Ok(())
}

/// 判断字节是否属于 RFC3986 非保留字符集。
const fn is_rfc3986_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// 返回 OIDC 默认授权策略。
const fn default_authorization_policy() -> OidcAuthorizationPolicy {
    OidcAuthorizationPolicy::TwoFactor
}

/// 判断授权策略是否为 OIDC 默认值。
const fn is_default_authorization_policy(value: &OidcAuthorizationPolicy) -> bool {
    matches!(value, OidcAuthorizationPolicy::TwoFactor)
}

/// 返回标准 OIDC scopes。
fn default_scopes() -> Vec<String> {
    ["openid", "profile", "email", "groups"]
        .into_iter()
        .map(String::from)
        .collect()
}

/// 判断 scopes 是否为模板默认值。
fn is_default_scopes(value: &[String]) -> bool {
    value == default_scopes()
}

/// 返回默认 authorization code grant。
fn default_grant_types() -> Vec<String> {
    vec![String::from("authorization_code")]
}

/// 判断授权类型是否为模板默认值。
fn is_default_grant_types(value: &[String]) -> bool {
    value == default_grant_types()
}

/// 判断 token endpoint 认证方式是否为默认 Basic。
const fn is_default_token_endpoint_auth_method(value: &OidcTokenEndpointAuthMethod) -> bool {
    matches!(value, OidcTokenEndpointAuthMethod::ClientSecretBasic)
}

/// 用于省略 `false` 值的 Serde 辅助函数。
const fn is_false(value: &bool) -> bool {
    !*value
}
