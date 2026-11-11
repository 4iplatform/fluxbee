#!/usr/bin/env python3
"""The factory reset's take-down of the published pages (FINDINGS A-212), against a fake SY.admin.

Hermetic: no hive, no root. It loads packaging/fluxbee-factory-reset as a module and drives its
page steps (`unpublish_pages`, `take_down_late_pages`) and its node purges (`kill_user_nodes`)
through a stand-in for its `api`, with SY.admin's ledger in a temporary directory and a fake clock:

- every page that is up comes down by its publication_id, a page published meanwhile too, and a
  failure is retried while a pass takes one down;
- a page that never comes down is named, and the reset goes on;
- when SY.admin does not list the pages (an older SY.admin's 404, or SY.admin gone during the
  run), the page step goes on and names the pages that stay public from SY.admin's ledger, read
  only (a reset without SY.admin at its start stops before this step);
- no link's key is ever printed, whole or in part;
- each node's purge is asked once: SY.admin refuses none for a page (FINDINGS A-242), and a
  refusal of any other kind is the node's failure, counted, never asked again;
- once a node was removed, the last page step waits until 120 s after the last removal and takes
  down a page a publish under way then put up; with no node removed it waits for nothing; a page
  it cannot take down is counted.

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
import urllib.parse
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
    """SY.admin's two routes the page steps use, over a set of pages that are up, and a node's
    purge (kill_node), which leaves the node's pages up and refuses none for them (FINDINGS A-242);
    it fails the purges of the nodes in `refuse_purge`, as the orchestrator may."""

    def __init__(self, pages, refuse=(), refuse_once=(), published_meanwhile=None, answer=None,
                 refuse_purge=()):
        self.up = {page["publication_id"]: page for page in pages}
        self.refuse = set(refuse)
        self.refuse_once = set(refuse_once)
        self.meanwhile = published_meanwhile
        self.answer = answer
        self.refuse_purge = set(refuse_purge)
        self.unpublished = []
        self.purges = []

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
        if method == "DELETE" and path.startswith("/hives/") and "/nodes/" in path:
            assert body == {"force": False, "purge_instance": True}, body
            node = urllib.parse.unquote(path.rsplit("/", 1)[1])
            self.purges.append(node)
            if node in self.refuse_purge:
                return 502, {"status": "error", "action": "kill_node",
                             "error_code": "TIMER_PURGE_FAILED",
                             "error_detail": "SY.timer did not answer"}
            mine = [pid for pid, page in self.up.items() if page["publisher_l2_name"] == node]
            return 200, {"status": "ok", "action": "kill_node",
                         "payload": {"node_name": node,
                                     "pages_left_up": {"count": len(mine), "more": 0,
                                                       "pages": [{"publication_id": pid}
                                                                 for pid in mine]}}}
        return 404, {"status": "error", "error_code": "UNKNOWN_ROUTE"}


class NoKey:
    """What every test checks of the reset's output: no link's key, whole or in part."""

    def assert_no_key(self, text, records):
        for record in records:
            for part in (record["key"], record["key"][:16], record["key"][-16:]):
                self.assertNotIn(part, text, "a key of %s was printed" % record["publication_id"])


class PageStepTest(NoKey, unittest.TestCase):
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


class FakeClock:
    """The reset's clock and its sleep (take_down_late_pages' `now` and `sleep`): a sleep moves it
    on at once, and is recorded."""

    def __init__(self, at):
        self.at = at
        self.slept = []

    def now(self):
        return self.at

    def sleep(self, secs):
        self.slept.append(secs)
        self.at += secs


class NodePurgeTest(NoKey, unittest.TestCase):
    """Each node's purge is asked once: SY.admin refuses none for a page (FINDINGS A-242)."""

    def setUp(self):
        self.reset = load_reset()

    def purge(self, admin, nodes):
        self.reset.api = admin.api
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            failed = self.reset.kill_user_nodes(nodes)
        return failed, out.getvalue()

    def test_each_purge_is_asked_once_and_takes_no_page_down(self):
        page = ledger_record()
        admin = FakeAdmin([page])
        failed, out = self.purge(admin, [("worker1", "AI.sales@worker1"),
                                         ("worker1", "AI.quiet@worker1")])
        self.assertEqual(failed, [])
        self.assertEqual(admin.purges, ["AI.sales@worker1", "AI.quiet@worker1"])
        self.assertEqual(admin.unpublished, [])
        self.assertIn(page["publication_id"], admin.up)
        self.assertIn("OK    kill+purge AI.sales@worker1", out)
        self.assert_no_key(out, [page])

    def test_a_failed_purge_is_counted_and_not_asked_again(self):
        admin = FakeAdmin([], refuse_purge={"AI.sales@worker1"})
        failed, out = self.purge(admin, [("worker1", "AI.sales@worker1"),
                                         ("worker1", "AI.quiet@worker1")])
        self.assertEqual([name for name, _, _ in failed], ["AI.sales@worker1"])
        self.assertIn("TIMER_PURGE_FAILED", failed[0][2])
        self.assertEqual(admin.purges, ["AI.sales@worker1", "AI.quiet@worker1"])
        self.assertIn("FAIL  kill+purge AI.sales@worker1", out)

    def test_the_reset_reads_no_refusal_for_pages(self):
        # A-233's refusals are gone, and with them the text the reset waited on (FINDINGS A-242,
        # A-249): nothing left reads them.
        source = SCRIPT.read_text()
        for gone in ("NODE_HAS_PAGES", "TENANT_HAS_PAGES", "NODE_BEING_PURGED",
                     "PAGES_CHECK_FAILED", "is in flight"):
            self.assertNotIn(gone, source)


class LatePagesTest(NoKey, unittest.TestCase):
    """[8]: the pages a publish under way when its node was removed put up after [0]."""

    def setUp(self):
        self.reset = load_reset()
        self.clock = FakeClock(1000.0)

    def late(self, admin, removed_at, publishers_removed):
        self.reset.api = admin.api
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            result = self.reset.take_down_late_pages(removed_at, publishers_removed,
                                                     now=self.clock.now, sleep=self.clock.sleep)
        return result, out.getvalue()

    def test_it_waits_out_the_last_publish_then_takes_a_late_page_down(self):
        # 50 s after the last removal: 70 s are left of a publish's 120.
        late = ledger_record()
        admin = FakeAdmin([late])
        self.clock.at = 1050.0
        (done, failed), out = self.late(admin, 1000.0, True)
        self.assertEqual(self.clock.slept, [70.0])
        self.assertEqual((done, failed), (1, []))
        self.assertEqual(admin.unpublished, [late["publication_id"]])
        self.assertEqual(admin.up, {})
        self.assertIn("waiting 70s", out)
        self.assert_no_key(out, [late])

    def test_past_the_last_publish_it_waits_for_nothing(self):
        late = ledger_record()
        admin = FakeAdmin([late])
        self.clock.at = 1000.0 + self.reset.LAST_PUBLISH_SECS + 5
        (done, failed), out = self.late(admin, 1000.0, True)
        self.assertEqual(self.clock.slept, [])
        self.assertEqual((done, failed), (1, []))
        self.assertNotIn("waiting", out)

    def test_with_no_node_removed_it_waits_for_nothing_and_takes_nothing_down(self):
        page = ledger_record()
        admin = FakeAdmin([page])
        result, out = self.late(admin, 1000.0, False)
        self.assertIsNone(result)
        self.assertEqual(self.clock.slept, [])
        self.assertEqual(admin.unpublished, [])
        self.assertIn("no node was removed", out)

    def test_a_page_still_up_after_it_is_a_counted_failure(self):
        stuck = ledger_record()
        admin = FakeAdmin([stuck], refuse={stuck["publication_id"]})
        (done, failed), out = self.late(admin, 1000.0, True)
        self.assertEqual(done, 0)
        self.assertEqual([pid for pid, _ in failed], [stuck["publication_id"]])
        self.assertIn("FAIL %s" % stuck["publication_id"], out)
        self.assert_no_key(out, [stuck])

    def test_a_removed_node_is_a_workflow_a_user_node_or_a_nodes_ilk(self):
        nodes_removed = self.reset.nodes_removed
        self.assertTrue(nodes_removed([("worker1", "invoice")], [], []))
        self.assertTrue(nodes_removed([], [("worker1", "AI.sales@worker1")], []))
        # The ILK of a node that died on its own, purged at [3].
        self.assertTrue(nodes_removed([], [], [{"ilk_id": "ilk:x", "node_name": "AI.dead@worker1"}]))
        # A person's ILK publishes nothing.
        self.assertFalse(nodes_removed([], [], [{"ilk_id": "ilk:y", "ilk_type": "human"}]))
        self.assertFalse(nodes_removed([], [], []))


if __name__ == "__main__":
    result = unittest.main(exit=False, verbosity=2).result
    sys.exit(0 if result.wasSuccessful() else 1)
