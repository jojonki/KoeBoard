# モデルと依存のライセンス

KoeBoard はモデルを同梱しません。設定画面でユーザーが選んだモデルだけを、ライセンスの URL を提示して同意を得たあとに、固定コミットから取得して SHA-256 を検証します。

## 採用モデル

| モデル | ライセンス | 取得元 | 注意 |
|---|---|---|---|
| Haas-ja-v1.0(量子化 ONNX、既定) | Apache-2.0 | [oboroge0/Haas-ja-v1.0-onnx-quantized](https://huggingface.co/oboroge0/Haas-ja-v1.0-onnx-quantized) | 原著は Third Intelligence。ONNX 版は非公式変換。詳細は[下記](#haas-ja-v10-のライセンス)を参照してください |
| reazonspeech-k2-v2(int8、軽量) | Apache-2.0 | [reazon-research/reazonspeech-k2-v2](https://huggingface.co/reazon-research/reazonspeech-k2-v2) | リポジトリに LICENSE ファイルはなく、モデルカードの表記に基づきます |

モデルの重みのライセンスと、学習データ(ReazonSpeech、Common Voice、YODAS など)の利用条件は別の話です。商用利用では学習データの条件も確認してください。

### Haas-ja-v1.0 のライセンス

| | 原著 | KoeBoard が取得する ONNX 変換版 |
|---|---|---|
| モデル | [Haas-ja-v1.0](https://huggingface.co/third-intelligence/Haas-ja-v1.0) | [Haas-ja-v1.0-onnx-quantized](https://huggingface.co/oboroge0/Haas-ja-v1.0-onnx-quantized) |
| 提供者 | Third Intelligence, Inc. | oboroge0(非公式変換。Third Intelligence の公認ではありません) |
| ライセンス | [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)(重みと推論コード。第三者のコンポーネントはそれぞれのライセンスに従います) | [Apache License 2.0](https://huggingface.co/oboroge0/Haas-ja-v1.0-onnx-quantized/blob/7f5ab0a4b1bf39db78afd6691eeecbc4f9d42e39/LICENSE) |
| ライセンスの根拠 | モデルカードの表記 | リポジトリの `LICENSE` と `NOTICE` |

ONNX 変換版の `LICENSE` と `NOTICE` は、モデルと一緒にモデルのフォルダーへ取得します。`NOTICE` の全文は次のとおりです。

```text
Haas-ja-v1.0-onnx-quantized

This is a modified version of Haas-ja-v1.0 by Third Intelligence
(https://huggingface.co/third-intelligence/Haas-ja-v1.0),
revision 660728d39019e5e065046154beca44ac9a66a540, licensed under the Apache License 2.0.

Changes from the original:
- Converted the weights from NeMo format to ONNX for use with sherpa-onnx.
- Quantized the encoder (pointwise convolutions to int8, MatMul weights to 4-bit).
- Flattened the prediction network's state into two tensors, and added the ONNX
  metadata sherpa-onnx expects.

conversion/fla_cpu_shim.py reimplements decode-time kernels of flash-linear-attention
(https://github.com/fla-org/flash-linear-attention, MIT License) in pure PyTorch.

This conversion is unofficial and is not endorsed by Third Intelligence.
```

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
