#!/usr/bin/env python3
"""Refresh the fork-only contributor table between the file markers."""

from collections import Counter
from datetime import datetime, timezone
import json
import os
import re
import sys
import urllib.request

REPO = "AetherBrowser/aether"
UPSTREAM_REPO = "servo/servo"
CONTRIBUTORS_FILE_PATH = os.environ.get("CONTRIBUTORS_FILE_PATH", "CONTRIBUTING.md")
TOKEN = os.environ.get("GITHUB_TOKEN", "")


def gh_get(url):
    headers = {
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2026-03-10",
    }
    if TOKEN:
        headers["Authorization"] = f"Bearer {TOKEN}"
    req = urllib.request.Request(url, headers=headers)
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read().decode())


def build_table(rows, avatars, start_rank=1):
    lines = [
        "| Rank | Contributor | Commits |",
        "| :---: | :--- | :---: |",
    ]
    for offset, (login, total) in enumerate(rows):
        rank = start_rank + offset
        avatar_url = avatars.get(login, "https://github.com/identicons/jasonlong.png")
        avatar = f'<img src="{avatar_url}&s=32" width="24" height="24" align="absmiddle" />'
        user_cell = f'{avatar} [**@{login}**](https://github.com/{login})'
        badge = f'![Commits](https://img.shields.io/badge/commits-{total}-blue?style=flat-square)'
        lines.append(f"| #{rank} | {user_cell} | {badge} |")
    return "\n".join(lines)


def main():
    comp = gh_get(
        f"https://api.github.com/repos/{REPO}/compare/servo:main...AetherBrowser:main"
    )
    target_total = comp.get("ahead_by", 0)
    print(f"Objective : {target_total} commits on this fork")

    upstream_shas = set()
    for page in range(1, 15):
        up_commits = gh_get(
            f"https://api.github.com/repos/{UPSTREAM_REPO}/commits?sha=main&per_page=100&page={page}"
        )
        if not up_commits:
            break
        for commit in up_commits:
            upstream_shas.add(commit["sha"])

    counts = Counter()
    avatars = {}
    fork_commits_found = 0
    page = 1

    while fork_commits_found < target_total:
        commits = gh_get(
            f"https://api.github.com/repos/{REPO}/commits?sha=main&per_page=100&page={page}"
        )
        if not commits:
            break

        for commit in commits:
            if commit["sha"] in upstream_shas:
                continue

            fork_commits_found += 1
            author = commit.get("author")
            login = author["login"] if author else commit["commit"]["author"]["name"]

            counts[login] += 1
            if author and "avatar_url" in author:
                avatars[login] = author["avatar_url"]

            if fork_commits_found >= target_total:
                break

        page += 1

    print(f"Total commits fork analyzed : {fork_commits_found} / {target_total}")
    all_contributors = counts.most_common()
    for idx, (login, total) in enumerate(all_contributors, start=1):
        print(f"  #{idx} {login}: {total}")

    today_str = datetime.now(timezone.utc).astimezone().strftime("%d %b %Y")
    section_parts = [
        f"_Last update : {today_str}_",
        "",
        build_table(all_contributors[:10], avatars, start_rank=1),
    ]
    rest = all_contributors[10:]
    if rest:
        section_parts.extend(
            [
                "",
                "<details>",
                f"<summary>See all contributors ({len(all_contributors)} total)</summary>",
                "",
                build_table(rest, avatars, start_rank=11),
                "",
                "</details>",
            ]
        )
    section_md = "\n".join(section_parts)

    with open(CONTRIBUTORS_FILE_PATH, "r", encoding="utf-8") as handle:
        readme = handle.read()

    pattern = r"(<!-- CONTRIBUTORS_START -->)(.*?)(<!-- CONTRIBUTORS_END -->)"
    new_readme, count = re.subn(
        pattern,
        lambda match: f"{match.group(1)}\n{section_md}\n{match.group(3)}",
        readme,
        count=1,
        flags=re.DOTALL,
    )
    if count != 1:
        print(f"Markers not found in {CONTRIBUTORS_FILE_PATH}", file=sys.stderr)
        return 1

    with open(CONTRIBUTORS_FILE_PATH, "w", encoding="utf-8") as handle:
        handle.write(new_readme)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
