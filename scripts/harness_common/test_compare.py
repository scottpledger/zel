import json, os, sys, tempfile, unittest
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import compare as C

def R(out="", err="", rc=0): return (rc, out, err)
def runs(items, rc=1):
    return [{"rc": rc, "out": o, "err": e} for o, e in items]
def ev(mode, rs, cmd="build //..."):
    return {"cmd": cmd, "mode": mode, "runs": rs}

HF = "INFO: Elapsed time: Ns, Critical Path: T\nINFO: 5 processes: 5 internal.\nINFO: Build completed successfully, 5 total actions\n"

class Strict(unittest.TestCase):
    def test_same(self): self.assertTrue(C.compare(C.STRICT, R("a\nb"), R("a\nb"))[0])
    def test_order_fails(self): self.assertFalse(C.compare(C.STRICT, R("a\nb"), R("b\na"))[0])
    def test_rc(self): self.assertFalse(C.compare(C.STRICT, R("a", rc=0), R("a", rc=1))[0])
    def test_streams_separate(self): self.assertFalse(C.compare(C.STRICT, R("a", "b"), R("b", "a"))[0])

class Orderless(unittest.TestCase):
    def cmp(self, r, z): return C.compare(C.ORDERLESS, r, z)[0]
    def test_reorder_ok(self): self.assertTrue(self.cmp(R("x\ny\nz\n" + HF), R("z\nx\ny\n" + HF)))
    def test_missing_fails(self): self.assertFalse(self.cmp(R("x\ny\nz"), R("x\ny")))
    def test_extra_fails(self): self.assertFalse(self.cmp(R("x\ny"), R("x\ny\nz")))
    def test_duplicate_fails(self): self.assertFalse(self.cmp(R("x\ny"), R("x\ny\ny")))
    def test_dup_removed_fails(self): self.assertFalse(self.cmp(R("x\nx\ny"), R("x\ny\ny")))
    def test_header_strict(self):
        z = HF.replace("5 processes", "6 processes")
        self.assertFalse(self.cmp(R("x\n" + HF), R("x\n" + z)))
    def test_header_order_strict(self):
        a = "INFO: Elapsed time: Ns\nINFO: 5 processes: 5 internal.\n"; b = "INFO: 5 processes: 5 internal.\nINFO: Elapsed time: Ns\n"
        self.assertFalse(self.cmp(R(a), R(b)))
    def test_body_cannot_hide_in_header(self): self.assertFalse(self.cmp(R("x\n" + HF), R("x\n" + HF + "INFO: Found 2 targets\n")))
    def test_info_from_reorder_ok(self): self.assertTrue(self.cmp(R("INFO: From a\nINFO: From b\n" + HF), R("INFO: From b\nINFO: From a\n" + HF)))
    def test_rc_still_checked(self): self.assertFalse(C.compare(C.ORDERLESS, R("a", rc=0), R("a", rc=1))[0])
    def test_streams_not_merged(self): self.assertFalse(self.cmp(R("a", "b"), R("b", "a")))
    def test_empty_ok(self): self.assertTrue(self.cmp(R(""), R("")))
    def test_ansi_blank_lines_ignored(self): self.assertTrue(self.cmp(R("a\n\nb\n"), R("b\na")))

BLK1 = "ERROR: /ws/BUILD:1:1: Executing genrule //:a failed: x\nsub line 1"
BLK2 = "ERROR: /ws/BUILD:2:1: Executing genrule //:b failed: y"
BLK3 = "ERROR: /ws/BUILD:3:1: Executing genrule //:c failed: z"
FOOT = "ERROR: Build did NOT complete successfully\n"

class AnyErr(unittest.TestCase):
    def setUp(self):
        self.r = [(BLK1 + "\n" + FOOT), (BLK2 + "\n" + FOOT), (BLK1 + "\n" + BLK2 + "\n" + FOOT)]
        self.real = R("", self.r[0], 1)
        self.extra = [(1, "", t) for t in self.r]
    def c(self, z, rc=1): return C.compare(C.ANYERR, self.real, R("", z, rc), self.extra)
    def test_other_winner_ok(self): self.assertTrue(self.c(BLK2 + "\n" + FOOT)[0])
    def test_both_ok(self): self.assertTrue(self.c(BLK2 + "\n" + BLK1 + "\n" + FOOT)[0])
    def test_unseen_block_fails(self): self.assertFalse(self.c(BLK3 + "\n" + FOOT)[0])
    def test_altered_block_fails(self): self.assertFalse(self.c(BLK1.replace("sub line 1", "sub line 2") + "\n" + FOOT)[0])
    def test_too_many_blocks_fails(self): self.assertFalse(self.c("\n".join([BLK1, BLK2, BLK1]) + "\n" + FOOT)[0])
    def test_zero_blocks_fails(self): self.assertFalse(self.c(FOOT)[0])
    def test_footer_must_match(self): self.assertFalse(self.c(BLK1 + "\nERROR: Build did NOT complete successfully!\n")[0])
    def test_rc_must_be_real(self): self.assertFalse(self.c(BLK1 + "\n" + FOOT, rc=3)[0])
    def test_extra_body_line_fails(self): self.assertFalse(self.c(BLK1 + "\nrandom line\n" + FOOT)[0] and False or self.c("random line\n" + BLK1 + "\n" + FOOT)[0])

SUG = "ERROR: --bogus :: Unrecognized option: --bogus"
class AnySuggest(unittest.TestCase):
    def setUp(self):
        self.real = R("", SUG + "\nDid you mean '--bogus_a'?", 2)
        self.extra = [(2, "", SUG + "\nDid you mean '--bogus_a'?"), (2, "", SUG + "\nDid you mean '--bogus_b'?")]
    def c(self, z, rc=2): return C.compare(C.ANYSUGGEST, self.real, R("", z, rc), self.extra)[0]
    def test_either_ok(self): self.assertTrue(self.c(SUG + "\nDid you mean '--bogus_b'?"))
    def test_unseen_fails(self): self.assertFalse(self.c(SUG + "\nDid you mean '--bogus_c'?"))
    def test_rest_strict(self): self.assertFalse(self.c(SUG + "x\nDid you mean '--bogus_b'?"))
    def test_missing_suggest_fails(self): self.assertFalse(self.c(SUG))

class SkipCount(unittest.TestCase):
    def setUp(self):
        self.real = R("", "INFO: 3 targets skipped due to a failure\nok", 1)
        self.extra = [(1, "", "INFO: 3 targets skipped due to a failure\nok"), (1, "", "INFO: 5 targets skipped due to a failure\nok")]
    def c(self, z): return C.compare(C.SKIPCOUNT, self.real, R("", z, 1), self.extra)[0]
    def test_seen(self): self.assertTrue(self.c("INFO: 5 targets skipped due to a failure\nok"))
    def test_unseen(self): self.assertFalse(self.c("INFO: 4 targets skipped due to a failure\nok"))
    def test_rest_strict(self): self.assertFalse(self.c("INFO: 5 targets skipped due to a failure\nno"))

class Evidence(unittest.TestCase):
    def test_deterministic_rejected(self):
        e = ev(C.ORDERLESS, runs([("a\nb", "")] * 6))
        self.assertFalse(C.validate_evidence(e)[0]); self.assertEqual(C.effective_mode(e), C.STRICT)
    def test_too_few_runs(self):
        e = ev(C.ORDERLESS, runs([("a\nb", ""), ("b\na", "")] * 2))
        self.assertFalse(C.validate_evidence(e)[0])
    def test_orderless_ok(self):
        e = ev(C.ORDERLESS, runs([("a\nb", ""), ("b\na", "")] * 3))
        self.assertTrue(C.validate_evidence(e)[0])
    def test_orderless_but_content_varies(self):
        e = ev(C.ORDERLESS, runs([("a\nb", ""), ("b\nc", "")] * 3))
        self.assertFalse(C.validate_evidence(e)[0])
    def test_cquery_never(self):
        e = ev(C.ORDERLESS, runs([("a\nb", ""), ("b\na", "")] * 3), cmd="cquery //...")
        self.assertFalse(C.validate_evidence(e)[0])
    def test_judge_cquery_strict(self):
        e = ev(C.ORDERLESS, runs([("a\nb", ""), ("b\na", "")] * 3), cmd="cquery //...")
        self.assertEqual(C.judge(R("a\nb"), R("b\na"), e, "cquery //...")[0], "REAL")
    def test_judge_orderless(self):
        e = ev(C.ORDERLESS, runs([("a\nb", ""), ("b\na", "")] * 3))
        self.assertEqual(C.judge(R("a\nb"), R("b\na"), e, "query //...")[0], "ORDER")
        self.assertEqual(C.judge(R("a\nb"), R("b\na\na"), e, "query //...")[0], "REAL")
        self.assertEqual(C.judge(R("a\nb"), R("a\nb"), e, "query //...")[0], "SAME")
    def test_judge_no_evidence_strict(self):
        self.assertEqual(C.judge(R("a\nb"), R("b\na"), None, "query //...")[0], "REAL")
    def test_classify(self):
        self.assertEqual(C.classify_runs(runs([("a", "x")] * 6)), C.STRICT)
        self.assertEqual(C.classify_runs(runs([("a\nb", ""), ("b\na", "")] * 3)), C.ORDERLESS)
        self.assertEqual(C.classify_runs(runs([("", SUG + "\nDid you mean '--a'?"), ("", SUG + "\nDid you mean '--b'?")] * 3, 2)), C.ANYSUGGEST)
        self.assertEqual(C.classify_runs(runs([("", "INFO: 3 targets skipped\n"), ("", "INFO: 4 targets skipped\n")] * 3)), C.SKIPCOUNT)
        self.assertEqual(C.classify_runs(runs([("", BLK1 + "\n" + FOOT), ("", BLK2 + "\n" + FOOT)] * 3)), C.ANYERR)
        self.assertEqual(C.classify_runs(runs([("a", ""), ("b", "")] * 3)), "UNCLASSIFIED")
    def test_check_all(self):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(d + "/nondet")
            json.dump(ev(C.ORDERLESS, runs([("a", "")] * 6)), open(d + "/nondet/x.json", "w"))
            json.dump(ev(C.ORDERLESS, runs([("a\nb", ""), ("b\na", "")] * 3)), open(d + "/nondet/y.json", "w"))
            self.assertEqual([f for f, _ in C.check_all(d)], ["x.json"])
    def test_case_key_stable(self): self.assertEqual(C.case_key("q", "ws", "query //..."), C.case_key("q", "ws", "query //..."))

if __name__ == "__main__":
    unittest.main()
