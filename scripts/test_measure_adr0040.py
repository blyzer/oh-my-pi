#!/usr/bin/env python3
import importlib.util
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).with_name("measure-adr0040.py")
spec = importlib.util.spec_from_file_location("driver", SCRIPT)
assert spec is not None and spec.loader is not None
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class DriverTests(unittest.TestCase):
    def test_dry_run_has_interleaved_synthetic_records(self):
        with tempfile.TemporaryDirectory() as directory:
            result = Path(directory) / "result.jsonl"
            proc = subprocess.run(
                [sys.executable, str(SCRIPT), "--dry-run", "--out", str(result)],
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr)
            records = [json.loads(line) for line in result.read_text().splitlines()]
        self.assertTrue(records[0]["synthetic"])
        self.assertEqual(
            records[0]["boundary"]["definition"], "role, not process ancestry"
        )
        self.assertEqual(len([r for r in records if r["type"] == "fetch"]), 1)
        guards = [r for r in records if r["type"] == "guard"]
        self.assertEqual(len(guards), 19)
        self.assertTrue(all(r["synthetic"] for r in guards))
        starts = [r for r in records if r["type"] == "start"]
        ends = [r for r in records if r["type"] == "repetition"]
        self.assertEqual(len(starts), 18)
        self.assertEqual(len(ends), 18)
        self.assertEqual([r["arm"] for r in ends[:6]], ["local", "remote"] * 3)
        self.assertTrue(all(r["status"] == "synthetic" for r in ends))
        self.assertTrue(
            all(
                r["remote_metrics"]["find_missing_ms"]["status"] == "synthetic"
                for r in ends
                if r["arm"] == "remote"
            )
        )
        self.assertEqual(records[-1]["status"], "complete")
        self.assertIn("Peak memory (process-tree RSS)", proc.stdout)
        self.assertIn("SYNTHETIC", proc.stdout)

    def test_failed_repetition_is_not_retried_and_abort_keeps_results(self):
        good = {"type": "guard", "verdict": "pass", "snapshot": {}}
        bad = {
            "type": "guard",
            "verdict": "refuse",
            "refusals": [{"condition": "runner_busy", "value": "runner", "pid": 1}],
            "snapshot": {},
        }
        calls = []

        def fake_run(out, recipe, arm, repetition, **kwargs):
            calls.append((recipe["name"], arm, repetition))
            return {
                "type": "repetition",
                "recipe": recipe["name"],
                "arm": arm,
                "repetition": repetition,
                "status": "failed",
                "exit": 101,
                "stdout": "three process-lifecycle tests failed",
            }

        with tempfile.TemporaryDirectory() as directory:
            result = Path(directory) / "partial.jsonl"
            with (
                mock.patch.object(
                    driver.quiet, "guard", side_effect=[good, good, good, bad]
                ),
                mock.patch.object(
                    driver,
                    "cache_state",
                    return_value={"action_cache": "off", "profile": "buildbuddy"},
                ),
                mock.patch.object(
                    driver, "fetch_crates", return_value={"type": "fetch", "exit": 0}
                ),
                mock.patch.object(driver, "run_one", side_effect=fake_run),
                mock.patch.object(driver, "SETTLE_TIMEOUT_S", 0.0),
                mock.patch.object(driver.time, "sleep") as sleep,
            ):
                code = driver.run_session(
                    result, root=Path(directory), omp_bin="omp", dry_run=False
                )
            records = [json.loads(line) for line in result.read_text().splitlines()]
        self.assertEqual(code, 2)
        self.assertEqual(len(calls), 2)
        self.assertEqual([call[1] for call in calls], ["local", "remote"])
        repetitions = [r for r in records if r["type"] == "repetition"]
        self.assertEqual(len(repetitions), 2)
        self.assertIn("three process-lifecycle tests failed", repetitions[-1]["stdout"])
        self.assertEqual(records[-1]["status"], "aborted")
        self.assertIn("between-repetition guard refused", records[-1]["reason"])
        sleep.assert_not_called()

    def test_refused_guard_waits_for_the_host_to_settle(self):
        good = {"type": "guard", "verdict": "pass", "snapshot": {}}
        bad = {
            "type": "guard",
            "verdict": "refuse",
            "refusals": [{"condition": "load_avg_1m", "value": 4.2, "limit": 1.0}],
            "snapshot": {},
        }
        verdicts = iter([good, bad])
        used = []

        def fake_run(out, recipe, arm, repetition, **kwargs):
            used.append(kwargs["guard"]["verdict"])
            return {
                "type": "repetition",
                "recipe": recipe["name"],
                "arm": arm,
                "repetition": repetition,
                "status": "passed",
            }

        with tempfile.TemporaryDirectory() as directory:
            result = Path(directory) / "settled.jsonl"
            with (
                mock.patch.object(
                    driver.quiet,
                    "guard",
                    side_effect=lambda _limit: dict(next(verdicts, good)),
                ),
                mock.patch.object(
                    driver,
                    "cache_state",
                    return_value={"action_cache": "off", "profile": "buildbuddy"},
                ),
                mock.patch.object(
                    driver, "fetch_crates", return_value={"type": "fetch", "exit": 0}
                ),
                mock.patch.object(driver, "run_one", side_effect=fake_run),
                mock.patch.object(driver.time, "sleep") as sleep,
                mock.patch("sys.stderr", new_callable=io.StringIO) as stderr,
            ):
                code = driver.run_session(
                    result, root=Path(directory), omp_bin="omp", dry_run=False
                )
            records = [json.loads(line) for line in result.read_text().splitlines()]
        self.assertEqual(code, 0)
        self.assertEqual(records[-1]["status"], "complete")
        sleep.assert_called_once_with(driver.SETTLE_POLL_S)
        self.assertIn("REFUSED load_avg_1m: 4.2 > limit 1.0", stderr.getvalue())
        guards = [r["verdict"] for r in records if r["type"] == "guard"]
        self.assertEqual(guards[:3], ["pass", "refuse", "pass"])
        self.assertEqual(len(guards), 20)
        self.assertEqual(used, ["pass"] * 18)

    def test_cli_refuses_result_inside_checkout(self):
        result = SCRIPT.parent.parent / "measurement-result.jsonl"
        with (
            mock.patch.object(
                sys, "argv", ["measure-adr0040.py", "--dry-run", "--out", str(result)]
            ),
            mock.patch.object(driver, "run_session") as run,
            mock.patch("sys.stderr", new_callable=io.StringIO),
        ):
            with self.assertRaises(SystemExit) as error:
                driver.main()
        self.assertEqual(error.exception.code, 2)
        run.assert_not_called()

    def test_real_run_never_accepts_synthetic_guard(self):
        guard = {
            "type": "guard",
            "verdict": "synthetic",
            "synthetic": True,
            "snapshot": {},
        }
        with tempfile.TemporaryDirectory() as directory:
            result = Path(directory) / "partial.jsonl"
            with (
                mock.patch.object(driver.quiet, "guard", return_value=guard),
                mock.patch.object(
                    driver,
                    "cache_state",
                    side_effect=AssertionError("must not inspect cache"),
                ),
                mock.patch.object(
                    driver, "fetch_crates", side_effect=AssertionError("must not fetch")
                ),
            ):
                code = driver.run_session(
                    result, root=Path(directory), omp_bin="omp", dry_run=False
                )
            records = [json.loads(line) for line in result.read_text().splitlines()]
        self.assertEqual(code, 2)
        self.assertEqual(records[-1]["status"], "aborted")
        self.assertFalse(any(r["type"] == "fetch" for r in records))

    def test_remote_repetition_reports_tree_and_envd_boundaries(self):
        prior = [{"pid": 50, "started": "a", "cpu_s": 10.0, "in_tree": False}]
        after = [{"pid": 50, "started": "a", "cpu_s": 10.4, "in_tree": False}]
        end = {
            "exit": 0,
            "tree_peak_rss_mib": 10,
            "tree_plus_envd_peak_rss_mib": 20,
            "envd_before": {"status": "captured", "processes": prior},
            "envd_after": {"status": "captured", "processes": after},
            "envd_observed": after,
            "tree_without_envd_peak_rss_mib": 10,
            "envd_ownership": {
                "status": "identified",
                "pid": 50,
                "started": "a",
                "sharing": "shared",
                "socket_pairs": [
                    {"client_pid": 1, "client_socket": "0xa", "envd_socket": "0xb"}
                ],
            },
        }

        def fake_sample(*args, **kwargs):
            kwargs["stderr"].write(
                "1.00 real 0.40 user 0.10 sys\n100 maximum resident set size\n"
            )
            self.assertEqual(kwargs["project_root"], "/repo")
            return end

        with (
            mock.patch.object(driver, "disk_size", return_value={"bytes": 0}),
            mock.patch.object(driver.quiet, "run_recipe", side_effect=fake_sample),
        ):
            record = driver.run_one(
                io.StringIO(),
                driver.RECIPES[0],
                "remote",
                1,
                root=Path("/repo"),
                omp_bin="omp",
                dry_run=False,
                guard={"verdict": "pass", "snapshot": {}},
            )
        self.assertEqual(record["client_accounting"]["attachment"], "preexisting")
        self.assertEqual(record["conditions"]["sharing"], "shared")
        self.assertAlmostEqual(
            record["content"]["serving_envd"]["cpu_bounds"]["upper_s"], 0.9
        )
        self.assertEqual(record["content"]["rss_bounds"]["lower_mib"], 10)
        self.assertEqual(record["content"]["rss_bounds"]["upper_mib"], 20)
        self.assertEqual(record["sampler"]["tree_peak_rss_mib"], 10)
        self.assertEqual(record["sampler"]["tree_plus_envd_peak_rss_mib"], 20)

    def test_summary_does_not_average_mixed_daemon_boundaries(self):
        def sample(mode, cpu):
            return {
                "recipe": driver.RECIPES[0]["name"],
                "arm": "remote",
                "repetition": 1,
                "status": "passed",
                "time": {"status": "captured", "wall_s": 2, "cpu_s": cpu},
                "sampler": {"tree_peak_rss_mib": 10, "tree_plus_envd_peak_rss_mib": 20},
                "client_accounting": {
                    "attachment": mode,
                    "sharing": "shared",
                    "cpu_bounds": {"upper_s": cpu + 1},
                },
            }

        rendered = driver.summary(
            [sample("preexisting", 1), sample("spawned", 3)], False
        )
        self.assertIn("preexisting, shared", rendered)
        self.assertIn("spawned, shared", rendered)
        self.assertIn("verdict withheld", rendered)
        self.assertNotIn("| 2.00 s |", rendered)

    def test_missing_trace_values_are_not_zero_or_failed(self):
        text = (
            "2026 INFO omp.exec.capture: close time.busy=10ms time.idle=5ms\n"
            "2026 INFO omp.exec.upload: close time.busy=1s time.idle=200ms\n"
            "2026 INFO omp.exec.execute: upload complete omp.exec.upload.bytes=0\n"
            "2026 INFO omp.exec.execute: worker metadata omp.exec.worker.queue_ms=Some(0) "
            "omp.exec.worker.input_fetch_ms=Some(20) omp.exec.worker.exec_ms=Some(31) "
            "omp.exec.worker.output_upload_ms=Some(4)\n"
        )
        metrics = driver.remote_metrics(text, failed=False, synthetic=False)
        self.assertEqual(metrics["capture_ms"], {"status": "captured", "value": 15.0})
        self.assertEqual(
            metrics["upload_inclusive_ms"], {"status": "captured", "value": 1200.0}
        )
        self.assertEqual(metrics["upload_bytes"], {"status": "captured", "value": 0})
        self.assertEqual(metrics["queue_ms"], {"status": "captured", "value": 0})
        self.assertEqual(metrics["find_missing_ms"]["status"], "not_captured")
        self.assertEqual(metrics["download_ms"]["status"], "not_captured")
        failed = driver.remote_metrics("", failed=True, synthetic=False)
        self.assertEqual(failed["download_ms"]["status"], "run_failed")
        self.assertEqual(failed["find_missing_ms"]["status"], "not_captured")

    def test_time_parser_uses_final_time_report(self):
        stderr = (
            "9.00 real 8.00 user 7.00 sys\n99 maximum resident set size\n"
            "2.00 real 0.40 user 0.20 sys\n200 maximum resident set size\n"
        )
        record = driver.time_metrics(stderr, failed=False, synthetic=False)
        self.assertEqual(record["wall_s"], 2.0)
        self.assertAlmostEqual(record["cpu_s"], 0.6)
        self.assertEqual(record["max_single_descendant_rss_bytes"], 200)

    def test_probe_command_failure_keeps_aborted_footer(self):
        good = {"type": "guard", "verdict": "pass", "snapshot": {}}
        with tempfile.TemporaryDirectory() as directory:
            result = Path(directory) / "partial.jsonl"
            with (
                mock.patch.object(driver.quiet, "guard", return_value=good),
                mock.patch.object(
                    driver,
                    "cache_state",
                    return_value={"action_cache": "off", "profile": "buildbuddy"},
                ),
                mock.patch.object(
                    driver, "fetch_crates", return_value={"type": "fetch", "exit": 0}
                ),
                mock.patch.object(
                    driver,
                    "run_one",
                    side_effect=subprocess.CalledProcessError(1, "du"),
                ),
            ):
                code = driver.run_session(
                    result, root=Path(directory), omp_bin="omp", dry_run=False
                )
            records = [json.loads(line) for line in result.read_text().splitlines()]
        self.assertEqual(code, 2)
        self.assertEqual(records[-1]["status"], "aborted")


if __name__ == "__main__":
    unittest.main()
