#!/usr/bin/env python3
"""The factory reset's take-down of the published pages (FINDINGS A-212), against a fake SY.admin.

Hermetic: no hive, no root. It loads packaging/fluxbee-factory-reset as a module and drives its
page step (`unpublish_pages`) through a stand-in for its `api`, with SY.admin's ledger in a
temporary directory:

- every page that is up comes down by its publication_id, a page published meanwhile too, and a
  failure is retried while a pass takes one down;
- a page that never comes down is named, and the reset goes on;
- when SY.admin does not list the pages (down, or older than the route), the reset goes on and
  names the pages that stay public from SY.admin's ledger, read only;
- no link's key is ever printed, whole or in part.

Usage: python3 scripts/factory_reset_pages_check.py
"""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
import pathlib
import secrets
import sys
import tempfile
import time
import unittest
import uuid

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "packaging" / "fluxbee-factory-reset"


def load_reset():
    # No bytecode cache beside the packaged script.
    sys.dont_write_bytecode = True
    loader = importlib.machinery.SourceFileLoader("fluxbee_factory_reset", str(SCRIPT))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def ledger_record(status="published", created_at=100, expires_in=3600):
    """A row of SY.admin's ledger, its link's key included, as publish_artifact writes it."""
    return {
        "publication_id": "pub:%s" % uuid.uuid4(),
        "key": secrets.token_hex(32),
        "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e",
        "publisher_l2_name": "AI.sales@worker1",
        "blob_ref": {"type": "blob_ref", "blob_name": "report_0123456789abcdef.html", "size": 3,
                     "mime": "text/html", "filename_original": "report.html",
                     "spool_day": "2026-11-10"},
        "public_name": "ab" * 32,
        "sha256": "ab" * 32,
        "size": 3,
        "content_type": "text/html; charset=utf-8",
        "presentation": "inline",
        "content_policy": "sandboxed-html-v1",
        "expires_at": int(time.time()) + expires_in,
        "edge_node": "SY.edge@ingress1",
        "status": status,
        "created_at": created_at,
    }


def listed_row(record):
    """The row list_published_artifacts gives of `record`: never its key."""
    return {field: record[field] for field in (
        "publication_id", "status", "content_type", "presentation", "size", "tenant_id",
        "publisher_l2_name", "edge_node", "created_at", "expires_at")} | {
        "filename_original": record["blob_ref"]["filename_original"]}


class FakeAdmin:
    """SY.admin's two routes the step uses, over a set of pages that are up."""

    def __init__(self, pages, refuse=(), refuse_once=(), published_meanwhile=None, answer=None):
        self.up = {page["publication_id"]: page for page in pages}
        self.refuse = set(refuse)
        self.refuse_once = set(refuse_once)
        self.meanwhile = published_meanwhile
        self.answer = answer
        self.unpublished = []

    def api(self, method, path, body=None, host=None, timeout=60):
        if self.answer is not None:
            return self.answer
        if (method, path) == ("GET", "/artifacts"):
            rows = [listed_row(page) for page in self.up.values()]
            if self.meanwhile is not None:
                self.up[self.meanwhile["publication_id"]] = self.meanwhile
                self.meanwhile = None
            return 200, {"status": "ok", "action": "list_published_artifacts",
                         "payload": {"publications": rows, "count": len(rows), "total": len(rows)}}
        if (method, path) == ("POST", "/artifacts/unpublish"):
            assert set(body) == {"publication_id"}, body
            pid = body["publication_id"]
            if pid in self.refuse or pid in self.refuse_once:
                self.refuse_once.discard(pid)
                return 502, {"status": "error", "action": "unpublish_artifact",
                             "error_code": "EDGE_UNPUBLISH_FAILED", "error_detail": "edge unavailable"}
            taken = self.up.pop(pid, None) is not None
            if taken:
                self.unpublished.append(pid)
            return 200, {"status": "ok", "action": "unpublish_artifact",
                         "payload": {"publication_id": pid, "unpublished": taken}}
        return 404, {"status": "error", "error_code": "UNKNOWN_ROUTE"}


class PageStepTest(unittest.TestCase):
    def setUp(self):
        self.reset = load_reset()
        self.tmp = tempfile.TemporaryDirectory()
        self.reset.PUBLICATION_LEDGER = os.path.join(self.tmp.name, "publications.json")

    def tearDown(self):
        self.tmp.cleanup()

    def run_step(self, admin):
        self.reset.api = admin.api
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            result = self.reset.unpublish_pages()
        return result, out.getvalue()

    def write_ledger(self, records):
        with open(self.reset.PUBLICATION_LEDGER, "w") as f:
            json.dump({"schema_version": 1,
                       "publications": {r["publication_id"]: r for r in records}}, f)

    def assert_no_key(self, text, records):
        for record in records:
            for part in (record["key"], record["key"][:16], record["key"][-16:]):
                self.assertNotIn(part, text, "a key of %s was printed" % record["publication_id"])

    def test_every_page_comes_down_by_its_id_a_late_one_too(self):
        pages = [ledger_record(created_at=n) for n in (3, 2)]
        late = ledger_record(created_at=4)
        admin = FakeAdmin(pages, refuse_once={pages[1]["publication_id"]}, published_meanwhile=late)
        (done, failed), out = self.run_step(admin)
        self.assertEqual(failed, [])
        self.assertEqual(done, 3)
        self.assertEqual(sorted(admin.unpublished),
                         sorted(p["publication_id"] for p in pages + [late]))
        self.assertEqual(admin.up, {})
        self.assertIn("pages: 3 taken down, 0 failed", out)
        self.assert_no_key(out, pages + [late])

    def test_a_page_that_never_comes_down_is_named_and_the_step_ends(self):
        pages = [ledger_record(created_at=n) for n in (2, 1)]
        stuck = pages[1]["publication_id"]
        (done, failed), out = self.run_step(FakeAdmin(pages, refuse={stuck}))
        self.assertEqual(done, 1)
        self.assertEqual([pid for pid, _ in failed], [stuck])
        self.assertIn("EDGE_UNPUBLISH_FAILED", failed[0][1])
        self.assertIn("FAIL %s" % stuck, out)
        self.assert_no_key(out, pages)

    def test_without_sy_admins_list_the_ledger_names_what_stays_public(self):
        up = ledger_record(created_at=3)
        half_published = ledger_record(status="curated", created_at=2)
        down = [ledger_record(status="unpublished"), ledger_record(status="expired"),
                ledger_record(created_at=5, expires_in=-60)]
        self.write_ledger([up, half_published] + down)
        for answer in [(0, "connection error: [Errno 111] Connection refused"),
                       (404, {"status": "error", "error_code": "UNKNOWN_ROUTE"})]:
            (done, failed), out = self.run_step(FakeAdmin([], answer=answer))
            self.assertEqual(done, 0)
            self.assertEqual(failed[0][0], "published pages")
            self.assertIn("SY.admin did not list the published pages", out)
            self.assertIn("2 stay public", out)
            self.assertLess(out.index(up["publication_id"]), out.index(half_published["publication_id"]))
            for record in down:
                self.assertNotIn(record["publication_id"], out)
            self.assert_no_key(out, [up, half_published] + down)

    def test_a_ledger_that_cannot_be_read_is_said_and_none_is_no_page(self):
        admin = FakeAdmin([], answer=(0, "connection error"))
        (_, failed), out = self.run_step(admin)
        self.assertEqual(len(failed), 1)
        self.assertIn("0 stay public", out)
        with open(self.reset.PUBLICATION_LEDGER, "w") as f:
            f.write("{ not json")
        (_, failed), out = self.run_step(admin)
        self.assertEqual(len(failed), 1)
        self.assertIn("cannot be read either", out)

    def test_a_page_line_names_the_page_never_its_key(self):
        record = ledger_record()
        for page in (record, listed_row(record), dict(record, expires_at="soon")):
            line = self.reset.page_line(page)
            self.assertIn(record["publication_id"], line)
            self.assertIn("report.html", line)
            self.assert_no_key(line, [record])


if __name__ == "__main__":
    result = unittest.main(exit=False, verbosity=2).result
    sys.exit(0 if result.wasSuccessful() else 1)
