//! Authelia `OpenID Connect` provider 与文件型密钥。

use super::generated_file;
use crate::template::GeneratedFile;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    /// 可选的具名 claims policy，由应用拥有的客户端按名称引用。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub claims_policies: BTreeMap<String, OidcClaimsPolicy>,
}

/// 单个 provider 级 claims policy。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::template) struct OidcClaimsPolicy {
    /// 除协议必需 claim 外额外写入 ID Token 的 claim 名称。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub id_token: Vec<String>,
    /// 除默认 claim 外额外写入 Access Token 的 claim 名称。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub access_token: Vec<String>,
}

impl OidcProviderConfig {
    /// 校验 provider HMAC、RS256 私钥与 claims policy。
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        validate_hmac_secret(&self.hmac_secret)?;
        validate_jwk_private_key(&self.jwk_private_key)?;
        for (name, policy) in &self.claims_policies {
            policy.validate(name)?;
        }
        Ok(())
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

    /// 生成 provider 的 claims policy、密钥与客户端声明。
    ///
    /// claims policy 是 provider 级结构，无法由应用片段贡献，且 Authelia 的配置
    /// 模板过滤器不支持命名模板，因此直接内联在生成的 YAML 中。客户端仍来自
    /// 应用拥有的只读片段目录；没有已声明客户端时不生成 `identity_providers`
    /// 块，也就不需要任何 provider 密钥。
    pub(super) fn configuration_yaml(&self) -> String {
        let claims_policies = claims_policies_block(&self.claims_policies);
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
{claims_policies}    clients:
{{{{- range $oidc_clients }}}}
{{{{ fileContent . | nindent 6 }}}}
{{{{- end }}}}
{{{{ end }}}}
"#
        )
    }
}

/// 生成内联的 `claims_policies` YAML 块，未声明任何 policy 时返回空串。
fn claims_policies_block(policies: &BTreeMap<String, OidcClaimsPolicy>) -> String {
    if policies.is_empty() {
        return String::new();
    }
    let yaml = serde_yaml::to_string(policies).unwrap_or_default();
    let mut output = String::from("    claims_policies:\n");
    for line in yaml.strip_prefix("---\n").unwrap_or(&yaml).lines() {
        output.push_str("      ");
        output.push_str(line);
        output.push('\n');
    }
    output
}

impl OidcClaimsPolicy {
    /// 校验 policy 至少声明一个 claim，且所有 claim 名可安全写入 YAML。
    fn validate(&self, name: &str) -> anyhow::Result<()> {
        if !valid_claims_policy_name(name) {
            anyhow::bail!("oidc.claims_policies 名称无效: {name}");
        }
        if self.id_token.is_empty() && self.access_token.is_empty() {
            anyhow::bail!(
                "oidc.claims_policies.{name} 至少需要声明 id_token 或 access_token claim"
            );
        }
        for claim in self.id_token.iter().chain(&self.access_token) {
            if !valid_claim_name(claim) {
                anyhow::bail!("oidc.claims_policies.{name} 的 claim 名无效: {claim}");
            }
        }
        Ok(())
    }
}

/// 校验 claims policy 名称与 OIDC `client_id` 使用同一字符集。
pub(in crate::template) fn valid_claims_policy_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 100 && value.bytes().all(is_rfc3986_unreserved)
}

/// 校验 claim 名称，允许点号分隔的自定义 claim。
fn valid_claim_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| is_rfc3986_unreserved(byte) || byte == b'.')
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
