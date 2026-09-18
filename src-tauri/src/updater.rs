//! 应用内更新：查询 GitHub 最新 Release → 版本比较 → 下载安装包 → 启动安装。
//!
//! 静默原则：自动检查路径上任何失败（断网、限流、解析失败、无本平台安装包）
//! 都只是"没有更新"——宁可漏报，不打扰用户；只有"用户点了立即更新却装不上"
//! 才如实反馈。本模块不依赖 tauri（纯函数 + std + HTTP），事件与线程编排在
//! main.rs。
//!
//! 安装包匹配依赖 CI 产物命名（build.yml）：Windows `*_x64-setup.exe`、
//! macOS Intel `*_x64.dmg` / Apple Silicon `*_aarch64.dmg`——改名/加架构须
//! 同步 `pick_asset`（key-rules #12）。

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// GitHub 仓库（与 git remote、CI Release 一致）
pub const REPO: &str = "Masterchiefm/zcode-speed-panel";

/// 成功解析的最新 Release（只保留本平台安装包）
#[derive(Clone, Debug)]
pub struct Release {
    /// Release tag，如 "v0.3.0"
    pub tag: String,
    /// 去 v 前缀的版本号，如 "0.3.0"
    pub version: String,
    /// Release 页面链接（"更新内容"用系统浏览器打开）
    pub url: String,
    /// Release body（更新说明）
    pub notes: String,
    /// 安装包文件名（自带版本号，天然不与旧版本撞名）
    pub asset_name: String,
    /// 安装包下载地址（302 重定向自动跟随）
    pub asset_url: String,
    /// API 报告的安装包字节数（进度分母兜底）
    pub asset_size: u64,
}

/// "v0.3.1" / "0.3.1" → (0, 3, 1)。v 前缀可省；`-rc.1` 先行版本后缀与
/// `+build` 元数据忽略（本项目不发预发布）；任一段非数字或超三段 → None
pub fn parse_version(tag: &str) -> Option<(u64, u64, u64)> {
    let core = tag.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next()?;
    let mut it = core.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next().unwrap_or("0").parse().ok()?;
    let pat = it.next().unwrap_or("0").parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((maj, min, pat))
}

/// candidate 是否比 current 新。任一侧解析失败 → false：解析不了的 tag
/// 不能当成新版本诱导用户"更新"
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

/// 安装包匹配的平台键（unsupported 平台检查恒"无更新"）
pub fn platform() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "win-x64"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "mac-aarch64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "mac-x64"
    } else {
        "unsupported"
    }
}

/// 在资产文件名里挑本平台安装包（CI 命名约定）：win-x64 → `*_x64-setup.exe`
/// （NSIS 安装版；免安装 portable 版不参与自动安装）；mac-x64 → `*_x64.dmg`；
/// mac-aarch64 → `*_aarch64.dmg`。`_x64.dmg` 不会误配 `_aarch64.dmg`
/// （x64 前是 h 不是下划线），单一后缀匹配即足够
pub fn pick_asset(assets: &[String], platform: &str) -> Option<usize> {
    let suffix = match platform {
        "win-x64" => "_x64-setup.exe",
        "mac-x64" => "_x64.dmg",
        "mac-aarch64" => "_aarch64.dmg",
        _ => return None,
    };
    assets.iter().position(|n| n.ends_with(suffix))
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .build()
}

/// 查询最新 Release（GitHub API 的 latest 端点天然排除 draft/prerelease）。
/// 任何失败（断网、403 限流、解析失败、无本平台安装包）→ None，静默
pub fn fetch_latest(user_agent: &str) -> Option<Release> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = http_agent()
        .get(&url)
        .set("User-Agent", user_agent) // API 对无 UA 的请求直接拒绝
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .call()
        .ok()?;
    let v: serde_json::Value = resp.into_json().ok()?;
    let tag = v.get("tag_name")?.as_str()?.trim().to_string();
    let html_url = v.get("html_url")?.as_str()?.to_string();
    let notes = v.get("body").and_then(|b| b.as_str()).unwrap_or("").trim().to_string();
    let assets = v.get("assets")?.as_array()?;
    // 只保留带 name 的资产（理论上都有），保证 pick_asset 的下标与列表对齐
    let named: Vec<(String, &serde_json::Value)> = assets
        .iter()
        .filter_map(|a| {
            let n = a.get("name").and_then(|n| n.as_str())?;
            Some((n.to_string(), a))
        })
        .collect();
    let names: Vec<String> = named.iter().map(|(n, _)| n.clone()).collect();
    let idx = pick_asset(&names, platform())?;
    let (asset_name, asset) = &named[idx];
    Some(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        asset_url: asset.get("browser_download_url")?.as_str()?.to_string(),
        asset_size: asset.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
        asset_name: asset_name.clone(),
        tag,
        url: html_url,
        notes,
    })
}

/// 下载安装包到系统临时目录（`zcode-speed-panel-update/<资产名>`），边读边回调
/// on_progress(已下载字节, 总字节[未知为 0])。总字节优先取重定向后最终响应的
/// Content-Length，缺失时回退 API 报告的 size；失败删除半截文件
pub fn download(rel: &Release, user_agent: &str, on_progress: &mut dyn FnMut(u64, u64)) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("zcode-speed-panel-update");
    fs::create_dir_all(&dir).map_err(|e| format!("创建下载目录失败: {e}"))?;
    let path = dir.join(&rel.asset_name);
    let resp = http_agent()
        .get(&rel.asset_url)
        .set("User-Agent", user_agent)
        .call()
        .map_err(|e| format!("下载失败: {e}"))?;
    let total = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&t| t > 0)
        .unwrap_or(rel.asset_size);
    let mut reader = resp.into_reader();
    let mut file = fs::File::create(&path).map_err(|e| fail(&path, format!("创建文件失败: {e}")))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut written = 0u64;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| fail(&path, format!("下载中断: {e}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| fail(&path, format!("写入文件失败: {e}")))?;
        written += n as u64;
        on_progress(written, total);
    }
    file.flush().map_err(|e| fail(&path, format!("落盘失败: {e}")))?;
    if written == 0 {
        return Err(fail(&path, "下载内容为空".into()));
    }
    if total > 0 && written != total {
        return Err(fail(&path, format!("下载不完整（{written}/{total} 字节）")));
    }
    Ok(path)
}

/// 删除半截产物并交回错误信息（供 map_err 统一善后）
fn fail(path: &Path, msg: String) -> String {
    let _ = fs::remove_file(path);
    msg
}

/// 启动安装。Windows：直接运行 NSIS 安装包（调用方随后退出应用交接）；
/// macOS：`open` 打开 dmg 由用户拖入 Applications（应用不退出，旧版本继续
/// 跑到用户重启）。其余平台理论到不了这里（platform() 已判 unsupported）
pub fn launch_installer(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("启动安装程序失败: {e}"))
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("打开安装镜像失败: {e}"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = path;
        Err("当前平台不支持自动安装".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("v0.3.1"), Some((0, 3, 1)));
        assert_eq!(parse_version("0.3.1"), Some((0, 3, 1)));
        assert_eq!(parse_version("V1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v0.3.1-rc.1"), Some((0, 3, 1))); // 先行版本后缀忽略
        assert_eq!(parse_version("v0.3.1+build.2"), Some((0, 3, 1)));
        assert_eq!(parse_version("v10.0.0"), Some((10, 0, 0)));
        assert_eq!(parse_version("abc"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn version_ordering() {
        assert!(is_newer("v0.3.0", "0.2.1"));
        assert!(is_newer("v0.2.2", "v0.2.1"));
        assert!(is_newer("v1.0.0", "v0.99.99"));
        assert!(!is_newer("v0.2.1", "0.2.1")); // 相等不算更新
        assert!(!is_newer("v0.2.0", "v0.2.1"));
        assert!(!is_newer("garbage", "0.2.1")); // 解析失败宁可漏报
        assert!(!is_newer("0.2.1", "garbage"));
    }

    #[test]
    fn asset_picking() {
        let assets = vec![
            "zcode-speed-panel_0.3.0_x64-setup.exe".to_string(),
            "zcode-speed-panel_0.3.0_x64-portable.exe".to_string(),
            "zcode-speed-panel_0.3.0_x64.dmg".to_string(),
            "zcode-speed-panel_0.3.0_aarch64.dmg".to_string(),
        ];
        assert_eq!(pick_asset(&assets, "win-x64"), Some(0)); // 安装版优先，portable 不参与
        assert_eq!(pick_asset(&assets, "mac-x64"), Some(2));
        assert_eq!(pick_asset(&assets, "mac-aarch64"), Some(3));
        assert_eq!(pick_asset(&assets, "unsupported"), None);
        // 只有一种产物时不得跨架构误配
        let only_arm = vec!["zcode-speed-panel_0.3.0_aarch64.dmg".to_string()];
        assert_eq!(pick_asset(&only_arm, "mac-x64"), None);
        assert_eq!(pick_asset(&only_arm, "mac-aarch64"), Some(0));
    }
}
