import concurrent.futures
import io
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.error
from pathlib import Path
from unittest import mock

import publish_eval_workset as publisher


class PublishEvalWorksetTests(unittest.TestCase):
    def test_evidence_archive_compression_is_bounded(self):
        active = 0
        peak = 0
        lock = threading.Lock()

        def write_archive(source, target):
            nonlocal active, peak
            with lock:
                active += 1
                peak = max(peak, active)
            time.sleep(0.02)
            target.write_bytes(b"archive")
            with lock:
                active -= 1

        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            publisher,
            "write_archive",
            side_effect=write_archive,
        ), mock.patch.object(publisher, "upload_archive_file"):
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            state_file = root / "state.json"
            with concurrent.futures.ThreadPoolExecutor(max_workers=12) as executor:
                list(
                    executor.map(
                        lambda index: publisher.publish_archive(
                            "https://example.test",
                            "token",
                            f"attempts/{index}.tar.zst",
                            source,
                            {},
                            state_file,
                        ),
                        range(12),
                    )
                )

        self.assertEqual(peak, 4)

    def test_streaming_retry_rewinds_a_partially_consumed_body(self):
        body = io.BytesIO(b"archive")
        request = urllib.request.Request(
            "https://example.test/upload",
            data=body,
            method="PUT",
        )
        response = mock.MagicMock()
        response.__enter__.return_value = response
        response.read.return_value = b"done"
        positions = []

        def urlopen(current, *, timeout):
            positions.append(current.data.tell())
            if len(positions) == 1:
                current.data.read(3)
                raise urllib.error.URLError("connection reset")
            return response

        with mock.patch.object(
            publisher.urllib.request,
            "urlopen",
            side_effect=urlopen,
        ), mock.patch.object(publisher.time, "sleep"):
            self.assertEqual(publisher.perform(request, "put object"), b"done")

        self.assertEqual(positions, [0, 0])

    def test_direct_archive_upload_streams_the_file_without_reading_it(self):
        class UnreadableBody(io.BytesIO):
            def read(self, *args, **kwargs):
                raise AssertionError("upload body was buffered")

        body = UnreadableBody(b"archive")
        with mock.patch.object(publisher, "perform", return_value=b"") as perform:
            publisher.put_stream(
                "https://example.test",
                "token",
                "attempts/a.tar.zst",
                publisher.ARCHIVE_CONTENT_TYPE,
                body,
                7,
            )

        request, operation = perform.call_args.args
        self.assertIs(request.data, body)
        self.assertEqual(request.get_header("Content-length"), "7")
        self.assertEqual(request.get_method(), "PUT")
        self.assertEqual(operation, "put object")

    def test_object_head_retries_transient_worker_failures(self):
        transient = urllib.error.HTTPError(
            "https://example.test/v1/import/objects",
            500,
            "worker failure",
            {},
            io.BytesIO(b"temporary"),
        )
        response = mock.MagicMock()
        response.status = 204
        response.__enter__.return_value = response

        with mock.patch.object(
            publisher.urllib.request,
            "urlopen",
            side_effect=[transient, response],
        ) as urlopen, mock.patch.object(publisher.time, "sleep"):
            self.assertTrue(
                publisher.object_exists("https://example.test", "token", "tasks/x")
            )

        self.assertEqual(urlopen.call_count, 2)
        transient.close()

    def test_public_id_matches_the_coordinator_nul_delimited_hash(self):
        self.assertEqual(
            publisher.public_id("workset", "42"),
            "4385863bb5ffcb8bed09d5ff",
        )

    def test_archive_contains_only_canonical_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "events.jsonl").write_text("{}\n")
            (root / "reward.txt").write_text("1\n")
            (root / "notes.txt").write_text("not retained\n")
            (root / "nested").mkdir()
            (root / "nested" / "result.json").write_text("{}\n")
            (root / "workspace").mkdir()
            (root / "workspace" / "secret.json").write_text("{}\n")
            archive = root / "evidence.tar.zst"

            publisher.write_archive(root, archive)
            decompressed = subprocess.run(
                ["zstd", "--decompress", "--quiet", "--stdout", archive],
                check=True,
                stdout=subprocess.PIPE,
            ).stdout
            listing = sorted(subprocess.run(
                ["tar", "--list", "--file=-"],
                input=decompressed,
                check=True,
                stdout=subprocess.PIPE,
            ).stdout.decode().splitlines())

            self.assertEqual(
                listing,
                ["events.jsonl", "nested/result.json", "reward.txt"],
            )

    def test_board_requeues_running_rows_and_retains_terminal_pointers(self):
        connection = sqlite3.connect(":memory:")
        connection.row_factory = sqlite3.Row
        connection.executescript(
            """
            CREATE TABLE task_definitions(
              id INTEGER PRIMARY KEY, workset_id INTEGER, selector TEXT,
              name TEXT, root TEXT, digest TEXT
            );
            CREATE TABLE rows(
              id INTEGER, definition_id INTEGER, family_key TEXT, repetition INTEGER,
              state TEXT, claim_id TEXT, worker TEXT, started_at_ms INTEGER,
              finished_at_ms INTEGER, result_path TEXT, error TEXT, harness TEXT,
              model TEXT, thinking TEXT, web_search INTEGER, status TEXT,
              outcome TEXT, input_tokens INTEGER, cached_input_tokens INTEGER,
              output_tokens INTEGER, reasoning_output_tokens INTEGER,
              total_tokens INTEGER, cost_usd REAL, agent_duration_ms INTEGER
            );
            INSERT INTO task_definitions VALUES (7, 1, 'task', 'Task Name', '/tasks/task', 'td');
            INSERT INTO rows VALUES
              (11, 7, 'family', 0, 'running', 'live', 'worker', 100, NULL,
               NULL, NULL, 'h', 'm', 'low', 0, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
              (12, 7, 'family', 1, 'success', 'done', 'worker', 100, 200,
               '/result', NULL, 'h', 'm', 'low', 0, 'passed', 'passed', 1, 2, 3, 4, 10, 0.5, 1234);
            """
        )
        rows = list(connection.execute("SELECT * FROM rows ORDER BY id"))
        definitions = list(connection.execute("SELECT * FROM task_definitions"))
        workset = {
            "id": 1,
            "profile": "profile",
            "digest": "digest",
            "created_at_ms": 50,
        }
        terminal_id = publisher.public_id("digest", "12")
        task_id = publisher.public_id("digest", "Task Name")
        case_key = f"cases/digest/{terminal_id}.json"
        artifact_key = f"attempts/digest/historical/{terminal_id}/evidence.tar.zst"

        board = publisher.build_board(
            connection,
            workset,
            definitions,
            rows,
            {f"tasks/digest/{task_id}.tar.zst"},
            {case_key},
            {artifact_key},
        )
        running, terminal = board["tasks"][0]["coordinates"]

        self.assertNotIn("state", running)
        self.assertEqual(terminal["state"], "success")
        self.assertEqual(terminal["artifactKey"], artifact_key)
        self.assertEqual(terminal["result"]["caseKey"], case_key)
        self.assertEqual(terminal["result"]["totalTokens"], 10)
        self.assertEqual(terminal["result"]["durationMs"], 1234)
        connection.close()


if __name__ == "__main__":
    unittest.main()
