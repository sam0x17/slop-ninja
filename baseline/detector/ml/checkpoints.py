"""Allowed upstream encoder identities; importing this module never loads ML or uses the network."""

DEFAULT_MODEL_ID = "answerdotai/ModernBERT-base"
CHECKPOINTS = {
    DEFAULT_MODEL_ID: {
        "revision": "8949b909ec900327062f0ebf497f51aef5e6f0c8",
        "architecture": "ModernBERT-base",
        "weight_license": "Apache-2.0",
        "config": {"num_hidden_layers": 22, "hidden_size": 768,
                   "intermediate_size": 1152, "num_attention_heads": 12},
    },
    "answerdotai/ModernBERT-large": {
        "revision": "45bb4654a4d5aaff24dd11d4781fa46d39bf8c13",
        "architecture": "ModernBERT-large",
        "weight_license": "Apache-2.0",
        "config": {"num_hidden_layers": 28, "hidden_size": 1024,
                   "intermediate_size": 2624, "num_attention_heads": 16},
    },
}
TOKENIZER_FILES = ("config.json", "tokenizer.json", "tokenizer_config.json", "special_tokens_map.json")
MODEL_FILES = (*TOKENIZER_FILES, "model.safetensors")


def checkpoint_spec(pin):
    spec = CHECKPOINTS.get(pin.get("model_id"))
    if spec is None or pin.get("revision") != spec["revision"]:
        raise ValueError("checkpoint does not match an allowed pinned ModernBERT revision")
    if pin.get("weight_license") != spec["weight_license"]:
        raise ValueError("checkpoint license differs from its pinned upstream declaration")
    return spec


def validate_config(config, spec):
    expected = {"model_type": "modernbert", "vocab_size": 50368,
                "max_position_embeddings": 8192, "classifier_pooling": "mean", **spec["config"]}
    for key, value in expected.items():
        if config.get(key) != value:
            raise ValueError(f"checkpoint architecture differs from its pin: {key}")
