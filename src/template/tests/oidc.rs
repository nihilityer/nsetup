//! Authelia OIDC provider 与 app 拥有的客户端测试。

use super::super::{apply, export};
use crate::config::Config;

/// Authelia 骨架只说明 provider，OIDC 客户端指引位于应用骨架。
#[test]
fn authelia_skeleton_documents_oidc_configuration() {
    for example in [
        "openssl genpkey -algorithm RSA",
        "# [oidc]",
        "# hmac_secret =",
        "# jwk_private_key =",
        "[authelia.oidc_clients.<client_id>]",
    ] {
        assert!(
            super::super::skeleton::AUTHELIA_SKELETON.contains(example),
            "Authelia 骨架缺少 OIDC provider 指引: {example}"
        );
    }
    assert!(!super::super::skeleton::AUTHELIA_SKELETON.contains("# [oidc.clients."));
    for example in [
        "authelia crypto rand --length 72 --charset rfc3986",
        "authelia crypto hash generate pbkdf2",
        "# [authelia.oidc_clients.my-app]",
        "# client_secret_hash =",
        "# redirect_uris =",
        "# token_endpoint_auth_method =",
        "public = true",
    ] {
        assert!(
            super::super::skeleton::APP_SKELETON.contains(example),
            "应用骨架缺少 Authelia OIDC client 指引: {example}"
        );
    }
}

/// OIDC provider 只拥有文件型密钥，并从应用片段目录汇总客户端。
#[test]
fn authelia_oidc_provider_round_trip() -> anyhow::Result<()> {
    let input = valid_authelia_oidc_provider_config();
    let config = Config::default();
    let generated = apply(input, &config)?;
    let service = &generated.spec.document.services["authelia"];
    assert_eq!(
        service.environment.get("X_AUTHELIA_CONFIG_FILTERS"),
        Some(&String::from("template"))
    );
    for name in [
        "OIDC_HMAC_SECRET",
        "OIDC_JWK_PRIVATE_KEY",
        ".nsetup-managed",
    ] {
        let secret = generated
            .files
            .iter()
            .find(|file| file.path.ends_with(name))
            .ok_or_else(|| anyhow::anyhow!("missing {name}"))?;
        assert_eq!(
            secret.mode,
            if name == ".nsetup-managed" {
                0o640
            } else {
                0o600
            }
        );
    }
    let configuration = generated
        .files
        .iter()
        .find(|file| file.path.ends_with("configuration.yml"))
        .ok_or_else(|| anyhow::anyhow!("missing configuration.yml"))?;
    let configuration = String::from_utf8(configuration.content.clone())?;
    assert!(configuration.contains("glob \"/config/oidc-clients/*.yml\""));
    assert!(configuration.contains("if $oidc_clients"));
    assert!(configuration.contains("filename: '/data/notification.txt'\n{{ $oidc_clients := glob"));
    assert!(!configuration.contains("filename: '/data/notification.txt'\n{{-"));
    assert!(configuration.contains("identity_providers:\n  oidc:"));
    assert!(configuration.contains("secret \"/secrets/OIDC_HMAC_SECRET\""));
    assert!(configuration.contains("secret \"/secrets/OIDC_JWK_PRIVATE_KEY\""));
    assert!(configuration.contains("fileContent . | nindent 6"));
    assert!(
        !configuration.contains("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
    );
    assert!(!configuration.contains("BEGIN PRIVATE KEY"));

    let restored = crate::spec::StackSpec::parse(
        &generated.spec.name,
        &generated.spec.compose_yaml()?,
        &generated.spec.env_file(),
    )?;
    assert_eq!(restored.environment, generated.spec.environment);
    let exported = export(&restored, &config)?;
    assert!(exported.contains("[oidc]"));
    assert!(!exported.contains("oidc_clients"));
    assert_eq!(generated, apply(&exported, &config)?);
    Ok(())
}

/// OIDC 客户端由应用持有，并生成按项目命名的 Authelia 配置片段。
#[test]
fn app_owned_oidc_clients_round_trip() -> anyhow::Result<()> {
    let input = valid_app_oidc_config();
    let config = Config::default();
    let generated = apply(input, &config)?;
    let fragment = super::super::app_oidc_client_fragment(&generated.spec)?
        .ok_or_else(|| anyhow::anyhow!("missing app OIDC fragment"))?;
    assert_eq!(
        fragment.path,
        std::path::Path::new("config/oidc-clients/media.yml")
    );
    assert_eq!(fragment.mode, 0o640);
    let fragment = String::from_utf8(fragment.content)?;
    assert!(fragment.contains("client_id: cli"));
    assert!(fragment.contains("client_secret: ''"));
    assert!(fragment.contains("client_id: gitea"));
    assert!(fragment.contains("token_endpoint_auth_method: client_secret_basic"));

    let restored = crate::spec::StackSpec::parse(
        &generated.spec.name,
        &generated.spec.compose_yaml()?,
        &generated.spec.env_file(),
    )?;
    assert_eq!(restored.environment, generated.spec.environment);
    let exported = export(&restored, &config)?;
    assert!(exported.contains("[authelia.oidc_clients.cli]"));
    assert!(exported.contains("[authelia.oidc_clients.gitea]"));
    assert!(exported.contains("client_secret_hash ="));
    assert_eq!(generated, apply(&exported, &config)?);
    Ok(())
}

/// OIDC 机密客户端拒绝明文密钥，避免将应用凭据直接写入 Authelia。
#[test]
fn authelia_oidc_client_rejects_plaintext_secret() {
    let input = valid_app_oidc_config().replace(
        "$pbkdf2-sha512$310000$c2FsdA$ZGlnZXN0ZGlnZXN0ZGlnZXN0ZGlnZXN0",
        "plaintext-client-secret",
    );
    assert!(apply(&input, &Config::default()).is_err());
}

/// OIDC 公共客户端必须使用 PKCE，不能退化为无客户端认证的裸授权码流程。
#[test]
fn authelia_oidc_public_client_requires_pkce() {
    let input = valid_app_oidc_config().replace(
        "redirect_uris = [\"http://127.0.0.1:17890/oauth/callback\"]\nscopes = [\"openid\", \"profile\"]\nrequire_pkce = true",
        "redirect_uris = [\"http://127.0.0.1:17890/oauth/callback\"]\nscopes = [\"openid\", \"profile\"]\nrequire_pkce = false",
    );
    assert!(apply(&input, &Config::default()).is_err());
}

/// OIDC 回调仅允许 HTTPS，开发期 HTTP 只能绑定本机回环地址。
#[test]
fn authelia_oidc_rejects_remote_http_callback() {
    let input = valid_app_oidc_config().replace(
        "http://127.0.0.1:17890/oauth/callback",
        "http://client.example.com/oauth/callback",
    );
    assert!(apply(&input, &Config::default()).is_err());
}

/// 返回只声明 OIDC provider 的有效 Authelia TOML。
fn valid_authelia_oidc_provider_config() -> &'static str {
    r#"
format = 1
template = "authelia"
host = "auth"
version = "4.39.20"
default_redirection_url = "https://example.com"
default_policy = "two_factor"
jwt_secret = "jwt-secret-value-0123456789-abcdef"
session_secret = "session-secret-value-0123456789-ab"
storage_encryption_key = "storage-secret-value-0123456789-a"

[oidc]
hmac_secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
jwk_private_key = """
-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789
abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefgh
ijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnop
-----END PRIVATE KEY-----
"""

[users.admin]
display_name = "Administrator"
password_hash = '$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$aGFzaA'
email = "admin@example.com"
groups = ["admins"]
"#
}

/// 返回拥有机密客户端和公共客户端的有效应用 TOML。
fn valid_app_oidc_config() -> &'static str {
    r#"
format = 1
name = "media"

[authelia.oidc_clients.gitea]
client_name = "Gitea"
client_secret_hash = '$pbkdf2-sha512$310000$c2FsdA$ZGlnZXN0ZGlnZXN0ZGlnZXN0ZGlnZXN0'
authorization_policy = "two_factor"
redirect_uris = ["https://git.example.com/user/oauth2/authelia/callback"]
scopes = ["openid", "profile", "email", "groups"]
grant_types = ["authorization_code", "refresh_token"]
require_pkce = false
token_endpoint_auth_method = "client_secret_basic"

[authelia.oidc_clients.cli]
client_name = "CLI"
public = true
redirect_uris = ["http://127.0.0.1:17890/oauth/callback"]
scopes = ["openid", "profile"]
require_pkce = true
token_endpoint_auth_method = "none"

[services.web]
image = "example/media"
version = "1.0"
port = 8080

[services.web.traefik]
hosts = ["media"]
"#
}
