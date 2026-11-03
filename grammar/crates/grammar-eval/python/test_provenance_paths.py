"""Offline checks of provenance_paths.py: package JSON records no absolute local path, user name or private
directory name. Synthetic paths only; nothing is read from disk beyond the working directory's name and this
module, which one check copies to a scratch directory outside the repository layout."""
import getpass
import os
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest

import provenance_paths as pp

ROOT = "/srv/checkout"


class LocalPaths(unittest.TestCase):
    def test_absolute_local_paths_are_recognized(self):
        for yes in ("/opt/u/x", "/x", "~/x", "~user/x", "C:\\x", "c:/x", "\\\\host\\share"):
            self.assertTrue(pp.is_local_path(yes), yes)
        for no in ("", "/", "x/y", "norm/run-000", "~", "~ u/x", "a:b", "C:", "line\n/x", "/x\r", "1/2", 5, None):
            self.assertFalse(pp.is_local_path(no), repr(no))

    def test_embedded_home_paths_and_file_urls_are_recognized(self):
        for yes in ("file:///opt/x", "run in /home/someone/x", "see /Users/someone/fit", "D:\\Users\\someone\\x",
                    "at /root/x"):
            self.assertTrue(pp.embeds_local_path(yes), yes)
        for no in ("/v1/load", "norm/run-000", "a home directory", "users", 5):
            self.assertFalse(pp.embeds_local_path(no), repr(no))

    def test_relative_paths_resolve_against_the_working_directory(self):
        old = os.getcwd()
        with tempfile.TemporaryDirectory() as d:
            root = Path(d) / "checkout"
            (root / "data" / "grammar-private" / "exp-17").mkdir(parents=True)
            try:
                os.chdir(root)
                self.assertEqual(pp.portable("norm/run-000", root), "norm/run-000")
                self.assertEqual(pp.portable("./norm//run-000/", root), "norm/run-000")
                self.assertEqual(pp.portable(".", root), ".")
                self.assertEqual(pp.portable(Path("fit"), root), "fit")
                # The repository's ignored private tree: its directory names are never recorded.
                self.assertEqual(pp.portable("data/grammar-private/exp-17/fits/fit", root), "fit")
                self.assertEqual(pp.portable("data", root), pp.OUTSIDE)
                # Run from inside the private tree: a relative path is still under it.
                os.chdir(root / "data" / "grammar-private" / "exp-17")
                self.assertEqual(pp.portable("spans-v1/fits/S0+-seed-17/fit", root), "fit")
                self.assertEqual(pp.portable("../../../grammar/x", root), "grammar/x")
                # Run from elsewhere in the repository: repository-relative.
                (root / "grammar").mkdir()
                os.chdir(root / "grammar")
                self.assertEqual(pp.portable("crates/x", root), "grammar/crates/x")
                # Run from outside the repository: only the final component.
                os.chdir(d)
                self.assertEqual(pp.portable("elsewhere/fits/fit", root), "fit")
            finally:
                os.chdir(old)

    def test_recorded_relative_strings_are_read_as_repository_relative(self):
        self.assertEqual(pp.portable_recorded("norm/run-001", ROOT), "norm/run-001")
        self.assertEqual(pp.portable_recorded("./norm//run-001/", ROOT), "norm/run-001")
        self.assertEqual(pp.portable_recorded("data/grammar-private/exp-17/fits/fit", ROOT), "fit")
        self.assertEqual(pp.portable_recorded("../exp-17/fit", ROOT), "fit")
        self.assertEqual(pp.portable_recorded("fit", ROOT), "fit")
        self.assertEqual(pp.portable_recorded(pp.OUTSIDE, ROOT), pp.OUTSIDE)
        self.assertEqual(pp.portable_recorded("/srv/checkout/data/x/run-001", ROOT), "run-001")
        self.assertEqual(pp.portable_recorded("~someone/fits/e1", ROOT), "e1")

    def test_absolute_paths_become_repository_relative_or_their_final_component(self):
        self.assertEqual(pp.portable("/srv/checkout/grammar/x/run-001", ROOT), "grammar/x/run-001")
        self.assertEqual(pp.portable("/srv/checkout/./grammar/../grammar/y", ROOT), "grammar/y")
        self.assertEqual(pp.portable("/srv/checkout/data/x/run-001", ROOT), "run-001")
        self.assertEqual(pp.portable("/srv/checkout", ROOT), ".")
        self.assertEqual(pp.portable("/opt/elsewhere/a-recipe/fit", ROOT), "fit")
        self.assertEqual(pp.portable("/srv/checkout-other/fit", ROOT), "fit")
        self.assertEqual(pp.portable("D:\\work\\fit", ROOT), "fit")
        self.assertEqual(pp.portable("~/a-recipe/norm", ROOT), "norm")
        self.assertEqual(pp.portable("~someone/a-recipe/norm", ROOT), "norm")
        # `..` leaves the given form: resolved against the working directory.
        cwd = Path.cwd()
        self.assertEqual(pp.portable(f"../{cwd.name}/z", "/nowhere"), "z")
        self.assertEqual(pp.portable(f"../{cwd.name}/z", cwd), "z")
        self.assertEqual(pp.portable(cwd / "q" / "r", cwd), "q/r")

    def test_no_user_name_or_home_directory_is_recorded(self):
        for home in ("~", "~/", "~someone", "~someone/", "/home/someone", "/home/someone/", "/Users/someone",
                     "/var/home/someone", "/root", "C:\\Users\\someone", "C:\\Users", str(Path.home())):
            self.assertEqual(pp.portable(home, ROOT), pp.OUTSIDE, home)
        self.assertEqual(pp.portable(f"/mnt/disk/{getpass.getuser()}", ROOT), pp.OUTSIDE)
        self.assertEqual(pp.portable("/home/someone/fits/e1", ROOT), "e1")

    def test_symlinked_spellings_of_the_root_are_inside_it(self):
        with tempfile.TemporaryDirectory() as d:
            real = Path(d) / "real"
            (real / "grammar").mkdir(parents=True)
            link = Path(d) / "link"
            os.symlink(real, link)
            self.assertEqual(pp.portable(link / "grammar" / "x", real), "grammar/x")
            self.assertEqual(pp.portable(real / "grammar" / "x", link), "grammar/x")

    def test_the_default_root_is_this_checkout(self):
        self.assertTrue((pp.REPO / "grammar/crates/grammar-eval/python/provenance_paths.py").is_file())
        self.assertEqual(pp.portable(pp.REPO / "grammar" / "fixtures" / "a"), "grammar/fixtures/a")
        self.assertEqual(pp.portable(pp.REPO / "data" / "fits" / "a"), "a")

    def test_a_copy_outside_the_repository_imports_and_refuses_only_a_default_root(self):
        # A bitwise harness copies the reference tree's modules to a shallow scratch directory.
        with tempfile.TemporaryDirectory() as d:
            copy = Path(d) / "tree" / "provenance_paths.py"
            copy.parent.mkdir()
            copy.write_bytes(Path(pp.__file__).read_bytes())
            script = ("import provenance_paths as p\n"
                      "assert p.is_local_path('/x') and p.portable_recorded('data/fits/a') == 'a'\n"
                      "assert p.portable('/srv/checkout/grammar/x', '/srv/checkout') == 'grammar/x'\n"
                      "assert p.portable_at({'f': '/srv/checkout/n/r'}, ('/f',), '/srv/checkout')[0] == {'f': 'n/r'}\n"
                      "for use in (lambda: p.REPO, lambda: p.portable('/srv/x'), lambda: p.portable_at({'f': '/x'}, ('/f',))):\n"
                      "    try:\n        use()\n    except ValueError as e:\n"
                      "        assert 'repository layout' in str(e), e\n"
                      "    else:\n        raise AssertionError('a default root was guessed')\n")
            run = subprocess.run([sys.executable, "-B", "-c", script], cwd=copy.parent, capture_output=True, text=True)
            self.assertEqual(run.returncode, 0, run.stderr)

    def test_only_known_provenance_pointers_are_rewritten(self):
        value = {"runs": [{"run": "/srv/checkout/n/run-000", "shard": 0}, {"run": "n/run-001", "shard": 1},
                          {"run": "data/grammar-private/exp-17/norm/run-002", "shard": 2}],
                 "a/b~": {"file": "/opt/elsewhere/cal.json"}, "note": "see /x", "n": 1.5, "knots": [0.25, 1e-7],
                 "heads_pointer": "/heads/sha256", "route": "/v1/load", "other": "data/grammar-private/x/y"}
        new, changes = pp.portable_at(value, ("/runs/*/run", "/a~1b~0/file"), ROOT)
        self.assertEqual(sorted(changes), [("/a~1b~0/file", "cal.json"), ("/runs/0/run", "n/run-000"),
                                           ("/runs/2/run", "run-002")])
        self.assertEqual(new["runs"][0]["run"], "n/run-000")
        self.assertEqual(new["runs"][1], value["runs"][1])
        # A recorded relative string under the private tree keeps only its final component.
        self.assertEqual(new["runs"][2]["run"], "run-002")
        # Outside the known pointers nothing changes; the warning names it.
        self.assertEqual(new["other"], "data/grammar-private/x/y")
        self.assertEqual(sorted(pp.local_paths(new)), ["/heads_pointer", "/other", "/route"])
        self.assertEqual(new["a/b~"]["file"], "cal.json")
        # JSON pointers, routes and free text are never changed.
        self.assertEqual((new["note"], new["heads_pointer"], new["route"]), ("see /x", "/heads/sha256", "/v1/load"))
        self.assertEqual((new["n"], new["knots"]), (1.5, [0.25, 1e-7]))
        # The input is not changed, and a portable value is a fixed point.
        self.assertEqual(value["runs"][0]["run"], "/srv/checkout/n/run-000")
        self.assertEqual(pp.portable_at(new, ("/runs/*/run", "/a~1b~0/file"), ROOT), (new, []))

    def test_warnings_name_strings_and_keys_by_pointer(self):
        value = {"runs": [{"run": "/opt/x/run-000"}], "note": "run in /home/someone/x", "ok": "n/run-001",
                 "/home/someone/key": 1, "nested": {"file:///x": {"y": 2}},
                 "private": "data/grammar-private/exp-17/fit", "words": "data and more"}
        found = pp.local_paths(value)
        self.assertEqual(sorted(found), ["/<path-like key>", "/nested/<path-like key>", "/note", "/private",
                                         "/runs/0/run"])
        self.assertFalse(any("someone" in p or "grammar-private" in p for p in found))
        self.assertEqual(pp.local_paths({"run": "n/run-001"}), [])
        for yes in ("data/x", "./data/x/y", "data\\x"):
            self.assertTrue(pp.names_private_tree(yes), yes)
        for no in ("data", "data/", "metadata/x", "data x/y", "n/data/x", 5):
            self.assertFalse(pp.names_private_tree(no), repr(no))


if __name__ == "__main__":
    unittest.main()
