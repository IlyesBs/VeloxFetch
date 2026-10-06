//! Live preview over a Tauri v2 asynchronous custom protocol:
//!
//! ```text
//! <video src="http://velox-preview.localhost/12">   (Windows; velox-preview://localhost/12 on macOS / Linux)
//!     -> handler (this file): main window only, id only, GET / HEAD only
//!     -> DownloadManager::read_preview(12, Range)   (only written bytes of an audio / video download)
//!     -> 206 Partial Content + Content-Range, or an error status
//! ```
//! The answer is produced on the async runtime, never on the web view's thread, and every problem is an
//! HTTP status: a broken preview never touches the download.

use std::borrow::Cow;

use tauri::http::{header, Method, Request, Response, StatusCode};
use tauri::{Manager, Runtime, UriSchemeContext, UriSchemeResponder};

use crate::AppState;

pub const SCHEME: &str = "velox-preview";
/// The only window allowed to use it (the details live in the main window).
const WINDOW: &str = "main";

fn plain(status: StatusCode) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Cow::Borrowed(&[][..]))
        .expect("a static response")
}

/// The download id from `/12` (nothing else is accepted).
pub fn id_from_path(path: &str) -> Option<i64> {
    let s = path.strip_prefix('/')?;
    (!s.is_empty() && s.len() <= 15 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// What the player asks for: a download (`/12`) or a video job (`/m3`).
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Download(i64),
    Media(u64),
}

pub fn target_from_path(path: &str) -> Option<Target> {
    if let Some(rest) = path.strip_prefix("/m") {
        return id_from_path(&format!("/{rest}")).and_then(|n| u64::try_from(n).ok()).map(Target::Media);
    }
    id_from_path(path).map(Target::Download)
}

/// The handler given to `register_asynchronous_uri_scheme_protocol`.
pub fn handle<R: Runtime>(ctx: UriSchemeContext<'_, R>, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    if ctx.webview_label() != WINDOW {
        return responder.respond(plain(StatusCode::FORBIDDEN));
    }
    let head = request.method() == Method::HEAD;
    if request.method() != Method::GET && !head {
        return responder.respond(plain(StatusCode::METHOD_NOT_ALLOWED));
    }
    let Some(target) = target_from_path(request.uri().path()) else {
        return responder.respond(plain(StatusCode::NOT_FOUND));
    };
    let range = request.headers().get(header::RANGE).and_then(|v| v.to_str().ok()).map(str::to_string);
    let app = ctx.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return responder.respond(plain(StatusCode::SERVICE_UNAVAILABLE));
        };
        let r = match target {
            Target::Download(id) => state.manager.read_preview(id, range.as_deref()).await,
            Target::Media(id) => state.manager.read_media_preview(id, range.as_deref()).await,
        };
        let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut b = Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, r.content_type)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CACHE_CONTROL, "no-store")
            .header("X-Content-Type-Options", "nosniff")
            .header(header::CONTENT_SECURITY_POLICY, "default-src 'none'")
            .header(header::CONTENT_LENGTH, r.body.len());
        if let Some(cr) = r.content_range {
            b = b.header(header::CONTENT_RANGE, cr);
        }
        if status == StatusCode::SERVICE_UNAVAILABLE {
            b = b.header(header::RETRY_AFTER, "1");
        }
        let body = if head { Cow::Borrowed(&[][..]) } else { Cow::Owned(r.body) };
        responder.respond(b.body(body).unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR)));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_video_job_is_asked_for_by_its_id_only() {
        assert_eq!(target_from_path("/m3"), Some(Target::Media(3)));
        assert_eq!(target_from_path("/12"), Some(Target::Download(12)));
        for bad in ["/m", "/m-1", "/m3/x", "/mm3", "/m../x", "/m%33", "/x3"] {
            assert_eq!(target_from_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_a_download_id_is_accepted() {
        assert_eq!(id_from_path("/12"), Some(12));
        for bad in ["/", "", "12", "/12/x", "/-1", "/1e3", "/../velox.db", "/C:/x.mp4", "/1234567890123456", "/%31"] {
            assert_eq!(id_from_path(bad), None, "{bad}");
        }
    }
}
