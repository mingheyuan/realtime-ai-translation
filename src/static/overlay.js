const stack = document.querySelector("#caption-stack");
const waiting = document.querySelector("#waiting-card");
const statusLabel = document.querySelector("#overlay-status");
const modeLabel = document.querySelector("#overlay-mode");
const startButton = document.querySelector("#overlay-start");
const stopButton = document.querySelector("#overlay-stop");

let socket;
let reconnectTimer;
let sessionRunning = false;
let activeSegmentId = null;
let historySegmentId = null;
const segmentVersions = new Map();
let preferences = {
  source_language: "zh-CN",
  target_language: "en-US",
  audio_source: "microphone",
};

function setRunning(running) {
  sessionRunning = running;
  startButton.disabled = running;
  stopButton.disabled = !running;
  document.body.classList.toggle("session-running", running);
}

function updateModeLabel() {
  const source = preferences.audio_source === "system_audio" ? "系统音频" : "麦克风";
  const direction = preferences.source_language.startsWith("zh") ? "中→英" : "英→中";
  modeLabel.textContent = `${source} · ${direction}`;
}

async function jsonRequest(url, options = {}) {
  const response = await fetch(url, {
    ...options,
    headers: { "content-type": "application/json", ...(options.headers || {}) },
  });
  const body = response.status === 204 ? null : await response.json();
  if (!response.ok) throw new Error(body?.error || `请求失败（${response.status}）`);
  return body;
}

async function refreshOverlayState() {
  try {
    const state = await jsonRequest("/api/overlay/state");
    preferences = state.preferences;
    updateModeLabel();
    setRunning(state.running);
    if (state.running) statusLabel.textContent = "实时翻译中";
  } catch (error) {
    statusLabel.textContent = error.message;
  }
}

function connect() {
  window.clearTimeout(reconnectTimer);
  const protocol = location.protocol === "https:" ? "wss:" : "ws:";
  socket = new WebSocket(`${protocol}//${location.host}/ws`);
  socket.addEventListener("open", () => {
    document.body.classList.add("connected");
    document.body.classList.remove("disconnected");
    statusLabel.textContent = sessionRunning ? "实时翻译中" : "已连接 · 等待开始";
  });
  socket.addEventListener("close", () => {
    document.body.classList.remove("connected");
    document.body.classList.add("disconnected");
    statusLabel.textContent = "连接中断 · 正在重连";
    reconnectTimer = window.setTimeout(connect, 1200);
  });
  socket.addEventListener("message", ({ data }) => {
    try {
      handleEvent(JSON.parse(data));
    } catch {
      // Ignore malformed or unrelated WebSocket messages.
    }
  });
}

function handleEvent(event) {
  if (event.type === "caption") renderCaption(event);
  if (event.type === "session_status") {
    if (event.running && !sessionRunning) resetCaptions();
    setRunning(event.running);
    statusLabel.textContent = event.message;
  }
  if (event.type === "error") statusLabel.textContent = event.message;
}

function resetCaptions() {
  activeSegmentId = null;
  historySegmentId = null;
  segmentVersions.clear();
  for (const card of stack.querySelectorAll(".caption-card:not(.waiting)")) card.remove();
  waiting.hidden = false;
}

function createCard(segmentId) {
  const card = document.createElement("article");
  card.className = "caption-card current";
  card.dataset.segment = segmentId;
  card.innerHTML = `
    <div class="caption-meta"><span class="phase">当前段 · ASR 原文</span><span class="latency">实时</span></div>
    <p class="source"></p>
    <p class="translation">正在生成快速译文…</p>`;
  return card;
}

function archiveCurrent() {
  const current = stack.querySelector(".caption-card.current");
  if (!current || activeSegmentId === null) return;
  stack.querySelector(".caption-card.history")?.remove();
  historySegmentId = activeSegmentId;
  activeSegmentId = null;
  current.className = "caption-card history";
  current.querySelector(".phase").textContent = "上一段 · 已封存";
  current.querySelector(".latency").textContent = "";
  stack.prepend(current);
}

function cardForEvent(event) {
  if (event.segment_id === historySegmentId) return null;
  if (event.segment_id === activeSegmentId) {
    return stack.querySelector(".caption-card.current");
  }
  if (activeSegmentId !== null && event.segment_id > activeSegmentId) archiveCurrent();
  const newestVisibleId = Math.max(activeSegmentId ?? -1, historySegmentId ?? -1);
  if (event.segment_id <= newestVisibleId) return null;

  const card = createCard(event.segment_id);
  activeSegmentId = event.segment_id;
  stack.append(card);
  return card;
}

function renderCaption(event) {
  if (!sessionRunning) {
    setRunning(true);
    statusLabel.textContent = "实时翻译中";
  }
  const knownRevision = segmentVersions.get(event.segment_id) ?? -1;
  if (knownRevision >= event.revision) return;
  segmentVersions.set(event.segment_id, event.revision);

  const card = cardForEvent(event);
  if (!card) return;
  waiting.hidden = true;

  const source = card.querySelector(".source");
  const translation = card.querySelector(".translation");
  const receivedTranslation = event.translation_text.trim().length > 0;
  const hasVisibleTranslation = translation.dataset.hasTranslation === "true";
  const holdTranslation = event.state === "partial" && !receivedTranslation && hasVisibleTranslation;

  source.textContent = event.source_text;
  if (receivedTranslation) {
    translation.textContent = event.translation_text;
    translation.dataset.hasTranslation = "true";
  } else if (!hasVisibleTranslation) {
    translation.textContent = "正在生成快速译文…";
  }
  card.classList.toggle("translation-pending", holdTranslation);

  const phases = {
    partial: holdTranslation ? "当前段 · ASR 更新" : "当前段 · ASR 原文",
    draft: "当前段 · 快速译文",
    final: event.llm_applied ? "当前段 · LLM 终稿" : "当前段 · 最终译文",
  };
  card.querySelector(".phase").textContent = phases[event.state];
  card.querySelector(".latency").textContent = holdTranslation
    ? "等待新译文"
    : event.latency_ms
      ? `${event.latency_ms} ms`
      : "实时";

  if (event.state === "final" && event.segment_id === activeSegmentId) archiveCurrent();
}

startButton.addEventListener("click", async () => {
  startButton.disabled = true;
  statusLabel.textContent = "正在启动 Apple Speech…";
  try {
    const state = await jsonRequest("/api/overlay/state");
    preferences = state.preferences;
    updateModeLabel();
    await jsonRequest("/api/session/start", {
      method: "POST",
      body: JSON.stringify(preferences),
    });
    setRunning(true);
  } catch (error) {
    setRunning(false);
    statusLabel.textContent = error.message;
  }
});

stopButton.addEventListener("click", async () => {
  stopButton.disabled = true;
  statusLabel.textContent = "正在结束当前句…";
  try {
    await jsonRequest("/api/session/stop", { method: "POST" });
  } catch (error) {
    setRunning(true);
    statusLabel.textContent = error.message;
  }
});

updateModeLabel();
refreshOverlayState();
connect();
