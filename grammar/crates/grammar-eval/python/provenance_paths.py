"""Provenance paths in package JSON: no absolute local path, user name or private directory name.

An artifact's JSON role files (contract, heads, calibration, export manifest) are published after its
cohort closes. A provenance string that names a local file or directory must not carry its author's
directory layout: an absolute path names the operating-system user and the home directory, and the
repository's ignored trees (`data/`) hold private corpora, fits and experiment names. `portable` gives the
form package JSON records:

- a path is resolved against the working directory (a relative path too, so a tool run from inside a
  private tree records nothing of it); one that resolves inside the repository, lexically or after
  resolving symlinks, is repository-relative, with `/` separators and `.` parts dropped;
- a path under an ignored private tree of the repository (`PRIVATE_TREES`), or outside the repository,
  is only its final component: its location is omitted;
- a final component that names a user or a home directory (`~`, `~user`, `/home/user`, the user's own
  name) is `OUTSIDE` instead, so no user name is recorded.

`portable_at` rewrites only the strings at known provenance pointers (the tools that write them name
them); every other string is left as it is, so JSON pointers, routes and free text are never changed. A
string already recorded in a file (`portable_recorded`) was written relative to a working directory no
longer known: an absolute one is made portable as above, and a relative one is read as
repository-relative, so one under a private tree (or with `..`) keeps only its final component.
`is_local_path` says whether a string reads as an absolute local path; `local_paths` lists the JSON
pointers of the strings, and the keys, that read as one, embed one (`embeds_local_path`) or name a path
under a private tree (`names_private_tree`), for warnings. Pure Python, no third-party imports. The
miner client warns about the same strings (`slop_ninja_miner pack` and `check`).

`REPO`, the default repository root, is resolved lazily from this file's place in the checkout
(`grammar/crates/grammar-eval/python/`). A copy outside that layout (a reference tree a bitwise harness
copies to a scratch directory) imports cleanly; it refuses only when a default root is actually needed,
so pass `root` explicitly there."""
from __future__ import annotations

import getpass
import os
from pathlib import Path, PurePosixPath, PureWindowsPath

# This file's directory, relative to the repository root.
LAYOUT = ("grammar", "crates", "grammar-eval", "python")
# Ignored trees of the repository whose directory names are private (AGENTS.md: generated corpora,
# credentials, local databases, private fits and their experiment names live in `data/`).
PRIVATE_TREES = ("data",)
# Recorded for a location whose final component would name a user or a home directory.
OUTSIDE = "(outside the repository)"
# Parents whose children are home directories, so a child's name is a user name.
HOME_ROOTS = ("/home", "/Users", "/var/home", "/usr/home")
# Substrings that put a local path inside a longer string (free text, a URL).
EMBEDDED = ("file://", "/home/", "/Users/", "/root/", "/var/home/", ":\\Users\\", ":/Users/")


def repo_root():
    """The repository root holding this file, or a ValueError when this copy is not inside the repository
    layout (`LAYOUT`): no default root is guessed for a copy elsewhere."""
    here = Path(__file__).resolve()
    if len(here.parts) <= len(LAYOUT) + 1 or here.parts[-len(LAYOUT) - 1:-1] != LAYOUT:
        raise ValueError(f"provenance_paths.py is not inside the repository layout ({'/'.join(LAYOUT)}/); "
                         "pass the repository root explicitly")
    return here.parents[len(LAYOUT)]


def __getattr__(name):
    # `REPO` is resolved on first use (PEP 562), so importing a copy outside the repository never fails.
    if name == "REPO":
        return repo_root()
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


def is_local_path(value):
    """Whether `value` reads as an absolute local path: POSIX `/x`, `~/x` or `~user/x`, a Windows drive
    path (`C:\\x`, `C:/x`) or a UNC path (`\\\\host\\share`). Text with a line break is never a path."""
    if not isinstance(value, str) or len(value) < 2 or "\n" in value or "\r" in value:
        return False
    if value[0] == "/" or value.startswith("\\\\"):
        return True
    if value[0] == "~":
        head, sep, _ = value.partition("/")
        return bool(sep) and not any(c.isspace() for c in head)
    return value[0].isascii() and value[0].isalpha() and value[1] == ":" and len(value) > 2 and value[2] in "\\/"


def embeds_local_path(value):
    """Whether `value` carries a home-directory path or a file URL anywhere in it (a path embedded in text)."""
    return isinstance(value, str) and any(e in value for e in EMBEDDED)


def names_private_tree(value):
    """Whether `value` reads as a relative path under one of the repository's private trees (`data/x`):
    no whitespace, its first `/` component a private tree, and at least one more component."""
    if not isinstance(value, str) or any(c.isspace() for c in value):
        return False
    first, sep, rest = value.replace("\\", "/").lstrip("./").partition("/")
    return bool(sep) and first in PRIVATE_TREES and bool(rest.strip("/"))


def _within(path, root):
    try:
        rel = path.relative_to(root)
    except ValueError:
        return None
    return rel.as_posix() if rel.parts else "."


def _user_names():
    names = set()
    for get in (getpass.getuser, lambda: Path.home().name):
        try:
            name = get()
        except (KeyError, OSError, RuntimeError):
            continue
        if name:
            names.add(name)
    return names


def _final(name, location):
    """The final component `name` of `location` (a POSIX path or the original text), or `OUTSIDE` when it
    names a user or a home directory."""
    if not name or name in (".", "..", "~") or name.startswith("~") or name in _user_names():
        return OUTSIDE
    posix = PurePosixPath(location) if isinstance(location, str) else location
    if str(posix.parent) in HOME_ROOTS or str(posix) in ("/root", str(Path.home())):
        return OUTSIDE
    return name


def _inside(rel):
    """A repository-relative path as recorded: itself, or its final component under a private tree."""
    parts = PurePosixPath(rel).parts
    if parts and parts[0] in PRIVATE_TREES:
        return _final(parts[-1], rel) if len(parts) > 1 else OUTSIDE
    return rel


def portable(path, root=None):
    """`path` as package JSON records it (see the module docstring), resolved against the working
    directory; `root` is the repository root (default `repo_root()`)."""
    text = os.fspath(path)
    if text[:2] == "\\\\" or (len(text) > 2 and text[1] == ":" and text[2] in "\\/"):
        # A Windows path on a POSIX host: never inside this checkout; keep the final component.
        win = PureWindowsPath(text)
        if win.parent.name.lower() == "users" or win.name.lower() == "users":
            return OUTSIDE
        return _final(win.name, win.name)
    if text[:1] == "~" and not (text == "~" or text.startswith("~/")):
        # Another user's home: never this checkout's.
        head, _, rest = text.partition("/")
        return _final(PurePosixPath(rest).name, rest) if rest.strip("/") else OUTSIDE
    lexical = Path(os.path.normpath(os.path.join(os.getcwd(), os.path.expanduser(text))))
    root = Path(repo_root() if root is None else root)
    for base, candidate in ((Path(os.path.normpath(root)), lexical), (root.resolve(), lexical.resolve())):
        rel = _within(candidate, base)
        if rel is not None:
            return "." if rel == "." else _inside(rel)
    return _final(lexical.name, lexical)


def _pointer(parent, key):
    return parent + "/" + str(key).replace("~", "~0").replace("/", "~1")


def _matches(pointer, pattern):
    """Whether JSON pointer `pointer` matches `pattern`, whose `*` segments match any one segment."""
    a, b = pointer.split("/"), pattern.split("/")
    return len(a) == len(b) and all(q == "*" or p == q for p, q in zip(a, b))


def portable_recorded(value, root=None):
    """A location string already recorded in a file, as package JSON records it. An absolute local path
    is made portable (`portable`). A relative one was written against a working directory that is no
    longer known, so it is read as repository-relative: under a private tree, or with `..`, only its
    final component is kept; otherwise it stays as written (`/` separators, `.` parts dropped)."""
    if is_local_path(value) or value[:1] == "~":
        return portable(value, root)
    posix = PurePosixPath(value.replace("\\", "/"))
    if ".." in posix.parts:
        return _final(posix.name, posix.name)
    rel = posix.as_posix()
    return "." if rel in (".", "") else _inside(rel)


def portable_at(value, pointers, root=None):
    """(copy of `value` with the location strings at the JSON pointer patterns `pointers` made portable
    (`portable_recorded`), [(JSON pointer, new string)] for each one that changed). Strings anywhere
    else are never changed."""
    changes = []

    def walk(v, pointer):
        if isinstance(v, dict):
            return {k: walk(x, _pointer(pointer, k)) for k, x in v.items()}
        if isinstance(v, list):
            return [walk(x, _pointer(pointer, i)) for i, x in enumerate(v)]
        if isinstance(v, str) and v and "\n" not in v and any(_matches(pointer, p) for p in pointers):
            new = portable_recorded(v, root)
            if new != v:
                changes.append((pointer, new))
            return new
        return v

    return walk(value, ""), changes


def _path_like(v):
    return is_local_path(v) or embeds_local_path(v) or names_private_tree(v)


def local_paths(value):
    """JSON pointers of the strings in `value` that read as an absolute local path, embed one or name a
    path under a private tree, and of the keys that do (a key's pointer is its object's, with the key
    written as `<path-like key>`)."""
    found = []

    def walk(v, pointer):
        if isinstance(v, dict):
            for k, x in v.items():
                if _path_like(k):
                    found.append(pointer + "/<path-like key>")
                    continue
                walk(x, _pointer(pointer, k))
        elif isinstance(v, list):
            for i, x in enumerate(v):
                walk(x, _pointer(pointer, i))
        elif _path_like(v):
            found.append(pointer)

    walk(value, "")
    return found
