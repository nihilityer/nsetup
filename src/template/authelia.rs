//! Authelia 基础认证设施模板及其可重建状态。

mod oidc;
mod users;

use self::oidc::OidcProviderConfig;
use self::users::AutheliaUser;
use super::{FORMAT_VERSION, GeneratedFile, TemplateKind, TemplateOutput};
use crate::config::{Config, validate_domain};
use crate::constants::PROXY_NETWORK;
use crate::spec::{Document, Healthcheck, Network, Route, RouteProtocol, Service, StackSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(super) use self::oidc::valid_claims_policy_name;

/// Authelia 模板在项目 `.env` 中持久化镜像版本的键。
const VERSION_KEY: &str = "AUTHELIA_VERSION";
/// Authelia 模板在项目 `.env` 中持久化默认跳转地址的键。
const REDIRECTION_KEY: &str = "NSETUP_AUTHELIA_DEFAULT_REDIRECTION_URL";
/// Authelia 模板在项目 `.env` 中持久化默认认证策略的键。
const POLICY_KEY: &str = "NSETUP_AUTHELIA_DEFAULT_POLICY";
/// Authelia 模板在项目 `.env` 中持久化用户数据库的键。
const USERS_KEY: &str = "NSETUP_AUTHELIA_USERS_JSON";
/// Authelia 模板在项目 `.env` 中持久化密码重置密钥的键。
const JWT_SECRET_KEY: &str = "NSETUP_AUTHELIA_JWT_SECRET";
/// Authelia 模板在项目 `.env` 中持久化会话密钥的键。
const SESSION_SECRET_KEY: &str = "NSETUP_AUTHELIA_SESSION_SECRET";
/// Authelia 模板在项目 `.env` 中持久化存储加密密钥的键。
const STORAGE_SECRET_KEY: &str = "NSETUP_AUTHELIA_STORAGE_ENCRYPTION_KEY";
/// Authelia 模板在项目 `.env` 中持久化 OIDC HMAC 的键。
const OIDC_HMAC_SECRET_KEY: &str = "NSETUP_AUTHELIA_OIDC_HMAC_SECRET";
/// Authelia 模板在项目 `.env` 中持久化 OIDC RS256 私钥的键。
const OIDC_JWK_PRIVATE_KEY: &str = "NSETUP_AUTHELIA_OIDC_JWK_PRIVATE_KEY";
/// Authelia 模板在项目 `.env` 中持久化 OIDC claims policy 的键。
const OIDC_CLAIMS_POLICIES_KEY: &str = "NSETUP_AUTHELIA_OIDC_CLAIMS_POLICIES_JSON";
/// Authelia 模板在项目 `.env` 中持久化遥测配置的键。
const TELEMETRY_KEY: &str = "NSETUP_AUTHELIA_TELEMETRY_JSON";

/// Authelia 基础认证设施的 TOML 文档。
///
/// 项目名固定为 `authelia`；这里的 `name` 只用于兼容通用头部写法与导出结果，
/// 声明时必须与模板一致。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct AutheliaConfig {
    /// 配置模式版本。
    pub format: u32,
    /// 必填模板选择器。
    pub template: String,
    /// 可选项目名；省略或写 `authelia` 均可。
    #[serde(default = "super::default_authelia_name")]
    pub name: String,
    /// 认证门户的短主机名或完整域名。
    #[serde(default = "default_host")]
    pub host: String,
    /// 明确的 Authelia 镜像版本。
    pub version: String,
    /// 用户直接访问认证门户时的默认跳转地址。
    pub default_redirection_url: String,
    /// 未命中更具体规则时采用的认证策略。
    #[serde(default)]
    pub default_policy: AutheliaPolicy,
    /// 密码重置令牌签名密钥。
    pub jwt_secret: String,
    /// 浏览器会话签名密钥。
    pub session_secret: String,
    /// `SQLite` 敏感字段加密密钥。
    pub storage_encryption_key: String,
    /// 可选的 `OpenID Connect` provider；客户端由应用声明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc: Option<OidcProviderConfig>,
    /// 可选自身遥测；省略时 Authelia 不暴露指标也不导出 trace。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<AutheliaTelemetryConfig>,
    /// 以登录名为键的声明式本地用户库。
    pub users: BTreeMap<String, AutheliaUser>,
}

/// Authelia 自身的遥测配置。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct AutheliaTelemetryConfig {
    /// 暴露 Prometheus 指标的监听地址，例如 `tcp://0.0.0.0:9959`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_address: Option<String>,
    /// 指标路径，默认 `/metrics`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_path: Option<String>,
    /// 导出 trace 的地址，例如 `udp://otel-collector:4318`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracing_address: Option<String>,
    /// trace 采样率；省略时沿用 Authelia 默认值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracing_sample_rate: Option<f64>,
}

impl AutheliaTelemetryConfig {
    /// 判断是否声明了任何遥测端点。
    const fn is_empty(&self) -> bool {
        self.metrics_address.is_none()
            && self.metrics_path.is_none()
            && self.tracing_address.is_none()
            && self.tracing_sample_rate.is_none()
    }

    /// 校验监听地址、指标路径与采样率。
    fn validate(&self) -> anyhow::Result<()> {
        if let Some(address) = &self.metrics_address {
            validate_listen_address("telemetry.metrics_address", address)?;
        }
        if let Some(address) = &self.tracing_address {
            validate_listen_address("telemetry.tracing_address", address)?;
        }
        if let Some(path) = &self.metrics_path
            && (!path.starts_with('/') || path.len() > 256 || path.contains(char::is_whitespace))
        {
            anyhow::bail!("telemetry.metrics_path 必须是以 / 开头的路径: {path}");
        }
        if let Some(rate) = self.tracing_sample_rate
            && !(0.0..=1.0).contains(&rate)
        {
            anyhow::bail!("telemetry.tracing_sample_rate 必须在 0.0 到 1.0 之间");
        }
        Ok(())
    }

    /// 生成 `telemetry` YAML 块。
    fn yaml(&self) -> String {
        let mut output = String::from("telemetry:\n");
        if self.metrics_address.is_some() || self.metrics_path.is_some() {
            output.push_str("  metrics:\n    enabled: true\n    address: '");
            output.push_str(
                self.metrics_address
                    .as_deref()
                    .unwrap_or("tcp://0.0.0.0:9959"),
            );
            output.push_str("'\n");
            if let Some(path) = &self.metrics_path {
                output.push_str(&format!("    path: '{path}'\n"));
            }
        }
        if self.tracing_address.is_some() || self.tracing_sample_rate.is_some() {
            output.push_str("  tracing:\n    enabled: true\n");
            if let Some(address) = &self.tracing_address {
                output.push_str(&format!("    address: '{address}'\n"));
            }
            if let Some(rate) = self.tracing_sample_rate {
                output.push_str(&format!("    sample_rate: {rate}\n"));
            }
        }
        output
    }
}

/// 校验 `scheme://host:port` 或 `host:port` 形式的监听地址。
///
/// 允许 `tcp://`、`udp://` 等 Authelia 支持的传输前缀，以及容器名这类主机名。
fn validate_listen_address(label: &str, value: &str) -> anyhow::Result<()> {
    let address = match value.split_once("://") {
        Some((scheme, rest)) => {
            if scheme.is_empty() {
                anyhow::bail!("{label} 的传输前缀无效: {value}");
            }
            rest
        }
        None => value,
    };
    let Some((host, port)) = address.rsplit_once(':') else {
        anyhow::bail!("{label} 必须是 host:port 形式的监听地址: {value}");
    };
    let port = port
        .parse::<u16>()
        .map_err(|_| anyhow::anyhow!("{label} 的端口无效: {value}"))?;
    if port == 0 || host.is_empty() || host.contains(char::is_whitespace) {
        anyhow::bail!("{label} 必须是 host:port 形式的监听地址: {value}");
    }
    Ok(())
}

/// Authelia 默认访问控制策略。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum AutheliaPolicy {
    /// 用户名和密码认证。
    #[default]
    OneFactor,
    /// 用户名、密码和第二认证因素。
    TwoFactor,
}

impl AutheliaPolicy {
    /// 返回 Authelia 配置使用的稳定名称。
    const fn as_str(self) -> &'static str {
        match self {
            Self::OneFactor => "one_factor",
            Self::TwoFactor => "two_factor",
        }
    }

    /// 解析存储在项目环境中的策略名称。
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "one_factor" => Ok(Self::OneFactor),
            "two_factor" => Ok(Self::TwoFactor),
            _ => anyhow::bail!("不支持的 Authelia 默认策略: {value}"),
        }
    }
}

/// 从 TOML 生成 Authelia 项目、配置文件和文件型密钥。
pub(super) fn generate(input: &AutheliaConfig, config: &Config) -> anyhow::Result<TemplateOutput> {
    validate_input(input, config)?;
    let project = input.name.as_str();
    let host = expand_host(&input.host, &config.domain)?;
    let directory = config.stacks_root.join(project);
    let data_directory = config
        .data_roots
        .first()
        .ok_or_else(|| anyhow::anyhow!("Authelia 模板需要至少一个 data_root"))?
        .join("authelia");
    let users_json = serde_json::to_string(&input.users)?;
    let mut document = Document::default();
    document.networks.insert(
        String::from("proxy"),
        Network {
            external: true,
            name: Some(String::from(PROXY_NETWORK)),
        },
    );
    let mut healthcheck = Healthcheck::command(String::from("/app/healthcheck.sh"));
    healthcheck.interval = Some(String::from("30s"));
    healthcheck.timeout = Some(String::from("3s"));
    healthcheck.start_period = Some(String::from("1m"));
    healthcheck.retries = Some(3);
    let mut service = Service {
        image: String::from("authelia/authelia:${AUTHELIA_VERSION}"),
        container_name: Some(String::from("authelia")),
        restart: Some(String::from("unless-stopped")),
        networks: vec![String::from("proxy")],
        volumes: vec![
            // `/config` 必须可写：官方镜像的 entrypoint 在 PUID/PGID 为 0 时执行
            // `chown -R 0:0 /config`（镜像默认值就是 0），只读挂载会让每次启动都往
            // 容器日志刷 `chown: ... Read-only file system`。密钥目录保持只读。
            format!("{}:/config", directory.join("config").display()),
            format!("{}:/secrets:ro", directory.join("secrets").display()),
            format!("{}:/data", data_directory.display()),
        ],
        environment: BTreeMap::from([
            (
                String::from("AUTHELIA_IDENTITY_VALIDATION_RESET_PASSWORD_JWT_SECRET_FILE"),
                String::from("/secrets/JWT_SECRET"),
            ),
            (
                String::from("AUTHELIA_SESSION_SECRET_FILE"),
                String::from("/secrets/SESSION_SECRET"),
            ),
            (
                String::from("AUTHELIA_STORAGE_ENCRYPTION_KEY_FILE"),
                String::from("/secrets/STORAGE_ENCRYPTION_KEY"),
            ),
        ]),
        labels: vec![String::from("io.nsetup.template=authelia")],
        healthcheck: Some(healthcheck),
        ..Service::default()
    };
    if input.oidc.is_some() {
        service.environment.insert(
            String::from("X_AUTHELIA_CONFIG_FILTERS"),
            String::from("template"),
        );
    }
    service.set_routes(
        "authelia",
        "authelia",
        &[Route {
            name: String::from("default"),
            hosts: vec![host],
            path_prefix: None,
            container_port: 9091,
            middlewares: vec![String::from("tls")],
            protocol: RouteProtocol::Http,
            entrypoint: String::from("https"),
            sticky_cookie: false,
            pass_host_header: None,
            priority: None,
        }],
    )?;
    document.services.insert(String::from("authelia"), service);
    let mut environment = BTreeMap::from([
        (String::from(VERSION_KEY), input.version.clone()),
        (
            String::from(REDIRECTION_KEY),
            input.default_redirection_url.clone(),
        ),
        (
            String::from(POLICY_KEY),
            input.default_policy.as_str().to_string(),
        ),
        (String::from(USERS_KEY), users_json),
        (String::from(JWT_SECRET_KEY), input.jwt_secret.clone()),
        (
            String::from(SESSION_SECRET_KEY),
            input.session_secret.clone(),
        ),
        (
            String::from(STORAGE_SECRET_KEY),
            input.storage_encryption_key.clone(),
        ),
    ]);
    if let Some(oidc) = &input.oidc {
        environment.insert(String::from(OIDC_HMAC_SECRET_KEY), oidc.hmac_secret.clone());
        environment.insert(
            String::from(OIDC_JWK_PRIVATE_KEY),
            oidc.jwk_private_key.clone(),
        );
        if !oidc.claims_policies.is_empty() {
            environment.insert(
                String::from(OIDC_CLAIMS_POLICIES_KEY),
                serde_json::to_string(&oidc.claims_policies)?,
            );
        }
    }
    if let Some(telemetry) = &input.telemetry
        && !telemetry.is_empty()
    {
        environment.insert(
            String::from(TELEMETRY_KEY),
            serde_json::to_string(telemetry)?,
        );
    }
    let spec = StackSpec {
        name: project.to_string(),
        document,
        environment,
    };
    spec.validate()?;
    Ok(TemplateOutput {
        spec,
        files: generated_files(input, &config.domain)?,
        kind: TemplateKind::Authelia,
    })
}

/// 从当前 Compose IR 与项目环境重建 Authelia TOML。
pub(super) fn export(spec: &StackSpec) -> anyhow::Result<AutheliaConfig> {
    if spec.name != "authelia" {
        anyhow::bail!("Authelia 模板项目名必须为 authelia");
    }
    let service = spec
        .document
        .services
        .get("authelia")
        .ok_or_else(|| anyhow::anyhow!("authelia 模板缺少服务 authelia"))?;
    let host = service
        .routes(&spec.name, "authelia")?
        .into_iter()
        .next()
        .and_then(|route| route.hosts.into_iter().next())
        .ok_or_else(|| anyhow::anyhow!("authelia 模板缺少认证门户路由"))?;
    let users = serde_json::from_str(required_environment(spec, USERS_KEY)?)?;
    let oidc = match spec.environment.get(OIDC_HMAC_SECRET_KEY) {
        Some(hmac_secret) => Some(OidcProviderConfig {
            hmac_secret: hmac_secret.clone(),
            jwk_private_key: required_environment(spec, OIDC_JWK_PRIVATE_KEY)?.to_string(),
            claims_policies: match spec.environment.get(OIDC_CLAIMS_POLICIES_KEY) {
                Some(value) => serde_json::from_str(value)?,
                None => BTreeMap::new(),
            },
        }),
        None => None,
    };
    Ok(AutheliaConfig {
        format: FORMAT_VERSION,
        template: String::from("authelia"),
        name: spec.name.clone(),
        host,
        version: required_environment(spec, VERSION_KEY)?.to_string(),
        default_redirection_url: required_environment(spec, REDIRECTION_KEY)?.to_string(),
        default_policy: AutheliaPolicy::parse(required_environment(spec, POLICY_KEY)?)?,
        jwt_secret: required_environment(spec, JWT_SECRET_KEY)?.to_string(),
        session_secret: required_environment(spec, SESSION_SECRET_KEY)?.to_string(),
        storage_encryption_key: required_environment(spec, STORAGE_SECRET_KEY)?.to_string(),
        oidc,
        telemetry: match spec.environment.get(TELEMETRY_KEY) {
            Some(value) => serde_json::from_str(value)?,
            None => None,
        },
        users,
    })
}

/// 判断当前 Authelia 项目状态是否启用了 OIDC provider。
pub(super) fn oidc_enabled(spec: &StackSpec) -> bool {
    spec.environment.contains_key(OIDC_HMAC_SECRET_KEY)
        && spec.environment.contains_key(OIDC_JWK_PRIVATE_KEY)
}

/// 校验 Authelia TOML 的模板、版本、域名、密钥与用户字段。
fn validate_input(input: &AutheliaConfig, config: &Config) -> anyhow::Result<()> {
    if input.format != FORMAT_VERSION {
        anyhow::bail!("不支持的配置 format: {}", input.format);
    }
    if input.template != "authelia" {
        anyhow::bail!("Authelia 配置的 template 必须为 authelia");
    }
    if input.name != "authelia" {
        anyhow::bail!(
            "Authelia 模板的项目名固定为 authelia，不能声明 name = \"{}\"",
            input.name
        );
    }
    crate::spec::validate_version(&input.version)?;
    let _host = expand_host(&input.host, &config.domain)?;
    validate_https_url(&input.default_redirection_url)?;
    validate_secret("jwt_secret", &input.jwt_secret)?;
    validate_secret("session_secret", &input.session_secret)?;
    validate_secret("storage_encryption_key", &input.storage_encryption_key)?;
    if let Some(oidc) = &input.oidc {
        oidc.validate()?;
    }
    if let Some(telemetry) = &input.telemetry {
        telemetry.validate()?;
    }
    if input.users.is_empty() {
        anyhow::bail!("Authelia 至少需要一个声明式用户");
    }
    for (username, user) in &input.users {
        crate::spec::validate_name("Authelia 用户名", username)?;
        users::validate(username, user)?;
    }
    Ok(())
}

/// 校验认证基础设施密钥的最低长度和占位符。
fn validate_secret(label: &str, value: &str) -> anyhow::Result<()> {
    if value.len() < 32
        || value.starts_with("replace-with")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        anyhow::bail!("{label} 必须是至少 32 字符且仅含字母、数字、-、_ 的非占位密钥");
    }
    Ok(())
}

/// 校验默认跳转地址只使用无凭据、无端口的 HTTPS 域名。
fn validate_https_url(value: &str) -> anyhow::Result<()> {
    let authority = value
        .strip_prefix("https://")
        .ok_or_else(|| anyhow::anyhow!("default_redirection_url 必须使用 https://"))?
        .trim_end_matches('/');
    if authority.contains(['/', '?', '#', '@', ':', '\'', '"']) {
        anyhow::bail!("default_redirection_url 仅支持无端口的 HTTPS 域名: {value}");
    }
    validate_domain(authority)
}

/// 将认证门户短主机名扩展为完整域名。
fn expand_host(value: &str, domain: &str) -> anyhow::Result<String> {
    let output = if value.contains('.') {
        value.to_string()
    } else {
        format!("{value}.{domain}")
    };
    validate_domain(&output)?;
    Ok(output)
}

/// 构造 Authelia 拥有的配置、用户数据库和只读密钥文件。
fn generated_files(input: &AutheliaConfig, domain: &str) -> anyhow::Result<Vec<GeneratedFile>> {
    let host = expand_host(&input.host, domain)?;
    let mut files = vec![
        generated_file(
            "config/configuration.yml",
            configuration_yaml(input, domain, &host)?,
            0o640,
        ),
        generated_file(
            "config/users_database.yml",
            users::database_yaml(&input.users)?,
            0o600,
        ),
        generated_file(
            "secrets/JWT_SECRET",
            format!("{}\n", input.jwt_secret),
            0o600,
        ),
        generated_file(
            "secrets/SESSION_SECRET",
            format!("{}\n", input.session_secret),
            0o600,
        ),
        generated_file(
            "secrets/STORAGE_ENCRYPTION_KEY",
            format!("{}\n", input.storage_encryption_key),
            0o600,
        ),
        generated_file(
            "config/oidc-clients/.nsetup-managed",
            String::from("应用拥有的 OIDC 客户端片段由 nsetup 管理。\n"),
            0o640,
        ),
    ];
    if let Some(oidc) = &input.oidc {
        files.extend(oidc.generated_files());
    }
    Ok(files)
}

/// 构造每次整体应用时都会替换的模板附属文件。
fn generated_file(path: &str, content: String, mode: u32) -> GeneratedFile {
    GeneratedFile {
        path: PathBuf::from(path),
        content: content.into_bytes(),
        mode,
        directory_mode: super::PRIVATE_DIRECTORY_MODE,
        replace: true,
        overwrite: true,
    }
}

/// 生成不包含密钥明文的 Authelia YAML 配置。
fn configuration_yaml(input: &AutheliaConfig, domain: &str, host: &str) -> anyhow::Result<String> {
    let mut output = format!(
        r#"server:
  address: 'tcp://:9091'
  endpoints:
    authz:
      forward-auth:
        implementation: 'ForwardAuth'
log:
  level: 'info'
default_2fa_method: 'totp'
totp:
  disable: false
  issuer: '{domain}'
webauthn:
  disable: true
identity_validation:
  reset_password: {{}}
authentication_backend:
  password_change:
    disable: true
  password_reset:
    disable: true
  file:
    path: '/config/users_database.yml'
    watch: false
access_control:
  default_policy: '{}'
session:
  cookies:
    - name: 'authelia_session'
      domain: '{domain}'
      authelia_url: 'https://{}'
      default_redirection_url: '{}'
      same_site: 'lax'
      inactivity: '5m'
      expiration: '1h'
      remember_me: '1M'
regulation:
  max_retries: 3
  find_time: '2m'
  ban_time: '5m'
storage:
  local:
    path: '/data/db.sqlite3'
notifier:
  filesystem:
    filename: '/data/notification.txt'
"#,
        input.default_policy.as_str(),
        host,
        input.default_redirection_url,
    );
    if let Some(oidc) = &input.oidc {
        output.push_str(&oidc.configuration_yaml());
    }
    if let Some(telemetry) = &input.telemetry
        && !telemetry.is_empty()
    {
        output.push_str(&telemetry.yaml());
    }
    Ok(output)
}

/// 读取 Authelia 导出所需的项目环境字段。
fn required_environment<'a>(spec: &'a StackSpec, key: &str) -> anyhow::Result<&'a str> {
    spec.environment
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("Authelia 项目 .env 缺少 {key}"))
}

/// 返回默认认证门户短主机名。
fn default_host() -> String {
    String::from("auth")
}
