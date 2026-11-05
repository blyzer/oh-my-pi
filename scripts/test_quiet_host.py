#!/usr/bin/env python3
import contextlib
import importlib.util
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

spec = importlib.util.spec_from_file_location(
    "quiet_host", Path(__file__).with_name("quiet-host.py")
)
assert spec is not None and spec.loader is not None
quiet = importlib.util.module_from_spec(spec)
spec.loader.exec_module(quiet)


def snapshot(*, pageouts=100, swapouts=7, used=6000, load=0.5, state="idle"):
    return {
        "ts": 1,
        "load_avg": [load, 0.5, 0.5],
        "swap": {"total_mib": 8000, "used_mib": used, "free_mib": 2000},
        "vm": {"page_size": 16384, "pageouts": pageouts, "swapouts": swapouts},
        "runners": {
            "active": [
                {
                    "label": "actions.runner.example",
                    "pid": 10,
                    "state": state,
                    "worker_pids": [12] if state == "busy" else [],
                }
            ],
            "inactive": [],
            "unattributed_worker_pids": [],
        },
    }


class GuardTests(unittest.TestCase):
    def guard(self, first, second):
        with (
            mock.patch.object(quiet, "snapshot_or_error", side_effect=[first, second]),
            mock.patch.object(quiet.time, "sleep"),
        ):
            return quiet.guard(1.0)

    def test_retained_swap_does_not_refuse_idle_runner(self):
        record = self.guard(snapshot(), snapshot())
        self.assertEqual(record["verdict"], "pass")
        self.assertEqual(record["snapshot"]["swap"]["used_mib"], 6000)
        self.assertEqual(record["baseline"]["pageouts_delta"], 0)
        self.assertEqual(record["baseline"]["swapouts_delta"], 0)

    def test_live_paging_and_busy_runner_refuse(self):
        record = self.guard(snapshot(state="busy"), snapshot(pageouts=101, swapouts=8))
        self.assertEqual(record["verdict"], "refuse")
        self.assertEqual(
            {item["condition"] for item in record["refusals"]},
            {"runner_busy", "pageouts_delta", "swapouts_delta"},
        )

    def test_load_and_counter_reset_refuse(self):
        record = self.guard(snapshot(load=1.2), snapshot(pageouts=1))
        self.assertEqual(record["verdict"], "refuse")
        self.assertEqual(
            {item["condition"] for item in record["refusals"]},
            {"load_avg_1m", "probe_failed"},
        )

    def test_probe_failure_refuses(self):
        with (
            mock.patch.object(
                quiet,
                "snapshot_or_error",
                return_value={"error": "vm_stat unavailable"},
            ),
            mock.patch.object(quiet.time, "sleep"),
        ):
            record = quiet.guard(1.0)
        self.assertEqual(record["verdict"], "refuse")
        self.assertEqual(record["refusals"][0]["condition"], "probe_failed")

    def test_ambiguous_owners_do_not_create_a_bound(self):
        first = {
            "status": "identified",
            "pid": 50,
            "started": "a",
            "sharing": "shared",
            "other_peer_pids": [70],
            "socket_pairs": [],
        }
        second = {
            "status": "identified",
            "pid": 51,
            "started": "b",
            "sharing": "no_other_peer_observed",
            "other_peer_pids": [],
            "socket_pairs": [],
        }
        proof = quiet.owner_proof([first, second])
        self.assertEqual(proof["status"], "unresolved")
        accounting = quiet.envd_accounting([], [], [], 2.0, proof)
        self.assertEqual(accounting["attachment"], "unresolved")
        self.assertIsNone(accounting["cpu_bounds"]["upper_s"])

    def test_socket_peer_identifies_only_serving_envd(self):
        rows = {
            10: {
                "ppid": 1,
                "rss_kib": 100,
                "cpu_s": 1.0,
                "started": "a",
                "command": "time",
            },
            11: {
                "ppid": 10,
                "rss_kib": 200,
                "cpu_s": 2.0,
                "started": "b",
                "command": "omp adw",
            },
            50: {
                "ppid": 1,
                "rss_kib": 300,
                "cpu_s": 5.0,
                "started": "c",
                "command": "/bin/omp envd --root /repo --state-dir /data",
            },
            60: {
                "ppid": 1,
                "rss_kib": 800,
                "cpu_s": 9.0,
                "started": "d",
                "command": "/bin/omp envd --root /repo --state-dir /other",
            },
            70: {
                "ppid": 1,
                "rss_kib": 2400,
                "cpu_s": 20.0,
                "started": "e",
                "command": "coordinator",
            },
        }
        sockets = "p11\nf3\nd0xaaa\nn->0xbbb\np50\nf4\nd0xbbb\nn/tmp/socket\nf5\nd0xccc\nn/tmp/socket\np60\nf4\nd0xddd\nn/tmp/other\np70\nf3\nd0xeee\nn->0xccc\n"
        with mock.patch.object(quiet, "probe", return_value=sockets):
            owner = quiet.serving_envd(rows, 10, "/repo")
        self.assertEqual(owner["status"], "identified")
        self.assertEqual(owner["pid"], 50)
        self.assertEqual(owner["sharing"], "shared")
        self.assertEqual(owner["other_peer_pids"], [70])
        self.assertEqual(
            owner["socket_pairs"],
            [{"client_pid": 11, "client_socket": "0xaaa", "envd_socket": "0xbbb"}],
        )
        measured = quiet.attribute_processes(rows, 10, "/repo", owner=owner)
        self.assertEqual(measured["tree_rss_kib"], 300)
        self.assertEqual(measured["envd_rss_kib"], 300)
        self.assertEqual(measured["combined_rss_kib"], 600)
        unknown = quiet.attribute_processes(rows, 10, "/repo")
        self.assertIsNone(unknown["combined_rss_kib"])

    def test_detached_envd_is_counted_once_not_its_unrelated_child(self):
        rows = {
            10: {
                "ppid": 1,
                "rss_kib": 100,
                "cpu_s": 1.0,
                "started": "a",
                "command": "time",
            },
            11: {
                "ppid": 10,
                "rss_kib": 200,
                "cpu_s": 2.0,
                "started": "b",
                "command": "omp adw",
            },
            50: {
                "ppid": 1,
                "rss_kib": 300,
                "cpu_s": 5.0,
                "started": "c",
                "command": "/bin/omp envd --root /repo --state-dir /data",
            },
            51: {
                "ppid": 50,
                "rss_kib": 400,
                "cpu_s": 0.5,
                "started": "d",
                "command": "git",
            },
        }
        owner = {"status": "identified", "pid": 50, "started": "c"}
        measured = quiet.attribute_processes(rows, 10, "/repo", owner=owner)
        self.assertEqual(measured["tree_rss_kib"], 300)
        self.assertEqual(measured["envd_rss_kib"], 300)
        self.assertEqual(measured["combined_rss_kib"], 600)
        self.assertEqual(measured["envd"][0]["cpu_s"], 5.0)
        self.assertFalse(measured["envd"][0]["in_tree"])
        rows[50]["ppid"] = 11
        measured = quiet.attribute_processes(rows, 10, "/repo", owner=owner)
        self.assertEqual(measured["tree_rss_kib"], 1000)
        self.assertEqual(measured["combined_rss_kib"], 1000)
        self.assertTrue(measured["envd"][0]["in_tree"])

    def test_envd_attachment_candidates_are_not_conflated(self):
        previous = [{"pid": 50, "started": "a", "cpu_s": 10.0, "in_tree": False}]
        current = [{"pid": 50, "started": "a", "cpu_s": 10.4, "in_tree": False}]
        proof = {
            "status": "identified",
            "pid": 50,
            "started": "a",
            "sharing": "shared",
            "socket_pairs": [],
        }
        result = quiet.envd_accounting(previous, current, [], 2.0, proof)
        self.assertEqual(result["attachment"], "preexisting")
        self.assertAlmostEqual(result["envd_cpu_s"], 0.4)
        self.assertAlmostEqual(result["cpu_bounds"]["upper_s"], 2.4)
        self.assertEqual(result["cpu_bounds"]["lower_s"], 2.0)
        self.assertEqual(result["sharing"], "shared")
        new = [{"pid": 51, "started": "b", "cpu_s": 1.0, "in_tree": True}]
        proof = {
            "status": "identified",
            "pid": 51,
            "started": "b",
            "sharing": "no_other_peer_observed",
            "socket_pairs": [],
        }
        result = quiet.envd_accounting([], new, new, 2.0, proof)
        self.assertEqual(result["attachment"], "spawned")
        self.assertEqual(result["envd_cpu_s"], 1.0)
        self.assertEqual(
            quiet.envd_accounting(previous, current + new, new, 2.0, None)[
                "attachment"
            ],
            "unresolved",
        )

    def test_sampler_discovers_a_shared_synthetic_envd(self):
        with tempfile.TemporaryDirectory() as project:
            socket_path = project + "/env.sock"
            server_code = (
                "import socket,sys,time; s=socket.socket(socket.AF_UNIX); "
                "s.bind(sys.argv[6]); s.listen(); print('ready',flush=True); "
                "a,_=s.accept(); b,_=s.accept(); time.sleep(20)"
            )
            client_code = (
                "import socket,sys,time; s=socket.socket(socket.AF_UNIX); "
                "s.connect(sys.argv[1]); print('ready',flush=True); time.sleep(8)"
            )
            proc = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    server_code,
                    "envd",
                    "--root",
                    project,
                    "--state-dir",
                    project + "/state",
                    socket_path,
                ],
                stdout=subprocess.PIPE,
                text=True,
            )
            other = None
            try:
                assert proc.stdout is not None
                self.assertEqual(proc.stdout.readline().strip(), "ready")
                other = subprocess.Popen(
                    [sys.executable, "-c", client_code, socket_path],
                    stdout=subprocess.PIPE,
                    text=True,
                )
                assert other.stdout is not None
                self.assertEqual(other.stdout.readline().strip(), "ready")
                out = io.StringIO()
                with (
                    mock.patch.object(quiet, "snapshot_or_error", return_value={}),
                    mock.patch.object(quiet, "load_avg", return_value=[0, 0, 0]),
                    mock.patch.object(quiet, "swap_usage", return_value={}),
                    mock.patch.object(quiet, "vm_counters", return_value={}),
                    mock.patch.object(quiet, "runner_services", return_value={}),
                ):
                    end = quiet.run_recipe(
                        [sys.executable, "-c", client_code, socket_path],
                        "",
                        "synthetic",
                        0.3,
                        {"snapshot": {}},
                        output=out,
                        project_root=project,
                        stdout=subprocess.DEVNULL,
                        echo=False,
                    )
                records = [json.loads(line) for line in out.getvalue().splitlines()]
                proof = end["envd_ownership"]
                self.assertEqual(proof["status"], "identified")
                self.assertEqual(proof["pid"], proc.pid)
                self.assertEqual(proof["sharing"], "shared")
                self.assertIn(other.pid, proof["other_peer_pids"])
                self.assertTrue(proof["socket_pairs"])
                self.assertTrue(
                    any(
                        row["content"].get("envd_rss_kib")
                        for row in records
                        if row["type"] == "sample"
                    )
                )
                self.assertIsNotNone(end["tree_plus_envd_peak_rss_mib"])
                accounting = quiet.envd_accounting(
                    end["envd_before"]["processes"],
                    end["envd_after"]["processes"],
                    end["envd_observed"],
                    0.0,
                    proof,
                )
                self.assertEqual(accounting["attachment"], "preexisting")
            finally:
                if other is not None:
                    other.terminate()
                    other.wait(timeout=5)
                    if other.stdout is not None:
                        other.stdout.close()
                proc.terminate()
                proc.wait(timeout=5)
                if proc.stdout is not None:
                    proc.stdout.close()

    def test_sampler_emits_envd_series_with_combined_peak(self):
        rows = {
            10: {
                "ppid": 1,
                "rss_kib": 100,
                "cpu_s": 1.0,
                "started": "a",
                "command": "time",
            },
            50: {
                "ppid": 1,
                "rss_kib": 300,
                "cpu_s": 5.0,
                "started": "c",
                "command": "/bin/omp envd --root /repo --state-dir /data",
            },
        }
        stop = mock.Mock()
        stop.is_set.side_effect = [False, True]
        records, peak, seen, proofs = [], [None], {}, []
        proof = {
            "status": "identified",
            "pid": 50,
            "started": "c",
            "sharing": "shared",
            "other_peer_pids": [70],
            "socket_pairs": [],
        }
        with (
            mock.patch.object(quiet, "load_avg", return_value=[0, 0, 0]),
            mock.patch.object(quiet, "swap_usage", return_value={}),
            mock.patch.object(quiet, "vm_counters", return_value={}),
            mock.patch.object(quiet, "runner_services", return_value={}),
            mock.patch.object(quiet, "process_table", return_value=rows),
            mock.patch.object(quiet, "serving_envd", return_value=proof),
        ):
            quiet.sample_loop(
                10,
                1.0,
                quiet.time.monotonic(),
                records.append,
                stop,
                peak,
                project_root="/repo",
                envd_seen=seen,
                proofs=proofs,
            )
        self.assertEqual(records[0]["content"]["tree_rss_kib"], 100)
        self.assertEqual(records[0]["content"]["envd_rss_kib"], 300)
        self.assertEqual(records[0]["content"]["envd_cpu_s"], 5.0)
        self.assertEqual(records[0]["content"]["combined_rss_kib"], 400)
        self.assertEqual(records[0]["conditions"]["envd_candidates"][0]["cpu_s"], 5.0)
        self.assertEqual(peak[0], 100)
        self.assertEqual(len(seen), 1)
        self.assertEqual(proofs, [proof])

    def test_missing_process_tree_is_not_zero(self):
        with mock.patch.object(quiet, "probe", return_value="1 0 10\n"):
            with self.assertRaises(ValueError):
                quiet.tree_rss_kib(999)

    def test_no_valid_samples_do_not_report_zero_peak(self):
        out = io.StringIO()
        with (
            mock.patch.object(quiet, "snapshot_or_error", return_value={}),
            mock.patch.object(quiet, "sample_loop", return_value=None),
        ):
            record = quiet.run_recipe(
                ["/usr/bin/true"], "", "test", 1, None, output=out, echo=False
            )
        self.assertIsNone(record["tree_peak_rss_mib"])
        self.assertEqual(record["tree_peak_status"], "not_captured")

    def test_listener_is_idle_but_worker_child_is_busy(self):
        launch = "PID\tStatus\tLabel\n10\t0\tactions.runner.example\n"
        idle = "10 1 /bin/bash\n11 10 /runner/Runner.Listener\n"
        busy = idle + "12 11 /runner/Runner.Worker\n13 12 /bin/sh\n"
        for processes, expected in [(idle, "idle"), (busy, "busy")]:
            with (
                self.subTest(expected=expected),
                mock.patch.object(quiet, "probe", side_effect=[launch, processes]),
            ):
                runners = quiet.runner_services()
            self.assertEqual(runners["active"][0]["state"], expected)
            self.assertEqual(
                runners["active"][0]["worker_pids"], [12] if expected == "busy" else []
            )

    def test_reused_output_is_a_clean_error(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "existing.jsonl"
            path.write_text("keep\n")
            stderr = io.StringIO()
            with (
                mock.patch.object(
                    sys,
                    "argv",
                    [
                        "quiet-host.py",
                        "sample",
                        "--out",
                        str(path),
                        "--",
                        "/usr/bin/true",
                    ],
                ),
                contextlib.redirect_stderr(stderr),
            ):
                self.assertEqual(quiet.main(), 2)
            self.assertIn("already exists", stderr.getvalue())
            self.assertNotIn("Traceback", stderr.getvalue())
            self.assertEqual(path.read_text(), "keep\n")


if __name__ == "__main__":
    unittest.main()
