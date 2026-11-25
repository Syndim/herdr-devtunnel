[private]
default:
    @just --list

[doc("Bump the first nonzero version component with --major, or the next component with --minor (requires just 1.46+ and Python 3.11+)")]
[arg("major", long="major", value="1")]
[arg("minor", long="minor", value="1")]
[script("python3")]
bump-version $major="0" $minor="0":
    import os
    import re
    import subprocess
    import tomllib
    from pathlib import Path

    flags = (os.environ["major"], os.environ["minor"])
    if flags not in [("1", "0"), ("0", "1")]:
        raise SystemExit("Usage: just bump-version --major | --minor")

    manifest = Path("Cargo.toml")
    original = manifest.read_bytes()
    version = tomllib.loads(original.decode("utf-8"))["package"]["version"]
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)(?:\.(?:0|[1-9][0-9]*)){2}", version):
        raise SystemExit(f"Expected a MAJOR.MINOR.PATCH package version, got {version!r}")

    components = list(map(int, version.split(".")))
    first_nonzero = next((index for index, value in enumerate(components) if value), None)
    if first_nonzero is None:
        raise SystemExit("Cannot bump 0.0.0: it has no nonzero version component")
    index = first_nonzero + int(flags[1])
    if index >= len(components):
        raise SystemExit(f"Cannot bump {version} with --minor: there is no component after the first nonzero one")
    components[index:] = [components[index] + 1] + [0] * (len(components) - index - 1)
    new_version = ".".join(map(str, components))
    pattern = rb'(?m)^(\[package\][ \t]*\r?\n(?:(?![ \t]*\[)[^\n]*\n)*?version[ \t]*=[ \t]*")'
    updated, count = re.subn(pattern + re.escape(version.encode()) + rb'(")',
                             lambda match: match[1] + new_version.encode() + match[2], original)
    if count != 1 or tomllib.loads(updated.decode("utf-8"))["package"]["version"] != new_version:
        raise SystemExit("Could not update the Cargo package version assignment")

    lockfile = Path("Cargo.lock")
    original_lock = lockfile.read_bytes()
    try:
        manifest.write_bytes(updated)
        subprocess.run(["cargo", "update", "--workspace", "--offline"], check=True)
    except (OSError, subprocess.CalledProcessError) as error:
        manifest.write_bytes(original)
        lockfile.write_bytes(original_lock)
        raise SystemExit(f"Version bump failed: {error}")
    print(f"{version} -> {new_version}")

[doc("Tag the latest origin/master version and push the tag (requires Python 3.11+)")]
[script("python3")]
release-tag:
    import re
    import subprocess
    import tomllib

    def git(*args):
        return subprocess.check_output(["git", *args], text=True).strip()

    fetch_url = git("remote", "get-url", "origin")
    push_urls = git("remote", "get-url", "--push", "--all", "origin").splitlines()
    if push_urls != [fetch_url]:
        raise SystemExit("origin fetch and push URLs differ; refusing to tag a different repository")
    subprocess.run(["git", "fetch", "--no-tags", "origin", "master"], check=True)
    commit = git("rev-parse", "FETCH_HEAD^{commit}")
    manifest = git("show", f"{commit}:Cargo.toml")
    version = tomllib.loads(manifest)["package"]["version"]
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)(?:\.(?:0|[1-9][0-9]*)){2}", version):
        raise SystemExit(f"Expected a MAJOR.MINOR.PATCH package version, got {version!r}")
    tag = f"v{version}"
    ref = f"refs/tags/{tag}"
    local = subprocess.run(["git", "show-ref", "--verify", "--quiet", ref])
    if local.returncode not in (0, 1):
        raise SystemExit("Could not check local tags")
    if local.returncode == 0:
        raise SystemExit(f"Tag {tag} already exists locally; nothing was pushed")
    if git("ls-remote", "--tags", "origin", ref):
        raise SystemExit(f"Tag {tag} already exists on origin; nothing was pushed")
    master = git("ls-remote", "--heads", "origin", "refs/heads/master").split()
    if len(master) != 2 or master[0] != commit:
        raise SystemExit("origin/master is missing or changed; rerun the recipe")
    subprocess.run(["git", "tag", "-a", tag, commit, "-m", tag], check=True)
    tag_object = git("rev-parse", ref)
    try:
        subprocess.run(["git", "push", "origin", f"{tag_object}:{ref}"], check=True)
    except subprocess.CalledProcessError:
        raise SystemExit(f"Push failed or was not confirmed. Local tag {tag} remains; check origin before removing it and retrying.")
    print(f"Pushed {tag} for {commit}")
