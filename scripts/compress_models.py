# /// script
# requires-python = ">=3.10"
# dependencies = ["onnx==1.23.1", "onnxslim==0.1.97", "numpy==2.5.3", "blake3==1.0.10"]
# ///
"""Rebuild the semantic models of the `models-v1` release from the fp32 originals.

The app downloads two weight-compressed models on first use (`src-tauri/src/tools.rs`). This
script documents how they were made: it downloads `iscc-sci-v0.1.0.onnx` and
`iscc-sct-v0.1.0.onnx` from iscc-binaries v1.0.0 (checked by BLAKE3) and writes into
`build/models/`:

- `iscc-sci-v0.1.0-w16.onnx`: input fixed to 1x3x512x512 and the graph simplified with onnxslim,
  then every float weight of 1,024 or more elements stored as fp16 and cast back to fp32.
- `iscc-sct-v0.1.0-emb8-w16.onnx`: the pooler output removed, the word-embedding table stored as
  int8 with one scale per row and dequantised after the lookup, then every other float weight of
  1,024 or more elements stored as fp16.

All maths stays fp32. The script prints each file's BLAKE3 and whether it equals the published
file. The published files stay authoritative: their drift from the fp32 models is the one
measured (at most 1 and 2 of 256 bits). Checked on 2026-10-04 with the pinned versions: the text
model rebuilds byte for byte; the image model serialises differently (onnxslim's output changes
between its releases, 0.1.96 too), and its outputs in rten equal the published model's exactly
on all 16 test images.

    uv run scripts/compress_models.py
"""

import urllib.request
from pathlib import Path

import blake3
import numpy as np
import onnx
import onnxslim
from onnx import helper, numpy_helper

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "build" / "models"
ORIGINALS = "https://github.com/iscc/iscc-binaries/releases/download/v1.0.0/"
SOURCES = {
    "iscc-sci-v0.1.0.onnx": "af95054d463e4c95de4c099a7947dbc2f3db168507fef25e91e6984a6f32dd3c",
    "iscc-sct-v0.1.0.onnx": "ff254d62db55ed88a1451b323a66416f60838dd2f0338dba21bc3b8822459abc",
}
PUBLISHED = {
    "iscc-sci-v0.1.0-w16.onnx": "9f24dd3d0440eaa07adf121b21a1cbd8518fe36048b9d8a72b606d8a6416e8a3",
    "iscc-sct-v0.1.0-emb8-w16.onnx": "e04671ff5ff9cc325400dd73318af9919322a367986f03a199c769f8143a4679",
}
# Smaller float weights stay fp32.
MIN_ELEMS = 1024
EMBEDDINGS = "embeddings.word_embeddings.weight"


def hash_file(path):
    """BLAKE3 of a file, hex."""
    return blake3.blake3(path.read_bytes()).hexdigest()


def fetch(name):
    """Download an fp32 original into OUT unless it is there, and check its BLAKE3."""
    path = OUT / name
    if not path.exists():
        print("downloading", name)
        urllib.request.urlretrieve(ORIGINALS + name, path)
    if hash_file(path) != SOURCES[name]:
        raise SystemExit(f"{name} failed its integrity check")
    return path


def consumers(graph):
    """Map each tensor name to the (node, input index) pairs that read it."""
    uses = {}
    for node in graph.node:
        for i, name in enumerate(node.input):
            uses.setdefault(name, []).append((node, i))
    return uses


def quant_axis(uses, arr):
    """Per-channel axis of a weight: the columns of a MatMul's B, else axis 0."""
    node, index = uses[0]
    if node.op_type == "MatMul" and index == 1:
        return arr.ndim - 1
    return 0


def int8_per_channel(arr, axis):
    """Symmetric int8 per channel along `axis`: values and scales (absmax / 127)."""
    others = tuple(i for i in range(arr.ndim) if i != axis)
    amax = np.max(np.abs(arr), axis=others)
    scale = np.where(amax > 0, amax / 127.0, 1.0).astype(np.float32)
    shape = [1] * arr.ndim
    shape[axis] = -1
    q = np.clip(np.round(arr / scale.reshape(shape)), -127, 127).astype(np.int8)
    return q, scale


def weights_fp16(model):
    """Store every used float weight of MIN_ELEMS or more elements as fp16 with a Cast to fp32."""
    graph = model.graph
    uses = consumers(graph)
    keep, casts = [], []
    for init in graph.initializer:
        arr = numpy_helper.to_array(init)
        eligible = arr.dtype == np.float32 and arr.size >= MIN_ELEMS and init.name in uses
        if not eligible or init.name.endswith("_scale"):
            keep.append(init)
            continue
        stored = init.name + "_stored"
        keep.append(numpy_helper.from_array(arr.astype(np.float16), stored))
        casts.append(helper.make_node("Cast", [stored], [init.name], to=onnx.TensorProto.FLOAT, name=init.name + "_cast"))
    del graph.initializer[:]
    graph.initializer.extend(keep)
    nodes = casts + list(graph.node)
    del graph.node[:]
    graph.node.extend(nodes)
    return model


def embeddings_int8(model):
    """Store the word-embedding table as int8 with a scale per row; only looked-up rows are expanded:
    Gather(q, ids) -> Cast -> Mul(Unsqueeze(Gather(scale, ids), -1))."""
    graph = model.graph
    init = next(i for i in graph.initializer if i.name == EMBEDDINGS)
    gather = next(n for n in graph.node if n.op_type == "Gather" and n.input[0] == EMBEDDINGS)
    ids, output, t = gather.input[1], gather.output[0], EMBEDDINGS
    q, scale = int8_per_channel(numpy_helper.to_array(init), 0)
    inits = [i for i in graph.initializer if i.name != t] + [
        numpy_helper.from_array(q, t + "_q"),
        numpy_helper.from_array(scale, t + "_scale"),
        numpy_helper.from_array(np.array([-1], np.int64), t + "_axes"),
    ]
    lookup = [
        helper.make_node("Gather", [t + "_q", ids], [t + "_gq"], axis=0, name=t + "_gq"),
        helper.make_node("Cast", [t + "_gq"], [t + "_gf"], to=onnx.TensorProto.FLOAT, name=t + "_cast"),
        helper.make_node("Gather", [t + "_scale", ids], [t + "_gs"], axis=0, name=t + "_gs"),
        helper.make_node("Unsqueeze", [t + "_gs", t + "_axes"], [t + "_gs1"], name=t + "_unsq"),
        helper.make_node("Mul", [t + "_gf", t + "_gs1"], [output], name=t + "_mul"),
    ]
    del graph.initializer[:]
    graph.initializer.extend(inits)
    nodes = []
    for node in graph.node:
        nodes += lookup if node is gather else [node]
    del graph.node[:]
    graph.node.extend(nodes)
    return model


def build_sci(source):
    """The image model with its input fixed, simplified and its weights in fp16."""
    fixed = OUT / "_sci_fixed.onnx"
    onnxslim.slim(str(source), str(fixed), input_shapes=["input_0:1,3,512,512"])
    model = weights_fp16(onnx.load(fixed))
    fixed.unlink()
    return model


def build_sct(source):
    """The text model without its pooler output, embeddings in int8, other weights in fp16."""
    model = onnx.load(source)
    inputs = [i.name for i in model.graph.input]
    pruned = onnx.utils.Extractor(model).extract_model(inputs, ["output"])
    return weights_fp16(embeddings_int8(pruned))


def save(model, name):
    """Check and write a model, then report its hash against the published file's."""
    onnx.checker.check_model(model)
    path = OUT / name
    onnx.save(model, path)
    digest = hash_file(path)
    same = "equals the published file" if digest == PUBLISHED[name] else "differs from the published file"
    print(f"{name}: {path.stat().st_size:,} bytes, BLAKE3 {digest}, {same}")


def main():
    """Build both models into build/models/."""
    OUT.mkdir(parents=True, exist_ok=True)
    save(build_sci(fetch("iscc-sci-v0.1.0.onnx")), "iscc-sci-v0.1.0-w16.onnx")
    save(build_sct(fetch("iscc-sct-v0.1.0.onnx")), "iscc-sct-v0.1.0-emb8-w16.onnx")


if __name__ == "__main__":
    main()
