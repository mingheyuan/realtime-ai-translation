# Realtime AI Translation

[![CI](https://github.com/mingheyuan/realtime-ai-translation/actions/workflows/ci.yml/badge.svg)](https://github.com/mingheyuan/realtime-ai-translation/actions/workflows/ci.yml)

macOS 上的中英双向实时语音翻译 MVP。它沿用 Saymore 值得借鉴的思路：Apple Speech 流式听写、个人词典、可回写的错误纠正；在 ASR 与可选 LLM 之间增加本地 MarianMT，让字幕不必等待云端模型。

> 本项目是独立实现，没有复制 Saymore 源码。Saymore 使用 PolyForm Shield 许可证，本项目使用 MIT 许可证。

## 实际处理流程

```text
麦克风（自己的声音）或 ScreenCaptureKit 系统音频（通话对方）
  ↓
Apple Speech（partial / final + contextualStrings 热词）
  ↓
个人词典标准化（已知 ASR 错词 → 标准词）
  ↓
Segment Manager（segment_id + revision）
  ├─ 立即推送 ASR 原文
  ├─ Translation State Machine 提交稳定前缀，保留可回滚尾部
  ├─ 最多每 500ms 只翻译当前滑动窗口，拼接已缓存译文
  └─ 句号 / ASR final / 900ms 停顿后整句校准，再调用可选 LLM
```

同一句话不会不断新增字幕，而是使用相同的 `segment_id` 和更高的 `revision` 替换旧内容。即使旧翻译请求较晚返回，浏览器也会丢弃它，因此不会把已经修正的文本回滚成旧错误。

字幕区固定保留“当前段”和颜色较浅的“上一段”两个位置。后端根据 Apple Speech `final`、中英文句末标点和停顿智能断句；一段结束后进入上一段位置并立即封存，下一段结束时原位替换旧历史，不会持续堆积对话框。封存后到达的快速译文或 LLM 终稿会被丢弃，历史内容、尺寸和样式都不再刷新。

当新一版 ASR 原文已经到达、对应的新译文仍在生成时，界面会保留上一版译文并标记“等待新译文”，直到新草稿或终稿到达后再原位覆盖，避免字幕在两次结果之间闪白。

实时页面始终复用同一个字幕框。后端仍按句子分段以维护翻译上下文，但新句不会创建新的字幕卡片；它会在原框中置换内容。新句出现后，迟到的旧句翻译会被直接丢弃，不能把字幕框回滚到上一句。

## 流式翻译状态机

MarianMT 本身不是流式模型，不能安全地只翻译 ASR 新增的几个字；中英词序变化可能同时修改前面的译文。因此应用层维护 `Collecting → TranslatingWindow → Finalizing → Finalized` 状态机：

1. 比较连续两次 ASR hypothesis，找出未变化的稳定前缀。
2. 中文保留最后 12 个字、英文保留最后约 32 个字符作为可回滚区。
3. 稳定前缀超过窗口后分块翻译并缓存；之后只把尾部窗口送给 MarianMT。
4. 如果 Apple Speech 修改已经提交的前缀，立即清空该段缓存并从修正文本重建。
5. 断句时只进行一次完整句翻译，替换分块草稿，修复跨窗口语序。

默认可变窗口上限为中文 36 个字、英文 96 个字符。窗口结果仍使用 `segment_id + revision` 校验，旧 generation 即使晚返回也不会覆盖新字幕。

这不是三套 ASR。Apple Speech 是唯一的语音识别器；MarianMT 和 LLM 接收的都是文字：

- Apple Speech：尽快把声音变成可修订的源语言文字。
- MarianMT：本地专用翻译模型，生成低延迟草稿。
- LLM：只在断句后结合最近两句和术语表做保守润色，可以完全关闭。

## 当前功能

- Apple Speech 中英文流式 `partial` / `final` 识别。
- 音频来源可选择麦克风或系统音频；系统音频可识别视频通话、播放器等应用播放的声音。
- SQLite 双语个人词典。
- 词典术语和别名作为 Apple `contextualStrings` 热词。
- ASR 后确定性别名纠正和 MarianMT glossary 约束。
- 本地 OPUS-MT 中文→英文、英文→中文模型，进程常驻并按方向懒加载。
- 可选 OpenAI-compatible LLM 终稿。
- WebSocket 可替换字幕：ASR 原文 → 快速译文 → LLM 终稿。
- 模型忙时丢弃过期 partial，final 可靠排队，避免冷启动积压拖慢终稿。
- 稳定前缀缓存和可回滚滑动窗口，避免连续讲话时从句首反复翻译。
- 终稿可编辑；短差异可保存为双语词典项。
- 浏览器界面支持语言切换、启动停止、字幕历史和词典管理。

## 环境要求

- macOS 13 或更高版本。
- Xcode Command Line Tools（需要 `swiftc` 和 `codesign`）。
- Rust stable。
- Python 3.9 或更高版本。
- 首次使用麦克风时授予“麦克风”和“语音识别”权限。
- 首次使用系统音频时授予“屏幕与系统音频录制”权限。

如果之前拒绝过权限，请在“系统设置 → 隐私与安全性 → 麦克风 / 语音识别 / 屏幕与系统音频录制”中启用启动本项目所用的终端或应用。系统音频权限变更后通常需要重启该终端或应用，再重新开始会话。

## 启动

克隆后在项目目录执行：

```bash
./scripts/build-macos-speech.sh
./scripts/setup-models.sh
cargo run
```

打开 <http://127.0.0.1:8765>，选择音频来源和翻译方向，再点击“开始实时翻译”。

### 识别视频通话声音

选择“系统音频（通话对方）”后，macOS ScreenCaptureKit 会采集当前系统播放的声音并送入 Apple Speech。它适用于 Zoom、Teams、腾讯会议、FaceTime 和浏览器通话等场景，不要求把声音外放给麦克风。

当前系统音频模式采集当前显示器范围内所有应用的播放声音，并排除本程序自身音频。为避免字幕混入通知声或其他播放器，通话期间请关闭无关音频。当前一次会话只选择一个来源；如需同时区分自己与对方，需要后续增加双 ASR 通道和说话方标记，不能简单把两路声音混给同一个识别任务。

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

项目启动时会自动读取根目录的 `.env.local`，且真实配置已被 Git 忽略。可以复制 `.env.example` 后填写本机 key。使用 DeepSeek 快速非思考模式时推荐：

```dotenv
RT_TRANSLATION_LLM_ENABLED=1
RT_TRANSLATION_LLM_BASE_URL=https://api.deepseek.com
RT_TRANSLATION_LLM_API_KEY=your-key
RT_TRANSLATION_LLM_MODEL=deepseek-v4-flash
RT_TRANSLATION_LLM_THINKING_DISABLED=1
RT_TRANSLATION_LLM_TIMEOUT=12
```

`RT_TRANSLATION_LLM_THINKING_DISABLED=1` 会在 Chat Completions 请求中发送 `{"thinking":{"type":"disabled"}}`，避免实时字幕等待默认思考过程。

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

构建脚本会把 Swift Bridge 打包为 `target/RealtimeTranslationSpeechBridge.app`。不要改为直接运行裸 Mach-O；macOS TCC 需要从 app bundle 的 `Info.plist` 读取麦克风、语音识别和系统音频权限用途说明。

## 当前限制

- 仅支持中文和英文。
- Web UI 是本机服务，不是已签名的 `.app` 安装包。
- 系统音频当前按显示器采集，不提供单个通话应用/窗口选择；一次会话也不能同时分离麦克风与系统音频。
- Apple Speech 的可用性和离线能力取决于系统、语言包与 macOS。
- MarianMT 首次加载和第一次推理明显慢于后续请求；CPU 内存占用取决于 PyTorch 和已加载的模型方向。
- 已经播放出去的 TTS 无法像字幕一样撤回，因此 MVP 暂不自动朗读。
- 自动词典学习只接受用户明确保存的短文本差异，避免静默污染个人词典。

## License

MIT
