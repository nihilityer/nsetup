//! Authelia `OpenID Connect` provider 与文件型密钥。

use super::generated_file;
use crate::template::GeneratedFile;
use serde::{Deserialize, Serialize};

/// OIDC HMAC 文件在容器中的固定路径。
const HMAC_SECRET_PATH: &str = "/secrets/OIDC_HMAC_SECRET";
/// OIDC RS256 私钥文件在容器中的固定路径。
const JWK_PRIVATE_KEY_PATH: &str = "/secrets/OIDC_JWK_PRIVATE_KEY";

/// Authelia `OpenID Connect` provider 声明。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::template) struct OidcProviderConfig {
    /// provider 用于签名内部令牌的随机 HMAC 密钥。
    pub hmac_secret: String,
    /// provider 用于签发 ID Token 的 RS256 PEM 私钥。
    pub jwk_private_key: String,
}

impl OidcProviderConfig {
    /// 校验 provider HMAC 与 RS256 私钥。
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        validate_hmac_secret(&self.hmac_secret)?;
        validate_jwk_private_key(&self.jwk_private_key)
    }

    /// 生成 OIDC provider 的 HMAC 与 RS256 私钥文件。
    pub(super) fn generated_files(&self) -> Vec<GeneratedFile> {
        vec![
            generated_file(
                "secrets/OIDC_HMAC_SECRET",
                format!("{}\n", self.hmac_secret),
                0o600,
            ),
            generated_file(
                "secrets/OIDC_JWK_PRIVATE_KEY",
                format!("{}\n", self.jwk_private_key.trim_end()),
                0o600,
            ),
        ]
    }

    /// 生成从应用拥有的客户端片段目录汇总客户端的 provider YAML。
    pub(super) fn configuration_yaml(&self) -> String {
        format!(
            r#"{{{{ $oidc_clients := glob "/config/oidc-clients/*.yml" }}}}
{{{{ if $oidc_clients }}}}
identity_providers:
  oidc:
    hmac_secret: '{{{{ secret "{HMAC_SECRET_PATH}" }}}}'
    jwks:
      - algorithm: 'RS256'
        use: 'sig'
        key: {{{{ secret "{JWK_PRIVATE_KEY_PATH}" | mindent 10 "|" | msquote }}}}
    clients:
{{{{- range $oidc_clients }}}}
{{{{ fileContent . | nindent 6 }}}}
{{{{- end }}}}
{{{{- end }}}}
"#
        )
    }
}

/// 校验 OIDC HMAC 密钥满足长度和字符集要求。
fn validate_hmac_secret(value: &str) -> anyhow::Result<()> {
    if value.len() < 64
        || value.contains("replace-with")
        || !value.bytes().all(is_rfc3986_unreserved)
    {
        anyhow::bail!("oidc.hmac_secret 必须是至少 64 字符的 RFC3986 非保留随机密钥");
    }
    Ok(())
}

/// 校验 OIDC JWK 是非占位的 PKCS#8 或 PKCS#1 RSA PEM 私钥。
fn validate_jwk_private_key(value: &str) -> anyhow::Result<()> {
    let value = value.trim();
    let pkcs8 = value.starts_with("-----BEGIN PRIVATE KEY-----")
        && value.ends_with("-----END PRIVATE KEY-----");
    let pkcs1 = value.starts_with("-----BEGIN RSA PRIVATE KEY-----")
        && value.ends_with("-----END RSA PRIVATE KEY-----");
    if value.len() < 128 || value.contains("replace-with") || (!pkcs8 && !pkcs1) {
        anyhow::bail!("oidc.jwk_private_key 必须是非占位的 PKCS#8 或 PKCS#1 RSA PEM 私钥");
    }
    Ok(())
}

/// 判断字节是否属于 RFC3986 非保留字符集。
const fn is_rfc3986_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}
