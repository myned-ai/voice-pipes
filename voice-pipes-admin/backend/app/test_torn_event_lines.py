"""One torn line in a capture must cost one event, not the session.

Session 0bb0cba0 had a record torn by an overlapping write on row 5 of its
`pipeline_events.jsonl`. Every reader parsed the file inside a single `try`,
so the rest of the file was never read: the timings of all six turns were
dropped and the page showed a healthy session with no metrics.
"""

import tempfile
import unittest
from pathlib import Path

from app.captures.jsonl import count_unreadable_lines, iter_jsonl, parse_jsonl_line

GOOD_1 = '{"kind":"utterance_received","source":"Audio","text":"Hello","utterance_id":1}\n'
TORN = '},"utterance_id":1}\n'
GOOD_2 = '{"kind":"llm_completed","utterance_id":1}\n'
GOOD_3 = '{"kind":"utterance_received","source":"Audio","text":"Again","utterance_id":2}\n'


class TornLineTest(unittest.TestCase):
    def _file(self, text: str) -> Path:
        d = tempfile.mkdtemp()
        p = Path(d) / "pipeline_events.jsonl"
        p.write_text(text, encoding="utf-8")
        return p

    def test_rows_after_a_torn_line_are_still_read(self):
        rows = list(iter_jsonl(self._file(GOOD_1 + TORN + GOOD_2 + "\n" + GOOD_3)))
        self.assertEqual([r["kind"] for r in rows],
                         ["utterance_received", "llm_completed", "utterance_received"])
        self.assertEqual(rows[-1]["utterance_id"], 2)

    def test_torn_lines_are_counted(self):
        self.assertEqual(count_unreadable_lines(self._file(GOOD_1 + TORN + GOOD_2 + TORN)), 2)
        self.assertEqual(count_unreadable_lines(self._file(GOOD_1 + GOOD_2)), 0)

    def test_a_line_that_is_json_but_not_an_object_is_not_a_row(self):
        self.assertIsNone(parse_jsonl_line("[1, 2]"))
        self.assertIsNone(parse_jsonl_line('"text"'))

    def test_a_missing_file_is_empty_not_an_error(self):
        self.assertEqual(list(iter_jsonl(Path("no/such/file.jsonl"))), [])
        self.assertEqual(count_unreadable_lines(Path("no/such/file.jsonl")), 0)


class RealSessionTest(unittest.TestCase):
    """The reader itself, on a capture shaped like the one that failed."""

    def test_timings_survive_a_torn_line(self):
        from app.captures import reader

        root = Path(tempfile.mkdtemp())
        sid = "11111111-2222-3333-4444-555555555555"
        d = root / sid
        d.mkdir()
        (d / "metadata.json").write_text(f'{{"id": "{sid}"}}', encoding="utf-8")
        (d / "pipeline_events.jsonl").write_text(
            GOOD_1 + TORN + GOOD_2 + '{"kind":"tts_completed","utterance_id":1}\n', encoding="utf-8")
        (d / "events.jsonl").write_text(
            '{"event_type":"stt_timing","payload":{"elapsed_ms":100,"endpointing_ms":50}}\n'
            '{"event_type":"llm_timing","payload":{"elapsed_ms":200,"tokens":5}}\n'
            '{"event_type":"tts_timing","payload":{"elapsed_ms":300,"chars":9}}\n', encoding="utf-8")

        from unittest import mock
        with mock.patch.object(reader, "_resolve_session_dir", lambda _sid: d):
            result = reader._read_capture_artifacts(sid)
        utt = next(u for u in result["utterances"] if u["utterance_id"] == 1)
        self.assertIn("llm_timing", utt, "the LLM timing was dropped after the torn line")
        self.assertIn("tts_timing", utt)
        self.assertEqual(result["unreadable_event_lines"], 1)


class NoWholeFileParseTest(unittest.TestCase):
    """A line of a capture file is parsed in one place only.

    The fault was not one bad loop but the same loop written seven times, each
    wrapping a whole file in one `try`. Parsing a line anywhere but
    `captures/jsonl.py` is how that comes back, so it is refused here rather
    than rediscovered from a session with no metrics.
    """

    def test_no_module_parses_a_jsonl_line_itself(self):
        app_dir = Path(__file__).resolve().parent
        offenders = []
        for path in app_dir.rglob("*.py"):
            if path.name.startswith("test_") or path.name == "jsonl.py":
                continue
            for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
                if "json.loads(line" in line or "json.loads(l)" in line:
                    offenders.append(f"{path.relative_to(app_dir)}:{n}")
        self.assertEqual(offenders, [], "use captures.jsonl.parse_jsonl_line / iter_jsonl instead")


if __name__ == "__main__":
    unittest.main()
