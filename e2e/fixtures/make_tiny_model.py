"""Builds the stand-in model the browser test uses for code suggestions.

The real model is a download of a few hundred megabytes from Hugging Face.
This one is a few kilobytes and needs no network, and it runs through the very
same path: the tokenizer, ONNX Runtime (on WebGPU where the browser has it),
the generation loop and the worker.

It "knows" one thing: after a letter comes the next letter of the alphabet.
So after `ab` it suggests `cdefg...`, which a test can check.

    pip install onnx
    python make_tiny_model.py        # writes tiny-model/ next to this file
"""

import json
from pathlib import Path

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper

LETTERS = "abcdefghijklmnopqrstuvwxyz"
END, UNKNOWN = "<|endoftext|>", "<unk>"
VOCAB = [END, *LETTERS, UNKNOWN]


def model() -> onnx.ModelProto:
    size = len(VOCAB)
    # table[token] is the scores for what follows it: one clear winner.
    table = np.zeros((size, size), dtype=np.float32)
    for token in range(1, len(LETTERS)):
        table[token, token + 1] = 10.0  # a -> b, b -> c, ...
    table[len(LETTERS), 0] = 10.0  # z -> end
    table[0, 0] = 10.0  # end -> end
    table[size - 1, 1] = 10.0  # anything unknown -> a

    nodes = [
        helper.make_node("Gather", ["table", "input_ids"], ["scores"], axis=0),
        # The runtime always supplies attention_mask, so the graph has to take
        # it. It is all ones here; multiplying by it changes nothing.
        helper.make_node("Cast", ["attention_mask"], ["mask"], to=TensorProto.FLOAT),
        helper.make_node("Unsqueeze", ["mask", "last_axis"], ["mask_3d"]),
        helper.make_node("Mul", ["scores", "mask_3d"], ["logits"]),
    ]
    graph = helper.make_graph(
        nodes,
        "next_letter",
        inputs=[
            helper.make_tensor_value_info("input_ids", TensorProto.INT64, ["batch", "sequence"]),
            helper.make_tensor_value_info("attention_mask", TensorProto.INT64, ["batch", "sequence"]),
        ],
        outputs=[helper.make_tensor_value_info("logits", TensorProto.FLOAT, ["batch", "sequence", size])],
        initializer=[
            numpy_helper.from_array(table, "table"),
            numpy_helper.from_array(np.array([-1], dtype=np.int64), "last_axis"),
        ],
    )
    built = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)])
    built.ir_version = 9
    onnx.checker.check_model(built)
    return built


def main() -> None:
    out = Path(__file__).parent / "tiny-model"
    (out / "onnx").mkdir(parents=True, exist_ok=True)
    onnx.save(model(), out / "onnx" / "model.onnx")

    def write(name: str, value: dict) -> None:
        (out / name).write_text(json.dumps(value, indent=2) + "\n")

    write(
        "config.json",
        {
            "model_type": "gpt2",
            "architectures": ["GPT2LMHeadModel"],
            "vocab_size": len(VOCAB),
            "n_positions": 1024,
            "n_embd": 8,
            "n_head": 1,
            "n_layer": 0,
            "bos_token_id": 0,
            "eos_token_id": 0,
            "use_cache": False,
        },
    )
    write("generation_config.json", {"bos_token_id": 0, "eos_token_id": 0})
    write(
        "tokenizer_config.json",
        {"tokenizer_class": "PreTrainedTokenizerFast", "eos_token": END, "bos_token": END, "unk_token": UNKNOWN},
    )
    write(
        "tokenizer.json",
        {
            "version": "1.0",
            "truncation": None,
            "padding": None,
            "added_tokens": [
                {"id": 0, "content": END, "single_word": False, "lstrip": False, "rstrip": False, "normalized": False, "special": True},
                {"id": len(VOCAB) - 1, "content": UNKNOWN, "single_word": False, "lstrip": False, "rstrip": False, "normalized": False, "special": True},
            ],
            "normalizer": None,
            "pre_tokenizer": None,
            "post_processor": None,
            # No merges: every character is its own token.
            "decoder": {"type": "Fuse"},
            "model": {
                "type": "BPE",
                "dropout": None,
                "unk_token": UNKNOWN,
                "continuing_subword_prefix": None,
                "end_of_word_suffix": None,
                "fuse_unk": False,
                "byte_fallback": False,
                "vocab": {token: index for index, token in enumerate(VOCAB)},
                "merges": [],
            },
        },
    )
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
