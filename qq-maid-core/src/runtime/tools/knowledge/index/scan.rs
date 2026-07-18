use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone)]
pub(super) struct ScannedMarkdown {
    pub relative_path: String,
    pub absolute_path: PathBuf,
    pub modified_at: Option<String>,
}

pub(super) fn scan_markdown_files(dir: &Path) -> Result<Vec<ScannedMarkdown>, io::Error> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    scan_dir(dir, dir, &mut files)?;
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

fn scan_dir(root: &Path, dir: &Path, files: &mut Vec<ScannedMarkdown>) -> Result<(), io::Error> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if should_ignore_name(&file_name) {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        // 知识目录只能扫描目录内的真实文件；符号链接可能指向 prompt、旧上下文或
        // 目录外私有资料，不能跟随后写入默认检索索引。
        if file_type.is_symlink() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        if file_type.is_dir() {
            scan_dir(root, &path, files)?;
            continue;
        }
        // 公开 release 包会带 *.example.md 模板；模板用于说明配置格式，不能进入真实知识索引。
        if !file_type.is_file() || !is_markdown_file(&path) || is_markdown_example_file(&path) {
            continue;
        }
        let Some(relative_path) = relative_slash_path(root, &path) else {
            continue;
        };
        files.push(ScannedMarkdown {
            relative_path,
            absolute_path: path,
            modified_at: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs().to_string()),
        });
    }
    Ok(())
}

fn should_ignore_name(name: &str) -> bool {
    name.starts_with('.')
        || name.ends_with('~')
        || name.ends_with(".tmp")
        || name.ends_with(".temp")
        || name.ends_with(".bak")
        || name.ends_with(".swp")
}

fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown"))
        .unwrap_or(false)
}

fn is_markdown_example_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            let name = name.to_ascii_lowercase();
            name.ends_with(".example.md") || name.ends_with(".example.markdown")
        })
        .unwrap_or(false)
}

fn relative_slash_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}
