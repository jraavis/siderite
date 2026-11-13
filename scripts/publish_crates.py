"""Publish the workspace crates to crates.io, skipping published versions.

Usage: python3 scripts/publish_crates.py [--execute]

Without --execute this is a dry run (`cargo publish --dry-run`). Before
anything is uploaded, every publishable crate is checked on crates.io: a
name that already exists must point at this workspace's repository, so a
release never uploads part of the workspace and then stops at a name that
belongs to someone else. Versions already on crates.io are excluded, so a
failed run can be repeated. `cargo publish --workspace` orders the rest by
their dependencies.

Exit status is 1 when the preflight or cargo fails and 2 on usage,
network or I/O errors.
"""

import argparse
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field

API = "https://crates.io/api/v1/crates/"
# crates.io asks automated clients for at most one request per second.
REQUEST_INTERVAL = 1.0


@dataclass(frozen=True)
class Crate:
    """A publishable workspace package.

    Attributes:
        name: The package name.
        version: The version this release would publish.
    """

    name: str
    version: str


@dataclass(frozen=True)
class Published:
    """What crates.io already holds for a name.

    Attributes:
        repository: The repository URL of the latest version, if any.
        versions: Every version published under the name.
    """

    repository: str
    versions: frozenset = field(default_factory=frozenset)


@dataclass
class Plan:
    """The outcome of the preflight.

    Attributes:
        publish: Crates to upload.
        skip: Crates whose version is already on crates.io.
        conflicts: Messages for names that belong to another repository.
    """

    publish: list = field(default_factory=list)
    skip: list = field(default_factory=list)
    conflicts: list = field(default_factory=list)


def normalize_repository(url):
    """Reduce a repository URL to a comparable form.

    Args:
        url: A repository URL, possibly None or empty.

    Returns:
        The lower-cased URL without scheme, "www.", ".git" or trailing
        slashes.
    """
    text = (url or "").strip().lower()
    for prefix in ("https://", "http://", "www."):
        text = text.removeprefix(prefix)
    return text.rstrip("/").removesuffix(".git")


def workspace_crates(metadata):
    """List the publishable packages of `cargo metadata --no-deps` output.

    Args:
        metadata: The parsed metadata.

    Returns:
        A tuple of the crates, in metadata order, and the workspace
        repository URL.
    """
    members = set(metadata["workspace_members"])
    crates = []
    repository = ""
    for package in metadata["packages"]:
        if package["id"] not in members or package.get("publish") == []:
            continue
        crates.append(Crate(package["name"], package["version"]))
        repository = repository or package.get("repository") or ""
    return crates, repository


def make_plan(crates, published, repository):
    """Decide what to upload and what blocks the release.

    Args:
        crates: The publishable crates.
        published: Mapping of crate name to Published, for names that
            exist on crates.io.
        repository: This workspace's repository URL.

    Returns:
        The Plan.
    """
    plan = Plan()
    ours = normalize_repository(repository)
    for crate in crates:
        existing = published.get(crate.name)
        if existing is None:
            plan.publish.append(crate)
        elif normalize_repository(existing.repository) != ours:
            plan.conflicts.append(
                f"{crate.name}: crates.io already has this name for "
                f"{existing.repository or 'an unknown repository'}"
            )
        elif crate.version in existing.versions:
            plan.skip.append(crate)
        else:
            plan.publish.append(crate)
    return plan


def fetch_published(name, user_agent):
    """Look a crate name up on crates.io.

    Args:
        name: The crate name.
        user_agent: The User-Agent crates.io requires of API clients.

    Returns:
        The Published record, or None when the name is free.

    Raises:
        urllib.error.URLError: The request failed for another reason.
    """
    request = urllib.request.Request(
        API + name, headers={"User-Agent": user_agent}
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            body = json.load(response)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise
    versions = frozenset(v["num"] for v in body.get("versions") or [])
    return Published(body["crate"].get("repository") or "", versions)


def cargo_metadata():
    """Run `cargo metadata` for the workspace.

    Returns:
        The parsed metadata.
    """
    output = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(output)


def publish_command(plan, execute):
    """Build the cargo command that uploads the plan.

    Args:
        plan: The Plan.
        execute: Whether to upload, rather than dry run.

    Returns:
        The argument list.
    """
    command = ["cargo", "publish", "--workspace", "--locked"]
    if not execute:
        command.append("--dry-run")
    for crate in plan.skip:
        command += ["--exclude", crate.name]
    return command


def main(argv=None):
    """Run the preflight, then cargo publish.

    Args:
        argv: Command-line arguments without the program name.

    Returns:
        The process exit status.
    """
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--execute", action="store_true", help="upload instead of dry run"
    )
    args = parser.parse_args(argv)
    try:
        crates, repository = workspace_crates(cargo_metadata())
        user_agent = f"siderite-release-script ({repository})"
        published = {}
        for crate in crates:
            found = fetch_published(crate.name, user_agent)
            if found is not None:
                published[crate.name] = found
            time.sleep(REQUEST_INTERVAL)
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    plan = make_plan(crates, published, repository)
    for crate in plan.skip:
        print(f"skip {crate.name} {crate.version}: already on crates.io")
    if plan.conflicts:
        print("error: crate names owned elsewhere:", file=sys.stderr)
        for message in plan.conflicts:
            print(f"  {message}", file=sys.stderr)
        return 1
    if not plan.publish:
        print("nothing to publish")
        return 0
    command = publish_command(plan, args.execute)
    print("+ " + " ".join(command), flush=True)
    return subprocess.run(command, check=False).returncode


if __name__ == "__main__":
    sys.exit(main())
