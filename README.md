# Real-time AI Translation

实时语音翻译项目：结合 Saymore 的个人词典/纠错学习能力，以及 LiveTranslate 的流式音频、ASR 和字幕展示能力。

## 1. 项目目标

在用户持续说话时，尽快显示可读的译文；当 ASR 文本变得稳定或句子结束后，再用更强的模型修正当前句子，并把最终结果锁定。

目标不是零延迟，而是让用户始终看到反馈，同时控制字幕抖动、错误传播和延迟。

## 2. 推荐架构

```text
麦克风/系统音频
        ↓
VAD（检测说话和停顿）
        ↓
流式 ASR（语音 → 原文）
        ↑
Saymore 个人词典：热词、别名、专有名词
        ↓
Segment Manager（按句子/停顿管理片段）
        ├── 快速翻译模型：生成低延迟草稿
        │       ↓
        │   实时字幕 Overlay
        │
        └── LLM：结合上下文和术语表做最终润色
                ↓
            替换当前片段并锁定
```

## 3. 三类模型的职责

### ASR

负责将语音识别为源语言文字，例如 Whisper/faster-whisper、SenseVoice、Paraformer 或 Apple Speech。

### 快速翻译模型

负责低延迟的文字翻译，优先选择本地的专用模型：

- `Helsinki-NLP/opus-mt-zh-en`
- `Helsinki-NLP/opus-mt-en-zh`
- MarianMT、NLLB 或 Argos Translate

它用于当前未完成句子的草稿翻译，不负责复杂润色。

### LLM

负责在有更多上下文后进行最终翻译和润色：

- 处理口语和上下文
- 遵守个人术语表
- 修正快速模型的表达
- 输出最终字幕

LLM 可使用云端 API，也可以使用 Ollama、LM Studio 或 vLLM 等本地 OpenAI-compatible 服务。

## 4. 个人词典设计

Saymore 的词典需要从“标准拼写词典”扩展为“多语言术语词典”：

```json
{
  "source": "实时翻译",
  "aliases": ["实时翻译应用", "real time translator"],
  "target": {
    "en": "real-time translation",
    "ja": "リアルタイム翻訳"
  },
  "domain": "AI",
  "confidence": 0.95
}
```

词典在三个位置使用：

1. ASR 前：作为热词，提高人名、产品名和技术词的识别率。
2. ASR 后：做确定性的标准化，修正已知错误变体。
3. LLM 翻译时：作为 glossary，控制固定译法。

用户修改字幕后，可以比较原始文本和修正文本，由 LLM 判断是否为可复用术语；保留 Saymore 的多次证据门槛，避免一次误改就污染词典。

## 5. 实时字幕状态

每个语音片段维护独立状态：

```text
segment_id
source_partial
source_final
translation_draft
translation_final
revision
state: partial | stable | final
```

- `partial`：ASR 仍在变化，允许替换。
- `stable`：已有稳定前缀，尽量减少修改。
- `final`：遇到句号、停顿或 ASR final 后锁定。

每个翻译请求都必须携带 `segment_id + revision`。只接受同一片段中版本更高的响应，丢弃乱序返回的旧结果。

## 6. 延迟策略

推荐采用两条处理路径：

```text
ASR partial
    ↓
快速翻译模型 → 立即显示草稿

ASR stable/final
    ↓
LLM → 替换草稿为最终译文
```

不要每秒无限重复发送完整历史文本。优先发送：

```text
当前片段 + 最近一两句上下文 + 相关术语
```

如果使用累积请求，例如 `0–1s`、`0–2s`、`0–3s`，必须使用版本号，并且只替换当前片段。

## 7. MVP 范围

第一版建议：

1. 支持中文 ↔ 英文。
2. 接入流式 ASR 和 VAD。
3. 使用 Saymore 词典做 ASR 热词和翻译 glossary。
4. 使用 Marian/OPUS-MT 生成低延迟草稿。
5. 句子稳定后调用 LLM 做最终翻译。
6. 使用浏览器/桌面 Overlay 展示字幕。
7. 暂不自动播放 TTS，避免已经说出口的错误无法撤回。
8. 会话结束后提供字幕纠错和词典学习入口。

## 8. 主要风险

- ASR partial 不稳定，可能导致译文频繁变化。
- 远程 LLM 有网络和首 token 延迟。
- LLM 返回可能乱序，必须做版本控制。
- 中英混说、口音、噪声和专有名词会导致错误传播。
- 多目标语言会增加模型内存、请求数和翻译成本。
- 已经播放的 TTS 无法像字幕一样回滚。

## 9. 参考项目

- Saymore：个人词典、用户纠错观察、纠错学习。
- LiveTranslate：流式音频、VAD、ASR、LLM 翻译和实时 Overlay。
- SubsVibe：commit-on-silence、上下文 LLM 修正和中途预览。
- livetl：Whisper 分段、低延迟取舍和本地翻译模型。

