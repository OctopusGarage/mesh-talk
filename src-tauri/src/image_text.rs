//! On-demand, bounded system OCR for ordinary image previews.
use crate::{
    chat_commands::NodeState, commands::CommandError, runtime_admission::RuntimeAdmission,
    state::AppState,
};
use base64::Engine;

const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;

fn decode_png(encoded: &str) -> Result<Vec<u8>, CommandError> {
    if encoded.len() > MAX_PNG_BYTES * 4 / 3 + 4 {
        return Err(CommandError::InvalidInput(
            "image text input exceeds limit".into(),
        ));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| CommandError::InvalidInput("invalid image encoding".into()))?;
    if bytes.len() > MAX_PNG_BYTES
        || bytes.len() < 33
        || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
        || &bytes[12..16] != b"IHDR"
    {
        return Err(CommandError::InvalidInput(
            "expected bounded PNG image".into(),
        ));
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 4_000_000 {
        return Err(CommandError::InvalidInput(
            "image text dimensions exceed limit".into(),
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires an explicitly supplied multilingual PNG fixture"]
    fn image_text_native_multilingual_png() {
        let path = std::env::var("MESH_TALK_OCR_TEST_PNG").expect("set MESH_TALK_OCR_TEST_PNG");
        let bytes = std::fs::read(path).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let bytes = decode_png(&encoded).unwrap();
        let text = recognize(&bytes).unwrap();
        assert!(text.contains("Mesh Talk native OCR 12345"), "{text}");
        assert!(text.contains("图片文字识别测试"), "{text}");
    }
    #[test]
    fn image_text_rejects_unbounded_or_invalid_inputs() {
        assert!(decode_png("not base64").is_err());
        assert!(decode_png(&"a".repeat(MAX_PNG_BYTES * 4 / 3 + 5)).is_err());
        let mut bytes = vec![0; 33];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes[12..16].copy_from_slice(b"IHDR");
        bytes[16..20].copy_from_slice(&4000_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&4000_u32.to_be_bytes());
        assert!(decode_png(&base64::engine::general_purpose::STANDARD.encode(&bytes)).is_err());
    }
}

#[tauri::command]
pub async fn owner_recognize_image_text(
    owner: String,
    png: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let revision = app.app_lock().authorization_revision()?;
    let bytes = decode_png(&png)?;
    static CAPACITY: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(1)));
    let permit = CAPACITY
        .clone()
        .try_acquire_owned()
        .map_err(|_| CommandError::InvalidInput("image recognition is busy; try again".into()))?;
    admission.with_runtime(|_| Ok(())).await?;
    let worker_app = (*app).clone();
    let lease = admission.lease().clone();
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        // Check authority after blocking-pool admission; no session guard spans OCR.
        worker_app
            .session()
            .matching(&lease, |_| {
                worker_app
                    .app_lock()
                    .with_unchanged_revision(revision, || Ok(()))
            })
            .map_err(CommandError::Authorization)??;
        recognize(&bytes)
    });
    let text = tokio::time::timeout(std::time::Duration::from_secs(20), task)
        .await
        .map_err(|_| CommandError::InvalidInput("image recognition timed out".into()))?
        .map_err(|_| CommandError::InvalidInput("image recognition failed".into()))??;
    admission
        .with_runtime(|_| {
            app.app_lock()
                .with_unchanged_revision(revision, || Ok(text))
        })
        .await
}

#[cfg(not(target_os = "macos"))]
fn recognize(_bytes: &[u8]) -> Result<String, CommandError> {
    Err(CommandError::InvalidInput(
        "system image text is unavailable on this platform".into(),
    ))
}

#[cfg(target_os = "macos")]
fn recognize(bytes: &[u8]) -> Result<String, CommandError> {
    use objc2::{
        class, msg_send,
        rc::{Allocated, Retained, autoreleasepool},
        runtime::AnyObject,
    };
    use objc2_foundation::NSString;
    #[link(name = "Vision", kind = "framework")]
    unsafe extern "C" {}
    // SAFETY: Foundation/Vision objects stay on this worker, NSData copies bounded input,
    // selectors match Apple's Objective-C APIs, and retained values outlive every call.
    unsafe {
        autoreleasepool(|_| {
            let data: Retained<AnyObject> =
                msg_send![class!(NSData), dataWithBytes: bytes.as_ptr(), length: bytes.len()];
            let request_class = objc2::runtime::AnyClass::get(c"VNRecognizeTextRequest")
                .ok_or_else(|| {
                    CommandError::InvalidInput(
                        "system image text requires macOS 10.15 or later".into(),
                    )
                })?;
            let request: Retained<AnyObject> = msg_send![request_class, new];
            let _: () = msg_send![&request, setRecognitionLevel: 0_isize];
            let zh = NSString::from_str("zh-Hans");
            let en = NSString::from_str("en-US");
            let languages = [
                Retained::as_ptr(&zh).cast::<AnyObject>(),
                Retained::as_ptr(&en).cast::<AnyObject>(),
            ];
            let languages: Retained<AnyObject> = msg_send![class!(NSArray), arrayWithObjects: languages.as_ptr(), count: languages.len()];
            let _: () = msg_send![&request, setRecognitionLanguages: &*languages];
            let options: Retained<AnyObject> = msg_send![class!(NSDictionary), new];
            let handler: Allocated<AnyObject> = msg_send![class!(VNImageRequestHandler), alloc];
            let handler: Retained<AnyObject> =
                msg_send![handler, initWithData: &*data, options: &*options];
            let requests = [Retained::as_ptr(&request)];
            let requests: Retained<AnyObject> =
                msg_send![class!(NSArray), arrayWithObjects: requests.as_ptr(), count: 1_usize];
            let mut error: *mut AnyObject = std::ptr::null_mut();
            let ok: bool = msg_send![&handler, performRequests: &*requests, error: &mut error];
            if !ok {
                return Err(CommandError::InvalidInput(
                    "system image recognition failed".into(),
                ));
            }
            let results: Retained<AnyObject> = msg_send![&request, results];
            let count: usize = msg_send![&results, count];
            let mut lines = Vec::new();
            let mut total = 0;
            for index in 0..count.min(4096) {
                let observation: Retained<AnyObject> = msg_send![&results, objectAtIndex: index];
                let candidates: Retained<AnyObject> =
                    msg_send![&observation, topCandidates: 1_usize];
                let n: usize = msg_send![&candidates, count];
                if n == 0 {
                    continue;
                }
                let candidate: Retained<AnyObject> = msg_send![&candidates, objectAtIndex: 0_usize];
                let text: Retained<NSString> = msg_send![&candidate, string];
                let text = text.to_string();
                total += text.len();
                if total > 256 * 1024 {
                    return Err(CommandError::InvalidInput(
                        "recognized text exceeds limit".into(),
                    ));
                }
                lines.push(text);
            }
            Ok(lines.join("\n"))
        })
    }
}
