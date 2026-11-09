# モデルと依存のライセンス

KoeBoard はモデルを同梱しません。設定画面でユーザーが選んだモデルだけを、ライセンスの URL を提示して同意を得たあとに、固定コミットから取得して SHA-256 を検証します。

## 採用モデル

| モデル | ライセンス | 取得元 | 注意 |
|---|---|---|---|
| Haas-ja-v1.0(量子化 ONNX、既定) | Apache-2.0 | [oboroge0/Haas-ja-v1.0-onnx-quantized](https://huggingface.co/oboroge0/Haas-ja-v1.0-onnx-quantized) | 原著は [Third Intelligence](https://huggingface.co/third-intelligence/Haas-ja-v1.0)。ONNX 版は非公式変換。`LICENSE` と `NOTICE` も一緒に取得します |
| reazonspeech-k2-v2(int8、軽量) | Apache-2.0 | [reazon-research/reazonspeech-k2-v2](https://huggingface.co/reazon-research/reazonspeech-k2-v2) | リポジトリに LICENSE ファイルはなく、モデルカードの表記に基づきます |

モデルを同梱して再配布する場合は、Apache-2.0 の条文と `NOTICE` を一緒に配布し、変更点を明示してください。
モデルの重みのライセンスと、学習データ(ReazonSpeech、YODAS など)の利用条件は別の話です。商用利用では学習データの条件も確認してください。

## 採用しなかったモデル

| モデル | 理由 |
|---|---|
| SenseVoice Small | コードは MIT ですが、重みは [FunASR Model License v1.1](https://github.com/modelscope/FunASR/blob/main/MODEL_LICENSE) です。批判的発言での権利喪失(4.2)、提供元による一方的な改定(6)、準拠法が未記入(7)という条項があります。Apache-2.0 と表記するミラーもありますが、原本の条件が優先されます |

新しいモデルを `src/models.rs` に追加するときは、変換版の表記ではなく、重みの原本のライセンスを確認してください。

## 主なランタイム依存

| 依存 | ライセンス |
|---|---|
| sherpa-onnx | Apache-2.0 |
| ONNX Runtime | MIT |
| cpal | Apache-2.0 |
| global-hotkey / tray-icon / tao | Apache-2.0 または MIT |
| arboard | Apache-2.0 または MIT |

| Noto Sans JP(Windows 版に同梱、`assets/fonts/OFL.txt`) | SIL Open Font License 1.1 |
| WebView2Loader(Windows 版に同梱) | Microsoft WebView2 SDK のライセンス(再配布可) |
| MinGW-w64 / GCC ランタイム(Windows 版にリンク) | ZPL ほか、GCC Runtime Library Exception |

設計は [Handy](https://github.com/cjpais/Handy)(MIT)を参考にしていますが、コードは流用していません。
