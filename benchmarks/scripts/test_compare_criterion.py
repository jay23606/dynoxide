#!/usr/bin/env python3
"""Tests for the criterion comparison: the bencher-output parser, the
regression rule, the rendered report and the exit codes.

Run with: python3 -m unittest discover -s benchmarks/scripts -p 'test_*.py'
"""

import json
import os
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import compare_criterion
from compare_criterion import (
    COMPARED,
    MISSING,
    NEW,
    Row,
    build_baseline,
    build_report,
    evaluate,
    missing_names,
    parse_bencher,
    regressed_names,
    render_markdown,
)


# What criterion writes to stdout when nothing goes wrong. The workflows pipe
# through `tee`, which captures stdout only, so criterion's own progress notes
# ("Gnuplot not found", "Warning: Unable to complete 100 samples...") never
# reach the file being parsed -- it writes those with eprintln!. The blank line
# is the separator it emits between benchmark groups.
CLEAN = """test put_item/put_item/small ... bench:       26475 ns/iter (+/- 1730)
test get_item ... bench:       14739 ns/iter (+/- 273)

test query_base_table ... bench:     1,112,845 ns/iter (+/- 12302)
"""

# What it writes when a stale target/criterion leaves a base directory behind
# without the sample.json inside it. That error goes to stdout, unlike the
# progress notes above, so it lands between the header and the measurement.
INTERLEAVED = """test put_item/put_item/small ... Criterion.rs ERROR: error: \
Failed to access file \
"/tmp/benchmarks/target/criterion/put_item/put_item/small/base/sample.json": \
No such file or directory (os error 2)
bench:       27261 ns/iter (+/- 2501)
test get_item ... Criterion.rs ERROR: error: Failed to access file \
"/tmp/benchmarks/target/criterion/get_item/base/sample.json": No such file or \
directory (os error 2)
bench:       15543 ns/iter (+/- 755)
"""


class ParseBencherTest(unittest.TestCase):
    def test_reads_clean_output(self):
        self.assertEqual(
            parse_bencher(CLEAN),
            {
                "put_item/put_item/small": 26475,
                "get_item": 14739,
                "query_base_table": 1112845,
            },
        )

    def test_reads_output_split_by_a_criterion_error(self):
        self.assertEqual(
            parse_bencher(INTERLEAVED),
            {"put_item/put_item/small": 27261, "get_item": 15543},
        )

    def test_ignores_noise_that_is_not_a_result(self):
        self.assertEqual(parse_bencher("\nCriterion.rs ERROR: error: boom\n"), {})

    def test_leaves_out_a_benchmark_that_produced_no_measurement(self):
        # The caller reports a baselined benchmark with no current result as a
        # failed comparison, so a name with nothing behind it must not appear.
        self.assertEqual(parse_bencher("test get_item ... \n"), {})

    def test_does_not_credit_a_measurement_to_the_wrong_benchmark(self):
        # get_item died before reporting. Its name must not pick up the figure
        # belonging to the benchmark that ran next.
        text = "test get_item ... \ntest delete_item ... bench: 53258 ns/iter (+/- 5623)\n"
        self.assertEqual(parse_bencher(text), {"delete_item": 53258})

    def test_reads_a_measurement_that_follows_two_lines_of_noise(self):
        text = (
            "test get_item ... Criterion.rs ERROR: first\n"
            "Criterion.rs ERROR: second\n"
            "bench:       15543 ns/iter (+/- 755)\n"
        )
        self.assertEqual(parse_bencher(text), {"get_item": 15543})


class BuildReportTest(unittest.TestCase):
    """The report's own text calls a partial run a failed comparison, so the
    caller has to be able to see that state rather than infer it."""

    HISTORY = [
        ("runs/2026-01-01-aaaaaaa", {"get_item": 100, "put_item": 200}),
        ("runs/2026-01-02-bbbbbbb", {"get_item": 100, "put_item": 200}),
    ]

    def report(self, current, threshold=1.50):
        baseline = build_baseline(self.HISTORY)
        return build_report(current, baseline, self.HISTORY, threshold)

    def test_names_a_baselined_benchmark_that_produced_no_result(self):
        _, regressions, missing = self.report({"get_item": 100})
        self.assertEqual(missing, ["put_item"])
        self.assertEqual(regressions, [])

    def test_reports_nothing_missing_when_every_benchmark_ran(self):
        _, regressions, missing = self.report({"get_item": 100, "put_item": 200})
        self.assertEqual(missing, [])
        self.assertEqual(regressions, [])

    def test_a_benchmark_new_to_the_window_is_not_missing(self):
        current = {"get_item": 100, "put_item": 200, "scan": 300}
        _, _, missing = self.report(current)
        self.assertEqual(missing, [])

    def test_a_run_can_regress_and_lose_a_benchmark_at_once(self):
        _, regressions, missing = self.report({"get_item": 1000})
        self.assertEqual(regressions, ["get_item"])
        self.assertEqual(missing, ["put_item"])


class EvaluateTest(unittest.TestCase):
    """The rule itself, read off the rows rather than the rendered table."""

    HISTORY = [
        ("runs/2026-01-01-aaaaaaa",
         {"get_item": 100, "put_item": 200, "zero": 0}),
        ("runs/2026-01-02-bbbbbbb",
         {"get_item": 100, "put_item": 200, "zero": 0, "query": 50}),
    ]

    def rows(self, current, threshold=1.50):
        return evaluate(current, build_baseline(self.HISTORY), threshold)

    def row(self, name, current, threshold=1.50):
        return next(r for r in self.rows(current, threshold) if r.name == name)

    def test_flags_a_benchmark_over_the_threshold_on_two_samples(self):
        self.assertEqual(
            self.row("get_item", {"get_item": 1000}),
            Row("get_item", COMPARED, current=1000, median=100, low=100,
                high=100, count=2, ratio=10.0, regressed=True),
        )

    def test_does_not_flag_a_benchmark_with_a_single_sample(self):
        # query is in one stored run of two. Ten times its only sample is still
        # reported as a ratio, but one run is not a median to flag against.
        row = self.row("query", {"query": 500})
        self.assertEqual(row.count, 1)
        self.assertEqual(row.ratio, 10.0)
        self.assertFalse(row.regressed)

    def test_a_ratio_equal_to_the_threshold_is_not_a_regression(self):
        self.assertEqual(self.row("get_item", {"get_item": 150}).ratio, 1.5)
        self.assertFalse(self.row("get_item", {"get_item": 150}).regressed)
        self.assertTrue(self.row("get_item", {"get_item": 151}).regressed)

    def test_a_zero_median_has_no_ratio_and_is_not_flagged(self):
        row = self.row("zero", {"zero": 5})
        self.assertIsNone(row.ratio)
        self.assertFalse(row.regressed)

    def test_a_benchmark_new_to_the_window(self):
        self.assertEqual(self.row("scan", {"scan": 300}),
                         Row("scan", NEW, current=300))

    def test_a_baselined_benchmark_with_no_result_is_missing(self):
        self.assertEqual(
            self.row("put_item", {"get_item": 100}),
            Row("put_item", MISSING, median=200, low=200, high=200, count=2),
        )

    def test_rows_come_back_sorted_by_name_whatever_their_status(self):
        rows = self.rows({"scan": 300, "get_item": 100})
        self.assertEqual(
            [(r.name, r.status) for r in rows],
            [("get_item", COMPARED), ("put_item", MISSING),
             ("query", MISSING), ("scan", NEW), ("zero", MISSING)],
        )

    def test_names_what_regressed_and_what_went_missing(self):
        rows = self.rows({"get_item": 1000, "query": 50, "zero": 0})
        self.assertEqual(regressed_names(rows), ["get_item"])
        self.assertEqual(missing_names(rows), ["put_item"])


class RenderMarkdownTest(unittest.TestCase):
    HISTORY = EvaluateTest.HISTORY

    def test_shows_both_warnings_when_a_run_regresses_and_loses_a_benchmark(self):
        # The two warnings used to share an if/elif chain, so a run that both
        # regressed and lost a benchmark only ever mentioned the regression.
        rows = [
            Row("get_item", COMPARED, current=1000, median=100, low=100,
                high=100, count=2, ratio=10.0, regressed=True),
            Row("put_item", MISSING, median=200, low=200, high=200, count=2),
        ]
        markdown = render_markdown(rows, self.HISTORY, 1.50)
        self.assertIn("above the", markdown)
        self.assertIn("produced no result", markdown)

    def test_says_so_when_everything_is_within_threshold(self):
        rows = [
            Row("get_item", COMPARED, current=100, median=100, low=100,
                high=100, count=2, ratio=1.0),
        ]
        markdown = render_markdown(rows, self.HISTORY, 1.50)
        self.assertIn("All benchmarks within", markdown)


# The empty-cell glyph in the rendered table. Spelt as an escape so the source
# carries no literal em dash.
DASH = "\u2014"


class RenderedReportTest(unittest.TestCase):
    """Pins the whole report, byte for byte, for inputs that between them reach
    every branch of the comparison. The PR comment is what a reviewer reads, so
    restructuring the script must leave it exactly as it was."""

    HISTORY = [
        ("runs/2026-01-01T000000Z-aaaaaaa", {
            "get_item": 100, "put_item": 200, "zero": 0,
            "delete_item": 300, "weird|name`": 10,
        }),
        ("runs/2026-01-02T000000Z-bbbbbbb", {
            "get_item": 100, "put_item": 200, "zero": 0,
            "delete_item": 320, "weird|name`": 12, "query": 50,
        }),
    ]

    def test_a_run_reaching_every_kind_of_row(self):
        # Within the threshold, over it on two samples, over it on one sample
        # (n=1, so reported but not flagged), a zero median, new to the window,
        # and two baselined benchmarks with no result, one needing escaping.
        current = {
            "get_item": 110, "put_item": 400, "zero": 5,
            "query": 500, "batch_write": 1234567,
        }
        markdown, regressions, missing = build_report(
            current, build_baseline(self.HISTORY), self.HISTORY, 1.50
        )
        self.assertEqual(markdown, "\n".join([
            "## Criterion Benchmark Results",
            "",
            "Baseline is the per-benchmark median of the last 2 stored runs, so "
            "one unusually fast or slow runner cannot skew the comparison. The "
            "range column is the spread across those runs.",
            "",
            "| Benchmark | Baseline (ns/iter) | Range | Current | Change |",
            "|-----------|-------------------:|------:|--------:|--------|",
            "| batch_write | {0} | {0} | 1,234,567 | new |".format(DASH),
            "| delete_item | 310 | 300 - 320 | {0} | removed |".format(DASH),
            "| get_item | 100 | 100 - 100 | 110 | +10.0% |",
            "| put_item | 200 | 200 - 200 | 400 | +100.0% :warning: |",
            "| query | 50 (n=1) | 50 - 50 | 500 | +900.0% |",
            "| weird\\|name\\` | 11 | 10 - 12 | {0} | removed |".format(DASH),
            "| zero | 0 | 0 - 0 | 5 | {0} |".format(DASH),
            "",
            "> :warning: 1 benchmark(s) came in more than 50% above the 2-run "
            "median. Check whether the current value falls outside the range "
            "column before treating it as real; iai-callgrind gives a "
            "deterministic instruction-count answer.",
            "> :warning: 2 benchmark(s) in the baseline produced no result in "
            "this run: delete_item, weird\\|name\\`. A partial bench run reports "
            "the rest as clean, so treat this as a failed comparison rather "
            "than a pass.",
            "",
            "<details><summary>Runs in the baseline</summary>",
            "",
            "- `runs/2026-01-01T000000Z-aaaaaaa`",
            "- `runs/2026-01-02T000000Z-bbbbbbb`",
            "",
            "</details>",
            "",
        ]))
        self.assertEqual(regressions, ["put_item"])
        self.assertEqual(missing, ["delete_item", "weird|name`"])

    def test_a_clean_run_against_a_single_stored_run(self):
        # This pins the current wording, which says every benchmark is within
        # the threshold even though get_item is not: it is over on a single
        # sample, so it is reported without being flagged.
        history = [("runs/2026-01-01T000000Z-aaaaaaa",
                    {"get_item": 100, "put_item": 200})]
        markdown, regressions, missing = build_report(
            {"get_item": 1000, "put_item": 200},
            build_baseline(history), history, 1.50,
        )
        self.assertEqual(markdown, "\n".join([
            "## Criterion Benchmark Results",
            "",
            "Baseline is the per-benchmark median of the last 1 stored run, so "
            "one unusually fast or slow runner cannot skew the comparison. The "
            "range column is the spread across those runs.",
            "",
            "| Benchmark | Baseline (ns/iter) | Range | Current | Change |",
            "|-----------|-------------------:|------:|--------:|--------|",
            "| get_item | 100 | 100 - 100 | 1,000 | +900.0% |",
            "| put_item | 200 | 200 - 200 | 200 | +0.0% |",
            "",
            "> All benchmarks within 50% of the 1-run median.",
            "",
            "<details><summary>Runs in the baseline</summary>",
            "",
            "- `runs/2026-01-01T000000Z-aaaaaaa`",
            "",
            "</details>",
            "",
        ]))
        self.assertEqual(regressions, [])
        self.assertEqual(missing, [])

    def test_a_run_with_no_stored_history(self):
        markdown, regressions, missing = build_report(
            {"get_item": 100}, {}, [], 1.50
        )
        self.assertEqual(markdown, "\n".join([
            "## Criterion Benchmark Results",
            "",
            "| Benchmark | Baseline (ns/iter) | Range | Current | Change |",
            "|-----------|-------------------:|------:|--------:|--------|",
            "| get_item | {0} | {0} | 100 | new |".format(DASH),
            "",
            "> No stored runs to compare against. Baselines are recorded by "
            "the benchmark refresh workflow.",
            "",
        ]))
        self.assertEqual(regressions, [])
        self.assertEqual(missing, [])


class ExitCodeTest(unittest.TestCase):
    """Both workflows branch on these, so they are a contract."""

    STORED = json.dumps({"get_item": 100, "put_item": 200})

    def run_main(self, current, tmpdir):
        """Run main() against a synthetic history.

        Two stored runs, not one: a benchmark with a single sample behind it
        reports its change without flagging it, so a one-run history could
        never reach the regression exit.
        """
        current_path = os.path.join(tmpdir, "current.json")
        with open(current_path, "w") as handle:
            json.dump(current, handle)

        def fake_git(*args):
            if args[0] == "ls-tree":
                return "runs/2026-01-01-aaaaaaa\nruns/2026-01-02-bbbbbbb\n"
            return self.STORED

        argv = [
            "compare_criterion.py",
            "--current-json", current_path,
            "--output", os.path.join(tmpdir, "out.md"),
        ]
        with mock.patch.object(compare_criterion, "git", fake_git), \
                mock.patch.object(sys, "argv", argv):
            return compare_criterion.main()

    def test_clean_run_exits_ok(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            status = self.run_main({"get_item": 100, "put_item": 200}, tmpdir)
        self.assertEqual(status, compare_criterion.EXIT_OK)

    def test_a_run_over_the_threshold_exits_regressed(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            status = self.run_main({"get_item": 1000, "put_item": 200}, tmpdir)
        self.assertEqual(status, compare_criterion.EXIT_REGRESSED)

    def test_a_run_missing_a_benchmark_exits_incomplete(self):
        # Exit 0 here is how a partial run used to reach a green check while
        # the comparison it posted said not to trust it.
        with tempfile.TemporaryDirectory() as tmpdir:
            status = self.run_main({"get_item": 100}, tmpdir)
        self.assertEqual(status, compare_criterion.EXIT_INCOMPLETE)

    def test_missing_takes_precedence_over_a_regression(self):
        # Exit 3 is advisory and leaves the step green, which a run that lost
        # benchmarks has not earned.
        with tempfile.TemporaryDirectory() as tmpdir:
            status = self.run_main({"get_item": 1000}, tmpdir)
        self.assertEqual(status, compare_criterion.EXIT_INCOMPLETE)

    def test_a_run_that_parsed_nothing_exits_error(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            status = self.run_main({}, tmpdir)
        self.assertEqual(status, compare_criterion.EXIT_ERROR)


if __name__ == "__main__":
    unittest.main()
