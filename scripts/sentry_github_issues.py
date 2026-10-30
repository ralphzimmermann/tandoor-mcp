#!/usr/bin/env python3
"""Create GitHub tickets for repeated Sentry errors without a paid integration.

Keep this file in sync with recipes/scripts/sentry_github_issues.py.
Only aggregate issue metadata is copied; event payloads are never requested.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any

MARKER_RE = re.compile(r"<!-- sentry-issue-id: ([^ ]+) -->")


class ApiError(RuntimeError):
    """An HTTP API request failed."""


class ApiClient:
    def __init__(self, base_url: str, token: str, user_agent: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.user_agent = user_agent

    def request(
        self, method: str, path: str, payload: dict[str, Any] | None = None
    ) -> tuple[Any, dict[str, str]]:
        data = None if payload is None else json.dumps(payload).encode()
        request = urllib.request.Request(
            f"{self.base_url}{path}",
            data=data,
            method=method,
            headers={
                "Accept": "application/vnd.github+json, application/json",
                "Authorization": f"Bearer {self.token}",
                "Content-Type": "application/json",
                "User-Agent": self.user_agent,
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                body = response.read()
                return (
                    json.loads(body) if body else None,
                    {key.lower(): value for key, value in response.headers.items()},
                )
        except urllib.error.HTTPError as error:
            body = error.read().decode(errors="replace")
            raise ApiError(f"{method} {path} returned {error.code}: {body}") from error
        except urllib.error.URLError as error:
            raise ApiError(f"{method} {path} failed: {error.reason}") from error


@dataclass(frozen=True)
class Config:
    sentry_token: str
    github_token: str
    github_repository: str
    sentry_org: str
    sentry_projects: tuple[str, ...]
    environment: str
    minimum_events: int
    window: str

    @classmethod
    def from_env(cls) -> "Config":
        def required(name: str) -> str:
            value = os.getenv(name, "").strip()
            if not value:
                raise ValueError(f"{name} must be set")
            return value

        projects = tuple(
            project.strip()
            for project in required("SENTRY_PROJECTS").split(",")
            if project.strip()
        )
        minimum_events = int(os.getenv("SENTRY_MIN_EVENTS", "3"))
        if minimum_events < 2:
            raise ValueError("SENTRY_MIN_EVENTS must be at least 2")
        window = os.getenv("SENTRY_WINDOW", "1h").strip()
        if not re.fullmatch(r"[1-9][0-9]*[mhdw]", window):
            raise ValueError("SENTRY_WINDOW must look like 15m, 1h, 1d, or 1w")
        return cls(
            sentry_token=required("SENTRY_API_TOKEN"),
            github_token=required("GITHUB_TOKEN"),
            github_repository=required("GITHUB_REPOSITORY"),
            sentry_org=os.getenv("SENTRY_ORG", "virtuflight").strip(),
            sentry_projects=projects,
            environment=os.getenv("SENTRY_ENVIRONMENT", "production").strip(),
            minimum_events=minimum_events,
            window=window,
        )


def marker(issue_id: str) -> str:
    return f"<!-- sentry-issue-id: {issue_id} -->"


def existing_by_sentry_id(tickets: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    indexed = {}
    for ticket in tickets:
        match = MARKER_RE.search(ticket.get("body") or "")
        if match:
            indexed[match.group(1)] = ticket
    return indexed


def parse_timestamp(value: str | None) -> dt.datetime | None:
    if not value:
        return None
    return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))


def should_reopen(ticket: dict[str, Any], events: list[dict[str, Any]]) -> bool:
    if ticket.get("state") != "closed":
        return False
    closed_at = parse_timestamp(ticket.get("closed_at"))
    if closed_at is None:
        return False
    return any(
        (created := parse_timestamp(event.get("dateCreated"))) is not None
        and created > closed_at
        for event in events
    )


def render_ticket(issue: dict[str, Any], event_count: int, environment: str) -> tuple[str, str]:
    project = issue.get("project") or {}
    metadata = issue.get("metadata") or {}
    release = issue.get("lastRelease") or {}
    release_name = release.get("version") if isinstance(release, dict) else release
    title = f"[Sentry] {issue.get('shortId', issue['id'])}: {issue.get('title', 'Error')}"
    body = "\n".join(
        [
            marker(str(issue["id"])),
            "",
            f"Sentry observed a repeated {environment} error.",
            "",
            f"- Project: `{project.get('slug', 'unknown')}`",
            f"- Environment: `{environment}`",
            f"- Level: `{issue.get('level', 'error')}`",
            f"- Events in threshold window: `{event_count}`",
            f"- Lifetime events: `{issue.get('count', 'unknown')}`",
            f"- Affected users: `{issue.get('userCount', 'unknown')}`",
            f"- Culprit: `{issue.get('culprit') or metadata.get('function') or 'unknown'}`",
            f"- Release: `{release_name or 'unknown'}`",
            f"- First seen: `{issue.get('firstSeen', 'unknown')}`",
            f"- Last seen: `{issue.get('lastSeen', 'unknown')}`",
            "",
            f"[Open the Sentry issue]({issue.get('permalink', '')})",
            "",
            "_Generated from aggregate metadata; no event payload or user data was copied._",
        ]
    )
    return title[:256], body


def list_github_tickets(client: ApiClient, repository: str) -> list[dict[str, Any]]:
    tickets: list[dict[str, Any]] = []
    page = 1
    while True:
        query = urllib.parse.urlencode(
            {"state": "all", "labels": "sentry", "per_page": 100, "page": page}
        )
        raw_batch, _ = client.request("GET", f"/repos/{repository}/issues?{query}")
        tickets.extend(item for item in raw_batch if "pull_request" not in item)
        if len(raw_batch) < 100:
            return tickets
        page += 1


def ensure_label(client: ApiClient, repository: str, dry_run: bool) -> None:
    path = f"/repos/{repository}/labels/sentry"
    try:
        client.request("GET", path)
    except ApiError as error:
        if "returned 404:" not in str(error):
            raise
        if dry_run:
            print("Would create GitHub label: sentry")
        else:
            client.request(
                "POST",
                f"/repos/{repository}/labels",
                {"name": "sentry", "color": "b60205", "description": "Repeated Sentry error"},
            )


def sentry_candidates(
    client: ApiClient, config: Config
) -> list[dict[str, Any]]:
    candidates: dict[str, dict[str, Any]] = {}
    query_text = f"is:unresolved lastSeen:-{config.window} level:[error,fatal]"
    for project in config.sentry_projects:
        query = urllib.parse.urlencode(
            {
                "query": query_text,
                "environment": config.environment,
                "statsPeriod": config.window,
                "limit": 100,
            }
        )
        path = (
            f"/projects/{config.sentry_org}/{urllib.parse.quote(project)}/issues/?{query}"
        )
        issues, _ = client.request("GET", path)
        for issue in issues:
            candidates[str(issue["id"])] = issue
    return list(candidates.values())


def recent_events(
    client: ApiClient, config: Config, issue_id: str
) -> list[dict[str, Any]]:
    query = urllib.parse.urlencode(
        {
            "statsPeriod": config.window,
            "environment": config.environment,
            "per_page": config.minimum_events,
        }
    )
    events, _ = client.request(
        "GET", f"/organizations/{config.sentry_org}/issues/{issue_id}/events/?{query}"
    )
    return events


def run(config: Config, dry_run: bool) -> int:
    sentry = ApiClient("https://us.sentry.io/api/0", config.sentry_token, "sentry-ticket-poller")
    github = ApiClient("https://api.github.com", config.github_token, "sentry-ticket-poller")
    ensure_label(github, config.github_repository, dry_run)
    tickets = existing_by_sentry_id(
        list_github_tickets(github, config.github_repository)
    )
    changed = 0

    for issue in sentry_candidates(sentry, config):
        issue_id = str(issue["id"])
        events = recent_events(sentry, config, issue_id)
        if len(events) < config.minimum_events:
            continue

        existing = tickets.get(issue_id)
        if existing is None:
            title, body = render_ticket(issue, len(events), config.environment)
            if dry_run:
                print(f"Would create: {title}")
            else:
                github.request(
                    "POST",
                    f"/repos/{config.github_repository}/issues",
                    {"title": title, "body": body, "labels": ["sentry"]},
                )
            changed += 1
        elif should_reopen(existing, events):
            number = existing["number"]
            if dry_run:
                print(f"Would reopen GitHub issue #{number} for {issue_id}")
            else:
                github.request(
                    "PATCH",
                    f"/repos/{config.github_repository}/issues/{number}",
                    {"state": "open"},
                )
                github.request(
                    "POST",
                    f"/repos/{config.github_repository}/issues/{number}/comments",
                    {
                        "body": (
                            f"Sentry recorded at least {config.minimum_events} new events "
                            f"within {config.window} after this ticket was closed."
                        )
                    },
                )
            changed += 1

    print(f"{'Would change' if dry_run else 'Changed'} {changed} GitHub issue(s).")
    return changed


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    try:
        run(Config.from_env(), args.dry_run)
    except (ApiError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
