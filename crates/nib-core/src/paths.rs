// 数据/缓存根目录由宿主注入(critic V11/V12/V13):
// - 旧 Nib(src-tauri)传 Tauri app_data_dir + ~/Library/Caches/lite-editor —— 路径与历史完全一致;
// - 原生 nib-app 传 ~/Library/Application Support/nib + ~/Library/Caches/nib —— 与旧 app 零共享
//   可变文件,首启做一次性导入。并存期互踩(session 覆盖/jdtls 锁/索引撕裂)从结构上消除。

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DataDirs {
    /// session.json / settings.json 所在目录
    pub app_data: PathBuf,
    /// jdtls workspace / 反编译缓存 / java-index / usage-index 的根
    pub cache: PathBuf,
    /// astore 登录会话文件(旧 Nib 的历史位置在 data_local_dir/lite-editor 下,
    /// 与 app_data 不同目录,所以单列而不是从 app_data 推导)
    pub astore_session: PathBuf,
}

impl DataDirs {
    pub fn jdtls_workspaces(&self) -> PathBuf {
        self.cache.join("jdtls")
    }

    pub fn decompiled(&self) -> PathBuf {
        self.cache.join("decompiled")
    }

    pub fn java_index(&self) -> PathBuf {
        self.cache.join("java-index")
    }

    pub fn usage_index(&self) -> PathBuf {
        self.cache.join("usage-index")
    }
}
