//! One visible home for 小呆's state and work. Migration is deliberately
//! separate so an existing installation is never silently replaced.
use std::{fs, io, path::{Path, PathBuf}};

pub fn prepare(home: &Path, legacy: &Path) -> io::Result<PathBuf> {
    let root = home.join("Xiaodai_Server");
    match fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(io::Error::other("Xiaodai_Server 数据路径必须是实际目录"));
        }
        Ok(_) => {},
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if home.join("小呆 Server/brain.json").exists() {
                return Err(io::Error::other("检测到旧中文目录；请先运行 scripts/relocate_data_root.py，原数据不会被替换"));
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp { fn new()->Self {let p=std::env::temp_dir().join(format!("xiaodai-root-{}",uuid::Uuid::new_v4()));fs::create_dir(&p).unwrap();Self(p)} }
    impl Drop for Temp {fn drop(&mut self){let _=fs::remove_dir_all(&self.0);}}

    #[test]
    fn fresh_install_uses_private_english_directory_without_spaces() {
        let home=Temp::new();let root=prepare(&home.0,&home.0.join("legacy")).unwrap();
        assert_eq!(root,home.0.join("Xiaodai_Server"));
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;assert_eq!(fs::metadata(root).unwrap().permissions().mode() & 0o777,0o700);}
    }

    #[test]
    fn existing_chinese_or_legacy_data_cannot_be_replaced_by_empty_install() {
        for old in ["小呆 Server","legacy"] {
            let home=Temp::new();let old=home.0.join(old);fs::create_dir(&old).unwrap();
            fs::write(old.join("brain.json"),b"existing settings").unwrap();
            assert!(prepare(&home.0,&home.0.join("legacy")).is_err());
            assert!(!home.0.join("Xiaodai_Server").exists());
            assert_eq!(fs::read(old.join("brain.json")).unwrap(),b"existing settings");
        }
    }

    #[test]
    fn incomplete_or_symbolic_destination_cannot_start() {
        let home=Temp::new();let root=home.0.join("Xiaodai_Server");fs::create_dir(&root).unwrap();
        fs::write(root.join(".migration-in-progress"),b"pending").unwrap();
        assert!(prepare(&home.0,&home.0.join("legacy")).is_err());
        fs::remove_dir_all(&root).unwrap();
        #[cfg(unix)] {std::os::unix::fs::symlink(home.0.join("elsewhere"),&root).unwrap();assert!(prepare(&home.0,&home.0.join("legacy")).is_err());}
    }
}
