"""Tests for release_notes.py and publish_crates.py (no network)."""

import unittest

from publish_crates import (
    Crate,
    Published,
    make_plan,
    normalize_repository,
    publish_command,
    workspace_crates,
)
from release_notes import extract_section

CHANGELOG = """# Changelog

## [Unreleased]

- Pending.

## [0.2.0] - 2026-12-01

### Added
- New thing.

## [0.1.0]

- First.

[0.2.0]: https://example.com/compare/v0.1.0...v0.2.0
"""

REPO = "https://github.com/acme/siderite"


class ReleaseNotesTests(unittest.TestCase):
    """A version's section is cut out between headings."""

    def test_middle_section_stops_at_next_heading(self):
        self.assertEqual(
            extract_section(CHANGELOG, "0.2.0"), "### Added\n- New thing."
        )

    def test_tag_prefix_is_accepted(self):
        self.assertEqual(extract_section(CHANGELOG, "v0.1.0"), "- First.")

    def test_last_section_drops_link_definitions(self):
        self.assertNotIn("example.com", extract_section(CHANGELOG, "0.1.0"))

    def test_missing_version_is_none(self):
        self.assertIsNone(extract_section(CHANGELOG, "9.9.9"))


class PublishPlanTests(unittest.TestCase):
    """The preflight blocks foreign names and skips published versions."""

    crates = [Crate("siderite-core", "0.2.0"), Crate("siderite", "0.2.0")]

    def test_free_names_are_published(self):
        plan = make_plan(self.crates, {}, REPO)
        self.assertEqual(plan.publish, self.crates)
        self.assertFalse(plan.conflicts)

    def test_name_from_another_repository_conflicts(self):
        other = Published("https://github.com/someone/else", frozenset())
        plan = make_plan(self.crates, {"siderite": other}, REPO)
        self.assertEqual(len(plan.conflicts), 1)
        self.assertIn("someone/else", plan.conflicts[0])

    def test_published_version_is_skipped_and_excluded(self):
        ours = Published(REPO + ".git/", frozenset({"0.2.0"}))
        plan = make_plan(self.crates, {"siderite-core": ours}, REPO)
        self.assertEqual(plan.skip, [self.crates[0]])
        self.assertEqual(
            publish_command(plan, execute=True)[-2:],
            ["--exclude", "siderite-core"],
        )

    def test_dry_run_is_the_default_command(self):
        plan = make_plan(self.crates, {}, REPO)
        self.assertIn("--dry-run", publish_command(plan, execute=False))
        self.assertNotIn("--dry-run", publish_command(plan, execute=True))

    def test_repository_urls_normalize(self):
        self.assertEqual(
            normalize_repository("HTTPS://www.GitHub.com/a/b.git/"),
            "github.com/a/b",
        )
        self.assertEqual(normalize_repository(None), "")

    def test_unpublishable_and_foreign_packages_are_ignored(self):
        metadata = {
            "workspace_members": ["a", "b"],
            "packages": [
                {"id": "a", "name": "a", "version": "1.0.0",
                 "repository": REPO},
                {"id": "b", "name": "b", "version": "1.0.0",
                 "publish": []},
                {"id": "c", "name": "c", "version": "1.0.0"},
            ],
        }
        self.assertEqual(
            workspace_crates(metadata), ([Crate("a", "1.0.0")], REPO)
        )


if __name__ == "__main__":
    unittest.main()
