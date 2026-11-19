#!/usr/bin/env python3
"""Generates the native-patch differential corpus and records Bazel's own answers.

Each case is a directory ``<out>/<name>/`` with ``before/`` (the repository tree), ``patch`` (the patch
file), ``strip`` and, after the oracle ran, ``expected/`` (the tree after `PatchUtil.apply`) and
``result`` (``OK`` or ``ERR <message>`` with the output directory replaced by ``$OUT``).

The oracle is Bazel's real `PatchUtil` (+ java-diff-utils 4.12) taken from a Bazel 9.3.x server jar:
    python3 patch_corpus.py gen  <out>                 # (re)creates before/patch/strip
    python3 patch_corpus.py oracle <out> <A-server.jar> [<HEAD PatchUtil.java>]
        # runs PatchUtil.apply on a copy and records expected/result (HEAD source when given, else the jar's)
The Rust test `patch_corpus` in crates/zel-bzlmod compares `patch::apply` against the recorded answers.
"""
import difflib, os, shutil, subprocess, sys, tempfile

cases = {}

def C(name, files, patch, strip=0, modes=None, links=None):
    assert name not in cases, name
    cases[name] = dict(files=files, patch=patch, strip=strip, modes=modes or {}, links=links or {})

def nl(lines, final=True):
    s = "\n".join(lines)
    return s + ("\n" if final and lines else "")

def uni(old, new, a="f.txt", b=None, ctx=3, hdr=None):
    b = b or a
    out = list(difflib.unified_diff(old, new, "a/" + a if hdr is None else a, "b/" + b if hdr is None else b, n=ctx, lineterm=""))
    return "\n".join(out) + "\n"

L = lambda n, p="line": [f"{p} {i}" for i in range(1, n + 1)]
base = L(20)

# --- plain modifications, offsets, fuzz -------------------------------------------------
new = base[:]; new[9] = "TEN"
p = uni(base, new)
C("plain_strip1", {"f.txt": nl(base)}, p, 1)
C("plain_nostrip_hdr", {"f.txt": nl(base)}, uni(base, new, hdr=True), 0)
for off in (1, 2, 3, 5, 7, 12):
    C(f"offset_down_{off}", {"f.txt": nl(["pre %d" % i for i in range(off)] + base)}, p, 1)
for off in (1, 2, 4):
    C(f"offset_up_{off}", {"f.txt": nl(base[off:])}, p, 1)
# fuzz: perturb context lines at either end of the hunk
for which, idx in (("first1", 6), ("first2", 7), ("last1", 12), ("last2", 11), ("both1", None)):
    b2 = base[:]
    if idx is not None: b2[idx] = "CHANGED " + b2[idx]
    else: b2[6] = "CHANGED 7"; b2[12] = "CHANGED 13"
    C(f"fuzz_{which}", {"f.txt": nl(b2)}, p, 1)
for k in (3, 4):
    b2 = base[:]
    for i in range(6, 6 + k): b2[i] = "X" + b2[i]
    C(f"fuzz_toomuch_{k}", {"f.txt": nl(b2)}, p, 1)
b2 = base[:]; b2[9] = "OTHER"
C("mismatch_changed_line", {"f.txt": nl(b2)}, p, 1)
b2 = ["pre"] * 3 + base[:]; b2[12] = "CHANGED"
C("fuzz_and_offset", {"f.txt": nl(b2)}, p, 1)
b2 = ["pre"] * 3 + base[:]; b2[3 + 6] = "CHANGED"; b2[3 + 12] = "CHANGED"
C("fuzz_both_and_offset", {"f.txt": nl(b2)}, p, 1)
# context sizes
for ctx in (0, 1, 2, 3, 5):
    C(f"ctx{ctx}", {"f.txt": nl(base)}, uni(base, new, ctx=ctx), 1)
    C(f"ctx{ctx}_offset", {"f.txt": nl(["pre"] * 4 + base)}, uni(base, new, ctx=ctx), 1)
    b2 = base[:]; b2[8] = "CH"; b2[10] = "CH"
    C(f"ctx{ctx}_fuzzy_ctx", {"f.txt": nl(b2)}, uni(base, new, ctx=ctx), 1)
# multiple hunks
new2 = base[:]; new2[1] = "TWO"; new2[9] = "TEN"; new2[18] = "NINETEEN"
mp = uni(base, new2, ctx=2)
C("multi_hunk", {"f.txt": nl(base)}, mp, 1)
C("multi_hunk_offset", {"f.txt": nl(["a", "b"] + base)}, mp, 1)
C("multi_hunk_offset_inner", {"f.txt": nl(base[:5] + ["ins1", "ins2"] + base[5:])}, mp, 1)
C("multi_hunk_second_fails", {"f.txt": nl(base[:9] + ["zzz"] + base[10:])}, mp, 1)
C("multi_hunk_first_fails", {"f.txt": nl(["q"] + base[:1] + ["zzz"] + base[2:])}, mp, 1)
b2 = base[:]; del b2[3:6]
C("multi_hunk_deleted_between", {"f.txt": nl(b2)}, mp, 1)
# insertions / deletions only, zero context
ins = base[:5] + ["N1", "N2"] + base[5:]
dele = base[:5] + base[8:]
for ctx in (0, 1, 3):
    C(f"insert_ctx{ctx}", {"f.txt": nl(base)}, uni(base, ins, ctx=ctx), 1)
    C(f"delete_ctx{ctx}", {"f.txt": nl(base)}, uni(base, dele, ctx=ctx), 1)
    C(f"insert_ctx{ctx}_offset", {"f.txt": nl(["p"] * 3 + base)}, uni(base, ins, ctx=ctx), 1)
    C(f"delete_ctx{ctx}_offset", {"f.txt": nl(["p"] * 3 + base)}, uni(base, dele, ctx=ctx), 1)
C("insert_at_start_ctx0", {"f.txt": nl(base)}, uni(base, ["N0"] + base, ctx=0), 1)
C("insert_at_end_ctx0", {"f.txt": nl(base)}, uni(base, base + ["NEND"], ctx=0), 1)
C("insert_at_start_ctx3", {"f.txt": nl(base)}, uni(base, ["N0"] + base, ctx=3), 1)
C("insert_at_end_ctx3", {"f.txt": nl(base)}, uni(base, base + ["NEND"], ctx=3), 1)
C("delete_first_line", {"f.txt": nl(base)}, uni(base, base[1:], ctx=3), 1)
C("delete_last_line", {"f.txt": nl(base)}, uni(base, base[:-1], ctx=3), 1)
C("delete_all_lines", {"f.txt": nl(base)}, uni(base, [], ctx=3), 1)
C("delete_all_lines_nofile_after", {"f.txt": nl(["x"])}, "--- a/f.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n", 1)
# hand-written zero-length range headers
C("hand_insert_after_5", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -5,0 +6,2 @@\n+N1\n+N2\n", 1)
C("hand_insert_before_1", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -0,0 +1,2 @@\n+N1\n+N2\n", 1)
C("hand_delete_range", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -6,2 +5,0 @@\n-line 6\n-line 7\n", 1)
C("hand_hdr_no_counts", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -3 +3 @@\n-line 3\n+THREE\n", 1)
C("hand_hdr_wrong_line", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -9 +9 @@\n-line 3\n+THREE\n", 1)
C("hand_hdr_wrong_line_far", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -19 +19 @@\n-line 3\n+THREE\n", 1)
C("hand_hdr_beyond_eof", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -50 +50 @@\n-line 3\n+THREE\n", 1)
C("hand_hdr_count_too_big", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -3,5 +3,5 @@\n-line 3\n+THREE\n", 1)
C("hand_hdr_count_too_small", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -3,1 +3,1 @@\n-line 3\n+THREE\n-line 4\n+FOUR\n", 1)
C("hand_extra_text_after_hdr", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -3 +3 @@ some function()\n-line 3\n+THREE\n", 1)
C("hand_blank_context_line", {"f.txt": nl(["a", "", "b"])}, "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n a\n\n-b\n+B\n", 1)
C("hand_context_past_eof", {"f.txt": nl(["a", "b"])}, "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n a\n b\n c\n", 1)
C("hand_context_past_eof2", {"f.txt": nl(["a", "b"])}, "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n b\n c\n d\n", 1)
C("hand_context_past_eof_del", {"f.txt": nl(["a", "b"])}, "--- a/f.txt\n+++ b/f.txt\n@@ -2,2 +2,1 @@\n b\n-c\n", 1)
C("hand_lead_del_fuzz", {"f.txt": nl(["a", "x", "c", "d"])}, "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n-a\n+A\n b\n c\n d\n", 1)
C("hand_lead_add_fuzz", {"f.txt": nl(["a", "x", "c", "d"])}, "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,4 @@\n+Z\n a\n b\n c\n", 1)
C("hand_trailing_del_fuzz", {"f.txt": nl(["a", "b", "x", "d"])}, "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n b\n c\n-d\n+D\n", 1)
C("hand_only_adds_ctx0_mid", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -10,0 +11,1 @@\n+X\n", 1)
C("hand_only_dels_ctx0_mid", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -10,1 +9,0 @@\n-line 10\n", 1)
C("hand_only_dels_ctx0_wrong", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -10,1 +9,0 @@\n-line 11\n", 1)
# --- missing newline at EOF -------------------------------------------------------------
C("eof_nonl_file_modify_mid", {"f.txt": nl(base, False)}, p, 1)
C("eof_nonl_file_modify_last", {"f.txt": nl(base, False)}, uni(base, base[:-1] + ["LAST"], ctx=3), 1)
C("eof_nonl_patch_marker_old", {"f.txt": nl(base, False)}, "--- a/f.txt\n+++ b/f.txt\n@@ -19,2 +19,2 @@\n line 19\n-line 20\n\\ No newline at end of file\n+LAST\n", 1)
C("eof_nonl_patch_marker_new", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -19,2 +19,2 @@\n line 19\n-line 20\n+LAST\n\\ No newline at end of file\n", 1)
C("eof_nonl_patch_marker_both", {"f.txt": nl(base, False)}, "--- a/f.txt\n+++ b/f.txt\n@@ -19,2 +19,2 @@\n line 19\n-line 20\n\\ No newline at end of file\n+LAST\n\\ No newline at end of file\n", 1)
C("eof_nl_added_by_patch", {"f.txt": nl(base, False)}, "--- a/f.txt\n+++ b/f.txt\n@@ -20 +20 @@\n-line 20\n\\ No newline at end of file\n+line 20\n", 1)
C("eof_patch_file_nonl", {"f.txt": nl(base)}, p.rstrip("\n"), 1)
C("eof_patch_file_nonl_after_marker", {"f.txt": nl(base)}, "--- a/f.txt\n+++ b/f.txt\n@@ -20 +20 @@\n-line 20\n+LAST\n\\ No newline at end of file", 1)
C("eof_empty_file_add_line", {"f.txt": ""}, "--- a/f.txt\n+++ b/f.txt\n@@ -0,0 +1 @@\n+hello\n", 1)
C("eof_empty_file_add_line_nonl", {"f.txt": ""}, "--- a/f.txt\n+++ b/f.txt\n@@ -0,0 +1 @@\n+hello\n\\ No newline at end of file\n", 1)
C("eof_blank_lines_file", {"f.txt": "a\n\n\nb\n\n"}, "--- a/f.txt\n+++ b/f.txt\n@@ -3,2 +3,2 @@\n \n-b\n+B\n", 1)
# --- CRLF --------------------------------------------------------------------------------
crlf = lambda lines: "".join(l + "\r\n" for l in lines)
C("crlf_file_lf_patch", {"f.txt": crlf(base)}, p, 1)
C("crlf_file_crlf_patch", {"f.txt": crlf(base)}, p.replace("\n", "\r\n"), 1)
C("lf_file_crlf_patch", {"f.txt": nl(base)}, p.replace("\n", "\r\n"), 1)
C("crlf_mixed_file", {"f.txt": "".join(l + ("\r\n" if i % 2 else "\n") for i, l in enumerate(base))}, p, 1)
C("cr_only_file", {"f.txt": "\r".join(base) + "\r"}, p, 1)
C("crlf_file_nonl", {"f.txt": "\r\n".join(base)}, p, 1)
C("crlf_patch_new_file", {}, "--- /dev/null\r\n+++ b/n.txt\r\n@@ -0,0 +1,2 @@\r\n+one\r\n+two\r\n", 1)
C("trailing_ws_ctx", {"f.txt": "a \nb\nc\n"}, "--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n", 1)
# --- bytes: latin1 / binary-ish ------------------------------------------------------------
C("latin1_content", {"f.txt": b"caf\xe9\nline\nx\n"}, "--- a/f.txt\n+++ b/f.txt\n@@ -2 +2 @@\n-line\n+LINE\n", 1)
C("utf8_content", {"f.txt": "héllo\nwörld\n"}, "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n héllo\n-wörld\n+WORLD\n", 1)
C("nul_bytes_content", {"f.txt": b"a\0b\nc\n"}, "--- a/f.txt\n+++ b/f.txt\n@@ -2 +2 @@\n-c\n+C\n", 1)
C("git_binary_patch", {"b.bin": b"\x00\x01\x02"}, "diff --git a/b.bin b/b.bin\nindex 123..456 100644\nGIT binary patch\nliteral 3\nKcmZQzW@Kd`\n\nliteral 3\nKcmZQzW@Kd`\n", 1)
C("binary_files_differ", {"b.bin": b"\x00\x01\x02"}, "diff --git a/b.bin b/b.bin\nindex 123..456 100644\nBinary files a/b.bin and b/b.bin differ\n", 1)
C("binary_files_differ_plain", {"b.bin": b"\x00\x01\x02"}, "--- a/b.bin\n+++ b/b.bin\nBinary files a/b.bin and b/b.bin differ\n", 1)
# --- strip ---------------------------------------------------------------------------------
for s in (0, 1, 2, 3):
    pre = "/".join(["x"] * s)
    path = (pre + "/" if pre else "") + "f.txt"
    C(f"strip{s}", {"f.txt": nl(base)}, uni(base, new, a=path, hdr=True), s)
C("strip_too_many", {"f.txt": nl(base)}, uni(base, new, a="a/f.txt", hdr=True), 5)
C("strip_but_patch_has_ab", {"f.txt": nl(base)}, uni(base, new), 0)
C("strip_dir", {"d/e/f.txt": nl(base)}, uni(base, new, a="d/e/f.txt"), 1)
C("strip_tab_timestamp", {"f.txt": nl(base)}, "--- a/f.txt\t2020-01-01 00:00:00.000000000 +0000\n+++ b/f.txt\t2020-01-01 00:00:00.000000000 +0000\n@@ -3 +3 @@\n-line 3\n+THREE\n", 1)
C("strip_space_in_name", {"a b.txt": nl(base)}, "--- a/a b.txt\n+++ b/a b.txt\n@@ -3 +3 @@\n-line 3\n+THREE\n", 1)
C("strip_empty_path", {"f.txt": nl(base)}, "--- a/\n+++ b/\n@@ -3 +3 @@\n-line 3\n+THREE\n", 1)
C("strip_short_hdr", {"f.txt": nl(base)}, "--- \n+++ \n@@ -3 +3 @@\n-line 3\n+THREE\n", 1)
# --- new/delete files, renames, modes -----------------------------------------------------
C("new_file", {}, "--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,3 @@\n+a\n+b\n+c\n", 1)
C("new_file_in_new_dir", {}, "--- /dev/null\n+++ b/x/y/n.txt\n@@ -0,0 +1,1 @@\n+a\n", 1)
C("new_file_exists", {"n.txt": "old\n"}, "--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,1 @@\n+a\n", 1)
C("new_file_exists_diff_ctx", {"n.txt": "old\nold2\n"}, "--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n", 1)
C("delete_file", {"d.txt": nl(["a", "b"])}, "--- a/d.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n", 1)
C("delete_file_mismatch", {"d.txt": nl(["a", "c"])}, "--- a/d.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n", 1)
C("delete_file_missing", {}, "--- a/d.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n", 1)
C("modify_file_missing", {}, "--- a/d.txt\n+++ b/d.txt\n@@ -1 +1 @@\n-a\n+b\n", 1)
C("both_dev_null", {}, "--- /dev/null\n+++ /dev/null\n@@ -0,0 +1 @@\n+a\n", 1)
C("old_missing_new_exists", {"n.txt": "a\n"}, "--- a/o.txt\n+++ b/n.txt\n@@ -1 +1 @@\n-a\n+b\n", 1)
C("old_ne_new_both_exist", {"o.txt": "a\n", "n.txt": "a\n"}, "--- a/o.txt\n+++ b/n.txt\n@@ -1 +1 @@\n-a\n+b\n", 1)
G = lambda a, b, extra, body: f"diff --git a/{a} b/{b}\n{extra}{body}"
C("git_rename_only", {"old.txt": nl(base)}, G("old.txt", "new.txt", "similarity index 100%\nrename from old.txt\nrename to new.txt\n", ""), 1)
C("git_rename_with_edit", {"old.txt": nl(base)}, G("old.txt", "new.txt", "similarity index 90%\nrename from old.txt\nrename to new.txt\n", uni(base, new, "old.txt", "new.txt").replace("--- a/old.txt\n+++ b/new.txt", "--- a/old.txt\n+++ b/new.txt")), 1)
C("git_rename_target_exists", {"old.txt": "a\n", "new.txt": "b\n"}, G("old.txt", "new.txt", "rename from old.txt\nrename to new.txt\n", ""), 1)
C("git_rename_source_missing", {}, G("old.txt", "new.txt", "rename from old.txt\nrename to new.txt\n", ""), 1)
C("git_rename_into_dir", {"old.txt": "a\n"}, G("old.txt", "sub/dir/new.txt", "rename from old.txt\nrename to sub/dir/new.txt\n", ""), 1)
C("git_rename_from_only", {"old.txt": "a\n"}, G("old.txt", "new.txt", "rename from old.txt\n", ""), 1)
C("git_rename_to_only", {"old.txt": "a\n"}, G("old.txt", "new.txt", "rename to new.txt\n", ""), 1)
C("git_rename_swap_names", {"a.txt": "A\n", "b.txt": "B\n"}, G("a.txt", "b.txt", "rename from a.txt\nrename to b.txt\n", ""), 1)
C("git_rename_strip0", {"old.txt": "a\n"}, "diff --git old.txt new.txt\nrename from old.txt\nrename to new.txt\n", 0)
C("git_copy_not_supported", {"old.txt": "a\n"}, G("old.txt", "new.txt", "copy from old.txt\ncopy to new.txt\n", ""), 1)
C("git_mode_change_exec", {"f.txt": "a\n"}, G("f.txt", "f.txt", "old mode 100644\nnew mode 100755\n", ""), 1)
C("git_mode_change_noexec", {"f.txt": "a\n"}, G("f.txt", "f.txt", "old mode 100755\nnew mode 100644\n", ""), 1, modes={"f.txt": 0o755})
C("git_mode_change_with_edit", {"f.txt": nl(base)}, G("f.txt", "f.txt", "old mode 100644\nnew mode 100755\nindex 1..2\n", uni(base, new)), 1)
C("git_mode_ro", {"f.txt": "a\n"}, G("f.txt", "f.txt", "old mode 100644\nnew mode 100444\n", ""), 1)
C("git_mode_rw_only", {"f.txt": "a\n"}, G("f.txt", "f.txt", "old mode 100644\nnew mode 100600\n", ""), 1)
C("git_mode_group_bits", {"f.txt": "a\n"}, G("f.txt", "f.txt", "old mode 100644\nnew mode 100070\n", ""), 1)
C("git_mode_truncated", {"f.txt": "a\n"}, G("f.txt", "f.txt", "new mode 10\n", ""), 1)
C("git_mode_bad_char", {"f.txt": "a\n"}, G("f.txt", "f.txt", "new mode 10x755\n", ""), 1)
C("git_mode_digit8", {"f.txt": "a\n"}, G("f.txt", "f.txt", "new mode 100855\n", ""), 1)
C("git_new_file_mode", {}, G("n.txt", "n.txt", "new file mode 100755\nindex 0000000..e69de29\n", "--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n"), 1)
C("git_new_file_mode_644", {}, G("n.txt", "n.txt", "new file mode 100644\nindex 0000000..e69de29\n", "--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+a\n"), 1)
C("git_new_empty_file", {}, G("n.txt", "n.txt", "new file mode 100644\nindex 0000000..e69de29\n", ""), 1)
C("git_delete_file", {"d.txt": "a\n"}, G("d.txt", "d.txt", "deleted file mode 100644\nindex e69de29..0000000\n", "--- a/d.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-a\n"), 1)
C("git_delete_file_no_hunks", {"d.txt": "a\n"}, G("d.txt", "d.txt", "deleted file mode 100644\nindex e69de29..0000000\n", ""), 1)
C("git_empty_diff_header_only", {"f.txt": "a\n"}, G("f.txt", "f.txt", "index 1..2 100644\n", ""), 1)
C("git_two_files", {"a.txt": nl(base), "b.txt": nl(base)}, G("a.txt", "a.txt", "index 1..2 100644\n", uni(base, new, "a.txt")) + G("b.txt", "b.txt", "index 1..2 100644\n", uni(base, new2, "b.txt")), 1)
C("git_second_file_fails", {"a.txt": nl(base), "b.txt": nl(["z"])}, G("a.txt", "a.txt", "index 1..2 100644\n", uni(base, new, "a.txt")) + G("b.txt", "b.txt", "index 1..2 100644\n", uni(base, new2, "b.txt")), 1)
C("git_header_junk_lines", {"f.txt": nl(base)}, "From abc Mon Sep 17 00:00:00 2001\nSubject: [PATCH] x\n\n---\n f.txt | 2 +-\n\n" + G("f.txt", "f.txt", "index 1..2 100644\n", uni(base, new)) + "-- \n2.30.0\n", 1)
C("git_trailing_signature", {"f.txt": nl(base)}, G("f.txt", "f.txt", "index 1..2 100644\n", uni(base, new)) + "-- \n2.39.2\n\n", 1)
C("git_no_a_b_prefix_strip0", {"f.txt": nl(base)}, "diff --git f.txt f.txt\n" + uni(base, new, hdr=True).replace("--- f.txt", "--- f.txt"), 0)
C("git_diff_line_short", {"f.txt": nl(base)}, "diff --git\n" + uni(base, new), 1)
C("git_diff_line_many_args", {"f.txt": nl(base)}, "diff --git a/f.txt b/f.txt extra\n" + uni(base, new), 1)
C("git_hdr_rename_without_names_in_diff_line", {"o.txt": "a\n"}, "diff --git\nrename from o.txt\nrename to n.txt\n", 1)
# --- multiple files without git headers ----------------------------------------------------
C("two_files_plain", {"a.txt": nl(base), "b.txt": nl(base)}, uni(base, new, "a.txt") + uni(base, new2, "b.txt"), 1)
C("two_files_plain_second_new", {"a.txt": nl(base)}, uni(base, new, "a.txt") + "--- /dev/null\n+++ b/b.txt\n@@ -0,0 +1 @@\n+hi\n", 1)
C("same_file_twice", {"a.txt": nl(base)}, uni(base, new, "a.txt") + uni(new, new2, "a.txt"), 1)
C("same_file_twice_independent", {"a.txt": nl(base)}, uni(base, new, "a.txt") + uni(base, new2, "a.txt"), 1)
C("junk_between_files", {"a.txt": nl(base), "b.txt": nl(base)}, uni(base, new, "a.txt") + "Index: b.txt\n====\n" + uni(base, new2, "b.txt"), 1)
C("junk_after_chunk", {"a.txt": nl(base)}, uni(base, new, "a.txt") + "random trailing text\n", 1)
C("junk_before", {"a.txt": nl(base)}, "preamble text\nmore\n" + uni(base, new, "a.txt"), 1)
C("empty_patch", {"a.txt": nl(base)}, "", 1)
C("only_prelude", {"a.txt": nl(base)}, "--- a/a.txt\n+++ b/a.txt\n", 1)
C("only_old_line", {"a.txt": nl(base)}, "--- a/a.txt\n", 1)
C("missing_plus_line", {"a.txt": nl(base)}, "--- a/a.txt\n@@ -1 +1 @@\n-a\n+b\n", 1)
C("hunk_without_prelude", {"a.txt": nl(base)}, "@@ -1 +1 @@\n-line 1\n+b\n", 1)
C("truncated_hunk", {"a.txt": nl(base)}, "--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n line 1\n-line 2\n+X\n", 1)
C("hunk_too_many_lines", {"a.txt": nl(base)}, "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-line 1\n+X\n+Y\n", 1)
C("hunk_header_garbage", {"a.txt": nl(base)}, "--- a/a.txt\n+++ b/a.txt\n@@ -x +y @@\n-line 1\n+X\n", 1)
C("hunk_header_no_trailing_at", {"a.txt": nl(base)}, "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1\n-line 1\n+X\n", 1)
C("hunk_header_extra_ws", {"a.txt": nl(base)}, "--- a/a.txt\n+++ b/a.txt\n@@   -1   +1   @@\n-line 1\n+X\n", 1)
C("hunk_header_big_num", {"a.txt": nl(base)}, "--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-line 1\n+X\n@@ -5 +5 @@\n-line 5\n+Y\n", 1)
C("hunk_two_adjacent", {"a.txt": nl(base)}, "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n-line 1\n+A\n line 2\n@@ -3,2 +3,2 @@\n line 3\n-line 4\n+D\n", 1)
C("hunk_unsorted", {"a.txt": nl(base)}, "--- a/a.txt\n+++ b/a.txt\n@@ -10 +10 @@\n-line 10\n+TEN\n@@ -2 +2 @@\n-line 2\n+TWO\n", 1)
C("hunk_overlapping", {"a.txt": nl(base)}, "--- a/a.txt\n+++ b/a.txt\n@@ -2,3 +2,3 @@\n line 2\n-line 3\n+THREE\n line 4\n@@ -3,3 +3,3 @@\n THREE\n-line 4\n+FOUR\n line 5\n", 1)
C("hunk_same_line_twice", {"a.txt": nl(base)}, "--- a/a.txt\n+++ b/a.txt\n@@ -2 +2 @@\n-line 2\n+X\n@@ -2 +2 @@\n-line 2\n+Y\n", 1)
C("hunk_already_applied", {"a.txt": nl(new)}, p, 1)
C("hunk_already_applied_offset", {"a.txt": nl(["pre"] * 2 + new)}, uni(base, new, "a.txt"), 1)
C("add_line_matches_existing", {"a.txt": nl(base[:9] + ["TEN"] + base[10:])}, uni(base, new, "a.txt"), 1)
C("repeated_lines_ambiguous", {"a.txt": nl(["x", "y"] * 10)}, "--- a/a.txt\n+++ b/a.txt\n@@ -9,2 +9,2 @@\n x\n-y\n+Z\n", 1)
C("repeated_lines_offset_nearest", {"a.txt": nl(["x", "y"] * 10)}, "--- a/a.txt\n+++ b/a.txt\n@@ -2,2 +2,2 @@\n x\n-y\n+Z\n", 1)
C("repeated_lines_search_both_dirs", {"a.txt": nl(["m"] + ["x", "y"] * 5)}, "--- a/a.txt\n+++ b/a.txt\n@@ -6,2 +6,2 @@\n-x\n+X\n y\n", 1)
C("large_file_far_offset", {"a.txt": nl(["pad %d" % i for i in range(300)] + base)}, uni(base, new, "a.txt"), 1)
C("hunk_replace_all", {"a.txt": nl(["a", "b"])}, "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n-a\n-b\n+c\n+d\n", 1)
C("hunk_empty_file_delete_ctx", {"a.txt": ""}, "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-a\n+b\n", 1)
C("lines_starting_with_dashes", {"a.txt": nl(["--- x", "+++ y", "@@ z", "a"])}, "--- a/a.txt\n+++ b/a.txt\n@@ -1,4 +1,4 @@\n--- x\n+++ y\n @@ z\n-a\n+b\n", 1)
C("content_line_looks_like_hdr", {"a.txt": nl(["a", "b"])}, "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n a\n-b\n+@@ -1 +1 @@\n", 1)
C("content_line_starts_backslash", {"a.txt": nl(["a", "b"])}, "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n a\n-b\n\\ whatever\n+B\n", 1)
C("context_line_no_space_prefix", {"a.txt": nl(["a", "b", "c"])}, "--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\na\n-b\n+B\n c\n", 1)
# --- paths, symlinks, escapes ----------------------------------------------------------------
C("escape_dotdot", {"f.txt": "a\n"}, "--- a/../x.txt\n+++ b/../x.txt\n@@ -0,0 +1 @@\n+a\n", 1)
C("escape_absolute", {"f.txt": "a\n"}, "--- /tmp/zel_patch_escape_x\n+++ /tmp/zel_patch_escape_x\n@@ -0,0 +1 @@\n+a\n", 0)
C("escape_dotdot_inside", {"d/f.txt": "a\n"}, "--- a/d/../f2.txt\n+++ b/d/../f2.txt\n@@ -0,0 +1 @@\n+a\n", 1)
C("path_dot_components", {"d/f.txt": nl(base)}, uni(base, new, "./d/./f.txt"), 1)
C("path_double_slash", {"d/f.txt": nl(base)}, uni(base, new, "a//d//f.txt", hdr=True), 0)
C("symlink_file_inside", {"real.txt": nl(base)}, uni(base, new, "link.txt"), 1, links={"link.txt": "real.txt"})
C("symlink_dir_escape", {"f.txt": "a\n"}, "--- a/lnk/x.txt\n+++ b/lnk/x.txt\n@@ -0,0 +1 @@\n+a\n", 1, links={"lnk": "/tmp"})
C("symlink_dir_inside", {"d/f.txt": nl(base)}, uni(base, new, "lnk/f.txt"), 1, links={"lnk": "d"})
C("symlink_file_escape", {"f.txt": "a\n"}, uni(["x"], ["y"], "lnk.txt"), 1, links={"lnk.txt": "/etc/hosts"})
C("symlink_dangling", {"f.txt": "a\n"}, "--- a/dang.txt\n+++ b/dang.txt\n@@ -0,0 +1 @@\n+a\n", 1, links={"dang.txt": "nowhere"})
C("rename_symlink_source", {"real.txt": "a\n"}, G("link.txt", "new.txt", "rename from link.txt\nrename to new.txt\n", ""), 1, links={"link.txt": "real.txt"})
C("target_is_directory", {"d/f.txt": "a\n"}, "--- a/d\n+++ b/d\n@@ -1 +1 @@\n-a\n+b\n", 1)
C("new_file_target_is_directory", {"d/f.txt": "a\n"}, "--- /dev/null\n+++ b/d\n@@ -0,0 +1 @@\n+b\n", 1)
# --- permissions preserved -------------------------------------------------------------------
C("perm_preserve_exec", {"f.txt": nl(base)}, p, 1, modes={"f.txt": 0o755})
C("perm_preserve_ro", {"f.txt": nl(base)}, p, 1, modes={"f.txt": 0o444})
C("perm_preserve_600", {"f.txt": nl(base)}, p, 1, modes={"f.txt": 0o600})
C("perm_preserve_group_exec", {"f.txt": nl(base)}, p, 1, modes={"f.txt": 0o654})
C("perm_new_file_default", {}, "--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+a\n", 1)
C("perm_rename_preserves", {"o.txt": nl(base)}, G("o.txt", "n.txt", "rename from o.txt\nrename to n.txt\n", ""), 1, modes={"o.txt": 0o755})
C("perm_ro_file_modify_mode_change", {"f.txt": nl(base)}, G("f.txt", "f.txt", "new mode 100755\n", uni(base, new)), 1, modes={"f.txt": 0o444})
C("perm_old_mode_then_edit_exec", {"f.txt": nl(base)}, G("f.txt", "f.txt", "old mode 100755\nnew mode 100755\n", uni(base, new)), 1, modes={"f.txt": 0o644})


# --- seeded random differential cases (offset / fuzz / context perturbations) ------------------------------
import random as _random
_rng = _random.Random(20240607)
_VOCAB = ["alpha", "beta", "gamma", "delta", "eps", "x", "y", "", "}", "{", "return 0;", "  indent", "\tTab"]

def _rand_lines(n):
    return [_rng.choice(_VOCAB) + (" %d" % _rng.randrange(4) if _rng.random() < 0.6 else "") for _ in range(n)]

def _mutate(lines, edits):
    out = lines[:]
    for _ in range(edits):
        k = _rng.choice(["ins", "del", "rep"])
        if not out: k = "ins"
        i = _rng.randrange(len(out) + 1 if k == "ins" else len(out))
        if k == "ins": out[i:i] = _rand_lines(_rng.randrange(1, 3))
        elif k == "del": del out[i:i + _rng.randrange(1, 3)]
        else: out[i] = "NEW " + out[i]
    return out

for _i in range(180):
    _n = _rng.randrange(3, 40)
    _old = _rand_lines(_n)
    _new = _mutate(_old, _rng.randrange(1, 4))
    _ctx = _rng.choice([0, 1, 2, 3, 3, 3])
    _patch = uni(_old, _new, "f.txt", ctx=_ctx)
    _target = _old[:]
    _mode = _rng.choice(["same", "shift", "perturb", "perturb_shift", "unrelated", "dup"])
    if _mode in ("shift", "perturb_shift"):
        _k = _rng.randrange(1, 6)
        _target = _rand_lines(_k) + _target if _rng.random() < 0.6 else _target[_k:]
    if _mode in ("perturb", "perturb_shift"):
        for _ in range(_rng.randrange(1, 4)):
            if _target:
                _j = _rng.randrange(len(_target)); _target[_j] = "PERTURBED" + (str(_j) if _rng.random() < 0.5 else "")
    if _mode == "unrelated": _target = _rand_lines(_rng.randrange(0, 30))
    if _mode == "dup": _target = _target + _target
    _final_nl = _rng.random() < 0.85
    if _rng.random() < 0.1: _patch = _patch.replace("\n", "\r\n")
    C(f"rnd_{_i:03d}_{_mode}_c{_ctx}", {"f.txt": nl(_target, _final_nl)}, _patch, 1)

for _i in range(40):
    # two-file / multi-hunk random patches
    _files = {}; _patch = ""
    for _fn in ("a.txt", "b.txt"):
        _o = _rand_lines(_rng.randrange(5, 30)); _nw = _mutate(_o, _rng.randrange(1, 5))
        _patch += uni(_o, _nw, _fn, ctx=_rng.choice([1, 2, 3]))
        _t = _o[:]
        if _rng.random() < 0.5: _t = _mutate(_t, 1)
        _files[_fn] = nl(_t)
    _git = _rng.random() < 0.5
    if _git:
        _patch = "".join(f"diff --git a/{p.splitlines()[0][6:]} b/{p.splitlines()[0][6:]}\nindex 1..2 100644\n" + p for p in ("--- a/a.txt" + _patch.split("--- a/a.txt", 1)[1]).split("--- a/b.txt")[:1]) if False else _patch
    C(f"rnd2_{_i:03d}", _files, _patch, 1)

def gen(out):
    shutil.rmtree(out, ignore_errors=True)
    for name, c in cases.items():
        d = os.path.join(out, name)
        os.makedirs(os.path.join(d, "before"))
        for f, content in c["files"].items():
            fp = os.path.join(d, "before", f)
            os.makedirs(os.path.dirname(fp), exist_ok=True)
            open(fp, "wb").write(content if isinstance(content, bytes) else content.encode("utf-8"))
        for f, m in c["modes"].items(): os.chmod(os.path.join(d, "before", f), m)
        for f, tgt in c["links"].items():
            fp = os.path.join(d, "before", f); os.makedirs(os.path.dirname(fp), exist_ok=True); os.symlink(tgt, fp)
        pt = c["patch"]
        open(os.path.join(d, "patch"), "wb").write(pt if isinstance(pt, bytes) else pt.encode("utf-8"))
        open(os.path.join(d, "strip"), "w").write(str(c["strip"]))
    print(len(cases), "cases")

def snapshot(root):
    out = []
    for dp, dn, fn in os.walk(root):
        for x in sorted(dn + fn):
            p = os.path.join(dp, x); r = os.path.relpath(p, root)
            if os.path.islink(p): out.append(f"{r} L {os.readlink(p)}")
            elif os.path.isdir(p): out.append(f"{r} D")
            else:
                mode = os.stat(p).st_mode & 0o700
                os.chmod(p, 0o600)
                out.append(f"{r} F {oct(mode)} {open(p,'rb').read().hex()}")
    return "\n".join(sorted(out)) + "\n"

JAVA = ["java", "--add-opens=java.base/java.lang=ALL-UNNAMED", "--add-opens=java.base/java.util=ALL-UNNAMED",
        "--add-opens=java.base/java.nio=ALL-UNNAMED", "--add-opens=java.base/sun.nio.fs=ALL-UNNAMED"]

def oracle(out, jar, head_src=None):
    """Records answers of HEAD's PatchUtil (`head_src`, compiled against the 9.3.x jar; the spec) in
    `result`/`expected`, and of the jar's own 9.3.x PatchUtil in `result932`/`expected932`."""
    out = os.path.abspath(out)
    src = os.path.dirname(os.path.abspath(__file__))
    work = tempfile.mkdtemp(prefix="patchoracle")
    subprocess.check_call(["javac", "-cp", jar, "-d", work, os.path.join(src, "PatchOracle.java")])
    headdir = os.path.join(work, "head")
    variants = [("", jar + ":" + work)]
    if head_src:
        os.makedirs(headdir)
        subprocess.check_call(["javac", "-cp", jar, "-d", headdir, head_src])
        variants = [("", headdir + ":" + jar + ":" + work), ("932", jar + ":" + work)]
    names = sorted(os.listdir(out))
    for suffix, cp in variants:
        listing = []
        for name in names:
            d = os.path.join(out, name)
            target = os.path.join(work, name + suffix)
            shutil.rmtree(target, ignore_errors=True)
            shutil.copytree(os.path.join(d, "before"), target, symlinks=True)
            listing.append("\t".join([os.path.join(d, "patch"), open(os.path.join(d, "strip")).read().strip(), os.path.realpath(target)]))
        lf = os.path.join(work, "list" + suffix); rf = os.path.join(work, "res" + suffix)
        open(lf, "w").write("\n".join(listing) + "\n")
        subprocess.check_call(JAVA + ["-cp", cp, "PatchOracle", "--batch", lf, rf])
        results = open(rf).read().split("\n")
        for name, res in zip(names, results):
            d = os.path.join(out, name)
            target = os.path.join(work, name + suffix)
            res = res.replace(os.path.realpath(target), "$OUT")
            open(os.path.join(d, "result" + suffix), "w").write(res + "\n")
            open(os.path.join(d, "expected" + suffix), "w").write(snapshot(target))
        print(suffix or "head", len(names), "cases recorded", flush=True)

if __name__ == "__main__":
    if sys.argv[1] == "gen": gen(sys.argv[2])
    elif sys.argv[1] == "oracle": oracle(sys.argv[2], sys.argv[3], sys.argv[4] if len(sys.argv) > 4 else None)
