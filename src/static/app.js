const elements = {
  captions: document.querySelector("#captions"),
  empty: document.querySelector("#empty-state"),
  start: document.querySelector("#start"),
  stop: document.querySelector("#stop"),
  overlayOpen: document.querySelector("#overlay-open"),
  swap: document.querySelector("#swap"),
  audioSource: document.querySelector("#audio-source"),
  audioSourceHint: document.querySelector("#audio-source-hint"),
  source: document.querySelector("#source-language"),
  target: document.querySelector("#target-language"),
  status: document.querySelector("#session-status"),
  connectionDot: document.querySelector("#connection-dot"),
  connectionLabel: document.querySelector("#connection-label"),
  dictionaryPanel: document.querySelector("#dictionary-panel"),
  dictionaryToggle: document.querySelector("#dictionary-toggle"),
  dictionaryClose: document.querySelector("#dictionary-close"),
  dictionaryForm: document.querySelector("#dictionary-form"),
  dictionaryList: document.querySelector("#dictionary-list"),
  backdrop: document.querySelector("#backdrop"),
  toast: document.querySelector("#toast"),
};

let socket;
let reconnectTimer;
let sessionActive = false;
let activeSegmentId = null;
let historySegmentId = null;
const segmentVersions = new Map();

function otherLocale(locale) {
  return locale.startsWith("zh") ? "en-US" : "zh-CN";
}

function setRunning(running) {
  elements.start.disabled = running;
  elements.stop.disabled = !running;
  elements.source.disabled = running;
  elements.target.disabled = running;
  elements.audioSource.disabled = running;
  elements.swap.disabled = running;
  document.body.classList.toggle("recording", running);
}

function setStopping() {
  elements.start.disabled = true;
  elements.stop.disabled = true;
  elements.source.disabled = true;
  elements.target.disabled = true;
  elements.audioSource.disabled = true;
  elements.swap.disabled = true;
  document.body.classList.remove("recording");
}

function setStatus(message) {
  elements.status.textContent = message;
}

function showToast(message, tone = "normal") {
  elements.toast.textContent = message;
  elements.toast.dataset.tone = tone;
  elements.toast.classList.add("visible");
  window.clearTimeout(elements.toast.hideTimer);
  elements.toast.hideTimer = window.setTimeout(
    () => elements.toast.classList.remove("visible"),
    3200,
  );
}

function connectSocket() {
  window.clearTimeout(reconnectTimer);
  const protocol = location.protocol === "https:" ? "wss:" : "ws:";
  socket = new WebSocket(`${protocol}//${location.host}/ws`);
  socket.addEventListener("open", () => {
    elements.connectionDot.dataset.state = "ready";
    elements.connectionLabel.textContent = "服务已连接";
  });
  socket.addEventListener("close", () => {
    elements.connectionDot.dataset.state = "offline";
    elements.connectionLabel.textContent = "正在重连";
    reconnectTimer = window.setTimeout(connectSocket, 1200);
  });
  socket.addEventListener("message", ({ data }) => {
    let event;
    try {
      event = JSON.parse(data);
    } catch {
      return;
    }
    handleEvent(event);
  });
}

function handleEvent(event) {
  if (event.type === "caption") renderCaption(event);
  if (event.type === "session_status") {
    if (event.running && !sessionActive) {
      sessionActive = true;
      activeSegmentId = null;
      historySegmentId = null;
      segmentVersions.clear();
      elements.captions.replaceChildren();
      elements.empty.hidden = false;
    } else if (!event.running) {
      sessionActive = false;
    }
    setRunning(event.running);
    setStatus(event.message);
  }
  if (event.type === "error") {
    showToast(event.message, "error");
    setStatus(event.message);
  }
  if (event.type === "dictionary_changed") loadDictionary();
}

function captionTemplate(event, role) {
  const article = document.createElement("article");
  article.className = `caption caption-${role}`;
  article.dataset.segment = event.segment_id;
  article.innerHTML = `
    <div class="caption-meta">
      <div><span class="caption-role"></span><span class="phase"></span></div>
      <span class="latency"></span>
    </div>
    <label class="caption-field source-field">
      <span>原文</span>
      <textarea class="source" rows="1" spellcheck="false"></textarea>
    </label>
    <label class="caption-field translation-field">
      <span>译文</span>
      <textarea class="translation" rows="1" spellcheck="false"></textarea>
    </label>
    <div class="caption-actions">
      <span class="saved-label">修改后可保存到个人词典</span>
      <button type="button" class="save-correction">保存纠正</button>
    </div>`;
  article.querySelector(".save-correction").addEventListener("click", () => {
    saveCorrection(article);
  });
  for (const textarea of article.querySelectorAll("textarea")) {
    textarea.addEventListener("input", () => {
      autoResize(textarea);
      article.classList.add("edited");
    });
  }
  setCaptionRole(article, role);
  return article;
}

function setCaptionRole(article, role) {
  article.classList.toggle("caption-current", role === "current");
  article.classList.toggle("caption-history", role === "history");
  article.querySelector(".caption-role").textContent =
    role === "history" ? "上一段" : "当前段";
  article.setAttribute(
    "aria-label",
    role === "history" ? "上一段历史字幕" : "当前实时字幕",
  );
}

function archiveCurrentCaption() {
  const current = elements.captions.querySelector(".caption-current");
  if (!current || activeSegmentId === null) return;
  elements.captions.querySelector(".caption-history")?.remove();
  historySegmentId = activeSegmentId;
  activeSegmentId = null;
  setCaptionRole(current, "history");
  freezeHistoryCaption(current);
  elements.captions.prepend(current);
}

function freezeHistoryCaption(article) {
  article.dataset.state = "history";
  article.classList.remove("edited", "translation-pending");
  article.querySelector(".phase").textContent = "已完成";
  article.querySelector(".latency").textContent = "";
  for (const textarea of article.querySelectorAll("textarea")) {
    textarea.readOnly = true;
  }
}

function captionForEvent(event) {
  if (event.segment_id === historySegmentId) {
    return elements.captions.querySelector(".caption-history");
  }
  if (event.segment_id === activeSegmentId) {
    return elements.captions.querySelector(".caption-current");
  }
  if (activeSegmentId !== null && event.segment_id > activeSegmentId) {
    archiveCurrentCaption();
  }
  const newestVisibleId = Math.max(activeSegmentId ?? -1, historySegmentId ?? -1);
  if (event.segment_id <= newestVisibleId) return null;

  const article = captionTemplate(event, "current");
  activeSegmentId = event.segment_id;
  elements.captions.append(article);
  return article;
}

function renderCaption(event) {
  // Keep one mutable current segment and one replaceable history segment.
  // An archived segment is immutable, and an older segment can never replace
  // either of the two newer cards.
  const knownRevision = segmentVersions.get(event.segment_id) ?? -1;
  if (knownRevision >= event.revision) return;
  segmentVersions.set(event.segment_id, event.revision);
  elements.empty.hidden = true;

  const article = captionForEvent(event);
  if (!article) return;
  if (event.segment_id === historySegmentId) {
    // A history card is an immutable snapshot. Results that finish after the
    // segment has moved upward must never change its text, size, or styling.
    return;
  }
  if (article.classList.contains("edited")) return;

  article.dataset.state = event.state;
  article.dataset.revision = event.revision;
  article.dataset.sourceLanguage = event.source_language;
  article.dataset.targetLanguage = event.target_language;
  article.dataset.originalSource = event.source_text;

  const source = article.querySelector(".source");
  const translation = article.querySelector(".translation");
  const receivedTranslation = event.translation_text.trim().length > 0;
  const hasVisibleTranslation = translation.value.trim().length > 0;
  const holdsPreviousTranslation =
    event.state === "partial" && !receivedTranslation && hasVisibleTranslation;

  source.value = event.source_text;
  if (receivedTranslation || !hasVisibleTranslation) {
    translation.value = event.translation_text;
  }
  if (receivedTranslation) {
    article.dataset.originalTranslation = event.translation_text;
  }
  article.classList.toggle("translation-pending", holdsPreviousTranslation);
  translation.placeholder = translation.value ? "" : "正在生成快速译文…";
  source.readOnly = event.state !== "final";
  translation.readOnly = event.state !== "final";
  autoResize(source);
  autoResize(translation);

  const phaseNames = {
    partial: holdsPreviousTranslation ? "ASR 更新 · 译文待覆盖" : "ASR 原文",
    draft: "快速译文",
    final: event.llm_applied ? "LLM 终稿" : "最终译文",
  };
  article.querySelector(".phase").textContent = phaseNames[event.state];
  article.querySelector(".latency").textContent = holdsPreviousTranslation
    ? "等待新译文"
    : event.latency_ms
      ? `${event.latency_ms} ms`
      : "实时";

  if (event.state === "final" && event.segment_id === activeSegmentId) {
    archiveCurrentCaption();
  }
}

function autoResize(textarea) {
  textarea.style.height = "auto";
  textarea.style.height = `${textarea.scrollHeight}px`;
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

elements.start.addEventListener("click", async () => {
  setRunning(true);
  try {
    setStatus("正在启动 Apple Speech…");
    await jsonRequest("/api/session/start", {
      method: "POST",
      body: JSON.stringify({
        source_language: elements.source.value,
        target_language: elements.target.value,
        audio_source: elements.audioSource.value,
      }),
    });
  } catch (error) {
    setRunning(false);
    showToast(error.message, "error");
  }
});

elements.stop.addEventListener("click", async () => {
  setStopping();
  setStatus("正在结束当前句…");
  try {
    await jsonRequest("/api/session/stop", { method: "POST" });
  } catch (error) {
    setRunning(true);
    showToast(error.message, "error");
  }
});

elements.overlayOpen.addEventListener("click", async () => {
  elements.overlayOpen.disabled = true;
  try {
    await jsonRequest("/api/overlay/open", { method: "POST" });
    showToast("悬浮字幕已打开；可拖动到任意位置");
  } catch (error) {
    showToast(error.message, "error");
  } finally {
    elements.overlayOpen.disabled = false;
  }
});

elements.swap.addEventListener("click", () => {
  const source = elements.source.value;
  elements.source.value = elements.target.value;
  elements.target.value = source;
});

elements.source.addEventListener("change", () => {
  if (elements.source.value === elements.target.value) {
    elements.target.value = otherLocale(elements.source.value);
  }
});

elements.target.addEventListener("change", () => {
  if (elements.source.value === elements.target.value) {
    elements.source.value = otherLocale(elements.target.value);
  }
});

elements.audioSource.addEventListener("change", () => {
  const systemAudio = elements.audioSource.value === "system_audio";
  elements.audioSourceHint.textContent = systemAudio
    ? "系统音频模式识别视频通话、播放器等应用的声音；首次使用需要授予屏幕与系统音频录制权限。"
    : "麦克风模式识别你说的话；首次使用需要授予麦克风和语音识别权限。";
});

function setDictionaryOpen(open) {
  elements.dictionaryPanel.classList.toggle("open", open);
  elements.backdrop.classList.toggle("visible", open);
  elements.dictionaryPanel.setAttribute("aria-hidden", String(!open));
  if (open) loadDictionary();
}

elements.dictionaryToggle.addEventListener("click", () => setDictionaryOpen(true));
elements.dictionaryClose.addEventListener("click", () => setDictionaryOpen(false));
elements.backdrop.addEventListener("click", () => setDictionaryOpen(false));

elements.dictionaryForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = new FormData(elements.dictionaryForm);
  const [sourceLanguage, targetLanguage] = form.get("direction").split("-");
  try {
    await jsonRequest("/api/dictionary", {
      method: "POST",
      body: JSON.stringify({
        id: null,
        source: form.get("source").trim(),
        target: form.get("target").trim(),
        source_language: sourceLanguage,
        target_language: targetLanguage,
        aliases: form
          .get("aliases")
          .split(/[,，]/)
          .map((value) => value.trim())
          .filter(Boolean),
        domain: form.get("domain").trim() || "general",
        confidence: 1,
        evidence_count: 1,
        active: true,
      }),
    });
    elements.dictionaryForm.reset();
    elements.dictionaryForm.elements.domain.value = "general";
    showToast("术语已保存；下次启动听写时会加入 Apple Speech 热词");
    loadDictionary();
  } catch (error) {
    showToast(error.message, "error");
  }
});

async function loadDictionary() {
  try {
    const entries = await jsonRequest("/api/dictionary");
    elements.dictionaryList.replaceChildren();
    if (!entries.length) {
      const empty = document.createElement("p");
      empty.className = "dictionary-empty";
      empty.textContent = "还没有术语。保存字幕纠正或手动添加一条。";
      elements.dictionaryList.append(empty);
      return;
    }
    for (const entry of entries) {
      const item = document.createElement("div");
      item.className = "dictionary-item";
      item.innerHTML = `
        <div>
          <strong></strong><span>→</span><strong></strong>
          <small></small>
        </div>
        <button type="button" aria-label="删除术语">删除</button>`;
      const words = item.querySelectorAll("strong");
      words[0].textContent = entry.source;
      words[1].textContent = entry.target;
      item.querySelector("small").textContent =
        `${entry.source_language}→${entry.target_language} · ${entry.domain}` +
        (entry.aliases.length ? ` · 别名：${entry.aliases.join("、")}` : "");
      item.querySelector("button").addEventListener("click", async () => {
        try {
          await jsonRequest(`/api/dictionary/${entry.id}`, { method: "DELETE" });
          loadDictionary();
        } catch (error) {
          showToast(error.message, "error");
        }
      });
      elements.dictionaryList.append(item);
    }
  } catch (error) {
    showToast(error.message, "error");
  }
}

async function saveCorrection(article) {
  try {
    const learned = await jsonRequest("/api/corrections", {
      method: "POST",
      body: JSON.stringify({
        original_source: article.dataset.originalSource,
        corrected_source: article.querySelector(".source").value,
        original_translation: article.dataset.originalTranslation,
        corrected_translation: article.querySelector(".translation").value,
        source_language: article.dataset.sourceLanguage,
        target_language: article.dataset.targetLanguage,
      }),
    });
    article.classList.remove("edited");
    if (learned) {
      article.dataset.originalSource = article.querySelector(".source").value;
      article.dataset.originalTranslation = article.querySelector(".translation").value;
      showToast(`已学习术语：${learned.source} → ${learned.target}`);
    } else {
      showToast("修改已保留，但差异不像可复用的短术语，未写入词典");
    }
  } catch (error) {
    showToast(error.message, "error");
  }
}

async function loadHealth() {
  try {
    const health = await jsonRequest("/api/health");
    document.querySelector('[data-health="speech"]').dataset.ready =
      health.speech_bridge_ready;
    document.querySelector('[data-health="model"]').dataset.ready =
      health.model_worker_ready || health.fake_translation;
    document.querySelector('[data-health="llm"]').dataset.ready = health.llm_enabled;
    elements.overlayOpen.dataset.ready = health.overlay_ready;
    elements.overlayOpen.title = health.overlay_ready
      ? "打开始终置顶的半透明字幕窗口"
      : "需要先运行 ./scripts/build-macos-overlay.sh";
  } catch (error) {
    showToast(error.message, "error");
  }
}

connectSocket();
loadHealth();
loadDictionary();
