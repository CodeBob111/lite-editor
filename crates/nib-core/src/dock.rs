//! Dock 角标(macOS):终端响铃(BEL,如 Claude Code 跑完/等输入)时,若 Nib 不在前台,
//! 在 Dock 图标上叠加未读计数;回到 Nib 时清零。复用 nib-core 已有的 objc2-app-kit(同
//! clipboard.rs)。AppKit 调用必须在主线程(MainThreadMarker 守卫),非 macOS / 非主线程全部 no-op。

#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU32, Ordering};

/// Dock 角标未读计数。Dock 图标是进程级唯一资源,用全局单例正确建模它(无需穿过 Workbench)。
#[cfg(target_os = "macos")]
static BADGE_COUNT: AtomicU32 = AtomicU32::new(0);

/// 终端响铃且 Nib 不在前台 → 角标 +1 并写到 Dock。Nib 在前台(你正看着它)时不打扰。
/// 须在主线程调用(终端 `pull` 即主线程);非主线程直接 no-op。
pub fn bump_badge() {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;
        use objc2_foundation::NSString;
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        if app.isActive() {
            return;
        }
        let n = BADGE_COUNT.fetch_add(1, Ordering::AcqRel) + 1;
        let label = NSString::from_str(&n.to_string());
        app.dockTile().setBadgeLabel(Some(&label));
    }
}

/// 回到 Nib 时清角标。幂等:计数为 0 时不碰 OS,可被哨兵每拍安全调用。须在主线程调用。
pub fn clear_badge() {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if BADGE_COUNT.swap(0, Ordering::AcqRel) == 0 {
            return;
        }
        let app = NSApplication::sharedApplication(mtm);
        app.dockTile().setBadgeLabel(None);
    }
}
