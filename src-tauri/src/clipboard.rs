// 系统剪贴板：把文件以「文件引用」(public.file-url) 写入 NSPasteboard，
// 这样就能在 Finder 等其他 app 里 Cmd+V 粘贴出真正的文件 —— 这是 IDEA 的行为。
// JS 的 navigator.clipboard 只能放纯文本，放不了文件引用，所以必须走原生 AppKit。

/// 把若干文件路径写入系统剪贴板（macOS）。支持多选：一次写入多个 file-url。
#[tauri::command]
pub fn copy_files_to_clipboard(paths: Vec<String>) -> Result<(), String> {
    if paths.is_empty() {
        return Err("no files to copy".into());
    }

    #[cfg(target_os = "macos")]
    {
        use objc2::rc::Retained;
        use objc2::runtime::ProtocolObject;
        use objc2_app_kit::{NSPasteboard, NSPasteboardWriting};
        use objc2_foundation::{NSArray, NSString, NSURL};

        // 每个路径转成 file:// 的 NSURL，再擦成 NSPasteboardWriting 协议对象。
        let objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = paths
            .iter()
            .map(|p| {
                let url = NSURL::fileURLWithPath(&NSString::from_str(p));
                ProtocolObject::from_retained(url)
            })
            .collect();

        let array = NSArray::from_retained_slice(&objects);
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        if pasteboard.writeObjects(&array) {
            Ok(())
        } else {
            Err("NSPasteboard writeObjects failed".into())
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err("copy files to clipboard is only supported on macOS".into())
    }
}
