//! Authelia 基础认证设施模板及其可重建状态。

use super::{FORMAT_VERSION, GeneratedFile, TemplateKind, TemplateOutput};
use crate::config::{Config, validate_domain};
use crate::constants::PROXY_NETWORK;
use crate::spec::{Document, Healthcheck, Network, Route, RouteProtocol, Service, StackSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

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

/// Authelia 基础认证设施的 TOML 文档。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct AutheliaConfig {
    /// 配置模式版本。
    pub format: u32,
    /// 必填模板选择器。
    pub template: String,
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
    /// 以登录名为键的声明式本地用户库。
    pub users: BTreeMap<String, AutheliaUser>,
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

/// Authelia 文件认证后端中的单个用户。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct AutheliaUser {
    /// 登录后展示的名称。
    pub display_name: String,
    /// 由 Authelia 生成的密码哈希，禁止填写明文密码。
    pub password_hash: String,
    /// 用户邮件地址。
    pub email: String,
    /// 用户所属组。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// 是否禁止该用户登录。
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
}

/// 序列化为 Authelia `users_database.yml` 的顶层文档。
#[derive(Serialize)]
struct UserDatabase<'a> {
    /// 以登录名为键的用户映射。
    users: BTreeMap<&'a str, UserDatabaseEntry<'a>>,
}

/// Authelia 用户数据库要求的字段名称。
#[derive(Serialize)]
struct UserDatabaseEntry<'a> {
    /// 是否禁止该用户登录。
    disabled: bool,
    /// 用户展示名称。
    displayname: &'a str,
    /// 密码哈希。
    password: &'a str,
    /// 用户邮件地址。
    email: &'a str,
    /// 用户所属组。
    groups: &'a [String],
}

/// 从 TOML 生成 Authelia 项目、配置文件和文件型密钥。
pub(super) fn generate(input: &AutheliaConfig, config: &Config) -> anyhow::Result<TemplateOutput> {
    validate_input(input, config)?;
    let host = expand_host(&input.host, &config.domain)?;
    let directory = config.stacks_root.join("authelia");
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
            format!("{}:/config:ro", directory.join("config").display()),
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
            sticky_cookie: false,
            pass_host_header: None,
            priority: None,
        }],
    )?;
    document.services.insert(String::from("authelia"), service);
    let spec = StackSpec {
        name: String::from("authelia"),
        document,
        environment: BTreeMap::from([
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
        ]),
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
    Ok(AutheliaConfig {
        format: FORMAT_VERSION,
        template: String::from("authelia"),
        host,
        version: required_environment(spec, VERSION_KEY)?.to_string(),
        default_redirection_url: required_environment(spec, REDIRECTION_KEY)?.to_string(),
        default_policy: AutheliaPolicy::parse(required_environment(spec, POLICY_KEY)?)?,
        jwt_secret: required_environment(spec, JWT_SECRET_KEY)?.to_string(),
        session_secret: required_environment(spec, SESSION_SECRET_KEY)?.to_string(),
        storage_encryption_key: required_environment(spec, STORAGE_SECRET_KEY)?.to_string(),
        users,
    })
}

/// 校验 Authelia TOML 的模板、版本、域名、密钥与用户字段。
fn validate_input(input: &AutheliaConfig, config: &Config) -> anyhow::Result<()> {
    if input.format != FORMAT_VERSION {
        anyhow::bail!("不支持的配置 format: {}", input.format);
    }
    if input.template != "authelia" {
        anyhow::bail!("Authelia 配置的 template 必须为 authelia");
    }
    crate::spec::validate_version(&input.version)?;
    let _host = expand_host(&input.host, &config.domain)?;
    validate_https_url(&input.default_redirection_url)?;
    validate_secret("jwt_secret", &input.jwt_secret)?;
    validate_secret("session_secret", &input.session_secret)?;
    validate_secret("storage_encryption_key", &input.storage_encryption_key)?;
    if input.users.is_empty() {
        anyhow::bail!("Authelia 至少需要一个声明式用户");
    }
    for (username, user) in &input.users {
        crate::spec::validate_name("Authelia 用户名", username)?;
        validate_user(username, user)?;
    }
    Ok(())
}

/// 校验单个声明式用户且拒绝明文或占位密码。
fn validate_user(username: &str, user: &AutheliaUser) -> anyhow::Result<()> {
    if user.display_name.trim().is_empty()
        || user.display_name.len() > 128
        || user.display_name.contains(['\n', '\r'])
    {
        anyhow::bail!("Authelia 用户 {username} 的 display_name 无效");
    }
    if !user.password_hash.starts_with('$')
        || user.password_hash.len() < 20
        || user.password_hash.contains(char::is_whitespace)
        || user.password_hash.contains("replace-with")
    {
        anyhow::bail!("Authelia 用户 {username} 必须使用有效密码哈希，不能填写明文或占位值");
    }
    validate_email(&user.email)?;
    for group in &user.groups {
        if group.is_empty()
            || group.len() > 64
            || !group
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            anyhow::bail!("Authelia 用户 {username} 的组名无效: {group}");
        }
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

/// 校验 Authelia 文件用户的邮件地址。
fn validate_email(value: &str) -> anyhow::Result<()> {
    if value.len() > 254 || value.chars().any(char::is_whitespace) {
        anyhow::bail!("Authelia 用户邮件地址无效: {value}");
    }
    let (local, domain) = value
        .rsplit_once('@')
        .ok_or_else(|| anyhow::anyhow!("Authelia 用户邮件地址无效: {value}"))?;
    if local.is_empty() {
        anyhow::bail!("Authelia 用户邮件地址无效: {value}");
    }
    validate_domain(domain)
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
    let users = UserDatabase {
        users: input
            .users
            .iter()
            .map(|(name, user)| {
                (
                    name.as_str(),
                    UserDatabaseEntry {
                        disabled: user.disabled,
                        displayname: &user.display_name,
                        password: &user.password_hash,
                        email: &user.email,
                        groups: &user.groups,
                    },
                )
            })
            .collect(),
    };
    Ok(vec![
        generated_file(
            "config/configuration.yml",
            configuration_yaml(input, domain, &host),
            0o640,
        ),
        generated_file(
            "config/users_database.yml",
            serde_yaml::to_string(&users)?,
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
    ])
}

/// 构造每次整体应用时都会替换的模板附属文件。
fn generated_file(path: &str, content: String, mode: u32) -> GeneratedFile {
    GeneratedFile {
        path: PathBuf::from(path),
        content: content.into_bytes(),
        mode,
        replace: true,
    }
}

/// 生成不包含密钥明文的 Authelia YAML 配置。
fn configuration_yaml(input: &AutheliaConfig, domain: &str, host: &str) -> String {
    format!(
        r#"server:
  address: 'tcp://:9091'
  endpoints:
    authz:
      forward-auth:
        implementation: 'ForwardAuth'
log:
  level: 'info'
totp:
  issuer: '{domain}'
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
    )
}

/// 读取 Authelia 导出所需的项目环境字段。
fn required_environment<'a>(spec: &'a StackSpec, key: &str) -> anyhow::Result<&'a str> {
    spec.environment
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| anyhow::anyhow!("Authelia 项目 .env 缺少 {key}"))
}

/// 用于省略 `false` 值的 Serde 辅助函数。
const fn is_false(value: &bool) -> bool {
    !*value
}

/// 返回默认认证门户短主机名。
fn default_host() -> String {
    String::from("auth")
}
