"""MLX-LM server adapter for the pinned Mistral tokenizer's regex correction."""

from mlx_lm import server


class MistralModelProvider(server.ModelProvider):
    def __init__(self, cli_args):
        super().__init__(cli_args)
        self._tokenizer_config.update(
            fix_mistral_regex=True,
            trust_remote_code=False,
        )

    def load(self, model_path, adapter_path=None, draft_model_path=None):
        if model_path not in ("default_model", self.cli_args.model):
            raise ValueError("This process serves only its command-line Mistral model")
        return super().load(model_path, adapter_path, draft_model_path)


if __name__ == "__main__":
    server.ModelProvider = MistralModelProvider
    server.main()
