# Archived Python prototype

This is the original implementation from commit `8c6ce6a`, retained for result
reproduction. New development uses the Rust crate at the repository root.

From the repository root, the historical tests can still be run with:

```sh
PYTHONPATH=legacy/python-v0.1/src .venv/bin/python -m unittest discover -s legacy/python-v0.1
PYTHONPATH=legacy/python-v0.1/src .venv/bin/python legacy/python-v0.1/reproduce_preface.py
```

The reproduction script's root path was adjusted for this move. Its feature
version and recorded detector inputs remain those of the original experiment.
