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

/// 把纯文本写入系统剪贴板(macOS)。走原生 NSPasteboard,不依赖浏览器的
/// navigator.clipboard —— 后者要求处于「用户手势」上下文,异步 await 之后会失效
/// (Arthas 命令要点两遍才进剪贴板的根因)。这里同步写入,一次到位。
#[tauri::command]
pub fn copy_text_to_clipboard(text: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use objc2::rc::Retained;
        use objc2::runtime::ProtocolObject;
        use objc2_app_kit::{NSPasteboard, NSPasteboardWriting};
        use objc2_foundation::{NSArray, NSString};

        // NSString 同样实现 NSPasteboardWriting,套用与 copy_files_to_clipboard 一致的写法。
        let ns = NSString::from_str(&text);
        let obj: Retained<ProtocolObject<dyn NSPasteboardWriting>> = ProtocolObject::from_retained(ns);
        let array = NSArray::from_retained_slice(&[obj]);

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
        let _ = text;
        Err("copy text to clipboard is only supported on macOS".into())
    }
}
