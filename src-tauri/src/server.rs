//! Localhost bridge for the browser extension.
//!
//! This is the mechanism a real download manager uses to get past an
//! interactive anti-bot challenge: it does not solve the challenge, the
//! browser does. The extension watches for a download, reads the cookies the
//! browser already holds for that site (including whatever `cf_clearance` it
//! earned), and POSTs the URL plus that session here. fetchd then replays a
//! session the browser established.
//!
//! `tiny_http` on a dedicated thread rather than the Tokio runtime: the server
//! is a slow, low-volume control channel (a click at a time), so a blocking
//! listener is simpler than wiring another async stack, and it stays entirely
//! out of the way of the download tasks.

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use serde::Deserialize;
use tauri::{AppHandle, Emitter};
use tiny_http::{Header, Method, Response, Server};

use crate::download::Session;
use crate::state::{self, AppState, ConfirmRequest, PendingAdd};

/// Fixed port so the extension has a constant target. Bound to loopback only.
pub const PORT: u16 = 47831;

/// Threads serving `/add`. Each can block for seconds on a metadata probe, so
/// a few give batch grabs some parallelism without unbounded spawning.
const ADD_WORKERS: usize = 4;

/// The JSON the extension sends.
#[derive(Debug, Deserialize)]
struct AddRequest {
    url: String,
    cookie: Option<String>,
    #[serde(rename = "userAgent")]
    user_agent: Option<String>,
    referer: Option<String>,
    /// Force the yt-dlp video engine (from the extension's "Download video").
    #[serde(default)]
    video: bool,
    /// Open the app's add dialog instead of queuing straight away, so the user
    /// can pick a folder and options for this download.
    #[serde(default)]
    ask: bool,
}

/// Start the bridge on its own thread. Returns immediately; logs and keeps
/// running for the life of the process. A bind failure is not fatal — the app
/// still works without the extension — so it is logged, not propagated.
pub fn start(app: AppHandle, state: Arc<AppState>) {
    std::thread::spawn(move || {
        let addr = (Ipv4Addr::LOCALHOST, PORT);
        let server = match Server::http(addr) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("fetchd: extension bridge disabled, cannot bind 127.0.0.1:{PORT}: {e}");
                return;
            }
        };
        eprintln!("fetchd: extension bridge listening on 127.0.0.1:{PORT}");

        // A bounded pool rather than a thread per request: "download all links"
        // fires one POST per link, and any local process can hit this endpoint,
        // so unbounded spawning would be a cheap way to exhaust threads.
        let (work_tx, work_rx) = std::sync::mpsc::channel::<(tiny_http::Request, String)>();
        let work_rx = Arc::new(std::sync::Mutex::new(work_rx));
        for _ in 0..ADD_WORKERS {
            let rx = Arc::clone(&work_rx);
            let app = app.clone();
            let state = Arc::clone(&state);
            std::thread::spawn(move || loop {
                let job = { rx.lock().unwrap().recv() };
                let Ok((request, body)) = job else { return };
                let response = match handle_add(&app, &state, &body) {
                    Ok(id) => cors(Response::from_string(id)),
                    Err(e) => cors(Response::from_string(e).with_status_code(400)),
                };
                let _ = request.respond(response);
            });
        }

        for mut request in server.incoming_requests() {
            // Defence in depth: tiny_http is bound to loopback already, but a
            // request that somehow arrives from off-box is refused rather than
            // trusted. Any local process can still reach this — an accepted
            // limitation for a personal tool, matching how IDM's local bridge
            // works.
            // ponytail: loopback-only, no auth token; add a token handshake if
            // this ever runs somewhere multi-user.
            if !is_loopback(&request) {
                let _ = request.respond(cors(Response::empty(403)));
                continue;
            }

            match (request.method(), request.url()) {
                // Preflight for the extension's cross-origin POST.
                (Method::Options, _) => {
                    let _ = request.respond(cors(Response::empty(204)));
                }
                // Health check so the extension can tell whether fetchd is up.
                (Method::Get, "/ping") => {
                    let _ = request.respond(cors(Response::from_string("fetchd")));
                }
                (Method::Post, "/add") => {
                    let mut body = String::new();
                    if request.as_reader().read_to_string(&mut body).is_err() {
                        let _ = request.respond(cors(Response::from_string("bad body").with_status_code(400)));
                        continue;
                    }
                    // Hand to the worker pool: a video /add blocks on a ~15s
                    // yt-dlp metadata probe, and the accept loop must stay free
                    // to answer /ping meanwhile.
                    if work_tx.send((request, body)).is_err() {
                        break; // workers gone; nothing left to serve
                    }
                }
                _ => {
                    let _ = request.respond(cors(Response::empty(404)));
                }
            }
        }
    });
}

fn handle_add(app: &AppHandle, state: &Arc<AppState>, body: &str) -> Result<String, String> {
    let req: AddRequest =
        serde_json::from_str(body).map_err(|e| format!("invalid request JSON: {e}"))?;

    let session = Session {
        user_agent: req
            .user_agent
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| crate::download::USER_AGENT.to_string()),
        cookie: req.cookie.filter(|s| !s.is_empty()),
        referer: req.referer.filter(|s| !s.is_empty()),
    };

    // Hand off to the same async path the UI uses. The bridge thread is
    // blocking, so bounce onto the Tokio runtime and wait for the result to
    // report a real error back to the extension.
    let app = app.clone();
    let state = Arc::clone(state);
    let url = req.url.clone();

    let force_video = req.video;

    // "Ask before download": park the captured session and let the UI collect
    // the destination and options. Returns immediately — no metadata probe,
    // no queue entry until the user confirms.
    if req.ask {
        let token = state.stash_pending(PendingAdd {
            url: url.clone(),
            session: Some(session),
            force_video,
            added_at: crate::queue::now_secs(),
        });
        crate::show_main(&app);
        let _ = app.emit(
            "download://confirm",
            ConfirmRequest { token: token.clone(), url, video: force_video },
        );
        return Ok(token);
    }

    let (tx, rx) = std::sync::mpsc::channel();
    tauri::async_runtime::spawn(async move {
        let result = state.add_with_session(&app, &url, Some(session), force_video, Default::default()).await;
        if result.is_ok() {
            state::pump(&app, &state);
        }
        let _ = tx.send(result);
    });

    rx.recv().map_err(|_| "internal error".to_string())?
}

fn is_loopback(request: &tiny_http::Request) -> bool {
    match request.remote_addr() {
        Some(addr) => match addr.ip() {
            IpAddr::V4(ip) => ip.is_loopback(),
            IpAddr::V6(ip) => ip.is_loopback(),
        },
        // Unix socket or unknown transport: not a remote TCP peer, treat as local.
        None => true,
    }
}

/// The extension's service worker makes a cross-origin request, so every reply
/// needs permissive CORS or the browser discards it before the extension sees
/// it. There is nothing secret in these responses (an id or an error string).
fn cors<R>(response: Response<R>) -> Response<R>
where
    R: Read,
{
    let allow_origin = Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap();
    let allow_headers =
        Header::from_bytes(&b"Access-Control-Allow-Headers"[..], &b"Content-Type"[..]).unwrap();
    let allow_methods =
        Header::from_bytes(&b"Access-Control-Allow-Methods"[..], &b"POST, GET, OPTIONS"[..]).unwrap();
    response
        .with_header(allow_origin)
        .with_header(allow_headers)
        .with_header(allow_methods)
}
