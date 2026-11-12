//! One visible home for 小呆's state and work. Migration is deliberately
//! separate so an existing installation is never silently replaced.
use std::{fs, io, path::{Path, PathBuf}};

pub fn prepare(home: &Path, legacy: &Path) -> io::Result<PathBuf> {
    let root = home.join("小呆 Server");
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(io::Error::other("小呆 Server 数据路径必须是实际目录"));
        }
        Ok(_) => {},
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if legacy.join("brain.json").exists() {
                return Err(io::Error::other("检测到原有小呆数据；请先运行 scripts/migrate_data_root.py 迁移，原数据不会被删除"));
            }
            fs::create_dir(&root)?;
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
            }
        }
        Err(error) => return Err(error),
    }
    if root.join(".migration-in-progress").exists() {
        return Err(io::Error::other("小呆数据迁移尚未完成；请检查迁移结果后重试"));
    }
    Ok(root)
}
