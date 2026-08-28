# Realtime AI Translation

[![CI](https://github.com/mingheyuan/realtime-ai-translation/actions/workflows/ci.yml/badge.svg)](https://github.com/mingheyuan/realtime-ai-translation/actions/workflows/ci.yml)

macOS 上的中英双向实时语音翻译 MVP。它沿用 Saymore 值得借鉴的思路：流式听写、个人词典、可回写的错误纠正；在可替换 ASR 与可选 LLM 之间增加本地 MarianMT，让字幕不必等待云端模型。默认使用 Apple Speech，也支持本地 Sherpa-ONNX 中英混合流式识别。

> 本项目是独立实现，没有复制 Saymore 源码。Saymore 使用 PolyForm Shield 许可证，本项目使用 MIT 许可证。

## 实际处理流程

```text
麦克风（自己的声音）或 ScreenCaptureKit 系统音频（通话对方）
  ↓
按会话加载的 ASR provider（默认 Apple Speech，partial / final + 热词）
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

字幕区固定保留“当前段”和颜色较浅的“上一段”两个位置。后端根据 Apple Speech `final`、中英文句末标点和停顿智能断句：默认停顿 1.5 秒，标点需稳定 300ms，英文至少 6 个词或中文至少 10 个字，连续语音最长 8 秒切段。一段结束后进入上一段位置并立即封存，下一段结束时原位替换旧历史，不会持续堆积对话框。封存后到达的快速译文或 LLM 终稿会被丢弃，历史内容、尺寸和样式都不再刷新。

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

这不是三套 ASR。一次会话只加载一个已选择的语音识别 provider；MarianMT 和 LLM 接收的都是文字：

- Apple Speech / Sherpa-ONNX：尽快把声音变成可修订的源语言文字。
- MarianMT：本地专用翻译模型，生成低延迟草稿。
- LLM：只在断句后结合最近两句、术语表和可选参考背景，保守纠正 ASR 原文并生成终稿译文，可以完全关闭。

## 当前功能

- 可替换 ASR provider；默认 Apple Speech 中英文流式 `partial` / `final` 识别。
- 本地 Sherpa-ONNX Zipformer INT8 中英混合流式识别，支持同一句中英文混读。
- 切换 ASR 时只保存引擎 ID，点击“开始”才创建所选 provider，停止后释放进程和模型。
- 音频来源可选择麦克风或系统音频；系统音频可识别视频通话、播放器等应用播放的声音。
- SQLite 双语个人词典。
- 可直接输入 LLM 背景文字，也可提供 UTF-8 `.txt`、`.docx` 或 `.xlsx` 路径，用于术语、实体与领域语境消歧。
- 词典术语和别名作为 Apple `contextualStrings` 热词。
- ASR 后确定性别名纠正和 MarianMT glossary 约束。
- 本地 OPUS-MT 中文→英文、英文→中文模型，进程常驻并按方向懒加载。
- 可选 OpenAI-compatible LLM 终稿。
- WebSocket 可替换字幕：ASR 原文 → 快速译文 → LLM 终稿。
- 模型忙时丢弃过期 partial，final 可靠排队，避免冷启动积压拖慢终稿。
- 快速译文默认采用 200ms 预览节流；Apple Speech 原文仍在每次 partial 到达时立即显示。
- 稳定前缀缓存和可回滚滑动窗口，避免连续讲话时从句首反复翻译。
- 终稿可编辑；短差异可保存为双语词典项。
- 浏览器界面支持语言切换、启动停止、字幕历史和词典管理。
- macOS 半透明悬浮字幕，始终置顶并可跨桌面、全屏空间显示。

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
./scripts/setup-macos-codesigning.sh
./scripts/build-macos-speech.sh
./scripts/build-macos-overlay.sh
./scripts/setup-models.sh
# 可选：安装本地中英混合 Sherpa-ONNX ASR
./scripts/setup-sherpa-onnx.sh
cargo run
```

`setup-macos-codesigning.sh` 会在当前用户的登录钥匙串中创建项目专用的稳定签名身份，证书和私钥不会写入仓库。稳定签名可避免每次重编译 Apple Speech 桥接后，麦克风、语音识别和屏幕录制权限因 ad-hoc 签名哈希变化而失效。首次切换到稳定签名后仍需重新授权一次。

打开 <http://127.0.0.1:8765>，选择 ASR、音频来源和翻译方向，再点击“开始实时翻译”。点击“悬浮字幕”会打开一个不占 Dock、半透明、始终置顶的原生字幕窗口；窗口内可以直接切换 ASR、麦克风/系统音频和中英方向，也可以开始或停止翻译。拖动顶部可以移动，拖动右下角的原生缩放手柄可以调整大小，位置和尺寸会自动记忆，点右上角 `×` 关闭。它和主界面读取同一个 WebSocket，不会启动第二套 ASR 或翻译任务。

### 可替换 ASR 与按需加载

ASR 采用 provider/session 两层接口。设置页只保存 `apple_speech` 或 `sherpa_onnx`，切换选择不会启动进程、下载模型或占用模型内存；点击开始时才创建对应会话，停止后关闭并释放。`/api/health` 会返回每个 provider 的可用状态，未配置项在主界面和悬浮窗中会禁用。

Apple Speech bridge 随项目构建。要启用本地 Sherpa-ONNX，运行：

```bash
./scripts/setup-sherpa-onnx.sh
```

脚本安装 `sherpa-onnx` Python wheel，并下载官方 `sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20`。运行时采用 INT8 encoder/joiner 与 FP32 decoder，保留的权重约 200MB。模型来源与参数可在 [Sherpa-ONNX 官方模型文档](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-transducer/zipformer-transducer-models.html#csukuangfj-sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20-bilingual-chinese-english) 核对。

安装完成后项目会自动发现内置 bridge 和模型，无需修改 `.env.local`。如需使用自定义位置，可覆盖：

```bash
RT_TRANSLATION_SHERPA_BRIDGE=/absolute/project/path/scripts/sherpa-onnx-bridge
```

选择 Sherpa-ONNX 后，Rust 先启动 Python bridge，模型加载完成后再启动稳定签名的 macOS 音频桥。音频桥负责麦克风或 ScreenCaptureKit 系统音频权限，并发送单声道 Float32 PCM；Sherpa 内部重采样后负责 `partial`、endpoint 和 `final`。停止会话后两个进程和模型内存都会释放。模型文件放在被 Git 忽略的 `models/`，不会提交到仓库。

悬浮窗沿用了 Saymore 的关键窗口行为：无边框透明背景、floating 层级、`CanJoinAllSpaces` 与 `FullScreenAuxiliary`。字幕内部仍采用本项目的“一个可变当前段 + 一个不可变历史段”状态机，历史段封存后不会被迟到的翻译结果刷新。

## LLM 参考文档

主界面和悬浮窗都把这组可选设置默认折叠；已填写背景时会自动展开。展开后可直接填写背景文字，也可填写本地 `.txt`、`.docx` 或 `.xlsx` 路径；两者可以同时使用，直接文字优先占用上下文额度。路径支持以 `~/` 开头。文档在点击开始时解析一次：文件上限 20MB，直接文字与文档提取内容合计最多保留约 12,000 字符。ASR 和 MarianMT 不读取这些背景，因此首字与草稿延迟不受影响；背景只随最终断句发送给已配置的 LLM，用于识别术语、实体、缩写和领域语境。

系统提示词把文档明确标记为不可信参考资料：它不能覆盖翻译指令，也不能让 LLM 补入当前语音中没有表达的事实。文件路径本身不会发送给 LLM，但提取后的文字会发送到 `.env.local` 配置的 LLM 服务，请不要选择不应上传到该服务的敏感文档。

### 识别视频通话声音

选择“系统音频（通话对方）”后，macOS ScreenCaptureKit 会采集当前系统播放的声音并送入 Apple Speech。它适用于 Zoom、Teams、腾讯会议、FaceTime 和浏览器通话等场景，不要求把声音外放给麦克风。

当前系统音频模式采集当前显示器范围内所有应用的播放声音，并排除本程序自身音频。为避免字幕混入通知声或其他播放器，通话期间请关闭无关音频。当前一次会话只选择一个来源；如需同时区分自己与对方，需要后续增加双 ASR 通道和说话方标记，不能简单把两路声音混给同一个识别任务。

MarianMT 权重在第一次使用某个翻译方向时从 Hugging Face 下载。只想先查看界面和测试词典时，可以跳过 Python 模型安装：

```bash
RT_TRANSLATION_FAKE_TRANSLATION=1 cargo run
```

`RT_TRANSLATION_PREVIEW_INTERVAL_MS` 控制快速译文尝试更新的最短间隔，默认 `200`，程序会限制在 `100–1000ms`。降低它不会缩短 Apple Speech 自身产生 partial 的时间，只会减少 ASR 更新后等待下一次 MarianMT 预览的额外延迟。M1 Pro 实测短句热推理 CPU 约 `119ms`、MPS 约 `564ms`，因此默认继续使用 CPU。首次加载某个方向仍可能需要数秒；打开悬浮窗、切换方向或开始会话时会提前异步预热，后续推理会明显更快。

## 可选 LLM

不配置任何 key 也可以启动并完成实时翻译；此时 MarianMT 的结果直接成为终稿。要启用 OpenAI-compatible 润色：

```bash
RT_TRANSLATION_LLM_ENABLED=1 \
RT_TRANSLATION_LLM_BASE_URL=https://api.openai.com/v1 \
RT_TRANSLATION_LLM_API_KEY=your-key \
RT_TRANSLATION_LLM_MODEL=your-model \
cargo run
```

`RT_TRANSLATION_LLM_BASE_URL` 也可以指向本地的 Ollama、LM Studio 或其他兼容服务。程序调用 `/chat/completions`，要求模型返回包含 `corrected_source` 和 `translation` 的 JSON；旧的纯译文响应仍会作为兼容降级处理。

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

LLM 终稿采用受 Saymore 启发的保守转换策略：只处理当前段；上一段仅用于代词、术语和语气消歧；草稿译文仅作为可纠正候选；个人词典和参考背景可以帮助修复当前 ASR 中的同音词、实体名与中英混读错误。模型必须同时返回纠正后的原文和目标语言译文，并保留问题、否定、条件、不确定性、数字、实体和未完成语义。后端会用长度与字符编辑距离拒绝激进的原文改写；旧的纯译文响应也仍然兼容。

## 客观指标与优化基线

主界面的“客观指标”默认折叠，展开后会实时显示当前或最近一次会话的处理数据，并可导出最近 10 次会话的 JSON 摘要。完成 3 次 ASR、音频来源、语言方向和背景长度一致的会话后，“保存三次基线”会自动取三个会话各项数据的中位数；基线保存在数据库同目录的 `metrics-baseline.json`，服务重启后仍然有效。

当前记录的指标包括：

- ASR provider 启动时间、开始会话到首段文字的时间、partial 更新 P50/P95，以及已显示前缀被回改的字符比例。
- 断句前静音/稳定等待、句段时长、句段字符数、短碎片率及断句来源。
- MarianMT 单次模型推理时间、包含排队和拼接的草稿就绪时间。
- LLM 请求延迟、请求字符数、参考背景字符数、原文和译文修订比例，以及断句到终稿的端到端时间。
- 用户手工保存纠正时，原文和译文相对模型终稿的字符编辑比例。

“开始会话到首段文字”包含用户尚未开口的等待时间，不能单独当作 ASR 推理延迟。ASR/LLM/用户修订率是准确性的可重复代理，不是真实 WER、CER 或人工翻译评分；正式比较必须使用同一段音频和设置至少运行 3 次。项目规定后续阶段只有在主指标改善，且关键延迟或纠正率护栏没有明显退化时才能标记完成，详细门槛见 [`PLAN.md`](PLAN.md)。

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

两个构建脚本分别生成 `target/RealtimeTranslationSpeechBridge.app` 和 `target/RealtimeTranslationOverlay.app`。不要把 Speech Bridge 改为直接运行裸 Mach-O；macOS TCC 需要从 app bundle 的 `Info.plist` 读取麦克风、语音识别和系统音频权限用途说明。

## 当前限制

- 仅支持中文和英文。
- 主控制台仍是本机 Web UI；两个原生 `.app` 辅助组件尚未打包为可分发安装程序。Speech Bridge 推荐使用项目生成的本地稳定签名。
- 系统音频当前按显示器采集，不提供单个通话应用/窗口选择；一次会话也不能同时分离麦克风与系统音频。
- Apple Speech 的可用性和离线能力取决于系统、语言包与 macOS。
- MarianMT 首次加载和第一次推理明显慢于后续请求；CPU 内存占用取决于 PyTorch 和已加载的模型方向。
- 已经播放出去的 TTS 无法像字幕一样撤回，因此 MVP 暂不自动朗读。
- 自动词典学习只接受用户明确保存的短文本差异，避免静默污染个人词典。

## License

MIT
