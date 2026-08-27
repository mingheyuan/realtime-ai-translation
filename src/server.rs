use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio::{
    sync::{broadcast, oneshot, Mutex, Semaphore},
    task::JoinSet,
    time::interval,
};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    asr::{AppleSpeechBridge, AsrEvent},
    config::AppConfig,
    dictionary::{DictionaryError, DictionaryStore},
    domain::{
        CaptionState, CorrectionRequest, GlossaryEntry, SegmentSnapshot, ServerEvent,
        StartSessionRequest,
    },
    llm::LlmRefiner,
    segment::SegmentManager,
    streaming::TranslationStateMachine,
    translation::{
        FakeTranslator, PythonMarianTranslator, SharedTranslationProvider, TranslationOutput,
    },
};

#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    config: AppConfig,
    dictionary: DictionaryStore,
    translator: SharedTranslationProvider,
    translation_gate: Arc<Semaphore>,
    llm: LlmRefiner,
    events: broadcast::Sender<ServerEvent>,
    session: Mutex<Option<SessionHandle>>,
    context: Mutex<VecDeque<(String, String)>>,
    streaming: Mutex<TranslationStateMachine>,
}

struct SessionHandle {
    id: Uuid,
    stop: Option<oneshot::Sender<()>>,
}

impl AppState {
    pub fn new(config: AppConfig) -> anyhow::Result<Self> {
        let dictionary = DictionaryStore::open(config.database_path.clone())?;
        let translator: SharedTranslationProvider = if config.fake_translation {
            Arc::new(FakeTranslator)
        } else {
            Arc::new(PythonMarianTranslator::start(
                config.python_path.clone(),
                config.model_worker_path.clone(),
            ))
        };
        let llm = LlmRefiner::new(config.llm.clone())?;
        let (events, _) = broadcast::channel(128);
        Ok(Self {
            inner: Arc::new(AppStateInner {
                config,
                dictionary,
                translator,
                translation_gate: Arc::new(Semaphore::new(1)),
                llm,
                events,
                session: Mutex::new(None),
                context: Mutex::new(VecDeque::with_capacity(4)),
                streaming: Mutex::new(TranslationStateMachine::default()),
            }),
        })
    }

    fn emit(&self, event: ServerEvent) {
        let _ = self.inner.events.send(event);
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(javascript))
        .route("/styles.css", get(stylesheet))
        .route("/overlay", get(overlay))
        .route("/overlay.js", get(overlay_javascript))
        .route("/overlay.css", get(overlay_stylesheet))
        .route("/ws", get(websocket))
        .route("/api/health", get(health))
        .route("/api/overlay/open", post(open_overlay))
        .route("/api/session/start", post(start_session))
        .route("/api/session/stop", post(stop_session))
        .route(
            "/api/dictionary",
            get(list_dictionary).post(upsert_dictionary),
        )
        .route("/api/dictionary/:id", delete(delete_dictionary))
        .route("/api/corrections", post(record_correction))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("static/index.html"))
}

async fn javascript() -> impl IntoResponse {
    (
        [("content-type", "text/javascript; charset=utf-8")],
        include_str!("static/app.js"),
    )
}

async fn stylesheet() -> impl IntoResponse {
    (
        [("content-type", "text/css; charset=utf-8")],
        include_str!("static/styles.css"),
    )
}

async fn overlay() -> Html<&'static str> {
    Html(include_str!("static/overlay.html"))
}

async fn overlay_javascript() -> impl IntoResponse {
    (
        [("content-type", "text/javascript; charset=utf-8")],
        include_str!("static/overlay.js"),
    )
}

async fn overlay_stylesheet() -> impl IntoResponse {
    (
        [("content-type", "text/css; charset=utf-8")],
        include_str!("static/overlay.css"),
    )
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    speech_bridge_ready: bool,
    overlay_ready: bool,
    model_worker_ready: bool,
    fake_translation: bool,
    llm_enabled: bool,
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        speech_bridge_ready: state.inner.config.speech_bridge_path.is_dir(),
        overlay_ready: state.inner.config.overlay_app_path.is_dir(),
        model_worker_ready: state.inner.config.python_path.is_file()
            && state.inner.config.model_worker_path.is_file(),
        fake_translation: state.inner.config.fake_translation,
        llm_enabled: state.inner.config.llm.enabled,
    })
}

#[derive(Serialize)]
struct OverlayResponse {
    opened: bool,
}

async fn open_overlay(State(state): State<AppState>) -> Result<Json<OverlayResponse>, ApiError> {
    let app_path = &state.inner.config.overlay_app_path;
    if !app_path.is_dir() {
        return Err(ApiError::service_unavailable(
            "悬浮字幕组件尚未构建，请先运行 ./scripts/build-macos-overlay.sh",
        ));
    }

    #[cfg(target_os = "macos")]
    {
        let overlay_url = format!(
            "http://127.0.0.1:{}/overlay",
            state.inner.config.listen.port()
        );
        let status = tokio::process::Command::new("open")
            .arg(app_path)
            .arg("--args")
            .arg(overlay_url)
            .status()
            .await
            .map_err(|error| ApiError::internal(format!("无法启动悬浮字幕：{error}")))?;
        if !status.success() {
            return Err(ApiError::internal(format!(
                "macOS 打开悬浮字幕失败（状态：{status}）"
            )));
        }
        Ok(Json(OverlayResponse { opened: true }))
    }

    #[cfg(not(target_os = "macos"))]
    Err(ApiError::service_unavailable("悬浮字幕目前只支持 macOS"))
}

async fn websocket(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| websocket_loop(socket, state))
}

async fn websocket_loop(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = state.inner.events.subscribe();
    loop {
        tokio::select! {
            incoming = receiver.next() => {
                if incoming.is_none() || matches!(incoming, Some(Ok(Message::Close(_)))) {
                    break;
                }
            }
            event = events.recv() => {
                let event = match event {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let Ok(payload) = serde_json::to_string(&event) else {
                    continue;
                };
                if sender.send(Message::Text(payload)).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[derive(Serialize)]
struct SessionResponse {
    session_id: Option<Uuid>,
    running: bool,
}

async fn start_session(
    State(state): State<AppState>,
    Json(request): Json<StartSessionRequest>,
) -> Result<Json<SessionResponse>, ApiError> {
    validate_direction(&request)?;
    let hotwords = state.inner.dictionary.hotwords(&request.source_language)?;
    let mut session = state.inner.session.lock().await;
    if let Some(current) = session.as_ref() {
        return Ok(Json(SessionResponse {
            session_id: Some(current.id),
            running: true,
        }));
    }
    state.inner.context.lock().await.clear();
    state.inner.streaming.lock().await.reset();
    let id = Uuid::new_v4();
    let (stop_tx, stop_rx) = oneshot::channel();
    *session = Some(SessionHandle {
        id,
        stop: Some(stop_tx),
    });
    drop(session);
    let task_state = state.clone();
    tokio::spawn(async move {
        run_session(task_state.clone(), id, request, hotwords, stop_rx).await;
        let mut session = task_state.inner.session.lock().await;
        if session.as_ref().map(|handle| handle.id) == Some(id) {
            *session = None;
        }
    });
    Ok(Json(SessionResponse {
        session_id: Some(id),
        running: true,
    }))
}

async fn stop_session(State(state): State<AppState>) -> Json<SessionResponse> {
    let mut session = state.inner.session.lock().await;
    let Some(handle) = session.as_mut() else {
        return Json(SessionResponse {
            session_id: None,
            running: false,
        });
    };
    if let Some(stop) = handle.stop.take() {
        let _ = stop.send(());
    }
    Json(SessionResponse {
        session_id: Some(handle.id),
        running: false,
    })
}

async fn run_session(
    state: AppState,
    id: Uuid,
    request: StartSessionRequest,
    hotwords: Vec<String>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut bridge = match AppleSpeechBridge::spawn(
        &state.inner.config.speech_bridge_path,
        &request.source_language,
        request.audio_source,
        &hotwords,
    )
    .await
    {
        Ok(bridge) => bridge,
        Err(error) => {
            state.emit(ServerEvent::Error {
                code: "asr_start_failed".to_owned(),
                message: error.to_string(),
            });
            state.emit(ServerEvent::SessionStatus {
                running: false,
                message: "语音识别启动失败".to_owned(),
            });
            return;
        }
    };
    let mut manager =
        SegmentManager::new(Duration::from_millis(state.inner.config.segment_idle_ms));
    let mut ticker = interval(Duration::from_millis(100));
    let mut jobs = JoinSet::new();
    let mut last_preview = Instant::now() - Duration::from_secs(1);
    state.emit(ServerEvent::SessionStatus {
        running: true,
        message: format!(
            "正在等待 Apple Speech（{}）",
            request.audio_source.description()
        ),
    });

    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = ticker.tick() => {
                if manager.should_finalize(Instant::now()) {
                    if let Some(snapshot) = manager.finalize() {
                        emit_source_snapshot(&state, &request, &snapshot);
                        spawn_translation(&mut jobs, state.clone(), request.clone(), snapshot);
                    }
                }
            }
            event = bridge.next_event() => {
                match event {
                    Ok(AsrEvent::Ready { locale }) => {
                        state.emit(ServerEvent::SessionStatus {
                            running: true,
                            message: format!(
                                "Apple Speech 已就绪：{locale} · {}",
                                request.audio_source.description()
                            ),
                        });
                    }
                    Ok(AsrEvent::Partial { text }) => {
                        if let Some(snapshot) = manager.update(&text, Instant::now()) {
                            emit_source_snapshot(&state, &request, &snapshot);
                            if last_preview.elapsed() >= Duration::from_millis(500) {
                                spawn_translation(&mut jobs, state.clone(), request.clone(), snapshot);
                                last_preview = Instant::now();
                            }
                        }
                    }
                    Ok(AsrEvent::Final { text }) => {
                        if let Some(snapshot) = manager.update(&text, Instant::now()) {
                            emit_source_snapshot(&state, &request, &snapshot);
                        }
                        if let Some(snapshot) = manager.finalize() {
                            emit_source_snapshot(&state, &request, &snapshot);
                            spawn_translation(&mut jobs, state.clone(), request.clone(), snapshot);
                        }
                    }
                    Ok(AsrEvent::Error { message }) => {
                        state.emit(ServerEvent::Error {
                            code: "asr_failed".to_owned(),
                            message,
                        });
                        break;
                    }
                    Err(error) => {
                        state.emit(ServerEvent::Error {
                            code: "asr_bridge_failed".to_owned(),
                            message: error.to_string(),
                        });
                        break;
                    }
                }
            }
        }
    }

    if let Some(snapshot) = manager.finalize() {
        emit_source_snapshot(&state, &request, &snapshot);
        spawn_translation(&mut jobs, state.clone(), request, snapshot);
    }
    if let Err(error) = bridge.stop().await {
        warn!(session_id = %id, reason = %error, "speech bridge stop failed");
    }
    while jobs.join_next().await.is_some() {}
    state.emit(ServerEvent::SessionStatus {
        running: false,
        message: "会话已结束".to_owned(),
    });
    info!(session_id = %id, "translation session stopped");
}

fn spawn_translation(
    jobs: &mut JoinSet<()>,
    state: AppState,
    request: StartSessionRequest,
    snapshot: SegmentSnapshot,
) {
    jobs.spawn(async move {
        process_snapshot(state, request, snapshot).await;
    });
}

fn emit_source_snapshot(
    state: &AppState,
    request: &StartSessionRequest,
    snapshot: &SegmentSnapshot,
) {
    state.emit(ServerEvent::Caption {
        segment_id: snapshot.segment_id,
        revision: event_revision(snapshot.revision, 0),
        state: CaptionState::Partial,
        source_text: snapshot.source_text.clone(),
        translation_text: String::new(),
        source_language: request.source_language.clone(),
        target_language: request.target_language.clone(),
        llm_applied: false,
        latency_ms: 0,
    });
}

async fn process_snapshot(
    state: AppState,
    request: StartSessionRequest,
    snapshot: SegmentSnapshot,
) {
    let started = Instant::now();
    let translation_permit = if snapshot.state == CaptionState::Final {
        match state.inner.translation_gate.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return,
        }
    } else {
        match state.inner.translation_gate.clone().try_acquire_owned() {
            Ok(permit) => permit,
            // Keep the model queue bounded: stale partials are disposable,
            // while final snapshots use the awaited branch above.
            Err(_) => return,
        }
    };
    let normalized = match state
        .inner
        .dictionary
        .normalize_source(&snapshot.source_text, &request.source_language)
    {
        Ok(text) => text,
        Err(error) => {
            state.emit(dictionary_error_event(error));
            snapshot.source_text.clone()
        }
    };
    let glossary = match state.inner.dictionary.relevant(
        &normalized,
        &request.source_language,
        &request.target_language,
    ) {
        Ok(glossary) => glossary,
        Err(error) => {
            state.emit(dictionary_error_event(error));
            Vec::new()
        }
    };
    let plan = {
        let mut streaming = state.inner.streaming.lock().await;
        streaming.begin(
            snapshot.segment_id,
            snapshot.revision,
            &normalized,
            &request.source_language,
            snapshot.state == CaptionState::Final,
        )
    };
    let Some(plan) = plan else {
        return;
    };

    let mut draft_latency_ms: u128 = 0;
    let mut translation_text = if plan.full_reconcile {
        let output = translate_or_fallback(&state, &request, &glossary, &plan.mutable_window).await;
        draft_latency_ms = draft_latency_ms.saturating_add(output.latency_ms);
        let mut streaming = state.inner.streaming.lock().await;
        let Some(assembled) = streaming.finish_final(&plan, &output.text) else {
            return;
        };
        assembled
    } else {
        let mut committed_translations = Vec::with_capacity(plan.chunks_to_commit.len());
        for chunk in &plan.chunks_to_commit {
            let output =
                translate_or_fallback(&state, &request, &glossary, &chunk.model_text).await;
            draft_latency_ms = draft_latency_ms.saturating_add(output.latency_ms);
            committed_translations.push(output.text);
        }
        let mutable_translation = if plan.mutable_window.is_empty() {
            String::new()
        } else {
            let output =
                translate_or_fallback(&state, &request, &glossary, &plan.mutable_window).await;
            draft_latency_ms = draft_latency_ms.saturating_add(output.latency_ms);
            output.text
        };
        let mut streaming = state.inner.streaming.lock().await;
        let Some(assembled) = streaming.finish_window(
            &plan,
            &committed_translations,
            &mutable_translation,
            &request.target_language,
        ) else {
            return;
        };
        assembled
    };
    let mut llm_applied = false;
    drop(translation_permit);
    state.emit(ServerEvent::Caption {
        segment_id: snapshot.segment_id,
        revision: event_revision(snapshot.revision, 1),
        state: CaptionState::Draft,
        source_text: normalized.clone(),
        translation_text: translation_text.clone(),
        source_language: request.source_language.clone(),
        target_language: request.target_language.clone(),
        llm_applied: false,
        latency_ms: started.elapsed().as_millis().max(draft_latency_ms),
    });
    if snapshot.state == CaptionState::Final {
        let context = state
            .inner
            .context
            .lock()
            .await
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        match state
            .inner
            .llm
            .refine(
                &normalized,
                &translation_text,
                &request.source_language,
                &request.target_language,
                &context,
                &glossary,
            )
            .await
        {
            Ok(output) => {
                translation_text = output.text;
                llm_applied = output.applied;
            }
            Err(error) => state.emit(ServerEvent::Error {
                code: "llm_refinement_failed".to_owned(),
                message: error.to_string(),
            }),
        }
        let mut context = state.inner.context.lock().await;
        context.push_back((normalized.clone(), translation_text.clone()));
        while context.len() > 2 {
            context.pop_front();
        }
    } else {
        return;
    }
    state.emit(ServerEvent::Caption {
        segment_id: snapshot.segment_id,
        revision: event_revision(snapshot.revision, 2),
        state: CaptionState::Final,
        source_text: normalized,
        translation_text,
        source_language: request.source_language,
        target_language: request.target_language,
        llm_applied,
        latency_ms: started.elapsed().as_millis().max(draft_latency_ms),
    });
}

async fn translate_or_fallback(
    state: &AppState,
    request: &StartSessionRequest,
    glossary: &[GlossaryEntry],
    text: &str,
) -> TranslationOutput {
    match state
        .inner
        .translator
        .translate(
            text,
            &request.source_language,
            &request.target_language,
            glossary,
        )
        .await
    {
        Ok(output) => output,
        Err(error) => {
            state.emit(ServerEvent::Error {
                code: "translation_failed".to_owned(),
                message: error.to_string(),
            });
            TranslationOutput {
                text: text.to_owned(),
                latency_ms: 0,
            }
        }
    }
}

fn event_revision(asr_revision: u64, phase: u64) -> u64 {
    asr_revision.saturating_mul(3).saturating_add(phase)
}

async fn list_dictionary(
    State(state): State<AppState>,
) -> Result<Json<Vec<GlossaryEntry>>, ApiError> {
    Ok(Json(state.inner.dictionary.list()?))
}

async fn upsert_dictionary(
    State(state): State<AppState>,
    Json(entry): Json<GlossaryEntry>,
) -> Result<Json<GlossaryEntry>, ApiError> {
    let saved = state.inner.dictionary.upsert(&entry)?;
    state.emit(ServerEvent::DictionaryChanged);
    Ok(Json(saved))
}

async fn delete_dictionary(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    if state.inner.dictionary.delete(id)? {
        state.emit(ServerEvent::DictionaryChanged);
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("dictionary entry was not found"))
    }
}

async fn record_correction(
    State(state): State<AppState>,
    Json(correction): Json<CorrectionRequest>,
) -> Result<Json<Option<GlossaryEntry>>, ApiError> {
    let learned = state.inner.dictionary.learn_correction(&correction)?;
    if learned.is_some() {
        state.emit(ServerEvent::DictionaryChanged);
    }
    Ok(Json(learned))
}

fn validate_direction(request: &StartSessionRequest) -> Result<(), ApiError> {
    let source = base_language(&request.source_language);
    let target = base_language(&request.target_language);
    if !matches!((source, target), ("zh", "en") | ("en", "zh")) {
        return Err(ApiError::bad_request(
            "MVP only supports Chinese-English translation",
        ));
    }
    Ok(())
}

fn base_language(language: &str) -> &str {
    language.split(['-', '_']).next().unwrap_or(language)
}

fn dictionary_error_event(error: DictionaryError) -> ServerEvent {
    ServerEvent::Error {
        code: "dictionary_failed".to_owned(),
        message: error.to_string(),
    }
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.to_owned(),
        }
    }

    fn not_found(message: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.to_owned(),
        }
    }

    fn service_unavailable(message: &str) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.to_owned(),
        }
    }

    fn internal(message: String) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message,
        }
    }
}

impl From<DictionaryError> for ApiError {
    fn from(error: DictionaryError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tempfile::TempDir;
    use tokio::time::timeout;
    use tower::ServiceExt;

    use super::{event_revision, process_snapshot, router, AppState};
    use crate::{
        config::{AppConfig, LlmConfig},
        domain::{CaptionState, GlossaryEntry, SegmentSnapshot, ServerEvent, StartSessionRequest},
    };

    #[test]
    fn later_asr_revisions_outrank_every_phase_of_older_text() {
        assert!(event_revision(4, 0) > event_revision(3, 2));
        assert!(event_revision(4, 2) > event_revision(4, 1));
    }

    #[tokio::test]
    async fn busy_model_drops_partial_but_preserves_final() {
        let (state, _directory) = test_state();
        let mut events = state.inner.events.subscribe();
        let busy_permit = state
            .inner
            .translation_gate
            .clone()
            .acquire_owned()
            .await
            .expect("translation gate");
        let request = StartSessionRequest {
            source_language: "zh-CN".to_owned(),
            target_language: "en-US".to_owned(),
            audio_source: crate::domain::AudioSource::Microphone,
        };

        process_snapshot(
            state.clone(),
            request.clone(),
            SegmentSnapshot {
                segment_id: 1,
                revision: 1,
                state: CaptionState::Partial,
                source_text: "旧的部分文本".to_owned(),
            },
        )
        .await;
        assert!(timeout(Duration::from_millis(25), events.recv())
            .await
            .is_err());

        let final_task = tokio::spawn(process_snapshot(
            state.clone(),
            request,
            SegmentSnapshot {
                segment_id: 1,
                revision: 2,
                state: CaptionState::Final,
                source_text: "最终文本".to_owned(),
            },
        ));
        tokio::task::yield_now().await;
        assert!(!final_task.is_finished());

        drop(busy_permit);
        final_task.await.expect("final translation task");
        let draft = timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("draft timeout")
            .expect("draft event");
        let final_caption = timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("final timeout")
            .expect("final event");
        assert!(matches!(
            draft,
            ServerEvent::Caption {
                state: CaptionState::Draft,
                ..
            }
        ));
        assert!(matches!(
            final_caption,
            ServerEvent::Caption {
                state: CaptionState::Final,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn health_and_dictionary_work_through_http_router() {
        let (state, _directory) = test_state();
        let app = router(state);
        let health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .expect("health request"),
            )
            .await
            .expect("health response");
        assert_eq!(health.status(), StatusCode::OK);
        let health_body = to_bytes(health.into_body(), 64 * 1024)
            .await
            .expect("health body");
        let health_json: serde_json::Value =
            serde_json::from_slice(&health_body).expect("health JSON");
        assert_eq!(health_json["ok"], true);
        assert_eq!(health_json["fake_translation"], true);
        assert_eq!(health_json["overlay_ready"], false);

        let overlay = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/overlay")
                    .body(Body::empty())
                    .expect("overlay request"),
            )
            .await
            .expect("overlay response");
        assert_eq!(overlay.status(), StatusCode::OK);
        let overlay_body = to_bytes(overlay.into_body(), 64 * 1024)
            .await
            .expect("overlay body");
        assert!(String::from_utf8_lossy(&overlay_body).contains("caption-stack"));

        let missing_overlay_app = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/overlay/open")
                    .body(Body::empty())
                    .expect("open overlay request"),
            )
            .await
            .expect("open overlay response");
        assert_eq!(
            missing_overlay_app.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        let entry = serde_json::json!({
            "id": null,
            "source": "实时翻译",
            "source_language": "zh",
            "target": "real-time translation",
            "target_language": "en",
            "aliases": ["实时反应"],
            "domain": "AI",
            "confidence": 1.0,
            "evidence_count": 1,
            "active": true
        });
        let saved = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/dictionary")
                    .header("content-type", "application/json")
                    .body(Body::from(entry.to_string()))
                    .expect("dictionary request"),
            )
            .await
            .expect("dictionary response");
        assert_eq!(saved.status(), StatusCode::OK);

        let listed = app
            .oneshot(
                Request::builder()
                    .uri("/api/dictionary")
                    .body(Body::empty())
                    .expect("list request"),
            )
            .await
            .expect("list response");
        assert_eq!(listed.status(), StatusCode::OK);
        let listed_body = to_bytes(listed.into_body(), 64 * 1024)
            .await
            .expect("list body");
        let entries: Vec<GlossaryEntry> =
            serde_json::from_slice(&listed_body).expect("dictionary JSON");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].target, "real-time translation");
        assert_eq!(entries[0].aliases, ["实时反应"]);
    }

    fn test_state() -> (AppState, TempDir) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let state = AppState::new(AppConfig {
            listen: "127.0.0.1:0".parse().expect("listen address"),
            database_path: directory.path().join("dictionary.sqlite3"),
            speech_bridge_path: directory.path().join("speech-bridge"),
            overlay_app_path: directory.path().join("overlay.app"),
            python_path: directory.path().join("python"),
            model_worker_path: directory.path().join("worker.py"),
            fake_translation: true,
            segment_idle_ms: 900,
            llm: LlmConfig {
                enabled: false,
                base_url: "http://127.0.0.1:1/v1".to_owned(),
                api_key: String::new(),
                model: String::new(),
                timeout_seconds: 1,
                thinking_disabled: false,
            },
        })
        .expect("test app state");
        (state, directory)
    }
}
