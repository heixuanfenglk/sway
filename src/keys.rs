use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// 生成 Ed25519 密钥对：私钥写入 `private_path`，公钥写入同目录的 `*.pub`。
pub fn generate_ed25519_keypair(private_path: &Path, comment: &str) -> Result<PathBuf> {
    use ssh_key::getrandom::SysRng;
    use ssh_key::rand_core::UnwrapErr;
    use ssh_key::{Algorithm, LineEnding, PrivateKey};

    if let Some(parent) = private_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).context("创建密钥目录失败")?;
        }
    }

    let mut rng = UnwrapErr(SysRng);
    let mut key =
        PrivateKey::random(&mut rng, Algorithm::Ed25519).context("生成 Ed25519 密钥失败")?;
    if !comment.trim().is_empty() {
        key.set_comment(comment);
    }

    key.write_openssh_file(private_path, LineEnding::LF)
        .context("写入私钥失败")?;

    let pub_path = public_key_path(private_path);
    let pub_text = key
        .public_key()
        .to_openssh()
        .context("序列化公钥失败")?;
    std::fs::write(&pub_path, format!("{pub_text}\n")).context("写入公钥失败")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(private_path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(pub_path)
}

pub fn public_key_path(private_path: &Path) -> PathBuf {
    let mut pub_path = private_path.to_path_buf();
    let name = private_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("id_ed25519");
    pub_path.set_file_name(format!("{name}.pub"));
    pub_path
}
