import unittest

from scripts.sentry_github_issues import (
    existing_by_sentry_id,
    marker,
    render_ticket,
    should_reopen,
)


class SentryGitHubIssuesTest(unittest.TestCase):
    def test_existing_tickets_are_indexed_by_hidden_marker(self):
        tickets = [
            {"number": 4, "body": f"Details\n{marker('123')}"},
            {"number": 5, "body": "Unrelated"},
        ]
        self.assertEqual(existing_by_sentry_id(tickets), {"123": tickets[0]})

    def test_closed_ticket_reopens_only_for_a_later_event(self):
        ticket = {"state": "closed", "closed_at": "2026-10-30T10:00:00Z"}
        old = [{"dateCreated": "2026-10-30T09:59:59Z"}]
        new = old + [{"dateCreated": "2026-10-30T10:00:01Z"}]
        self.assertFalse(should_reopen(ticket, old))
        self.assertTrue(should_reopen(ticket, new))
        self.assertFalse(should_reopen({"state": "open"}, new))

    def test_ticket_contains_only_aggregate_metadata(self):
        issue = {
            "id": "123",
            "shortId": "TANDOOR-MCP-1",
            "title": "ValueError",
            "project": {"slug": "tandoor-mcp"},
            "level": "error",
            "count": "4",
            "userCount": 2,
            "firstSeen": "2026-10-30T09:00:00Z",
            "lastSeen": "2026-10-30T10:00:00Z",
            "permalink": "https://virtuflight.sentry.io/issues/123/",
            "metadata": {"function": "serve"},
        }
        title, body = render_ticket(issue, 3, "production")
        self.assertIn("TANDOOR-MCP-1", title)
        self.assertIn(marker("123"), body)
        self.assertIn("Events in threshold window: `3`", body)
        self.assertNotIn("request", body.lower())

    def test_timestamp_comparisons_are_timezone_aware(self):
        ticket = {"state": "closed", "closed_at": "2026-10-30T10:00:00+01:00"}
        event = [{"dateCreated": "2026-10-30T09:00:01Z"}]
        self.assertTrue(should_reopen(ticket, event))


if __name__ == "__main__":
    unittest.main()
