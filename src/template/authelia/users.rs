//! Authelia 文件认证后端的声明、校验与 YAML 生成。

use crate::config::validate_domain;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Authelia 文件认证后端中的单个用户。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::template) struct AutheliaUser {
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

/// 校验单个声明式用户且拒绝明文或占位密码。
pub(super) fn validate(username: &str, user: &AutheliaUser) -> anyhow::Result<()> {
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

/// 将声明式用户映射序列化为 Authelia 文件用户数据库。
pub(super) fn database_yaml(users: &BTreeMap<String, AutheliaUser>) -> anyhow::Result<String> {
    let database = UserDatabase {
        users: users
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
    Ok(serde_yaml::to_string(&database)?)
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

/// 用于省略 `false` 值的 Serde 辅助函数。
const fn is_false(value: &bool) -> bool {
    !*value
}
