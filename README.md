# Realtime AI Translation

macOS 上的中英双向实时语音翻译 MVP。它沿用 Saymore 值得借鉴的思路：Apple Speech 流式听写、个人词典、可回写的错误纠正；在 ASR 与可选 LLM 之间增加本地 MarianMT，让字幕不必等待云端模型。

> 本项目是独立实现，没有复制 Saymore 源码。Saymore 使用 PolyForm Shield 许可证，本项目使用 MIT 许可证。

## 实际处理流程

```text
麦克风
  ↓
Apple Speech（partial / final + contextualStrings 热词）
  ↓
个人词典标准化（已知 ASR 错词 → 标准词）
  ↓
Segment Manager（segment_id + revision）
  ├─ 立即推送 ASR 原文
  ├─ 最多每 500ms 调用本地 MarianMT，覆盖为快速译文
  └─ 句号 / ASR final / 900ms 停顿后调用可选 LLM，覆盖为终稿
```

同一句话不会不断新增字幕，而是使用相同的 `segment_id` 和更高的 `revision` 替换旧内容。即使旧翻译请求较晚返回，浏览器也会丢弃它，因此不会把已经修正的文本回滚成旧错误。

这不是三套 ASR。Apple Speech 是唯一的语音识别器；MarianMT 和 LLM 接收的都是文字：

- Apple Speech：尽快把声音变成可修订的源语言文字。
- MarianMT：本地专用翻译模型，生成低延迟草稿。
- LLM：只在断句后结合最近两句和术语表做保守润色，可以完全关闭。

## 当前功能

- Apple Speech 中英文流式 `partial` / `final` 识别。
- SQLite 双语个人词典。
- 词典术语和别名作为 Apple `contextualStrings` 热词。
- ASR 后确定性别名纠正和 MarianMT glossary 约束。
- 本地 OPUS-MT 中文→英文、英文→中文模型，进程常驻并按方向懒加载。
- 可选 OpenAI-compatible LLM 终稿。
- WebSocket 可替换字幕：ASR 原文 → 快速译文 → LLM 终稿。
- 终稿可编辑；短差异可保存为双语词典项。
- 浏览器界面支持语言切换、启动停止、字幕历史和词典管理。

## 环境要求

- macOS 13 或更高版本。
- Xcode Command Line Tools（需要 `swiftc` 和 `codesign`）。
- Rust stable。
- Python 3.9 或更高版本。
- 首次启动时授予“麦克风”和“语音识别”权限。

## 启动

克隆后在项目目录执行：

```bash
./scripts/build-macos-speech.sh
./scripts/setup-models.sh
cargo run
```

打开 <http://127.0.0.1:8765>，选择翻译方向并点击“开始实时翻译”。

MarianMT 权重在第一次使用某个翻译方向时从 Hugging Face 下载。只想先查看界面和测试词典时，可以跳过 Python 模型安装：

```bash
RT_TRANSLATION_FAKE_TRANSLATION=1 cargo run
```

## 可选 LLM

不配置任何 key 也可以启动并完成实时翻译；此时 MarianMT 的结果直接成为终稿。要启用 OpenAI-compatible 润色：

```bash
RT_TRANSLATION_LLM_ENABLED=1 \
RT_TRANSLATION_LLM_BASE_URL=https://api.openai.com/v1 \
RT_TRANSLATION_LLM_API_KEY=your-key \
RT_TRANSLATION_LLM_MODEL=your-model \
cargo run
```

`RT_TRANSLATION_LLM_BASE_URL` 也可以指向本地的 Ollama、LM Studio 或其他兼容服务。程序调用 `/chat/completions`，要求模型只返回翻译结果。

## 个人词典如何生效

每条词典项包含源词、固定译法、ASR 别名、语言方向和领域：

```json
{
  "source": "实时翻译",
  "source_language": "zh",
  "target": "real-time translation",
  "target_language": "en",
  "aliases": ["实时反应"],
  "domain": "AI"
}
```

它在三处使用：

1. 会话启动时，源词和别名注入 Apple Speech 热词。
2. Apple Speech 输出后，别名被标准化为源词。
3. MarianMT 和 LLM 翻译时，相关词条作为固定译法。

热词在会话启动时载入，所以新增词条后需要停止并重新开始听写，才能影响 Apple Speech；ASR 后标准化和翻译约束会立即生效。

## 开发与验证

```bash
cargo fmt -- --check
cargo test
cargo clippy --all-targets -- -D warnings
python3 -m py_compile model-worker/worker.py
```

核心 Rust 进程负责会话、分段、版本控制、SQLite、WebSocket 和 LLM。Swift 文件只承担 Apple 原生音频与 Speech framework 桥接。Python 只作为 MarianMT 常驻模型进程，不负责 ASR，也不负责业务状态。

## 当前限制

- 仅支持中文和英文。
- Web UI 是本机服务，不是已签名的 `.app` 安装包。
- Apple Speech 的可用性和离线能力取决于系统、语言包与 macOS。
- MarianMT 首次加载和第一次推理明显慢于后续请求；CPU 内存占用取决于 PyTorch 和已加载的模型方向。
- 已经播放出去的 TTS 无法像字幕一样撤回，因此 MVP 暂不自动朗读。
- 自动词典学习只接受用户明确保存的短文本差异，避免静默污染个人词典。

## License

MIT
