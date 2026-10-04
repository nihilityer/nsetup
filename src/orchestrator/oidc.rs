//! 应用 OIDC 客户端片段与 Authelia 项目之间的跨项目同步。

use super::Orchestrator;
use super::storage::{remove_attachment, replace_attachment};
use crate::constants::COMPOSE_FILE;
use crate::spec::StackSpec;
use crate::template::{self, GeneratedFile, TemplateKind, TemplateOutput};
use anyhow::Context;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Authelia 项目配置目录中客户端片段的私有目录权限。
const FRAGMENT_DIRECTORY_MODE: u32 = 0o750;

/// 一次应用对 Authelia 客户端片段造成的变更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OidcChange {
    /// 客户端片段无需变化。
    Unchanged,
    /// 应用拥有客户端，需要写入片段。
    Sync(GeneratedFile),
    /// 应用不再声明客户端，需要移除既有片段。
    Remove,
}

impl Orchestrator {
    /// 判断 Authelia 项目是否已部署。
    pub(super) fn authelia_deployed(&self) -> anyhow::Result<bool> {
        Ok(self.project_dir("authelia")?.is_dir())
    }

    /// 在部署 Authelia 时附加全部现有应用拥有的 OIDC 客户端片段。
    ///
    /// # 错误
    ///
    /// 任一现有项目状态无效或 client ID 冲突时返回错误。
    pub(super) fn attach_oidc_client_fragments(
        &self,
        generated: &mut TemplateOutput,
    ) -> anyhow::Result<()> {
        if generated.kind != TemplateKind::Authelia {
            return Ok(());
        }
        let fragments = self.collect_oidc_fragments(Some(&generated.spec))?;
        generated
            .files
            .extend(fragments.into_iter().filter_map(|(_, fragment)| fragment));
        Ok(())
    }

    /// 计算本次应用需要对 Authelia 客户端片段做的变更。
    ///
    /// 同时校验 client ID 全局唯一，以及应用声明客户端时 provider 已启用。
    ///
    /// # 错误
    ///
    /// client ID 冲突、provider 未启用或既有片段无法读取时返回错误。
    pub(super) fn prepare_oidc_change(&self, requested: &StackSpec) -> anyhow::Result<OidcChange> {
        if requested.name == "authelia" {
            return Ok(OidcChange::Unchanged);
        }
        let authelia = self.project_dir("authelia")?;
        if !authelia.is_dir() {
            // 还没有 Authelia 项目：仍然校验 client ID 唯一性，但不产生片段。
            let _fragments = self.collect_oidc_fragments(Some(requested))?;
            return Ok(OidcChange::Unchanged);
        }
        let fragments = self.collect_oidc_fragments(Some(requested))?;
        let desired = fragments
            .into_iter()
            .find_map(|(name, fragment)| (name == requested.name).then_some(fragment).flatten());
        let destination = authelia.join(oidc_fragment_path(&requested.name));
        let Some(file) = desired else {
            return Ok(match fs::symlink_metadata(&destination) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    OidcChange::Remove
                }
                Ok(_) => anyhow::bail!("OIDC 片段路径不是普通文件: {}", destination.display()),
                Err(_) => OidcChange::Unchanged,
            });
        };
        Ok(OidcChange::Sync(file))
    }

    /// 计算删除项目后需要移除的客户端片段。
    ///
    /// # 错误
    ///
    /// 既有片段路径不是普通文件时返回错误。
    pub(super) fn prepare_oidc_removal(&self, name: &str) -> anyhow::Result<OidcChange> {
        if name == "authelia" {
            return Ok(OidcChange::Unchanged);
        }
        let authelia = self.project_dir("authelia")?;
        if !authelia.is_dir() {
            return Ok(OidcChange::Unchanged);
        }
        let destination = authelia.join(oidc_fragment_path(name));
        match fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                Ok(OidcChange::Remove)
            }
            Ok(_) => anyhow::bail!("OIDC 片段路径不是普通文件: {}", destination.display()),
            Err(_) => Ok(OidcChange::Unchanged),
        }
    }

    /// 应用客户端片段变更，返回是否真的改动了 Authelia 项目的状态，以及是否顺带重启。
    ///
    /// `restart_dependents` 为真且确实发生变更时，顺带重启已部署的 Authelia 项目；
    /// Authelia 只在进程启动时读取客户端片段，不重启则新客户端不会生效。返回值里的
    /// 重启标记用于生成与实际行为一致的提示文案。
    ///
    /// # 错误
    ///
    /// 写入或删除片段失败，或重启 Authelia 失败时返回错误。
    pub(super) fn apply_oidc_change(
        &self,
        project_name: &str,
        change: OidcChange,
        restart_dependents: bool,
    ) -> anyhow::Result<(bool, bool)> {
        let updated = self.write_oidc_change(project_name, change)?;
        if !updated || !restart_dependents || !self.authelia_deployed()? {
            return Ok((updated, false));
        }
        crate::docker::compose_restart(&self.config, &self.project_dir("authelia")?)?;
        Ok((updated, true))
    }

    /// 将单个项目的期望客户端片段写入或移出已部署的 Authelia 项目。
    ///
    /// # 错误
    ///
    /// 写入或删除片段失败时返回错误。
    fn write_oidc_change(&self, project_name: &str, change: OidcChange) -> anyhow::Result<bool> {
        if project_name == "authelia" {
            return Ok(false);
        }
        let authelia = self.project_dir("authelia")?;
        if !authelia.is_dir() {
            return Ok(false);
        }
        let relative = oidc_fragment_path(project_name);
        match change {
            OidcChange::Unchanged => Ok(false),
            OidcChange::Remove => remove_attachment(&authelia, &relative),
            OidcChange::Sync(file) => {
                let destination = authelia.join(&relative);
                let current = fs::symlink_metadata(&destination).ok();
                let safe_file = current.as_ref().is_some_and(|metadata| {
                    metadata.is_file() && !metadata.file_type().is_symlink()
                });
                let expected_mode = current
                    .as_ref()
                    .is_some_and(|metadata| metadata.permissions().mode() & 0o777 == file.mode);
                if safe_file
                    && expected_mode
                    && fs::read(&destination).is_ok_and(|content| content == file.content)
                {
                    return Ok(false);
                }
                replace_attachment(&authelia, &file, FRAGMENT_DIRECTORY_MODE)?;
                Ok(true)
            }
        }
    }

    /// 收集现有项目并用请求状态替换同名项目，校验 client ID 全局唯一。
    fn collect_oidc_fragments(
        &self,
        requested: Option<&StackSpec>,
    ) -> anyhow::Result<Vec<(String, Option<GeneratedFile>)>> {
        let mut specs = self.existing_specs(requested.map(|spec| spec.name.as_str()))?;
        if let Some(spec) = requested {
            specs.push(spec.clone());
        }
        validate_and_collect(specs)
    }

    /// 加载全部现有受管项目，跳过请求将替换的同名项目。
    fn existing_specs(&self, excluded: Option<&str>) -> anyhow::Result<Vec<StackSpec>> {
        if !self.config.stacks_root.is_dir() {
            return Ok(Vec::new());
        }
        let mut specs = Vec::new();
        for entry in fs::read_dir(&self.config.stacks_root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !entry.file_type()?.is_dir()
                || name.starts_with('.')
                || excluded == Some(name.as_ref())
                || !entry.path().join(COMPOSE_FILE).is_file()
            {
                continue;
            }
            specs.push(
                StackSpec::load(&entry.path())
                    .with_context(|| format!("无法读取项目 {name} 的 OIDC 客户端声明"))?,
            );
        }
        specs.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(specs)
    }
}

/// 校验跨项目 provider 与 client ID 约束，生成每个 app 的客户端片段。
fn validate_and_collect(
    specs: Vec<StackSpec>,
) -> anyhow::Result<Vec<(String, Option<GeneratedFile>)>> {
    let mut owners = BTreeMap::new();
    let mut fragments = Vec::new();
    let mut provider = None;
    for spec in specs {
        if spec.name == "authelia" {
            provider = Some(template::authelia_oidc_enabled(&spec));
            continue;
        }
        for client_id in template::app_oidc_client_ids(&spec)? {
            if let Some(existing) = owners.insert(client_id.clone(), spec.name.clone()) {
                anyhow::bail!(
                    "Authelia OIDC client_id {client_id} 同时由项目 {existing} 和 {} 声明",
                    spec.name
                );
            }
        }
        fragments.push((
            spec.name.clone(),
            template::app_oidc_client_fragment(&spec)?,
        ));
    }
    if owners.is_empty() || provider != Some(false) {
        return Ok(fragments);
    }
    anyhow::bail!("应用声明了 Authelia OIDC 客户端，但现有 authelia 项目未启用 [oidc]")
}

/// 返回应用项目在 Authelia 配置目录内拥有的片段路径。
fn oidc_fragment_path(project_name: &str) -> PathBuf {
    Path::new("config/oidc-clients").join(format!("{project_name}.yml"))
}

#[cfg(test)]
mod tests {
    use super::validate_and_collect;
    use crate::config::Config;
    use crate::spec::StackSpec;

    /// 不同应用不能声明同一个全局 client ID。
    #[test]
    fn duplicate_client_ids_are_rejected() -> anyhow::Result<()> {
        let error = validate_and_collect(vec![app_spec("alpha")?, app_spec("beta")?])
            .err()
            .ok_or_else(|| anyhow::anyhow!("duplicate client ID should fail"))?;
        let message = error.to_string();
        assert!(message.contains("alpha"));
        assert!(message.contains("beta"));
        assert!(message.contains("shared"));
        Ok(())
    }

    /// 已存在的 Authelia 项目必须先启用 provider。
    #[test]
    fn deployed_authelia_without_provider_rejects_clients() -> anyhow::Result<()> {
        let error = validate_and_collect(vec![authelia_spec(false)?, app_spec("alpha")?])
            .err()
            .ok_or_else(|| anyhow::anyhow!("disabled provider should fail"))?;
        assert!(error.to_string().contains("未启用 [oidc]"));
        Ok(())
    }

    /// provider 存在时产生按 app 项目名隔离的片段。
    #[test]
    fn enabled_provider_collects_app_fragment() -> anyhow::Result<()> {
        let fragments = validate_and_collect(vec![authelia_spec(true)?, app_spec("alpha")?])?;
        let (_, fragment) = fragments
            .into_iter()
            .find(|(name, _)| name == "alpha")
            .ok_or_else(|| anyhow::anyhow!("missing alpha project"))?;
        let fragment = fragment.ok_or_else(|| anyhow::anyhow!("missing alpha fragment"))?;
        assert!(fragment.path.ends_with("oidc-clients/alpha.yml"));
        assert!(String::from_utf8(fragment.content)?.contains("client_id: shared"));
        Ok(())
    }

    /// 生成一个声明公共 OIDC 客户端的 app IR。
    fn app_spec(name: &str) -> anyhow::Result<StackSpec> {
        let input = format!(
            r#"
format = 1
name = "{name}"
[authelia.oidc_clients.shared]
client_name = "Shared"
public = true
redirect_uris = ["https://{name}.example.com/oauth/callback"]
require_pkce = true
token_endpoint_auth_method = "none"
[services.web]
image = "example/web"
version = "1.0"
"#
        );
        Ok(crate::template::apply(&input, &Config::default(), None)?.spec)
    }

    /// 生成启用或未启用 OIDC provider 的 Authelia IR。
    fn authelia_spec(oidc: bool) -> anyhow::Result<StackSpec> {
        let oidc = if oidc {
            r#"
[oidc]
hmac_secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
jwk_private_key = """
-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789
abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefgh
ijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnop
-----END PRIVATE KEY-----
"""
"#
        } else {
            ""
        };
        let input = format!(
            r#"
format = 1
template = "authelia"
version = "4.39.20"
default_redirection_url = "https://example.com"
jwt_secret = "jwt-secret-value-0123456789-abcdef"
session_secret = "session-secret-value-0123456789-ab"
storage_encryption_key = "storage-secret-value-0123456789-a"
{oidc}
[users.admin]
display_name = "Administrator"
password_hash = '$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$aGFzaA'
email = "admin@example.com"
"#
        );
        Ok(crate::template::apply(&input, &Config::default(), None)?.spec)
    }
}
